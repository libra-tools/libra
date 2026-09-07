//! `libra worktree` command implementation.
//!
//! Boundary: manages linked worktree metadata and filesystem layout while preserving
//! main-worktree safety invariants. Command tests cover add/list/remove, duplicate
//! paths, and main-worktree protection.

use std::{
    collections::HashSet,
    env, fs, io,
    path::{Path, PathBuf},
};

use clap::{Parser, Subcommand};
use sea_orm::{ConnectionTrait, Statement};
use serde::{Deserialize, Serialize};

#[cfg(unix)]
use crate::utils::fuse as fuse_utils;
use crate::{
    command::restore::{self, RestoreArgs},
    internal::{
        branch::Branch,
        head::Head,
        sequencer::WorktreeControl,
        workspace::{
            self, RepoIdentity, WorkspaceKind, WorkspaceRecord, WorkspaceState, WorkspaceStore,
        },
    },
    utils::{
        error::{CliError, CliResult, StableErrorCode},
        output::{OutputConfig, emit_json_data},
        util,
    },
};

/// `--help` examples shown in `libra worktree --help` output.
pub const WORKTREE_EXAMPLES: &str = "\
 EXAMPLES:
     libra worktree add ../feature-x                Create a linked worktree (detached at
                                                    the source commit)
     libra worktree add ../fix-1 hotfix             Check the existing branch `hotfix` out
     libra worktree add --detach ../probe v1.2.0    Detached worktree at a commit-ish
     libra worktree add -b topic ../topic main      Create branch `topic` from `main` and
                                                    check it out
     libra worktree list                            List every registered worktree
     libra worktree list --porcelain                Machine-readable worktree list
     libra worktree lock ../feature-x --reason wip  Lock a worktree to prevent prune/remove
     libra worktree unlock ../feature-x             Release the lock
     libra worktree move ../old ../new              Rename a worktree
     libra worktree prune                           Drop entries whose paths vanished
     libra worktree remove ../feature-x             Unregister, keep the directory on disk
     libra worktree remove ../feature-x --delete-dir
                                                    Unregister and delete the directory
                                                    (refused on a dirty worktree)
     libra worktree repair --confirm                Fix stale or duplicate registry rows
     libra worktree repair --confirm ../feature-x   Restore that worktree's gitdir identity
                                                    from the registry (registry v2)
     libra worktree repair --migrate-layout --confirm
                                                    Migrate every legacy shared-.libra
                                                    symlink worktree to the isolated layout
     libra worktree repair --migrate-layout --dry-run
                                                    Report what would be migrated (read-only)
     libra worktree doctor                          Read-only diagnostics of per-worktree
                                                    scopes and Agent workspaces (paginated)
     libra --json worktree doctor --limit 20        One machine-readable page
     libra worktree doctor ws-3f0c                  Diagnose a single workspace scope";

/// Manage multiple working trees attached to this repository.
//
// Note: the user-facing summary for `libra worktree --help` is set via
// `#[command(about = "...", long_about = ...)]` on the Cli enum binding
// in src/cli.rs. We use `long_about` here so clap renders the same one-
// liner in both the top-level command list and `worktree --help`'s
// header, instead of leaking the previous "CLI arguments for the
// `worktree` subcommand. This type is wired into..." rustdoc body.
#[derive(Parser, Debug)]
#[command(long_about = "Manage multiple working trees attached to this repository.")]
pub struct WorktreeArgs {
    #[clap(subcommand)]
    pub command: WorktreeSubcommand,
}
/// All supported `worktree` subcommands.
///
/// These roughly mirror `git worktree` operations while keeping Libra-specific
/// semantics (for example, `remove` does not delete directories on disk).
#[derive(Subcommand, Debug)]
pub enum WorktreeSubcommand {
    /// Create a new linked worktree at the given path.
    Add {
        /// Filesystem path at which to create the new worktree.
        path: String,
        /// Existing branch to check out in the new worktree, or a
        /// commit-ish for a detached HEAD. Omitted: detached at the source
        /// worktree's current commit (intentionally different from Git's
        /// basename-branch default). A nonexistent branch fails closed —
        /// Git's remote-branch DWIM is deferred.
        target: Option<String>,
        /// Detach HEAD in the new worktree even when <BRANCH-OR-COMMIT>
        /// names a branch.
        #[clap(long)]
        detach: bool,
        /// Create NEW_BRANCH (from <BRANCH-OR-COMMIT> or the source HEAD)
        /// and check it out in the new worktree. Refused if the branch
        /// already exists (no -B/--force).
        #[clap(short = 'b', long = "create-branch", value_name = "NEW_BRANCH")]
        new_branch: Option<String>,
    },
    /// List all known worktrees and their state.
    List {
        /// Emit a stable, machine-readable porcelain format (one attribute per
        /// line, blank line between worktrees).
        #[clap(long)]
        porcelain: bool,
        /// JSON data schema selector (§C.8). `2` — the shipped shape, with
        /// `worktree_id`/`layout`/`epoch` — is the default and currently the
        /// only version: the pre-worktree-identity v1 shape gained those
        /// fields IN PLACE across the W1/W3 releases (each with its compat
        /// fixtures updated), so there is no frozen v1 left to serve and
        /// requesting it is refused rather than answered with a lie.
        #[clap(long = "schema-version", default_value_t = 2)]
        schema_version: u32,
    },
    /// Mark a worktree as locked to prevent it from being pruned or removed.
    Lock {
        /// Filesystem path of the worktree to lock.
        path: String,
        /// Optional free-form explanation for why this worktree is locked (shown in `worktree list`)
        #[clap(long, value_name = "TEXT")]
        reason: Option<String>,
    },
    /// Remove the lock from a previously locked worktree.
    Unlock {
        /// Filesystem path of the worktree to unlock.
        path: String,
    },
    /// Move or rename an existing worktree.
    Move {
        /// Current filesystem path of the worktree.
        src: String,
        /// New filesystem path for the worktree.
        dest: String,
    },
    /// Prune worktrees that are no longer valid or reachable.
    Prune,
    /// Unregister a worktree. By default the directory on disk is preserved;
    /// pass `--delete-dir` for Git-style behavior that also removes the
    /// directory after a dirty-state check.
    Remove {
        /// Filesystem path of the worktree to unregister.
        path: String,
        /// Also delete the worktree directory on disk after unregistering it.
        /// Refuses on a dirty worktree (uncommitted changes).
        #[clap(long)]
        delete_dir: bool,
    },
    /// Unmount a FUSE task worktree mountpoint.
    #[cfg(unix)]
    #[clap(alias = "unmount", about = "Unmount a FUSE worktree mountpoint")]
    Umount {
        /// Filesystem path of the FUSE mountpoint or its task worktree root.
        path: String,
        /// Remove the Libra task worktree root after unmounting its workspace mountpoint.
        #[clap(long)]
        cleanup: bool,
    },
    /// Diagnose Agent workspace scopes.
    ///
    /// Without an id this is a keyset-paginated view over every workspace
    /// record that still has something to say — including records left behind
    /// by a previous repository identity, which the identity-scoped listings
    /// hide. With an id it is the single-scope view of that one workspace.
    /// The default invocation is strictly read-only; the only repair action
    /// currently available is explicit legacy capture adoption, which also
    /// requires `--confirm` and writes an audit event.
    Doctor {
        /// Diagnose exactly one workspace (ids come from `worktree doctor`
        /// or `libra agent workspace list`). Cannot be combined with
        /// `--limit`/`--cursor`: a single scope is not a page.
        #[clap(value_name = "WORKSPACE_ID")]
        workspace_id: Option<String>,
        /// Maximum diagnostics per page (default 50, capped at 500).
        #[clap(long, value_name = "N")]
        limit: Option<u64>,
        /// Keyset cursor: the `next_cursor` of the previous page, verbatim.
        #[clap(long, value_name = "CURSOR")]
        cursor: Option<String>,
        /// Explicitly attribute one legacy unscoped capture session to this
        /// workspace. Requires a workspace id and --confirm; the default
        /// doctor command remains strictly read-only.
        #[clap(
             long,
             value_name = "SESSION_ID",
             requires = "workspace_id",
             conflicts_with_all = ["limit", "cursor"]
         )]
        adopt_capture_session: Option<String>,
        /// Copy the repository's common `info/exclude`/`info/attributes`
        /// (main's `.libra/info/*`) into ONE linked worktree's local gitdir
        /// (W0 §C.4.1.1: since info files became worktree-local they apply
        /// only to main; adoption is explicit and per-worktree, never
        /// automatic). Requires --confirm.
        #[clap(
             long,
             value_name = "WORKTREE_PATH",
             conflicts_with_all = [
                 "workspace_id", "limit", "cursor", "adopt_capture_session",
                 "adopt_approved_project", "clear_approved_project"
             ]
         )]
        adopt_info_to: Option<String>,
        /// Delete the repository's common `.libra/info/exclude` and
        /// `info/attributes` (explicit clear for rules that should no longer
        /// apply anywhere). Requires --confirm.
        #[clap(
             long,
             conflicts_with_all = [
                 "workspace_id", "limit", "cursor", "adopt_capture_session", "adopt_info_to",
                 "adopt_approved_project", "clear_approved_project"
             ]
         )]
        clear_common_info: bool,
        /// Re-home Always approvals whose opaque `project_id` is not the
        /// current `libra.repoid` onto the canonical repository identity
        /// (plan-20260715 W4-07). Migrations never do this; requires
        /// --confirm.
        #[clap(
             long,
             value_name = "LEGACY_PROJECT_ID",
             conflicts_with_all = [
                 "workspace_id", "limit", "cursor", "adopt_capture_session", "adopt_info_to",
                 "clear_common_info", "clear_approved_project"
             ]
         )]
        adopt_approved_project: Option<String>,
        /// Delete Always approvals under a legacy (non-canonical) `project_id`
        /// without adopting them. Requires --confirm.
        #[clap(
             long,
             value_name = "LEGACY_PROJECT_ID",
             conflicts_with_all = [
                 "workspace_id", "limit", "cursor", "adopt_capture_session", "adopt_info_to",
                 "clear_common_info", "adopt_approved_project"
             ]
         )]
        clear_approved_project: Option<String>,
        /// Confirm a mutating doctor action (capture-scope adoption,
        /// info-file adoption, common-info clearing, or approved_permission
        /// adopt/clear).
        #[clap(long)]
        confirm: bool,
    },
    /// Repair worktree metadata, attempting to recover from inconsistencies.
    /// With a path, restores that linked worktree's gitdir identity
    /// (`.libra/worktree_id` + `commondir`) from the registry's persisted
    /// stable id (registry v2, W3 §C.7).
    Repair {
        /// Linked worktree whose gitdir identity should be restored (or,
        /// with --migrate-layout, the single legacy worktree to migrate).
        path: Option<String>,
        /// Migrate legacy shared-`.libra` symlink worktrees to the isolated
        /// layout (W3-s3 §C.6.2). Runs from the MAIN worktree; without a
        /// path every legacy-symlink entry is migrated.
        #[clap(long)]
        migrate_layout: bool,
        /// With --migrate-layout: report what would be migrated, write
        /// nothing.
        #[clap(long, requires = "migrate_layout")]
        dry_run: bool,
        /// Confirm a MUTATING repair action (W0 §C.11, Codex R16/R17).
        /// Required for the no-arg registry repair, `repair <path>`, and a
        /// non-dry-run `--migrate-layout`: without it the command refuses
        /// before any registry lock, database write, or filesystem touch
        /// (zero side effects), and with it the action records exactly one
        /// operation-log audit event (see `libra op log`). The read-only
        /// `--migrate-layout --dry-run` never needs it.
        #[clap(long)]
        confirm: bool,
        /// Resolve an AMBIGUOUS registry by unregistering the entry at
        /// `<path>` (W1 §C.7).
        ///
        /// `add A` → `move A B` → `add A` under an older binary leaves two
        /// entries claiming one path-derived identity. Every mutation is
        /// refused while that holds — including the remove that would fix it —
        /// so this is the one action that runs against the ambiguous registry.
        /// It DETACHES the named entry: the directory and its scoped rows are
        /// kept, and every command inside that directory fails closed until
        /// you re-add it or finish with `remove --delete-dir`. The surviving
        /// claimant owns the identity again. Requires --yes, because choosing
        /// which entry to detach is a judgement only the user can make.
        #[clap(long, requires = "path", conflicts_with = "migrate_layout")]
        resolve_identity: bool,
        /// Confirm a --resolve-identity unregistration.
        #[clap(long, requires = "resolve_identity")]
        yes: bool,
    },
}

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
    path: String,
    is_main: bool,
    locked: bool,
    lock_reason: Option<String>,
    /// Stable worktree identity (v2). `None` for the main worktree. Old v1
    /// entries lack it; the v1→v2 upgrade backfills from each worktree's
    /// gitdir (or the canonical-path synthesis fallback).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    worktree_id: Option<String>,
    /// Lifecycle state (W3-s1b). Absent in files written before v0.19.58;
    /// serde defaults it to `Active`.
    #[serde(default, skip_serializing_if = "WorktreeEntryState::is_active")]
    state: WorktreeEntryState,
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
    epoch: u64,
}

fn is_zero_epoch(epoch: &u64) -> bool {
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
    fn is_active(&self) -> bool {
        matches!(self, Self::Active)
    }

    fn as_str(&self) -> &'static str {
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
const REGISTRY_SCHEMA_VERSION: u32 = 3;

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
struct WorktreeStateV1 {
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
enum RegistryShape {
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
    fn parse_document(data: &[u8]) -> Result<(Self, RegistryShape), String> {
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

#[derive(Debug, Serialize)]
pub(crate) struct WorktreeListOutput {
    pub(crate) worktrees: Vec<WorktreeListEntry>,
}

/// plan-20260714 W0 (§C.11): the read-only `worktree doctor` report.
///
/// `next_cursor` is present and always `null` here. W4 turns this into a
/// paginated machine interface with a frozen envelope; carrying the field
/// from the start means that wave ADDS pagination rather than reshaping a
/// document consumers already parse.
#[derive(Debug, Serialize)]
pub(crate) struct WorktreeDoctorOutput {
    pub(crate) schema_version: u32,
    pub(crate) diagnostics: Vec<WorktreeDiagnostic>,
    pub(crate) next_cursor: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
pub(crate) struct WorktreeDiagnostic {
    /// Stable worktree identity; `None` for main (`worktree_id IS NULL`).
    pub(crate) worktree_id: Option<String>,
    pub(crate) path: String,
    pub(crate) is_main: bool,
    /// Same vocabulary as `worktree list`: `main`, `linked-v2`,
    /// `legacy-symlink`, `missing`, `corrupt`, `task-fuse`.
    pub(crate) layout: &'static str,
    /// `active`, `detached_from_registry`, or `tombstone`.
    pub(crate) state: &'static str,
    /// Whether the registry's identity for this entry is one the entry itself
    /// still carries. `false` means mutations there are refused until
    /// `worktree repair` restores it.
    pub(crate) identity_registered: bool,
    /// W0 §C.4.1.1 origin inventory: which worktree-local `info/*` sources
    /// THIS worktree's ignore/attributes engines read (`info/exclude`,
    /// `info/attributes` under its own gitdir). Surfaced in the TEXT report
    /// today; the machine surface joins the W4 pagination extension of the
    /// frozen `worktree.doctor` envelope (which deliberately carries only
    /// the workspace half pre-W4). Per-match origin for excludes is
    /// `check-ignore -v`.
    pub(crate) info_sources: Vec<String>,
    /// Human-readable findings; empty means nothing to report.
    pub(crate) findings: Vec<String>,
}

#[derive(Debug, Serialize)]
pub(crate) struct WorktreeListEntry {
    pub(crate) kind: &'static str,
    pub(crate) path: String,
    pub(crate) is_main: bool,
    pub(crate) locked: bool,
    pub(crate) lock_reason: Option<String>,
    pub(crate) exists: bool,
    /// Stable worktree identity (Part C §C.3.3): `None` = the main worktree
    /// (`worktree_id IS NULL`), `Some(id)` = a linked worktree. Consumers must
    /// use this as the primary key, never the path.
    pub(crate) worktree_id: Option<String>,
    /// Lifecycle state (W3-s1b): `active`, `detached_from_registry`, or
    /// `tombstone`.
    pub(crate) state: &'static str,
    /// Registration generation (W1 §C.4.1.1 service fence). A client that
    /// caches a worktree's identity must carry this too: ids and paths are
    /// both reused when a worktree is re-added in place, and the epoch is what
    /// tells the two registrations apart. `0` for main and for entries written
    /// before the field existed.
    #[serde(default, skip_serializing_if = "is_zero_epoch")]
    pub(crate) epoch: u64,
    /// On-disk layout (W3-s3 §C.6.1): `main`, `linked-v2`, `legacy-symlink`
    /// (a pre-isolation `.libra` symlink sharing main's HEAD/index),
    /// `missing`, `corrupt`, or `task-fuse` for FUSE task worktrees.
    pub(crate) layout: &'static str,
}

#[derive(Debug, Serialize)]
struct WorktreeAddOutput {
    path: String,
    already_exists: bool,
    /// The path was a DETACHED worktree and this add re-attached it (its
    /// scoped state and identity resume unchanged).
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    reattached: bool,
}

#[derive(Debug, Serialize)]
struct WorktreeLockOutput {
    path: String,
    locked: bool,
    lock_reason: Option<String>,
    changed: bool,
}

#[derive(Debug, Serialize)]
struct WorktreeUnlockOutput {
    path: String,
    locked: bool,
    changed: bool,
}

#[derive(Debug, Serialize)]
struct WorktreeMoveOutput {
    source: String,
    destination: String,
    registry_updated: bool,
    disk_directory_moved: bool,
}

#[derive(Debug, Serialize)]
struct WorktreePruneOutput {
    pruned: Vec<String>,
    pruned_count: usize,
    /// Entries whose directory is gone but whose scoped cleanup failed —
    /// kept as tombstones for `worktree repair` to retry.
    tombstoned: Vec<String>,
}

#[derive(Debug, Serialize)]
struct WorktreeRemoveOutput {
    path: String,
    registry_removed: bool,
    disk_directory_deleted: bool,
    /// Keep-dir remove (W3-s1b): the entry moved to `detached_from_registry`
    /// — scoped DB rows are preserved and the directory is frozen until
    /// re-add or `--delete-dir`.
    detached: bool,
    /// `--delete-dir` deleted the directory but the scoped-row cleanup
    /// failed; a tombstone entry remains for `worktree repair` to retry.
    tombstone: bool,
}

#[derive(Debug, Serialize)]
pub(crate) struct WorktreeRepairOutput {
    changed: bool,
    /// Stale intent-journal rows rolled forward/back and resolved.
    journal_recovered: usize,
    /// Tombstone entries whose scoped cleanup finally succeeded.
    tombstones_cleaned: usize,
    /// Tombstone entries still pending (cleanup failed again).
    tombstones_pending: usize,
    /// Human-readable recovery notes (also printed).
    notes: Vec<String>,
}

#[cfg(unix)]
#[derive(Debug, Serialize)]
struct WorktreeUmountOutput {
    mountpoint: String,
    unmounted: bool,
    cleanup_requested: bool,
    cleanup_root: Option<String>,
    cleanup_root_removed: bool,
}

pub(crate) type WorktreeResult<T> = Result<T, WorktreeError>;

#[derive(Debug)]
pub(crate) enum WorktreeError {
    InvalidTarget(String),
    OperationBlocked(String),
    NoSuchWorktree { path: String },
    MainWorktree { action: &'static str, path: String },
    LockedWorktree { action: &'static str, path: String },
    DirtyWorktree { path: String },
    StateRead { path: PathBuf, source: io::Error },
    StateWrite { path: PathBuf, source: io::Error },
    StateCorrupt { path: PathBuf, source: String },
    StateRepair { source: io::Error },
    IoRead(String),
    IoWrite(String),
}

impl WorktreeError {
    fn stable_code(&self) -> StableErrorCode {
        match self {
            Self::InvalidTarget(_)
            | Self::NoSuchWorktree { .. }
            | Self::MainWorktree { .. }
            | Self::LockedWorktree { .. } => StableErrorCode::CliInvalidTarget,
            Self::OperationBlocked(_) | Self::DirtyWorktree { .. } => {
                StableErrorCode::ConflictOperationBlocked
            }
            Self::StateCorrupt { .. } | Self::StateRepair { .. } => StableErrorCode::RepoCorrupt,
            Self::StateRead { .. } | Self::IoRead(_) => StableErrorCode::IoReadFailed,
            Self::StateWrite { .. } | Self::IoWrite(_) => StableErrorCode::IoWriteFailed,
        }
    }

    pub(crate) fn into_cli_error(self) -> CliError {
        let code = self.stable_code();
        let mut error = CliError::fatal(self.to_string()).with_stable_code(code);
        if matches!(self, Self::DirtyWorktree { .. }) {
            error = error.with_hint(
                "commit or stash changes, or remove without --delete-dir to keep the directory",
            );
        }
        error
    }
}

impl std::fmt::Display for WorktreeError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::InvalidTarget(message)
            | Self::OperationBlocked(message)
            | Self::IoRead(message)
            | Self::IoWrite(message) => f.write_str(message),
            Self::NoSuchWorktree { path } => write!(f, "no such worktree: {path}"),
            Self::MainWorktree { action, path } => {
                write!(f, "cannot {action} main worktree: {path}")
            }
            Self::LockedWorktree { action, path } => {
                write!(f, "cannot {action} locked worktree: {path}")
            }
            Self::DirtyWorktree { path } => {
                write!(
                    f,
                    "cannot delete dirty worktree '{path}' (uncommitted changes)"
                )
            }
            Self::StateRead { path, source } => {
                write!(
                    f,
                    "failed to read worktree state '{}': {source}",
                    path.display()
                )
            }
            Self::StateWrite { path, source } => {
                write!(
                    f,
                    "failed to write worktree state '{}': {source}",
                    path.display()
                )
            }
            Self::StateCorrupt { path, source } => {
                write!(
                    f,
                    "worktree state '{}' is corrupt: {source}",
                    path.display()
                )
            }
            Self::StateRepair { source } => {
                write!(f, "failed to repair worktree state invariant: {source}")
            }
        }
    }
}

impl std::error::Error for WorktreeError {}

/// RAII guard that temporarily changes the process current directory.
///
/// When created with `change_to`, it switches the current directory to the
/// provided path and remembers the previous one. When dropped, it restores
/// the original directory, even if the inner operation panics or early-returns.
struct DirGuard {
    old_dir: PathBuf,
    #[cfg(test)]
    _cwd_lock: crate::utils::test::CwdLockGuard,
}

impl DirGuard {
    fn change_to(new_dir: &Path) -> io::Result<Self> {
        #[cfg(test)]
        let cwd_lock = crate::utils::test::cwd_lock_guard();
        let old_dir = env::current_dir()?;
        env::set_current_dir(new_dir)?;
        Ok(Self {
            old_dir,
            #[cfg(test)]
            _cwd_lock: cwd_lock,
        })
    }
}

impl Drop for DirGuard {
    fn drop(&mut self) {
        let _ = env::set_current_dir(&self.old_dir);
    }
}

/// Entry point for the `worktree` subcommand.
///
/// This function verifies that a Libra repository exists and then dispatches
/// to the concrete handler for the requested worktree operation. Any `io::Error`
/// returned from handlers is formatted as a `fatal:` message on stderr.
#[cfg_attr(all(unix, feature = "worktree-fuse"), allow(dead_code))]
pub async fn execute(args: WorktreeArgs) {
    if let Err(e) = execute_safe(args, &OutputConfig::default()).await {
        e.print_stderr();
    }
}

/// Safe entry point that returns structured [`CliResult`] instead of printing
/// errors and exiting. Dispatches to the appropriate worktree sub-command
/// (add, list, lock, unlock, move, prune, remove, repair, and Unix umount).
/// Part C bare boundary (plan-20260714 §C.4.1): a bare repository has no
/// working trees at all — `WorktreeScope::Main` presumes a main working
/// tree, and the registry's authoritative-root election presumes storage
/// lives at `<root>/.libra`. The whole worktree family is refused with a
/// stable error BEFORE any registry IO; bare worktree semantics are
/// deferred by design (intentionally-different, see COMPATIBILITY.md).
///
/// Classification is CONFIG-FIRST: `init` records `core.bare` in the
/// repository config, which survives any directory name (`init --bare
/// .libra` creates bare storage literally named `.libra`, defeating a
/// basename probe). The storage-basename heuristic remains only as a
/// fallback for repositories predating the config key.
pub(crate) async fn reject_bare_repository() -> CliResult<()> {
    let storage = util::storage_path();
    reject_bare_repository_impl(
        &storage,
        crate::internal::config::ConfigKv::get("core.bare").await,
    )
}

/// No-migration counterpart of [`reject_bare_repository`] for the refused
/// repair path. An unconfirmed repair is documented as zero-side-effect, so
/// it cannot take the migration-applying open above — yet the bare boundary
/// must still precede the confirmation refusal (a bare repository has no
/// working trees at all, the more fundamental error). Classify through a
/// connection that does not apply pending migrations.
pub(crate) async fn reject_bare_repository_without_migrations() -> CliResult<()> {
    let storage = util::storage_path();
    let db_path = crate::utils::path::database();
    let conn = crate::internal::db::open_database_without_migrations(&db_path)
        .await
        .map_err(|source| {
            CliError::fatal(format!(
                "cannot open the repository database without applying migrations to classify \
                  this repository: {source}"
            ))
            .with_stable_code(StableErrorCode::IoReadFailed)
        })?;
    reject_bare_repository_impl(
        &storage,
        crate::internal::config::ConfigKv::get_with_conn(&conn, "core.bare").await,
    )
}

fn reject_bare_repository_impl(
    storage: &std::path::Path,
    entry: anyhow::Result<Option<crate::internal::config::ConfigKvEntry>>,
) -> CliResult<()> {
    use crate::internal::config::parse_git_bool;
    // FAIL CLOSED on read failures and unparseable values — a bare boundary
    // that cannot be determined must refuse, not fall through to the
    // basename heuristic (which a `.libra`-named bare directory defeats).
    let is_bare = match entry {
        Ok(Some(entry)) => parse_git_bool(&entry.value).ok_or_else(|| {
            CliError::fatal(format!(
                "invalid core.bare value '{}': expected true/false/yes/no/on/off/1/0",
                entry.value
            ))
            .with_stable_code(StableErrorCode::CliInvalidArguments)
        })?,
        // Key absent: repositories predating the recorded flag — fall back
        // to the standard-layout heuristic.
        Ok(None) => storage.file_name() != Some(std::ffi::OsStr::new(util::ROOT_DIR)),
        Err(error) => {
            return Err(CliError::fatal(format!(
                "cannot read core.bare to classify this repository: {error}"
            ))
            .with_stable_code(StableErrorCode::IoReadFailed));
        }
    };
    if is_bare {
        return Err(CliError::fatal(format!(
            "this is a bare repository ('{}'): it has no working trees, so the \
              `worktree` command family is unavailable here",
            storage.display()
        ))
        .with_stable_code(StableErrorCode::RepoStateInvalid));
    }
    Ok(())
}

pub async fn execute_safe(args: WorktreeArgs, output: &OutputConfig) -> CliResult<()> {
    let command = args.command;
    #[cfg(unix)]
    let needs_repo = !matches!(&command, WorktreeSubcommand::Umount { .. });
    #[cfg(not(unix))]
    let needs_repo = true;

    // W0 §C.11: `doctor` skips the migration-applying open below. It is a
    // read-only diagnostic, and applying migrations is a write — the one
    // command you want available on a repository you have not yet decided to
    // upgrade must not upgrade it as a side effect. An UNCONFIRMED mutating
    // repair skips it too (Codex R19 follow-up): the refusal is documented
    // as zero-side-effect, so it must reach `require_repair_confirmation`
    // without any migration-applying open on the way. The `--migrate-layout
    // --dry-run` preview skips it as well (Codex R20 follow-up): it is
    // documented as read-only end to end, so it enumerates layouts without
    // ever resolving a database connection.
    let readonly_layout_preview = repair_readonly_layout_preview(&command);
    let applies_migrations = !matches!(&command, WorktreeSubcommand::Doctor { .. })
        && !repair_invocation_refused_without_confirmation(&command)
        && !readonly_layout_preview;

    if needs_repo {
        util::require_repo().map_err(|_| CliError::repo_not_found())?;
    }
    if needs_repo && applies_migrations {
        // §C.7 ordering: apply pending repository migrations — including the
        // registry-v2 capability marker (2026072401) — BEFORE any
        // worktrees.json read or rewrite, so a pre-v2 binary is refused at
        // connect time no matter which worktree command first touches the v2
        // file. This also refuses a future-schema database gracefully
        // instead of parsing a registry this binary does not understand.
        // (Migrations may already have applied at the top-level CLI
        // preflight — the contract shared by repository commands using the
        // standard schema preflight, not a worktree-family side effect.)
        crate::internal::db::get_db_conn_instance_for_path(&crate::utils::path::database())
            .await
            .map_err(|source| {
                CliError::fatal(format!(
                    "cannot open the repository database before touching the worktree \
                      registry: {source}"
                ))
                .with_stable_code(StableErrorCode::IoReadFailed)
            })?;
        // Bare boundary: refused before ANY registry IO (the config read
        // needs the database opened just above).
        reject_bare_repository().await?;
    } else if needs_repo
        && (repair_invocation_refused_without_confirmation(&command) || readonly_layout_preview)
    {
        // The bare boundary precedes even the confirmation refusal — a bare
        // repository has no working trees at all, which is the more
        // fundamental error — but the refused repair and the read-only
        // layout preview must stay zero-side-effect, so classify through the
        // no-migration open.
        reject_bare_repository_without_migrations().await?;
    }

    match command {
        WorktreeSubcommand::Add {
            path,
            target,
            detach,
            new_branch,
        } => {
            let result = add_worktree(path, target, detach, new_branch)
                .await
                .map_err(WorktreeError::into_cli_error)?;
            render_add_worktree(&result, output)
        }
        WorktreeSubcommand::List {
            porcelain,
            schema_version,
        } => list_worktrees(output, porcelain, schema_version).await,
        WorktreeSubcommand::Lock { path, reason } => {
            let result = lock_worktree(path, reason)
                .await
                .map_err(WorktreeError::into_cli_error)?;
            render_lock_worktree(&result, output)
        }
        WorktreeSubcommand::Unlock { path } => {
            let result = unlock_worktree(path)
                .await
                .map_err(WorktreeError::into_cli_error)?;
            render_unlock_worktree(&result, output)
        }
        WorktreeSubcommand::Move { src, dest } => {
            let result = move_worktree(src, dest)
                .await
                .map_err(WorktreeError::into_cli_error)?;
            render_move_worktree(&result, output)
        }
        WorktreeSubcommand::Prune => {
            let result = prune_worktrees()
                .await
                .map_err(WorktreeError::into_cli_error)?;
            render_prune_worktrees(&result, output)
        }
        WorktreeSubcommand::Remove { path, delete_dir } => {
            let result = remove_worktree(path, delete_dir)
                .await
                .map_err(WorktreeError::into_cli_error)?;
            render_remove_worktree(&result, output)
        }
        #[cfg(unix)]
        WorktreeSubcommand::Umount { path, cleanup } => {
            let result = umount_fuse_path(path, cleanup).map_err(WorktreeError::into_cli_error)?;
            render_umount_fuse_path(&result, output)
        }
        WorktreeSubcommand::Doctor {
            workspace_id,
            limit,
            cursor,
            adopt_capture_session,
            adopt_info_to,
            clear_common_info,
            adopt_approved_project,
            clear_approved_project,
            confirm,
        } => {
            if let Some(session_id) = adopt_capture_session {
                let workspace_id = workspace_id.ok_or_else(|| {
                    CliError::command_usage(
                        "--adopt-capture-session requires the target WORKSPACE_ID",
                    )
                })?;
                adopt_legacy_capture_scope(&workspace_id, &session_id, confirm, output).await
            } else if let Some(legacy_project_id) = adopt_approved_project {
                adopt_or_clear_legacy_approved_project(
                    &legacy_project_id,
                    confirm,
                    /* clear */ false,
                    output,
                )
                .await
            } else if let Some(legacy_project_id) = clear_approved_project {
                adopt_or_clear_legacy_approved_project(
                    &legacy_project_id,
                    confirm,
                    /* clear */ true,
                    output,
                )
                .await
            } else if let Some(target) = adopt_info_to {
                // W0 §C.4.1.1: explicit, confirmed, audited — like every
                // other mutating doctor/repair action.
                require_repair_confirmation(confirm, "worktree doctor --adopt-info-to")?;
                let boundary =
                    begin_repair_operation("worktree doctor --adopt-info-to", Some(&target))
                        .await?;
                let result = adopt_common_info_files(&target);
                let result = finish_repair_operation(boundary, result).await?;
                if output.is_json() {
                    // Distinct envelope, like `worktree.doctor.adopt_capture`
                    // — the read-only `worktree.doctor` page schema stays
                    // untouched.
                    return emit_json_data(
                        "worktree.doctor.adopt_info",
                        &serde_json::json!({ "target": target, "report": result }),
                        output,
                    );
                }
                println!("{result}");
                Ok(())
            } else if clear_common_info {
                require_repair_confirmation(confirm, "worktree doctor --clear-common-info")?;
                let boundary =
                    begin_repair_operation("worktree doctor --clear-common-info", None).await?;
                let result = clear_common_info_files();
                let result = finish_repair_operation(boundary, result).await?;
                if output.is_json() {
                    return emit_json_data(
                        "worktree.doctor.clear_common_info",
                        &serde_json::json!({ "report": result }),
                        output,
                    );
                }
                println!("{result}");
                Ok(())
            } else {
                run_worktree_doctor(workspace_id, limit, cursor, output).await
            }
        }
        WorktreeSubcommand::Repair {
            path,
            migrate_layout,
            dry_run,
            confirm,
            resolve_identity,
            yes,
        } => {
            if resolve_identity {
                let Some(path) = path else {
                    return Err(WorktreeError::OperationBlocked(
                        "--resolve-identity needs the path of the entry to unregister".to_string(),
                    )
                    .into_cli_error());
                };
                if !yes {
                    return Err(WorktreeError::OperationBlocked(format!(
                        "--resolve-identity detaches '{path}' from the registry — its files and \
                          scoped state are kept, and every command inside it will fail closed \
                          until you re-add it or run `remove --delete-dir`; re-run with --yes to \
                          confirm"
                    ))
                    .into_cli_error());
                }
                // W0 (§C.11, Codex R18): `--yes` is this action's dedicated
                // confirmation; once given, the detach runs inside the same
                // one-row operation-log audit boundary as every other
                // mutating repair, closed with the action's outcome.
                let boundary = begin_repair_operation(
                    &format!("worktree repair {path} --resolve-identity"),
                    Some(&path),
                )
                .await?;
                let result = resolve_identity_collision(&path)
                    .await
                    .map_err(WorktreeError::into_cli_error);
                let result = finish_repair_operation(boundary, result).await?;
                if !output.is_json() {
                    println!("{result}");
                }
                return Ok(());
            }
            // W0 (§C.11, Codex R16/R17): each mutating repair action is gated
            // on `--confirm` (the refusal precedes EVERY side effect) and,
            // once confirmed, runs inside one operation-log audit boundary —
            // exactly one row per executed action, closed with the action's
            // outcome. `--dry-run` writes nothing and stays confirmation-free.
            if migrate_layout {
                if dry_run {
                    let result = migrate_layout_run(path, dry_run)
                        .await
                        .map_err(WorktreeError::into_cli_error)?;
                    return render_migrate_layout(&result, output);
                }
                require_repair_confirmation(confirm, "worktree repair --migrate-layout")?;
                let boundary =
                    begin_repair_operation("worktree repair --migrate-layout", path.as_deref())
                        .await?;
                let result = migrate_layout_run(path, dry_run)
                    .await
                    .map_err(WorktreeError::into_cli_error);
                let result = finish_repair_operation(boundary, result).await?;
                return render_migrate_layout(&result, output);
            }
            if let Some(path) = path {
                require_repair_confirmation(confirm, &format!("worktree repair {path}"))?;
                let boundary =
                    begin_repair_operation("worktree repair <path>", Some(&path)).await?;
                let result = repair_worktree_identity(path)
                    .await
                    .map_err(WorktreeError::into_cli_error);
                let result = finish_repair_operation(boundary, result).await?;
                return render_repair_identity(&result, output);
            }
            require_repair_confirmation(confirm, "worktree repair")?;
            let boundary = begin_repair_operation("worktree repair", None).await?;
            let result = repair_worktrees()
                .await
                .map_err(WorktreeError::into_cli_error);
            let result = finish_repair_operation(boundary, result).await?;
            render_repair_worktrees(&result, output)
        }
    }
}

/// W0 (§C.11, Codex R19 follow-up): an UNCONFIRMED mutating repair is
/// documented as byte-for-byte side-effect free — and applying pending
/// schema migrations is a WRITE. An invocation this predicate marks must
/// therefore skip every migration-applying database open (the CLI preflight
/// AND the dispatch-time open in this file and in `worktree-fuse.rs`) so it
/// reaches only `require_repair_confirmation`'s pure refusal. Confirmed
/// actions keep the standard schema preflight: their audit boundary writes.
pub(crate) fn repair_invocation_refused_without_confirmation(command: &WorktreeSubcommand) -> bool {
    match command {
        WorktreeSubcommand::Repair {
            migrate_layout,
            dry_run,
            confirm,
            resolve_identity,
            yes,
            ..
        } => {
            if *resolve_identity {
                // `--yes` is this action's dedicated confirmation.
                !*yes
            } else if *migrate_layout && *dry_run {
                // The read-only preview is confirmation-free by design.
                false
            } else {
                !*confirm
            }
        }
        _ => false,
    }
}

/// W0 (§C.11, Codex R20 follow-up): the `--migrate-layout --dry-run` preview
/// is documented as read-only end to end — and applying pending schema
/// migrations is a WRITE. An invocation this predicate marks must therefore
/// skip every migration-applying database open (the CLI preflight AND the
/// dispatch-time opens in this file and in `worktree-fuse.rs`) and never
/// resolve the global connection: the preview needs only the lockless
/// registry read and on-disk layout detection. The preview stays
/// confirmation-free by design, so it is NOT part of
/// [`repair_invocation_refused_without_confirmation`].
pub(crate) fn repair_readonly_layout_preview(command: &WorktreeSubcommand) -> bool {
    matches!(
        command,
        WorktreeSubcommand::Repair {
            migrate_layout: true,
            dry_run: true,
            ..
        }
    )
}

/// W0 (§C.11, Codex R16/R17): a MUTATING repair action runs only behind an
/// explicit `--confirm`. The refusal precedes every registry lock, database
/// write and filesystem touch, so an unconfirmed invocation is byte-for-byte
/// side-effect free — the property the table-driven regression
/// `worktree_doctor_mutations_require_confirmation_and_emit_audit` asserts.
pub(crate) fn require_repair_confirmation(confirm: bool, action: &str) -> CliResult<()> {
    if confirm {
        return Ok(());
    }
    Err(WorktreeError::OperationBlocked(format!(
        "refusing to run {action} without confirmation; re-run with --confirm"
    ))
    .into_cli_error())
}

/// Open the operation-log audit boundary for one confirmed repair action (W0
/// §C.11): exactly one row per EXECUTED action, closed with the action's
/// outcome by [`finish_repair_operation`]. The boundary claim also takes this
/// worktree's control slot for the duration, serializing the repair against
/// any concurrent sequencer control action here.
pub(crate) async fn begin_repair_operation(
    command_name: &str,
    target: Option<&str>,
) -> CliResult<crate::internal::operation_wrapper::OperationBoundary> {
    use crate::internal::operation_wrapper::{OperationMeta, OperationScope, begin_operation};

    let db = crate::internal::db::get_db_conn_instance().await;
    // Fail CLOSED on a missing/unreadable identity, like a sequencer control:
    // without it the audit row cannot be attributed, and a repair that ran
    // without its audit event is exactly what the W0 contract forbids.
    let repo_id = RepoIdentity::resolve(&db)
        .await
        .map(|identity| identity.as_str().to_string())
        .map_err(|error| {
            CliError::fatal(format!("cannot record the repair operation: {error}"))
                .with_stable_code(StableErrorCode::RepoCorrupt)
        })?;
    let meta = OperationMeta {
        command_name: command_name.to_string(),
        description: match target {
            Some(target) => format!("{command_name}: target {target}"),
            None => format!("{command_name}: every registered worktree"),
        },
        actor: crate::internal::config::ConfigKv::get("user.name")
            .await
            .ok()
            .flatten()
            .map(|entry| entry.value)
            .filter(|value| !value.trim().is_empty())
            .unwrap_or_else(|| "libra-user".to_string()),
        repo_id,
        // No dedup identity: re-running a confirmed repair to convergence is
        // the documented flow, and each execution is its own audit row —
        // never an accidental double submission to suppress.
        args_digest: None,
    };
    // A repair's audit row is never restorable, so snapshotting every branch
    // and workspace pointer would write rows nothing can ever read; record
    // the head pointer, which is what `op log`/`op show` display (§C.14).
    let scope = OperationScope {
        include_refs: false,
        include_workspace: false,
        // Re-running a repair is ordinary (heal one invariant, then the next);
        // overlap is excluded by the worktree-wide control slot, a real mutex
        // rather than the five-second heuristic.
        duplicate_window: false,
        ..OperationScope::default()
    };
    begin_operation(meta, scope).await.map_err(|err| {
        CliError::fatal(format!("cannot record the repair operation: {err}"))
            .with_stable_code(StableErrorCode::ConflictOperationBlocked)
            .with_hint(
                "another control action is running or just completed in this worktree; wait for \
                  it to finish, or inspect it with `libra op log`",
            )
    })
}

/// Close a repair action's audit boundary with its outcome, success OR
/// failure — a repair that failed mid-way is still an executed action and
/// its row says so. A failure to CLOSE is a command failure (W0 §C.11): the
/// contract is exactly one row per executed action, closed with the outcome,
/// and warning-and-continuing would leave a `running` row while reporting a
/// successful repair — the audit gap the boundary exists to exclude. The
/// error states plainly that the repair itself already happened, so the
/// failure is never misread as the repair not running.
pub(crate) async fn finish_repair_operation<T>(
    boundary: crate::internal::operation_wrapper::OperationBoundary,
    result: CliResult<T>,
) -> CliResult<T> {
    let outcome = if result.is_ok() {
        crate::internal::operation_wrapper::BoundaryOutcome::Succeeded
    } else {
        crate::internal::operation_wrapper::BoundaryOutcome::Failed
    };
    if let Err(err) = boundary.finish(outcome).await {
        return Err(CliError::fatal(format!(
            "the repair completed, but its operation-log record could not be closed: {err}"
        ))
        .with_stable_code(StableErrorCode::IoWriteFailed)
        .with_hint(
            "the repair's effects stand; inspect the unclosed record with `libra op log`, \
              and re-run the repair once the operation log is writable again",
        ));
    }
    result
}

/// What main-scope layer/sparse state exists, for the doctor finding above.
///
/// `Ok(None)` means "nothing, and that is known". Every read failure — a
/// missing table included — is an `Err`, and the caller reports it: §C.13
/// requires doctor to be fail-closed, and a diagnostic that silently reports
/// "nothing to see" because it could not look is worse than one that says so.
///
/// The connection comes from the caller — under `worktree doctor` that is the
/// no-migration open (§C.11 W0): a diagnostic must never upgrade the database
/// it is observing, so this helper must NOT resolve `get_db_conn_instance()`
/// (which applies pending migrations) on its own.
async fn adopted_scope_settings_present(
    conn: &sea_orm::DatabaseConnection,
) -> Result<Option<String>, String> {
    use sea_orm::{ConnectionTrait, DbBackend, Statement};

    let count = async |sql: &str| -> Result<i64, String> {
        conn.query_one_raw(Statement::from_string(DbBackend::Sqlite, sql.to_string()))
            .await
            .map_err(|error| format!("{error}"))?
            .ok_or_else(|| "a COUNT query returned no row".to_string())?
            .try_get_by_index::<i64>(0)
            .map_err(|error| format!("{error}"))
    };

    let mut parts = Vec::new();
    let layers = count("SELECT COUNT(*) FROM `layer` WHERE `worktree_id` = ''").await?;
    if layers > 0 {
        parts.push(format!("{layers} layer registration(s)"));
    }
    // The ownership rows matter more than the registrations: they are what keeps
    // a retained overlay file unstageable.
    let owned = count("SELECT COUNT(*) FROM `layer_path` WHERE `worktree_id` = ''").await?;
    if owned > 0 {
        parts.push(format!("{owned} materialized overlay path(s)"));
    }
    let patterns = count("SELECT COUNT(*) FROM `sparse_view` WHERE `worktree_id` = ''").await?;
    if patterns > 0 {
        parts.push(format!("{patterns} sparse pattern(s)"));
    }
    // A legacy `sparse.enabled = true` with NO patterns migrates into
    // `sparse_view_meta`, so counting patterns alone would miss it.
    let sparse_enabled = count(
        "SELECT COUNT(*) FROM `sparse_view_meta` \
          WHERE `worktree_id` = '' AND `enabled` <> 0",
    )
    .await?;
    if sparse_enabled > 0 {
        parts.push("an enabled sparse view".to_string());
    }
    if parts.is_empty() {
        return Ok(None);
    }
    Ok(Some(parts.join(", ")))
}

/// Unregister one side of an identity collision so the registry becomes
/// unambiguous again (W1 §C.7).
///
/// Runs against the AMBIGUOUS registry on purpose: `load_state` refuses it,
/// which is what makes every other repair impossible, so this uses the repair
/// loader and does its own narrow validation. The directory on disk is never
/// touched — only the registry entry and that scope's rows go, so the user can
/// re-add it afterwards and get a fresh identity and generation.
async fn resolve_identity_collision(path: &str) -> WorktreeResult<String> {
    let _lock = acquire_registry_lock_async().await?;
    let mut state = load_state_for_repair()?;
    let target = resolve_path(path, "worktree path")?;
    let target_key = target.to_string_lossy().to_string();

    let Some(index) = state
        .entries
        .iter()
        .position(|entry| !entry.is_main && entry.path == target_key)
    else {
        return Err(WorktreeError::NoSuchWorktree {
            path: path.to_string(),
        });
    };
    let Some(identity) = state.entries[index].worktree_id.clone() else {
        return Err(WorktreeError::OperationBlocked(format!(
            "the entry at '{target_key}' carries no identity, so it is not part of a collision"
        )));
    };
    let claimants = state
        .entries
        .iter()
        .filter(|entry| {
            !entry.is_main
                && entry.state.is_active()
                && entry.worktree_id.as_deref() == Some(identity.as_str())
        })
        .count();
    if claimants < 2 {
        return Err(WorktreeError::OperationBlocked(format!(
            "'{target_key}' is the only live entry claiming identity '{identity}' — there is \
              no collision to resolve here"
        )));
    }

    // DETACH, do not delete. The directory on disk is a real worktree with the
    // user's files in it: dropping the entry outright would leave a gitdir
    // that still resolves the duplicated identity and could mutate the
    // survivor's scope, and sweeping the identity-keyed rows would take the
    // SURVIVOR's HEAD and reflog with them. Detaching is the lifecycle state
    // that already means "unregistered, rows kept, every command in that
    // directory fails closed until re-add or --delete-dir" — and a detached
    // entry no longer claims the identity, so the collision is resolved.
    //
    // JOURNALLED first, exactly like every other lifecycle action: the marker
    // and the registry are two writes that cannot be joined into one
    // transaction, and a crash between them leaves a frozen directory that the
    // registry still calls active. Reconciliation would then read the marker
    // as stale and lift it, silently undoing the repair. The intent row is
    // what makes `worktree repair` finish it instead.
    let db = crate::internal::db::get_db_conn_instance().await;
    let payload = serde_json::json!({
        "path": target.to_string_lossy(),
        "delete_dir": false,
        "reason": "resolve_identity",
    });
    let journal_id = journal_append(
        &db,
        WorktreeControl::Remove.declare(),
        Some(&identity),
        &payload,
    )
    .await
    .map_err(WorktreeError::OperationBlocked)?;

    write_detached_marker(&target, &identity)?;
    // The SQL lifecycle mirror BEFORE the registry write, like every other
    // detach path: the 2026072402 down-migration guard reads only the
    // mirror/journal/sequencer tables, so a detach recorded solely in the
    // registry file would let the rollback proceed while a detached
    // directory still exists on disk.
    lifecycle_upsert(
        &db,
        &identity,
        WorktreeEntryState::DetachedFromRegistry.as_str(),
        &target.to_string_lossy(),
    )
    .await
    .map_err(WorktreeError::OperationBlocked)?;
    state.entries[index].state = WorktreeEntryState::DetachedFromRegistry;
    save_state(&state).map_err(|error| {
        WorktreeError::IoWrite(format!("failed to write the worktree registry: {error}"))
    })?;
    if let Err(error) = journal_resolve(&db, journal_id).await {
        tracing::warn!(error, "detach journal row not resolved; repair reconciles");
    }
    Ok(format!(
        "Detached '{target_key}' from the registry (identity '{identity}'). Its files and \
          scoped state are kept and every command inside it now fails closed; the remaining \
          claimant owns the identity again. Finish with `libra worktree remove --delete-dir \
          {target_key}`, or `libra worktree add {target_key}` to re-attach it."
    ))
}

/// Returns the path to the on-disk worktree state file.
fn state_path() -> PathBuf {
    util::storage_path().join("worktrees.json")
}

/// Loads the current `WorktreeState` from disk, ensuring a main worktree entry.
///
/// If the state file does not exist, this function initializes a fresh state
/// with a single main worktree derived from the storage path and persists it
/// before returning; an existing zero-byte file is refused as a torn write.
/// RAII guard over the worktree REGISTRY mutation lock (`worktrees.lock` in
/// the common storage). Serializes every registry mutator's
/// load → check → mutate → write sequence across processes: without it, a
/// concurrent `worktree add`'s strict pre-seed sweep could delete rows
/// another add just seeded for the same deterministic instance id, and two
/// load/modify/write registry updates could drop each other's entries. The
/// flock is BLOCKING (concurrent mutators queue rather than fail) and
/// released on drop (or process exit). Read-only paths (`list`) stay
/// lock-free.
pub(crate) struct RegistryLockGuard {
    file: fs::File,
}

impl Drop for RegistryLockGuard {
    fn drop(&mut self) {
        let _ = self.file.unlock();
    }
}

///
/// PRIVATE ON PURPOSE (plan-20260714 W1): the blocking variant has
/// exactly ONE caller — the `spawn_blocking` in
/// [`acquire_registry_lock_async`]. Every other acquisition in the
/// crate, sync or async, goes through that helper, so the
/// blocking-on-a-runtime-worker deadlock cannot be reintroduced from
/// another module by accident.
fn acquire_registry_lock() -> WorktreeResult<RegistryLockGuard> {
    let lock_path = util::storage_path().join("worktrees.lock");
    let file = fs::OpenOptions::new()
        .create(true)
        .read(true)
        .write(true)
        .truncate(false)
        .open(&lock_path)
        .map_err(|e| {
            WorktreeError::IoWrite(format!(
                "cannot open the worktree registry lock '{}': {e}",
                lock_path.display()
            ))
        })?;
    // std file locking is CROSS-PLATFORM (flock on Unix, LockFileEx on
    // Windows) and BLOCKING — concurrent mutators queue rather than fail.
    file.lock().map_err(|e| {
        WorktreeError::IoWrite(format!(
            "cannot lock the worktree registry '{}': {e}",
            lock_path.display()
        ))
    })?;
    Ok(RegistryLockGuard { file })
}

/// [`acquire_registry_lock`] for ASYNC callers: takes the blocking `flock`
/// on the blocking pool instead of on a runtime worker.
///
/// Blocking a worker here is not merely impolite, it is a LIVENESS BUG.
/// sqlx returns a pooled connection by SPAWNING a task; a spawn from inside
/// a poll lands in that worker's non-stealable LIFO slot; and sea-orm pins
/// SQLite pools to ONE connection. A worker blocked right after a query
/// therefore strands the connection return, and every database user in the
/// process — including whoever holds this very lock — waits out the full
/// sqlx acquire timeout for a connection that can never come back. The
/// service's dirty-mark handler deadlocked exactly that way.
///
/// The returned guard owns only a `File`, so it is `Send` and may be held
/// across subsequent awaits: the registry → SQLite lock ORDER is deliberate
/// (validate and write under one hold). Only the ACQUISITION must leave the
/// runtime worker.
pub(crate) async fn acquire_registry_lock_async() -> WorktreeResult<RegistryLockGuard> {
    match tokio::task::spawn_blocking(acquire_registry_lock).await {
        Ok(result) => result,
        Err(error) => Err(WorktreeError::IoWrite(format!(
            "the worktree registry lock task failed: {error}"
        ))),
    }
}

/// Load the registry for MUTATION. The caller MUST hold the registry lock
/// (`acquire_registry_lock`): this variant performs DURABLE repairs — it
/// creates a MISSING registry (an existing zero-byte file is refused as a
/// torn write), rewrites a legacy v1 file as v2 with
/// each linked entry's stable id backfilled, and persists main-entry fixes.
/// A v2 file violating the identity invariants is REFUSED (only the
/// explicit no-arg `worktree repair` may heal it — see
/// `load_state_for_repair`). Lockless readers use `load_state_readonly`
/// instead (a lockless writer could overwrite a concurrent locked mutation).
fn load_state() -> WorktreeResult<WorktreeState> {
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
fn load_state_for_repair() -> WorktreeResult<WorktreeState> {
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

fn load_state_readonly() -> WorktreeResult<WorktreeState> {
    load_state_readonly_at(&state_path())
}

/// [`load_state_readonly`] against an EXPLICIT registry path — for callers
/// that resolved their storage root once (§C.4.2) and must not let an
/// ambient re-resolution answer for a different repository (the GC root
/// enumeration binds everything to the request pin this way).
fn load_state_readonly_at(path: &std::path::Path) -> WorktreeResult<WorktreeState> {
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
fn normalize_v2_ids(state: &mut WorktreeState) -> bool {
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
fn save_state(state: &WorktreeState) -> io::Result<()> {
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

fn write_state(state: &WorktreeState) -> WorktreeResult<()> {
    let path = state_path();
    save_state(state).map_err(|source| WorktreeError::StateWrite { path, source })
}

fn resolve_path(path: impl AsRef<Path>, role: &'static str) -> WorktreeResult<PathBuf> {
    let path = path.as_ref();
    canonicalize(path).map_err(|source| {
        WorktreeError::IoRead(format!(
            "failed to resolve {role} '{}': {source}",
            path.display()
        ))
    })
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
fn canonicalize<P: AsRef<Path>>(path: P) -> io::Result<PathBuf> {
    util::canonicalize_deepest_existing(path)
}

/// Ensure the registry designates EXACTLY ONE main entry. In the standard
/// layout the repository root (the directory holding `.libra`) is
/// authoritative: it is crowned when present and restored when absent —
/// other entries are never elected main. The valid-path/first-entry/cwd
/// heuristics apply only to non-standard layouts where the root cannot be
/// inferred.
fn ensure_main_entry(state: &mut WorktreeState) -> io::Result<bool> {
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
fn find_entry_mut<'a>(state: &'a mut WorktreeState, path: &Path) -> Option<&'a mut WorktreeEntry> {
    state
        .entries
        .iter_mut()
        .find(|w| Path::new(&w.path) == path)
}

/// Finds an immutable worktree entry by canonical path.
fn find_entry<'a>(state: &'a WorktreeState, path: &Path) -> Option<&'a WorktreeEntry> {
    state.entries.iter().find(|w| Path::new(&w.path) == path)
}

/// Implements `worktree add <path>`.
///
/// This command:
/// - validates the requested path is outside `.libra` storage,
/// - creates the target directory if it does not exist,
/// - rejects paths that canonicalize inside `.libra` (with cleanup),
/// - ensures the worktree is not already registered,
/// - creates a real per-worktree `.libra` gitdir (its own local HEAD, index,
///   and HEAD reflog) that records a `commondir` pointer to the shared object
///   store and a stable `worktree_id` — it is NOT a symlink to shared storage,
/// - when `HEAD` exists, populates the new worktree from committed `HEAD`
///   content (not staged-only index changes).
///
/// The checkout the new worktree is seeded with (W3-s2 §C.7), resolved
/// FAIL-CLOSED before any side effect.
enum AddCheckout {
    /// No target: detached at the source worktree's current commit
    /// (intentionally different from Git's basename-branch default).
    DetachedAtSource,
    /// Explicit commit-ish (or a branch under `--detach`).
    Detached(git_internal::hash::ObjectHash),
    /// Check out an existing branch (refused when any scope has it out).
    AttachBranch { name: String },
    /// `-b`: create the branch at `start`, then check it out. Fully rolled
    /// back on any later failure (no branch-only residue).
    CreateBranch {
        name: String,
        start: git_internal::hash::ObjectHash,
    },
}

async fn add_worktree(
    path: String,
    target_spec: Option<String>,
    detach: bool,
    new_branch: Option<String>,
) -> WorktreeResult<WorktreeAddOutput> {
    // Registry mutation lock: the whole precheck → sweep → seed → registry
    // write sequence runs under it (a concurrent add's sweep must not
    // delete this add's freshly seeded rows).
    let _registry_lock = acquire_registry_lock_async().await?;
    let storage = util::storage_path();
    let target = resolve_path(&path, "worktree path")?;

    if util::is_sub_path(&target, &storage) {
        return Err(WorktreeError::InvalidTarget(format!(
            "worktree path cannot be inside .libra storage: {}",
            target.display()
        )));
    }

    let target_exists = target.exists();
    if target_exists && !target.is_dir() {
        return Err(WorktreeError::InvalidTarget(format!(
            "target exists and is not a directory: {}",
            target.display()
        )));
    }

    let canonical_target = resolve_path(&target, "worktree path")?;
    if util::is_sub_path(&canonical_target, &storage) {
        return Err(WorktreeError::InvalidTarget(format!(
            "worktree path cannot be inside .libra storage: {}",
            canonical_target.display()
        )));
    }

    let mut state = load_state()?;
    if let Some(existing_index) = state
        .entries
        .iter()
        .position(|w| Path::new(&w.path) == canonical_target)
    {
        match state.entries[existing_index].state {
            // W3-s1b (§C.7): re-adding a DETACHED worktree re-attaches it —
            // the frozen directory resumes with ITS OWN scoped state, so a
            // checkout target would be silently ignored: refuse it.
            WorktreeEntryState::DetachedFromRegistry => {
                if target_spec.is_some() || detach || new_branch.is_some() {
                    return Err(WorktreeError::InvalidTarget(format!(
                        "'{}' is a detached worktree; re-attaching resumes its own \
                          HEAD — drop the branch/commit arguments (then switch inside it)",
                        canonical_target.display()
                    )));
                }
                return reattach_worktree(&mut state, existing_index, &canonical_target).await;
            }
            WorktreeEntryState::Tombstone => {
                return Err(WorktreeError::OperationBlocked(format!(
                    "'{}' is a tombstone (scoped cleanup pending); run `libra worktree \
                      repair --confirm` first, then add",
                    canonical_target.display()
                )));
            }
            WorktreeEntryState::Active => {
                if target_spec.is_some() || detach || new_branch.is_some() {
                    return Err(WorktreeError::InvalidTarget(format!(
                        "'{}' is already a registered worktree; switch branches inside \
                          it instead",
                        canonical_target.display()
                    )));
                }
                return Ok(WorktreeAddOutput {
                    path: canonical_target.to_string_lossy().to_string(),
                    already_exists: true,
                    reattached: false,
                });
            }
        }
    }

    if target_exists
        && fs::read_dir(&target)
            .map_err(|source| {
                WorktreeError::IoRead(format!(
                    "failed to read target directory '{}': {source}",
                    target.display()
                ))
            })?
            .next()
            .transpose()
            .map_err(|source| {
                WorktreeError::IoRead(format!(
                    "failed to read target directory '{}': {source}",
                    target.display()
                ))
            })?
            .is_some()
    {
        return Err(WorktreeError::OperationBlocked(format!(
            "target directory exists and is not empty: {}",
            target.display()
        )));
    }

    // W3-s2 (§C.7): resolve the checkout target FAIL-CLOSED before any side
    // effect — a bad branch/commit, a branch checked out in any scope, or a
    // `-b` collision must refuse with the directory, branch set, registry,
    // and journal untouched.
    let source_commit = Head::current_commit_result().await.map_err(|e| {
        WorktreeError::IoRead(format!(
            "failed to read HEAD while resolving the target: {e}"
        ))
    })?;
    let checkout = if let Some(name) = new_branch {
        if detach {
            return Err(WorktreeError::InvalidTarget(
                "-b/--create-branch cannot be combined with --detach".to_string(),
            ));
        }
        if Branch::find_branch_result(&name, None)
            .await
            .map_err(|e| WorktreeError::IoRead(format!("failed to look up branch: {e}")))?
            .is_some()
        {
            return Err(WorktreeError::OperationBlocked(format!(
                "branch '{name}' already exists; -B/--force are not supported — pick a new \
                  name or check the existing branch out with `worktree add <path> {name}`"
            )));
        }
        let start = match &target_spec {
            Some(spec) => util::get_commit_base(spec).await.map_err(|error| {
                WorktreeError::InvalidTarget(format!(
                    "cannot resolve start-point '{spec}': {error}"
                ))
            })?,
            None => source_commit.ok_or_else(|| {
                WorktreeError::OperationBlocked(
                    "cannot create a branch in an unborn repository (no commits yet)".to_string(),
                )
            })?,
        };
        AddCheckout::CreateBranch { name, start }
    } else if let Some(spec) = &target_spec {
        match Branch::find_branch_result(spec, None)
            .await
            .map_err(|e| WorktreeError::IoRead(format!("failed to look up branch: {e}")))?
        {
            Some(branch) => {
                if detach {
                    AddCheckout::Detached(branch.commit)
                } else {
                    // Branches are SHARED: refuse when ANY scope (including
                    // the one running this command) has the branch out. The
                    // probe is Result-returning — a query failure refuses
                    // (fail closed), never reads as "the branch is free".
                    match Head::branch_checked_out_anywhere_result(spec).await {
                        Err(error) => {
                            return Err(WorktreeError::IoRead(format!(
                                "cannot verify whether branch '{spec}' is checked out: \
                                  {error}"
                            )));
                        }
                        Ok(Some(scope)) => {
                            return Err(WorktreeError::OperationBlocked(format!(
                                "branch '{spec}' is already checked out at worktree \
                                  '{scope}'; use --detach to share its tip read-only"
                            )));
                        }
                        Ok(None) => {}
                    }
                    AddCheckout::AttachBranch { name: spec.clone() }
                }
            }
            None => {
                let commit = util::get_commit_base(spec).await.map_err(|error| {
                    WorktreeError::InvalidTarget(format!(
                        "'{spec}' is neither a local branch nor a resolvable commit \
                          ({error}); Libra does not create branches from remote-tracking \
                          names automatically (Git's DWIM is deferred) — use `-b {spec} \
                          <path> <remote>/{spec}` explicitly"
                    ))
                })?;
                AddCheckout::Detached(commit)
            }
        }
    } else {
        AddCheckout::DetachedAtSource
    };

    // §C.7 identity preflight, BEFORE the directory exists: instance ids are
    // path-derived, so a collision means another entry already claims this
    // identity. Persisting it would produce the registry state `validate_v2`
    // refuses to load — locking the user out of every worktree command — and
    // creating the directory first would leave an unregistered one behind on
    // the refusal path.
    {
        let candidate = util::worktree_instance_id(
            &std::fs::canonicalize(&target).unwrap_or_else(|_| target.clone()),
        );
        if let Some(conflict) = state.identity_conflict_with(&candidate) {
            return Err(WorktreeError::OperationBlocked(format!(
                "cannot register worktree '{}': {conflict}",
                target.display()
            )));
        }
    }

    let mut created_target = false;
    if !target.exists() {
        fs::create_dir_all(&target).map_err(|source| {
            WorktreeError::IoWrite(format!(
                "failed to create worktree directory '{}': {source}",
                target.display()
            ))
        })?;
        created_target = true;
    }

    let link_path = target.join(util::ROOT_DIR);
    if link_path.exists() {
        return Err(WorktreeError::OperationBlocked(format!(
            "target already contains a .libra entry: {}",
            link_path.display()
        )));
    }

    let worktree_id = util::worktree_instance_id(&canonical_target);
    // W1 §C.4.1.1: instance ids are DETERMINISTIC (path-derived), so a
    // worktree re-added where one was previously removed would inherit any
    // scoped rows a best-effort remove/prune GC failed to delete — stale
    // sparse filters would silently re-gate ls-files/diff/hydrate, stale
    // layer ownership would block staging. Sweep the scope STRICTLY before
    // seeding; a sweep failure fails the add (fail closed, nothing seeded).
    let db = crate::internal::db::get_db_conn_instance().await;
    gc_worktree_scoped_rows_strict(&db, &worktree_id, true)
        .await
        .map_err(|e| {
            WorktreeError::IoWrite(format!(
                "cannot register worktree '{}': failed to clear stale scoped rows for its \
                  instance id: {e}",
                target.display()
            ))
        })?;
    // Durable intent for the whole gitdir/populate/registry window (§C.7).
    // Failure paths below roll the filesystem back themselves; a CRASH
    // leaves this row for `worktree repair`, whose `add` recovery sweeps
    // the scope and resolves it (directories are never deleted in
    // recovery).
    let mut add_payload = serde_json::json!({ "path": canonical_target.to_string_lossy() });
    if let AddCheckout::CreateBranch { name, start } = &checkout {
        // Recovery must be able to roll the `-b` branch back tip-
        // conditionally if we crash between its creation and publication.
        add_payload["create_branch"] = serde_json::json!({
            "name": name,
            "start": start.to_string(),
        });
    }
    let add_journal_id = journal_append(
        &db,
        WorktreeControl::Add.declare(),
        Some(&worktree_id),
        &add_payload,
    )
    .await
    .map_err(WorktreeError::OperationBlocked)?;
    create_worktree_gitdir(&storage, &link_path, &worktree_id).map_err(|source| {
        WorktreeError::IoWrite(format!(
            "failed to create per-worktree .libra gitdir in '{}': {source}",
            link_path.display()
        ))
    })?;

    let rollback_partial_add = || {
        let _ = remove_worktree_storage_link(&link_path);
        if created_target {
            let _ = fs::remove_dir_all(&target);
        } else if let Ok(entries) = fs::read_dir(&target) {
            for entry in entries.flatten() {
                let entry_path = entry.path();
                let _ = if entry_path.is_dir() {
                    fs::remove_dir_all(&entry_path)
                } else {
                    fs::remove_file(&entry_path)
                };
            }
        }
    };

    // W3-s2: the seed HEAD per resolved checkout mode. `source_commit` was
    // read via the RESULT-returning API before any side effect — only a
    // genuinely unborn HEAD (None) skips seeding, and only for the
    // no-target mode (explicit targets always carry a commit).
    // Branch-attach lock (W3-s2 §C.7): held from the final
    // checked-out-anywhere re-check through the HEAD seed, serializing with
    // `switch`/`checkout` (which hold it across their check + publication).
    let _attach_lock = if matches!(
        &checkout,
        AddCheckout::AttachBranch { .. } | AddCheckout::CreateBranch { .. }
    ) {
        match util::acquire_branch_attach_lock() {
            Ok(guard) => Some(guard),
            Err(error) => {
                rollback_partial_add();
                // A HANDLED failure fully rolled back — resolve the intent row
                // too, or repair keeps sweeping a scope that no longer exists.
                // Best-effort: an unresolved row is the crash contract anyway.
                let _ = journal_resolve(&db, add_journal_id).await;
                return Err(WorktreeError::IoWrite(format!(
                    "cannot acquire the branch-attach lock: {error}"
                )));
            }
        }
    } else {
        None
    };
    let (seed_head, seed_commit, created_branch): (Option<Head>, Option<_>, Option<(String, _)>) =
        match &checkout {
            AddCheckout::DetachedAtSource => {
                (source_commit.map(Head::Detached), source_commit, None)
            }
            AddCheckout::Detached(commit) => (Some(Head::Detached(*commit)), Some(*commit), None),
            AddCheckout::AttachBranch { name } => {
                // Final re-check UNDER the branch-attach lock, just before
                // the attach becomes durable — over EVERY scope (a
                // concurrent switch may have moved THIS worktree onto the
                // branch), and fail-closed on query errors.
                match Head::branch_checked_out_anywhere_result(name).await {
                    Err(error) => {
                        rollback_partial_add();
                        // A HANDLED failure fully rolled back — resolve the intent row
                        // too, or repair keeps sweeping a scope that no longer exists.
                        // Best-effort: an unresolved row is the crash contract anyway.
                        let _ = journal_resolve(&db, add_journal_id).await;
                        return Err(WorktreeError::IoRead(format!(
                            "cannot verify whether branch '{name}' is checked out: {error}"
                        )));
                    }
                    Ok(Some(scope)) => {
                        rollback_partial_add();
                        // A HANDLED failure fully rolled back — resolve the intent row
                        // too, or repair keeps sweeping a scope that no longer exists.
                        // Best-effort: an unresolved row is the crash contract anyway.
                        let _ = journal_resolve(&db, add_journal_id).await;
                        return Err(WorktreeError::OperationBlocked(format!(
                            "branch '{name}' is already checked out at worktree \
                              '{scope}'; use --detach to share its tip read-only"
                        )));
                    }
                    Ok(None) => {}
                }
                let branch = match Branch::find_branch_result(name, None).await {
                    Ok(Some(branch)) => branch,
                    Ok(None) => {
                        rollback_partial_add();
                        // A HANDLED failure fully rolled back — resolve the intent row
                        // too, or repair keeps sweeping a scope that no longer exists.
                        // Best-effort: an unresolved row is the crash contract anyway.
                        let _ = journal_resolve(&db, add_journal_id).await;
                        return Err(WorktreeError::InvalidTarget(format!(
                            "branch '{name}' disappeared while creating the worktree"
                        )));
                    }
                    Err(e) => {
                        rollback_partial_add();
                        // A HANDLED failure fully rolled back — resolve the intent row
                        // too, or repair keeps sweeping a scope that no longer exists.
                        // Best-effort: an unresolved row is the crash contract anyway.
                        let _ = journal_resolve(&db, add_journal_id).await;
                        return Err(WorktreeError::IoRead(format!(
                            "failed to re-read branch '{name}': {e}"
                        )));
                    }
                };
                (Some(Head::Branch(name.clone())), Some(branch.commit), None)
            }
            AddCheckout::CreateBranch { name, start } => {
                // Collision re-check UNDER the branch-attach lock: the
                // preflight ran before the lock, and `update_branch` would
                // silently overwrite an existing row — a concurrent
                // `add -b <same-name>` must lose here, not double-attach.
                match Branch::find_branch_result(name, None).await {
                    Ok(None) => {}
                    Ok(Some(_)) => {
                        rollback_partial_add();
                        // A HANDLED failure fully rolled back — resolve the intent row
                        // too, or repair keeps sweeping a scope that no longer exists.
                        // Best-effort: an unresolved row is the crash contract anyway.
                        let _ = journal_resolve(&db, add_journal_id).await;
                        return Err(WorktreeError::OperationBlocked(format!(
                            "branch '{name}' was created concurrently; pick another name"
                        )));
                    }
                    Err(e) => {
                        rollback_partial_add();
                        // A HANDLED failure fully rolled back — resolve the intent row
                        // too, or repair keeps sweeping a scope that no longer exists.
                        // Best-effort: an unresolved row is the crash contract anyway.
                        let _ = journal_resolve(&db, add_journal_id).await;
                        return Err(WorktreeError::IoRead(format!(
                            "failed to re-check branch '{name}': {e}"
                        )));
                    }
                }
                // Create the branch row NOW (cwd is still the source
                // worktree); every failure below deletes it back
                // tip-conditionally — no branch-only residue.
                if let Err(e) = Branch::update_branch(name, &start.to_string(), None).await {
                    rollback_partial_add();
                    // A HANDLED failure fully rolled back — resolve the intent row
                    // too, or repair keeps sweeping a scope that no longer exists.
                    // Best-effort: an unresolved row is the crash contract anyway.
                    let _ = journal_resolve(&db, add_journal_id).await;
                    return Err(WorktreeError::IoWrite(format!(
                        "failed to create branch '{name}': {e}"
                    )));
                }
                (
                    Some(Head::Branch(name.clone())),
                    Some(*start),
                    Some((name.clone(), *start)),
                )
            }
        };
    let rollback_created_branch = |created: Option<(String, git_internal::hash::ObjectHash)>| async move {
        if let Some((name, tip)) = created {
            match Branch::delete_branch_if_tip_result(&name, &tip).await {
                Ok(_) => {}
                Err(error) => {
                    tracing::warn!(
                        branch = name,
                        %error,
                        "could not roll back the created branch; delete it manually"
                    );
                }
            }
        }
    };
    if let (Some(seed_head), Some(commit)) = (seed_head, seed_commit) {
        let _ = commit;
        let _guard = match DirGuard::change_to(&target) {
            Ok(g) => g,
            Err(e) => {
                rollback_partial_add();
                rollback_created_branch(created_branch).await;
                // A HANDLED failure fully rolled back — resolve the intent row
                // too, or repair keeps sweeping a scope that no longer exists.
                // Best-effort: an unresolved row is the crash contract anyway.
                let _ = journal_resolve(&db, add_journal_id).await;
                return Err(WorktreeError::IoRead(format!(
                    "failed to enter worktree directory '{}': {e}",
                    target.display()
                )));
            }
        };
        let created_branch = created_branch.clone();
        // §C.4.2: this block acts on ANOTHER worktree than the one the user
        // invoked from, so it re-pins rather than inheriting the invoker's
        // pin. Without this the pinned resolvers — `path::index()` above all
        // — would seed the INVOKER's index from the new worktree's HEAD:
        // main's staged state overwritten, and the new worktree left with an
        // empty index in which every checked-out file reads as deleted.
        let _scope = crate::internal::worktree_scope::WorktreeScope::override_scope(target.clone());
        // lore.md 2.1: seed the NEW worktree's OWN HEAD per the resolved
        // checkout (detached commit, attached branch, or the just-created
        // `-b` branch), so `Head::current()` resolves here and the populate
        // below can read it. A seed-update failure rolls EVERYTHING back —
        // including a `-b` branch row.
        if let Err(e) = Head::update_result(seed_head, None).await {
            // Both the scope and the cwd go back to the invoker BEFORE the
            // rollbacks: those act on the invoking repository's rows.
            drop(_scope);
            drop(_guard);
            rollback_partial_add();
            rollback_created_branch(created_branch).await;
            // A HANDLED failure fully rolled back — resolve the intent row
            // too, or repair keeps sweeping a scope that no longer exists.
            // Best-effort: an unresolved row is the crash contract anyway.
            let _ = journal_resolve(&db, add_journal_id).await;
            return Err(WorktreeError::IoWrite(format!(
                "failed to seed HEAD for worktree '{}': {e}",
                target.display()
            )));
        }
        // Populate from HEAD so new worktrees reflect committed state instead
        // of carrying staged-but-uncommitted index content.
        if let Err(e) = restore::execute_checked(RestoreArgs {
            overlay: false,
            no_overlay: false,
            ours: false,
            theirs: false,
            ignore_unmerged: false,
            merge: false,
            conflict: None,
            pathspec: vec![util::working_dir_string()],
            source: Some("HEAD".to_string()),
            worktree: true,
            // lore.md 2.1: also restore the PRIVATE index to HEAD (a linked
            // worktree no longer shares the main index, so a fresh worktree's
            // index must be seeded to match HEAD or every file reads as a
            // phantom change).
            staged: true,
            pathspec_from_file: None,
            pathspec_file_nul: false,
            no_progress: false,
        })
        .await
        {
            // Restore the invoker's scope and cwd BEFORE the rollbacks:
            // deleting the target while it is the cwd would break the branch
            // rollback's storage resolution (and strand the shell in a
            // removed dir).
            drop(_scope);
            drop(_guard);
            rollback_partial_add();
            rollback_created_branch(created_branch).await;
            // A HANDLED failure fully rolled back — resolve the intent row
            // too, or repair keeps sweeping a scope that no longer exists.
            // Best-effort: an unresolved row is the crash contract anyway.
            let _ = journal_resolve(&db, add_journal_id).await;
            return Err(WorktreeError::IoWrite(format!(
                "failed to populate worktree '{}': {e}",
                target.display()
            )));
        }
    }

    let registration_epoch = state.next_epoch();
    // A linked worktree exists from here on, and that fact must OUTLIVE its
    // entry: the ambiguous-sidecar rules ask "did one ever exist", and a later
    // removal deletes the entry (§C.4.3).
    state.linked_history = LinkedHistory::Existed;
    state.entries.push(WorktreeEntry {
        path: canonical_target.to_string_lossy().to_string(),
        is_main: false,
        locked: false,
        lock_reason: None,
        // v2 (§C.7): persist the stable id at creation so `worktree repair
        // <path>` can later restore a corrupt/missing gitdir identity from
        // the registry.
        worktree_id: Some(worktree_id.clone()),
        state: WorktreeEntryState::Active,
        // A fresh generation for every registration, so a client fenced on the
        // previous one at this same path/id is refused rather than served.
        epoch: registration_epoch,
    });
    if let Err(e) = write_state(&state) {
        rollback_partial_add();
        // A HANDLED failure fully rolled back — resolve the intent row
        // too, or repair keeps sweeping a scope that no longer exists.
        // Best-effort: an unresolved row is the crash contract anyway.
        let _ = journal_resolve(&db, add_journal_id).await;
        if let AddCheckout::CreateBranch { name, start } = &checkout
            && let Err(error) = Branch::delete_branch_if_tip_result(name, start).await
        {
            tracing::warn!(
                branch = name,
                %error,
                "could not roll back the created branch; delete it manually"
            );
        }
        return Err(e);
    }
    if let Err(error) = journal_resolve(&db, add_journal_id).await {
        tracing::warn!(
            error,
            "add journal entry not resolved; repair will reconcile"
        );
    }

    // W0 §C.4.1.1 (plan line 2258, 2026-08-06 revision): probe the TARGET
    // filesystem's case behavior and WARN when it disagrees with the
    // repository's persisted `core.ignorecase` — persisted at init from
    // MAIN's filesystem, which this worktree may not share. The per-worktree
    // config overlay that will store this probe rides the W4 unified
    // resolver; until then the mismatch must at least be visible, not
    // silent (materialization guards fall back to a live per-use probe only
    // when the config key is UNSET).
    warn_on_case_probe_mismatch(&canonical_target).await;

    Ok(WorktreeAddOutput {
        path: canonical_target.to_string_lossy().to_string(),
        already_exists: false,
        reattached: false,
    })
}

/// The `worktree add` target-filesystem probe (W0 §C.4.1.1): compare the
/// repository's persisted `core.ignorecase` with what the target volume
/// actually does, and warn on a mismatch. Best-effort — a probe or config
/// read failure must never fail the add.
async fn warn_on_case_probe_mismatch(target: &std::path::Path) {
    let Ok(Some(entry)) =
        crate::internal::config::ConfigKv::get_var_case_insensitive("core.", "ignorecase").await
    else {
        // No persisted value: materialization guards probe per use, which
        // already answers from this worktree's own filesystem.
        return;
    };
    let persisted = matches!(
        entry.value.trim().to_ascii_lowercase().as_str(),
        "true" | "yes" | "on" | "1"
    );
    let probed = crate::utils::path_case::probe_dir_ignore_case(target);
    if probed != persisted {
        eprintln!(
            "warning: this worktree's filesystem is case-{} but the repository's persisted \
              core.ignorecase is {persisted} (probed from the main worktree at init); \
              case-collision guards here may misjudge until the per-worktree config overlay \
              lands (plan-20260714 W4). Set core.ignorecase explicitly if this repository \
              spans differing filesystems.",
            if probed { "insensitive" } else { "sensitive" }
        );
    }
}

/// Re-attach a detached worktree (W3-s1b, §C.7): verify the directory still
/// carries the SAME identity the registry persisted, then lift the
/// fail-closed marker and reactivate the entry. An identity mismatch (the
/// directory was recreated or swapped) refuses — never silently adopt.
async fn reattach_worktree(
    state: &mut WorktreeState,
    index: usize,
    target: &Path,
) -> WorktreeResult<WorktreeAddOutput> {
    let db = crate::internal::db::get_db_conn_instance().await;
    let Some(expected_id) = state.entries[index].worktree_id.clone() else {
        return Err(WorktreeError::OperationBlocked(format!(
            "cannot re-attach '{}': the registry entry has no persisted worktree id; run \
              `libra worktree repair --confirm` first",
            target.display()
        )));
    };
    let gitdir = target.join(util::ROOT_DIR);
    let current_id = fs::read_to_string(gitdir.join("worktree_id"))
        .ok()
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty());
    if current_id.as_deref() != Some(expected_id.as_str()) {
        return Err(WorktreeError::OperationBlocked(format!(
            "cannot re-attach '{}': its gitdir identity ({}) does not match the registry's \
              persisted id ({expected_id}); run `libra worktree repair --confirm {}` first",
            target.display(),
            current_id.as_deref().unwrap_or("missing"),
            target.display()
        )));
    }
    // The commondir must point at THIS repository's storage: a directory
    // whose (mutable) id happens to match but whose commondir targets
    // another repo must never be re-attached into this one. Missing or
    // corrupt pointers are repair's job, not re-attach's.
    let storage = util::storage_path();
    let commondir_ok = fs::read_to_string(gitdir.join("commondir"))
        .ok()
        .and_then(|contents| {
            contents
                .lines()
                .next()
                .map(str::trim)
                .filter(|line| !line.is_empty())
                .map(PathBuf::from)
        })
        .map(|existing| {
            let existing_abs = if existing.is_absolute() {
                existing
            } else {
                gitdir.join(existing)
            };
            fs::canonicalize(&existing_abs).unwrap_or(existing_abs)
                == fs::canonicalize(&storage).unwrap_or_else(|_| storage.clone())
        })
        .unwrap_or(false);
    if !commondir_ok {
        return Err(WorktreeError::OperationBlocked(format!(
            "cannot re-attach '{}': its commondir pointer is missing, corrupt, or targets a \
              different repository's storage; run `libra worktree repair --confirm {}` first",
            target.display(),
            target.display()
        )));
    }

    let payload = serde_json::json!({
        "path": target.to_string_lossy(),
        "reattach": true,
    });
    let journal_id = journal_append(
        &db,
        WorktreeControl::Add.declare(),
        Some(&expected_id),
        &payload,
    )
    .await
    .map_err(WorktreeError::OperationBlocked)?;

    // Publish Active FIRST, then lift the marker: a crash in between leaves
    // an Active entry whose gitdir still carries the marker — repair's
    // reconcile pass removes a stale marker whose entry is Active with a
    // matching id (and the pending journal row rolls the re-attach
    // forward). The reverse order would leave an UNFROZEN detached entry.
    // §C.7 identity check immediately before ACTIVATION. Re-attaching is the
    // one path that turns a dormant claim on an identity back into a live one,
    // so it is where a former collision can be recreated: detach one side,
    // re-add both directories, and without this the second activation restores
    // two active entries at one identity — poisoning every later registry
    // load, including the doctor that would explain it.
    if let Some(identity) = state.entries[index].worktree_id.clone()
        && state.entries.iter().enumerate().any(|(other, entry)| {
            other != index
                && !entry.is_main
                && entry.state.is_active()
                && entry.worktree_id.as_deref() == Some(identity.as_str())
        })
    {
        return Err(WorktreeError::OperationBlocked(format!(
            "cannot re-attach '{}': identity '{identity}' is already claimed by another ACTIVE \
              worktree. Run `libra worktree doctor`, then \
              `libra worktree repair <path> --resolve-identity --yes` on the one you do not want",
            state.entries[index].path
        )));
    }
    state.entries[index].state = WorktreeEntryState::Active;
    write_state(state)?;
    let marker = gitdir.join(DETACHED_MARKER);
    match fs::remove_file(&marker) {
        Ok(()) => {}
        Err(error) if error.kind() == io::ErrorKind::NotFound => {}
        Err(error) => {
            // Journal kept: repair finishes lifting the marker.
            return Err(WorktreeError::IoWrite(format!(
                "cannot remove the detached marker '{}' (run `libra worktree repair \
                  --confirm` to finish the re-attach): {error}",
                marker.display()
            )));
        }
    }
    if let Err(error) = lifecycle_delete(&db, &expected_id).await {
        tracing::warn!(
            error,
            "lifecycle row not cleared on re-attach; repair reconciles"
        );
    }
    if let Err(error) = journal_resolve(&db, journal_id).await {
        tracing::warn!(
            error,
            "re-attach journal entry not resolved; repair will reconcile"
        );
    }

    Ok(WorktreeAddOutput {
        path: target.to_string_lossy().to_string(),
        already_exists: false,
        reattached: true,
    })
}

fn render_add_worktree(result: &WorktreeAddOutput, output: &OutputConfig) -> CliResult<()> {
    if output.is_json() {
        return emit_json_data("worktree.add", result, output);
    }
    if output.quiet {
        return Ok(());
    }
    if result.already_exists {
        println!("worktree already exists at {}", result.path);
    } else if result.reattached {
        println!("re-attached detached worktree at {}", result.path);
    } else {
        println!("{}", result.path);
    }
    Ok(())
}

/// Resolve a (possibly already-deleted) worktree's stable instance id: read
/// its `.libra/worktree_id` file if present, else recompute deterministically
/// from the canonical path (lore.md 2.1).
fn resolve_worktree_id(target: &Path) -> Option<String> {
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

/// GC a removed worktree's PRIVATE HEAD + HEAD-reflog rows (lore.md 2.1) — and
/// its worktree-scoped sequencer/bisect session rows (Part C W1) — so a reused
/// instance id never inherits stale state. Instance ids are DETERMINISTIC
/// (FNV of the canonical path), so a worktree re-added at the same path gets
/// the same id: a surviving `bisect_state`/`sequence_state` row would make the
/// fresh worktree silently resume a dead session (a resumed bisect step even
/// repaints candidate trees — data loss). Best-effort: a failure is logged,
/// not fatal (the registry drop is the source of truth).
/// Scoped-row sweep (W3-s1b: every caller is STRICT — a failed cleanup
/// becomes a tombstone or fails the operation, never a silent orphan): the
/// first failed DELETE
/// aborts and surfaces the error. `worktree add` uses this as its pre-seed
/// sweep — inheriting another (removed) worktree's rows must fail the add,
/// not proceed with a polluted scope.
/// Upsert this worktree's row in the SQL `worktree_lifecycle` mirror
/// (§C.7 W3-s1b) — the down-migration guard and doctor read lifecycle state
/// from SQL, so every registry state change writes the mirror too.
async fn lifecycle_upsert(
    db: &sea_orm::DatabaseConnection,
    worktree_id: &str,
    state: &str,
    path: &str,
) -> Result<(), String> {
    use sea_orm::{ConnectionTrait, DbBackend, Statement};
    let now = chrono::Utc::now().timestamp_millis();
    db.execute_raw(Statement::from_sql_and_values(
        DbBackend::Sqlite,
        "INSERT INTO worktree_lifecycle (worktree_id, state, path, created_at, updated_at) \
          VALUES (?, ?, ?, ?, ?) \
          ON CONFLICT(worktree_id) DO UPDATE SET state = excluded.state, \
          path = excluded.path, updated_at = excluded.updated_at",
        [
            worktree_id.into(),
            state.into(),
            path.into(),
            now.into(),
            now.into(),
        ],
    ))
    .await
    .map_err(|error| format!("cannot record worktree lifecycle state: {error}"))?;
    Ok(())
}

/// Delete this worktree's lifecycle mirror row (entry back to active, or
/// fully cleaned up).
async fn lifecycle_delete(
    db: &sea_orm::DatabaseConnection,
    worktree_id: &str,
) -> Result<(), String> {
    use sea_orm::{ConnectionTrait, DbBackend, Statement};
    db.execute_raw(Statement::from_sql_and_values(
        DbBackend::Sqlite,
        "DELETE FROM worktree_lifecycle WHERE worktree_id = ?",
        [worktree_id.into()],
    ))
    .await
    .map_err(|error| format!("cannot clear worktree lifecycle state: {error}"))?;
    Ok(())
}

/// Every row of the SQL lifecycle mirror, for the repair sweep that keeps
/// it convergent with the registry (stale rows would block the down
/// migration forever; missing rows would let it proceed wrongly).
async fn lifecycle_rows(db: &sea_orm::DatabaseConnection) -> Result<Vec<(String, String)>, String> {
    use sea_orm::{ConnectionTrait, DbBackend, Statement};
    let rows = db
        .query_all_raw(Statement::from_string(
            DbBackend::Sqlite,
            "SELECT worktree_id, state FROM worktree_lifecycle".to_string(),
        ))
        .await
        .map_err(|error| format!("cannot read the worktree lifecycle mirror: {error}"))?;
    let mut out = Vec::new();
    for row in rows {
        let id: String = row
            .try_get_by_index(0)
            .map_err(|error| format!("corrupt lifecycle row: {error}"))?;
        let state: String = row
            .try_get_by_index(1)
            .map_err(|error| format!("corrupt lifecycle row: {error}"))?;
        out.push((id, state));
    }
    Ok(out)
}

/// Tri-state path probe for recovery decisions: only a NotFound stat
/// PROVES absence; any other error is AMBIGUOUS (permissions, an unmounted
/// volume) and must keep the intent journal pending rather than letting a
/// recovery branch guess.
enum PathPresence {
    Present,
    Missing,
    Unknown(String),
}

fn probe_path(path: &Path) -> PathPresence {
    match fs::symlink_metadata(path) {
        Ok(_) => PathPresence::Present,
        Err(error) if error.kind() == io::ErrorKind::NotFound => PathPresence::Missing,
        Err(error) => PathPresence::Unknown(error.to_string()),
    }
}

/// A pending row in the durable intent journal.
#[derive(Debug, Clone)]
struct PendingIntent {
    id: i64,
    op: String,
    worktree_id: Option<String>,
    payload: serde_json::Value,
}

/// Record a registry-mutation intent BEFORE any filesystem/registry write
/// (§C.7). SQLite cannot join a filesystem rename into one transaction, so
/// this is the recovery anchor: a crash leaves the row behind and
/// `worktree repair` rolls the operation forward or back deterministically.
/// A journal write failure ABORTS the mutation (fail-closed).
async fn journal_append(
    db: &sea_orm::DatabaseConnection,
    op: &str,
    worktree_id: Option<&str>,
    payload: &serde_json::Value,
) -> Result<i64, String> {
    use sea_orm::{ConnectionTrait, DbBackend, Statement};
    let now = chrono::Utc::now().timestamp_millis();
    let result = db
        .execute_raw(Statement::from_sql_and_values(
            DbBackend::Sqlite,
            "INSERT INTO worktree_intent_journal (op, worktree_id, payload, created_at) \
              VALUES (?, ?, ?, ?)",
            [
                op.into(),
                worktree_id.into(),
                payload.to_string().into(),
                now.into(),
            ],
        ))
        .await
        .map_err(|error| format!("cannot record the {op} intent journal entry: {error}"))?;
    Ok(result.last_insert_id() as i64)
}

/// Advance a 'migrate' intent's durable stage (§C.6.2 state machine).
async fn journal_set_stage(
    db: &sea_orm::DatabaseConnection,
    id: i64,
    stage: &str,
) -> Result<(), String> {
    use sea_orm::{ConnectionTrait, DbBackend, Statement};
    db.execute_raw(Statement::from_sql_and_values(
        DbBackend::Sqlite,
        "UPDATE worktree_intent_journal SET stage = ? WHERE id = ?",
        [stage.into(), id.into()],
    ))
    .await
    .map_err(|error| format!("cannot record migration stage {stage}: {error}"))?;
    Ok(())
}

/// Resolve (delete) a journal row after the mutation is fully published.
async fn journal_resolve(db: &sea_orm::DatabaseConnection, id: i64) -> Result<(), String> {
    use sea_orm::{ConnectionTrait, DbBackend, Statement};
    db.execute_raw(Statement::from_sql_and_values(
        DbBackend::Sqlite,
        "DELETE FROM worktree_intent_journal WHERE id = ?",
        [id.into()],
    ))
    .await
    .map_err(|error| format!("cannot resolve intent journal entry {id}: {error}"))?;
    Ok(())
}

/// Enumerate stale pending intents for `worktree repair` recovery.
async fn journal_pending(db: &sea_orm::DatabaseConnection) -> Result<Vec<PendingIntent>, String> {
    use sea_orm::{ConnectionTrait, DbBackend, Statement};
    let rows = db
        .query_all_raw(Statement::from_string(
            DbBackend::Sqlite,
            "SELECT id, op, worktree_id, payload FROM worktree_intent_journal ORDER BY id"
                .to_string(),
        ))
        .await
        .map_err(|error| format!("cannot read the intent journal: {error}"))?;
    let mut pending = Vec::new();
    for row in rows {
        let id: i64 = row
            .try_get_by_index(0)
            .map_err(|error| format!("corrupt intent journal row (id): {error}"))?;
        let op: String = row
            .try_get_by_index(1)
            .map_err(|error| format!("corrupt intent journal row (op): {error}"))?;
        let worktree_id: Option<String> = row
            .try_get_by_index(2)
            .map_err(|error| format!("corrupt intent journal row (worktree_id): {error}"))?;
        let payload_raw: String = row
            .try_get_by_index(3)
            .map_err(|error| format!("corrupt intent journal row (payload): {error}"))?;
        let payload = serde_json::from_str(&payload_raw)
            .map_err(|error| format!("corrupt intent journal payload (id {id}): {error}"))?;
        pending.push(PendingIntent {
            id,
            op,
            worktree_id,
            payload,
        });
    }
    Ok(pending)
}

/// True when this scope has ACTIVE sequencer/rebase/bisect state — remove
/// and prune refuse to detach/GC such a worktree (§C.7: "active
/// sequencer/bisect … 默认拒绝"). Fails CLOSED (treated active) on a read
/// error: an unreadable state must not be destroyed.
async fn scoped_state_active(db: &sea_orm::DatabaseConnection, worktree_id: &str) -> bool {
    use sea_orm::{ConnectionTrait, DbBackend, Statement};
    for table in ["sequence_state", "rebase_state", "bisect_state"] {
        let query = format!("SELECT COUNT(*) FROM {table} WHERE worktree_id = ?");
        match db
            .query_one_raw(Statement::from_sql_and_values(
                DbBackend::Sqlite,
                &query,
                [worktree_id.into()],
            ))
            .await
        {
            Ok(Some(row)) => match row.try_get_by_index::<i64>(0) {
                Ok(0) => {}
                Ok(_) => return true,
                Err(_) => return true,
            },
            Ok(None) => {}
            Err(_) => return true,
        }
    }
    false
}

async fn gc_worktree_scoped_rows_strict(
    db: &sea_orm::DatabaseConnection,
    worktree_id: &str,
    directory_gone: bool,
) -> Result<(), String> {
    use sea_orm::{ConnectionTrait, DbBackend, Statement};
    let mut stmts = vec![
        "DELETE FROM reference WHERE worktree_id = ? AND kind = 'Head'",
        "DELETE FROM reflog WHERE worktree_id = ?",
        "DELETE FROM sequence_state WHERE worktree_id = ?",
        "DELETE FROM rebase_state WHERE worktree_id = ?",
        "DELETE FROM working_dirty WHERE worktree_id = ?",
        "DELETE FROM working_dirty_meta WHERE worktree_id = ?",
    ];
    // Layer registrations/ownership and the sparse view (W1 §C.4.1.1):
    // purged ONLY when the worktree directory is actually gone
    // (`--delete-dir`, prune, or it had already vanished). A default
    // `remove` RETAINS the directory — and a retained `.libra` still
    // operates as a repository — so its layer ownership rows must survive
    // to keep the still-materialized overlay files un-stageable
    // (never-enters-commit), and its sparse view keeps filtering that
    // directory's queries. The retained directory cannot be re-registered
    // while non-empty (`worktree add` refuses), so the rows guard it until
    // the directory is cleared; orphaned rows are then reclaimed by the W3
    // worktree doctor (they are invisible to every live scope meanwhile).
    if directory_gone {
        stmts.push("DELETE FROM layer WHERE worktree_id = ?");
        stmts.push("DELETE FROM layer_path WHERE worktree_id = ?");
        stmts.push("DELETE FROM sparse_view WHERE worktree_id = ?");
        stmts.push("DELETE FROM sparse_view_meta WHERE worktree_id = ?");
    }
    // `bisect_state` is owned by migration `2026072301`, but bare or
    // pre-migration test databases may still lack it — only purge when the
    // table exists (a DELETE on a missing table would log a spurious warn).
    let has_bisect_table = db
        .query_one_raw(Statement::from_string(
            DbBackend::Sqlite,
            "SELECT name FROM sqlite_master WHERE type = 'table' AND name = 'bisect_state'",
        ))
        .await
        .ok()
        .flatten()
        .is_some();
    if has_bisect_table {
        stmts.push("DELETE FROM bisect_state WHERE worktree_id = ?");
    }
    for sql in stmts {
        db.execute_raw(Statement::from_sql_and_values(
            DbBackend::Sqlite,
            sql,
            [worktree_id.into()],
        ))
        .await
        .map_err(|e| format!("{sql}: {e}"))?;
    }
    Ok(())
}

/// Create a linked worktree's REAL `.libra` gitdir (lore.md 2.1) instead of a
/// symlink: it holds `commondir` (pointing at the shared `.libra` for
/// db/objects/hooks) and `worktree_id` (its private HEAD/index scope). The
/// per-worktree `index` is created later when the worktree is populated.
fn create_worktree_gitdir(
    common_storage: &Path,
    gitdir: &Path,
    worktree_id: &str,
) -> io::Result<()> {
    fs::create_dir_all(gitdir)?;
    fs::write(
        gitdir.join("commondir"),
        format!("{}\n", common_storage.display()),
    )?;
    fs::write(gitdir.join("worktree_id"), format!("{worktree_id}\n"))?;
    Ok(())
}

fn remove_worktree_storage_link(link_path: &Path) -> io::Result<()> {
    let metadata = fs::symlink_metadata(link_path)?;
    if metadata.file_type().is_symlink() {
        return fs::remove_file(link_path);
    }
    if metadata.is_dir() {
        return fs::remove_dir_all(link_path);
    }
    fs::remove_file(link_path)
}

/// Implements `worktree list`.
///
/// Each registered worktree is printed on its own line as either
/// `main <path>` or `worktree <path>`, with optional `[locked: <reason>]`
/// suffix when the entry is locked.
pub(crate) fn run_list_worktrees() -> WorktreeResult<WorktreeListOutput> {
    run_list_worktrees_at(&state_path())
}

/// [`run_list_worktrees`] against an EXPLICIT registry path (§C.4.2) — see
/// `load_state_readonly_at`.
pub(crate) fn run_list_worktrees_at(
    registry_path: &std::path::Path,
) -> WorktreeResult<WorktreeListOutput> {
    let state = load_state_readonly_at(registry_path)?;
    let worktrees = state
        .entries
        .into_iter()
        .map(|w| {
            // v2: prefer the registry's PERSISTED stable id; fall back to
            // the gitdir/synthesis probe for rows the upgrade could not
            // backfill.
            let worktree_id = w
                .worktree_id
                .clone()
                .or_else(|| resolve_entry_worktree_id(&w.path, w.is_main));
            let layout = detect_entry_layout(Path::new(&w.path), w.is_main);
            WorktreeListEntry {
                kind: if w.is_main { "main" } else { "worktree" },
                exists: Path::new(&w.path).exists(),
                worktree_id,
                state: w.state.as_str(),
                layout,
                epoch: w.epoch,
                path: w.path,
                is_main: w.is_main,
                locked: w.locked,
                lock_reason: w.lock_reason,
            }
        })
        .collect();
    Ok(WorktreeListOutput { worktrees })
}

/// Classify a registered worktree's ON-DISK layout (W3-s3 §C.6.1).
/// Read-only: `legacy-symlink` (a pre-isolation `.libra` symlink pointing at
/// the common storage — shares main's HEAD/index) is recognized here so
/// `repair --migrate-layout` can target it and mutation commands can refuse
/// it; anything unexplainable is `corrupt`, never guessed.
pub(crate) fn detect_entry_layout(path: &Path, is_main: bool) -> &'static str {
    if is_main {
        return "main";
    }
    match fs::symlink_metadata(path) {
        Err(error) if error.kind() == io::ErrorKind::NotFound => return "missing",
        Err(_) => return "corrupt",
        Ok(_) => {}
    }
    let gitdir = path.join(util::ROOT_DIR);
    match fs::symlink_metadata(&gitdir) {
        Err(error) if error.kind() == io::ErrorKind::NotFound => "corrupt",
        Err(_) => "corrupt",
        Ok(meta) if meta.file_type().is_symlink() => {
            let storage = util::storage_path();
            let resolved = fs::canonicalize(&gitdir).ok();
            let canonical_storage = fs::canonicalize(&storage).unwrap_or(storage);
            if resolved.as_deref() == Some(canonical_storage.as_path()) {
                "legacy-symlink"
            } else {
                "corrupt"
            }
        }
        Ok(meta) if meta.is_dir() => {
            let storage = util::storage_path();
            let canonical_storage = fs::canonicalize(&storage).unwrap_or(storage);
            let commondir_ok = fs::read_to_string(gitdir.join("commondir"))
                .ok()
                .and_then(|raw| raw.lines().next().map(str::trim).map(PathBuf::from))
                .map(|p| {
                    let abs = if p.is_absolute() { p } else { gitdir.join(p) };
                    fs::canonicalize(&abs).unwrap_or(abs)
                })
                .is_some_and(|p| p == canonical_storage);
            if commondir_ok || gitdir.join(DETACHED_MARKER).exists() {
                "linked-v2"
            } else {
                "corrupt"
            }
        }
        Ok(_) => "corrupt",
    }
}

/// Resolve a registered worktree's stable id from its path (Part C §C.3.3).
/// The main worktree's HEAD row is keyed `worktree_id IS NULL`, so it returns
/// `None`. A linked worktree returns its stored `.libra/worktree_id`, falling
/// back to the canonical-path derivation used at creation (so a recovered or
/// moved worktree still maps to its own scoped rows rather than aliasing main).
pub(crate) fn resolve_entry_worktree_id(path: &str, is_main: bool) -> Option<String> {
    if is_main {
        return None;
    }
    let gitdir = Path::new(path).join(util::ROOT_DIR);
    if let Ok(id) = fs::read_to_string(gitdir.join("worktree_id")) {
        let id = id.trim();
        if !id.is_empty() {
            return Some(id.to_string());
        }
    }
    let canonical = fs::canonicalize(path).unwrap_or_else(|_| PathBuf::from(path));
    Some(util::worktree_instance_id(&canonical))
}

/// Render the worktree list as Git-style `--porcelain` output: one attribute
/// per line, with a blank line between worktrees. In the isolated layout each
/// worktree owns its own HEAD (index and HEAD reflog too), so each entry
/// reports ITS OWN `HEAD <sha>` and either `branch <ref>` or `detached`
/// (Git semantics, Part C §C.3.3) — never the running command's HEAD stamped
/// onto every entry. A worktree with no resolvable HEAD row (a legacy-symlink
/// layout, or a missing/corrupt scope) emits neither line rather than
/// mislabeling it with another worktree's commit.
pub(crate) async fn format_worktree_porcelain(
    worktrees: &[WorktreeListEntry],
) -> Result<String, crate::internal::branch::BranchStoreError> {
    let mut out = String::new();
    for w in worktrees {
        out.push_str("worktree ");
        out.push_str(&w.path);
        out.push('\n');
        match Head::head_for_worktree_scope(w.worktree_id.as_deref()).await {
            Ok(Some((head, commit))) => {
                if let Some(sha) = commit {
                    out.push_str(&format!("HEAD {sha}\n"));
                }
                match head {
                    Head::Branch(name) => {
                        let full = if name.starts_with("refs/") {
                            name
                        } else {
                            format!("refs/heads/{name}")
                        };
                        out.push_str(&format!("branch {full}\n"));
                    }
                    Head::Detached(_) => out.push_str("detached\n"),
                }
            }
            // No HEAD row for this scope (legacy layout / missing): omit HEAD
            // lines deterministically rather than stamping a wrong commit.
            Ok(None) => {}
            // A CORRUPT row (unparseable id, or one whose hash algorithm is
            // not this repository's) is a different thing entirely: swallowing
            // it printed a successful listing with the HEAD lines silently
            // absent. Fail closed with the store's own message.
            Err(error) => return Err(error),
        }
        if w.locked {
            match w.lock_reason.as_deref() {
                Some(reason) if !reason.is_empty() => out.push_str(&format!("locked {reason}\n")),
                _ => out.push_str("locked\n"),
            }
        }
        // W3-s3 (§C.6.1): versioned layout line — additive to the frozen
        // porcelain attributes (declared in COMPATIBILITY/docs).
        out.push_str(&format!("layout {}\n", w.layout));
        out.push('\n');
    }
    Ok(out)
}

/// plan-20260714 W0 (§C.11): `worktree doctor` — read-only scope diagnostics.
///
/// Every value here comes from a READ: the registry (already loaded
/// read-only), each entry's on-disk layout, and whether its identity is one
/// the registry knows. Nothing is written, adopted, reclaimed or repaired,
/// because a diagnostic that mutates cannot be run safely on a repository you
/// do not yet understand — which is exactly when you reach for it.
/// The WORKTREE-scope half of `worktree doctor` (§C.11 W0): per-worktree
/// layout, lifecycle and identity findings.
///
/// The workspace-scope half lives in [`run_worktree_doctor`]. Both are
/// reported by one bare invocation, in one envelope, because the W0 document
/// promised that W4 would ADD to this report rather than reshape it.
pub(crate) async fn collect_worktree_scope_report(
    conn: &sea_orm::DatabaseConnection,
) -> CliResult<WorktreeDoctorOutput> {
    let listed = run_list_worktrees().map_err(WorktreeError::into_cli_error)?;
    // Which identities are claimed by more than one entry, and by which paths.
    let mut duplicate_identity_paths: std::collections::HashMap<String, Vec<String>> =
        std::collections::HashMap::new();
    for entry in &listed.worktrees {
        if entry.is_main {
            continue;
        }
        // Only LIVE registrations claim an identity — mirroring the mutation
        // guard (`identity_conflict`): a detached/tombstoned entry keeps its
        // id for attribution, and the documented `--resolve-identity` fix
        // produces exactly one Active + one Detached entry sharing an id.
        // Counting those as a collision would report a problem the
        // recommended command then refuses to act on.
        if entry.state != "active" {
            continue;
        }
        if let Some(id) = entry.worktree_id.as_deref() {
            duplicate_identity_paths
                .entry(id.to_string())
                .or_default()
                .push(entry.path.clone());
        }
    }
    let mut diagnostics = Vec::with_capacity(listed.worktrees.len());
    for entry in listed.worktrees {
        // Compare the worktree's OWN on-disk identity against the registry —
        // NOT `entry.worktree_id`, which `run_list_worktrees` already
        // resolves from the registry and which would therefore always agree
        // with it. The question this answers is whether the directory still
        // knows which registry row it belongs to.
        let identity_registered = if entry.is_main {
            // Main is spelled "no id" by convention, not by damage.
            true
        } else {
            resolve_entry_worktree_id(&entry.path, false)
                .and_then(|id| {
                    registry_knows_linked_worktree_in_storage(
                        &util::storage_path(),
                        &id,
                        Some(std::path::Path::new(&entry.path)),
                    )
                })
                .unwrap_or(false)
        };
        let mut findings = Vec::new();
        match entry.layout {
            "legacy-symlink" => findings.push(
                "uses the pre-isolation shared-`.libra` symlink layout; mutations here are \
                  refused because they would move the MAIN worktree's HEAD/index. Migrate with \
                  `libra worktree repair --migrate-layout --confirm <path>` from the main \
                  worktree."
                    .to_string(),
            ),
            "missing" => findings.push(
                "the registered directory is gone; `libra worktree prune` removes entries whose \
                  path is confirmed missing."
                    .to_string(),
            ),
            "corrupt" => findings.push(
                "the worktree's `.libra` metadata could not be read; `libra worktree repair \
                  --confirm <path>` restores its identity from the registry."
                    .to_string(),
            ),
            _ => {}
        }
        if !identity_registered && entry.layout != "missing" {
            findings.push(
                "this worktree's identity is not one the registry knows, so mutations here are \
                  refused; `libra worktree repair --confirm <path>` restores it from the \
                  registry's persisted id."
                    .to_string(),
            );
        }
        if entry.state == "detached_from_registry" {
            findings.push(
                "detached from the registry: re-attach with `libra worktree add <path>`, or \
                  finish the removal with `libra worktree remove --delete-dir <path>`."
                    .to_string(),
            );
        }
        if entry.state == "tombstone" {
            findings.push(
                "a removal did not finish cleaning up; `libra worktree repair --confirm` \
                  retries it."
                    .to_string(),
            );
        }
        // §C.4.3: layer/sparse settings whose provenance cannot be established.
        //
        // The scope migrations attributed pre-existing repository-global rows to
        // MAIN. That is right when main was always the only worktree, and
        // unprovable once a linked worktree existed and was removed before the
        // migration — its entry, and its HEAD row, are gone. Adoption is not
        // destructive (the overlay files stay on disk and their ownership rows
        // keep them unstageable), so this is REPORTED rather than blocked: the
        // user is the only one who knows whether these settings are theirs, and
        // `layer remove` / `sparse-view clear` are the ordinary way to drop them.
        if entry.is_main && crate::command::maintenance::repository_had_linked_worktrees() {
            match adopted_scope_settings_present(conn).await {
                Ok(Some(kinds)) => findings.push(format!(
                    "this worktree holds {kinds} that may have been adopted from a linked \
                      worktree removed before the scope migration — their provenance cannot be \
                      established (§C.4.3). Review them with `libra layer list` / \
                      `libra sparse-view list`; `libra layer remove <name>` and \
                      `libra sparse-view clear` drop the ones that are not yours. No file in the \
                      working tree is affected either way."
                )),
                Ok(None) => {}
                // Fail closed (§C.13): a diagnostic that could not look must say
                // so, not report "nothing to see".
                Err(error) => findings.push(format!(
                    "this repository has linked-worktree history, and whether it holds \
                      layer/sparse settings of unknown provenance COULD NOT BE DETERMINED: \
                      {error}. Treat the answer as unknown until the repository database is \
                      readable."
                )),
            }
        }
        // W0 §C.4.1.1 (plan line 2262) origin diagnostics: info files are
        // worktree-local since W0, so common `.libra/info/*` applies ONLY to
        // main. When linked worktrees exist, say so on the main entry and
        // name the explicit adopt/clear actions — never auto-copy.
        if entry.is_main && !duplicate_identity_paths.is_empty() {
            let common_info: Vec<&str> = WORKTREE_INFO_FILE_NAMES
                .iter()
                .copied()
                .filter(|name| {
                    let worktree_root = std::path::Path::new(&entry.path);
                    let path = worktree_root
                        .join(crate::utils::util::ROOT_DIR)
                        .join("info")
                        .join(name);
                    path.is_file() && info_file_has_effective_content(&path, name, worktree_root)
                })
                .collect();
            if !common_info.is_empty() {
                findings.push(format!(
                    "common info file(s) {} apply ONLY to this main worktree since W0 \
                      (info files are worktree-local; linked worktrees read their own \
                      `.libra/info/*`). Copy them into one linked worktree with \
                      `libra worktree doctor --adopt-info-to <path> --confirm`, or delete \
                      them with `libra worktree doctor --clear-common-info --confirm`.",
                    common_info
                        .iter()
                        .map(|name| format!("info/{name}"))
                        .collect::<Vec<_>>()
                        .join(", ")
                ));
            }
        }
        // A registry an older binary could produce: `add A` → `move A B` →
        // `add A` keeps A's path-derived id on the moved entry, so two entries
        // claim one identity. Every MUTATION is refused while that holds, so
        // doctor has to be the place it becomes visible — and has to name both
        // paths, because the fix is choosing which one to keep.
        if let Some(id) = entry.worktree_id.as_deref()
            && !entry.is_main
            && let Some(other) = duplicate_identity_paths.get(id)
            && other.len() > 1
        {
            findings.push(format!(
                "this identity ('{id}') is claimed by {} entries ({}); every worktree MUTATION \
                  is refused until one is unregistered — \
                  `libra worktree repair <path> --resolve-identity --yes` on the one you do not \
                  want. Ordinary remove cannot do it: it needs the same loader that is refusing.",
                other.len(),
                other.join(", ")
            ));
        }
        // W0 §C.4.1.1 origin inventory: which local info sources THIS
        // worktree's ignore/attributes engines actually read — via the SAME
        // resolver the engines use (`worktree_info_file_paths`), so a
        // dual-layout tree's `.git/info/*` candidates are reported too. For
        // a legacy-symlink layout the resolution follows the symlink to
        // main's gitdir — truthfully that worktree's view until migrated.
        let info_sources: Vec<String> = {
            let worktree_root = std::path::Path::new(&entry.path);
            let mut sources = Vec::new();
            for name in WORKTREE_INFO_FILE_NAMES {
                for candidate in crate::utils::util::worktree_info_file_paths(worktree_root, name) {
                    if candidate.is_file()
                        && info_file_has_effective_content(&candidate, name, worktree_root)
                    {
                        let label = candidate
                            .strip_prefix(worktree_root)
                            .map(|relative| relative.display().to_string())
                            .unwrap_or_else(|_| candidate.display().to_string());
                        sources.push(label);
                    }
                }
            }
            sources
        };
        diagnostics.push(WorktreeDiagnostic {
            worktree_id: entry.worktree_id,
            path: entry.path,
            is_main: entry.is_main,
            layout: entry.layout,
            state: entry.state,
            identity_registered,
            info_sources,
            findings,
        });
    }
    // Stable order so a diff between two runs is meaningful.
    diagnostics.sort_by(|a, b| a.path.cmp(&b.path));

    Ok(WorktreeDoctorOutput {
        schema_version: 1,
        diagnostics,
        next_cursor: None,
    })
}

/// The two W0 §C.4.1.1 info-file names the doctor actions operate on.
const WORKTREE_INFO_FILE_NAMES: [&str; 2] = ["exclude", "attributes"];

/// True when the info file at `path` carries at least one rule the ENGINE
/// would actually apply — asked of the ENGINES' OWN parsers, never a
/// line-shape heuristic (a heuristic disagrees in both directions: an
/// indented `#…` IS a gitignore pattern, and an attributes line with no
/// assignment is NOT a rule). The comments-only template `libra init`
/// writes matches nothing, so it is neither a reportable source nor
/// grounds for adoption advice — without this filter every freshly
/// initialized multi-worktree repository would carry a doctor finding
/// about a file that does nothing.
///
/// `base` is the worktree root the rules would be resolved against.
fn info_file_has_effective_content(path: &std::path::Path, name: &str, base: &Path) -> bool {
    match name {
        "exclude" => crate::utils::util::ignore_file_defines_any_pattern(path, base),
        "attributes" => crate::utils::attributes::file_defines_any_rule(path, base),
        // Unreachable for WORKTREE_INFO_FILE_NAMES; fail closed (report it)
        // rather than silently hiding an unknown info source.
        _ => path.is_file(),
    }
}

/// `worktree doctor --adopt-info-to <path>` (W0 §C.4.1.1, plan line 2262):
/// copy the repository's common `info/exclude`/`info/attributes` — main's
/// `.libra/info/*`, which since W0 applies only to the MAIN worktree — into
/// ONE linked worktree's local gitdir. Explicit and per-worktree, never
/// automatic, never all worktrees at once. A destination file that already
/// exists is kept (the worktree's own view wins) and reported as skipped.
fn adopt_common_info_files(target: &str) -> CliResult<String> {
    let common = crate::utils::util::try_get_storage_path(None).map_err(|error| {
        CliError::fatal(format!(
            "cannot resolve the repository common storage: {error}"
        ))
    })?;
    let target_path = std::path::PathBuf::from(target);
    let target_gitdir = target_path.join(crate::utils::util::ROOT_DIR);
    // No-follow, and only a definitive NotFound means "not linked": a
    // dangling or uninspectable commondir marker must reach the resolver
    // below, whose corruption message names the real problem rather than
    // claiming the path is not a worktree at all.
    if matches!(
        std::fs::symlink_metadata(target_gitdir.join("commondir")),
        Err(ref error) if error.kind() == std::io::ErrorKind::NotFound
    ) {
        return Err(CliError::command_usage(format!(
            "'{target}' is not a linked worktree of this repository (no `.libra/commondir`); \
              --adopt-info-to copies INTO a linked worktree's own gitdir"
        )));
    }
    let target_common = crate::utils::util::try_get_storage_path(Some(target_path.clone()))
        .map_err(|error| {
            CliError::fatal(format!(
                "cannot resolve '{target}' as a worktree of this repository: {error}"
            ))
        })?;
    if target_common != common {
        return Err(CliError::command_usage(format!(
            "'{target}' belongs to a DIFFERENT repository (its common storage is '{}'); \
              refusing to copy this repository's info files there",
            target_common.display()
        )));
    }
    let mut copied = Vec::new();
    let mut skipped = Vec::new();
    for name in WORKTREE_INFO_FILE_NAMES {
        let source = common.join("info").join(name);
        let mut reader = match std::fs::File::open(&source) {
            Ok(reader) => reader,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => continue,
            Err(error) => {
                return Err(CliError::fatal(format!(
                    "cannot read '{}': {error}",
                    source.display()
                )));
            }
        };
        let destination_dir = target_gitdir.join("info");
        std::fs::create_dir_all(&destination_dir).map_err(|error| {
            CliError::fatal(format!(
                "cannot create '{}': {error}",
                destination_dir.display()
            ))
        })?;
        let destination = destination_dir.join(name);
        // FAILURE-ATOMIC, NO-OVERWRITE publish: STREAM the source into a
        // same-directory temp file (bounded memory; no partial file can
        // ever sit at the final name), then `hard_link` it into place —
        // link(2) fails with AlreadyExists atomically and never follows a
        // symlink at the destination, so a concurrent creator's
        // worktree-local policy can neither be truncated nor redirected,
        // and a crash mid-copy leaves only a temp file the next run
        // removes.
        let temp_path = destination_dir.join(format!(".{name}.adopt-tmp-{}", std::process::id()));
        let publish = (|| -> std::io::Result<bool> {
            let mut temp = std::fs::OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(&temp_path)?;
            std::io::copy(&mut reader, &mut temp)?;
            temp.sync_all()?;
            drop(temp);
            match std::fs::hard_link(&temp_path, &destination) {
                Ok(()) => Ok(true),
                Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => Ok(false),
                Err(error) => Err(error),
            }
        })();
        // The temp file never survives, success or failure.
        let _ = std::fs::remove_file(&temp_path);
        match publish {
            Ok(true) => copied.push(format!("info/{name}")),
            Ok(false) => skipped.push(format!("info/{name} (destination already exists)")),
            Err(error) => {
                return Err(CliError::fatal(format!(
                    "cannot adopt '{}' into '{}': {error}",
                    source.display(),
                    destination.display()
                )));
            }
        }
    }
    if copied.is_empty() && skipped.is_empty() {
        return Ok(
            "nothing to adopt: the common storage has no info/exclude or \
                    info/attributes"
                .to_string(),
        );
    }
    let mut lines = Vec::new();
    if !copied.is_empty() {
        lines.push(format!("adopted into '{target}': {}", copied.join(", ")));
    }
    if !skipped.is_empty() {
        lines.push(format!("skipped: {}", skipped.join(", ")));
    }
    Ok(lines.join("\n"))
}

/// `worktree doctor --clear-common-info` (W0 §C.4.1.1): delete the
/// repository's common `.libra/info/exclude` and `info/attributes` — the
/// explicit "these rules should no longer apply anywhere" action. Absent
/// files are fine; the report says exactly what was removed.
fn clear_common_info_files() -> CliResult<String> {
    let common = crate::utils::util::try_get_storage_path(None).map_err(|error| {
        CliError::fatal(format!(
            "cannot resolve the repository common storage: {error}"
        ))
    })?;
    let mut removed = Vec::new();
    for name in WORKTREE_INFO_FILE_NAMES {
        let path = common.join("info").join(name);
        match std::fs::remove_file(&path) {
            Ok(()) => removed.push(format!("info/{name}")),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => {
                return Err(CliError::fatal(format!(
                    "cannot remove '{}': {error}",
                    path.display()
                )));
            }
        }
    }
    if removed.is_empty() {
        Ok(
            "nothing to clear: the common storage has no info/exclude or info/attributes"
                .to_string(),
        )
    } else {
        Ok(format!(
            "cleared from common storage: {}",
            removed.join(", ")
        ))
    }
}

/// Text rendering of the worktree-scope half.
fn print_worktree_scope_report(report: &WorktreeDoctorOutput) {
    let mut healthy = true;
    for diagnostic in &report.diagnostics {
        let identity = diagnostic.worktree_id.as_deref().unwrap_or("(main)");
        println!(
            "{} [{}] identity {} layout {} state {}",
            diagnostic.path,
            if diagnostic.is_main { "main" } else { "linked" },
            identity,
            diagnostic.layout,
            diagnostic.state
        );
        if !diagnostic.info_sources.is_empty() {
            println!(
                "  info sources (this worktree's own gitdir): {}",
                diagnostic.info_sources.join(", ")
            );
        }
        for finding in &diagnostic.findings {
            healthy = false;
            println!("  ! {finding}");
        }
    }
    if healthy {
        println!("no problems detected");
    }
}

/// §C.8: the JSON `data` half of `worktree list` carries its OWN
/// `schema_version` — the global `--json` flag controls format only, never
/// the schema.
#[derive(Serialize)]
pub(crate) struct VersionedWorktreeList<'a> {
    pub(crate) schema_version: u32,
    pub(crate) worktrees: &'a [WorktreeListEntry],
}

async fn list_worktrees(
    output: &OutputConfig,
    porcelain: bool,
    schema_version: u32,
) -> CliResult<()> {
    if schema_version != 2 {
        return Err(CliError::failure(format!(
            "unsupported worktree list schema version {schema_version}: the shipped shape is \
              version 2 (worktree_id/layout/epoch fields); the pre-identity v1 shape gained \
              those fields in place and no frozen v1 remains to serve"
        ))
        .with_exit_code(129)
        .with_stable_code(StableErrorCode::CliInvalidArguments));
    }
    let result = run_list_worktrees().map_err(WorktreeError::into_cli_error)?;
    if output.is_json() {
        return emit_json_data(
            "worktree.list",
            &VersionedWorktreeList {
                schema_version: 2,
                worktrees: &result.worktrees,
            },
            output,
        );
    }
    if output.quiet {
        return Ok(());
    }
    if porcelain {
        let porcelain = format_worktree_porcelain(&result.worktrees)
            .await
            .map_err(|error| {
                CliError::fatal(format!("cannot list worktrees: {error}"))
                    .with_stable_code(StableErrorCode::RepoStateInvalid)
                    .with_hint("a stored HEAD reference is corrupt; repair it and retry")
            })?;
        print!("{porcelain}");
        return Ok(());
    }
    for w in result.worktrees {
        let mut line = String::new();
        if w.is_main {
            line.push_str("main ");
        } else {
            line.push_str("worktree ");
        }
        line.push_str(&w.path);
        if w.locked {
            line.push_str(" [locked");
            if let Some(reason) = w.lock_reason.as_ref()
                && !reason.is_empty()
            {
                line.push_str(": ");
                line.push_str(reason);
            }
            line.push(']');
        }
        println!("{}", line);
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// `worktree doctor` — read-only scope diagnosis (plan-20260714 §C.8/C.13 W4)
// ---------------------------------------------------------------------------
//
// Grammar is FROZEN (§C.8, Codex R18/R19):
//
//     libra worktree doctor [<workspace-id>] [--limit N] [--cursor C]
//     libra worktree doctor <workspace-id> --adopt-capture-session <session-id> --confirm
//
// * no id  -> paginated view: `data.diagnostics[]` + `data.next_cursor`
// * an id  -> single-scope view: `data.diagnostic` (singular, NO pagination)
// * id together with `--limit`/`--cursor` -> usage error (`LBR-CLI-002`)
//
// The default invocation is STRICTLY READ-ONLY: it opens no write path, takes
// no lease, never adopts a legacy row, and never rewrites the registry (it uses
// the lockless `load_state_readonly` reader). Legacy capture adoption is a
// separate, explicitly-confirmed grammar; reclaim/clear/repair are not part
// of this slice, so no hint may promise those actions.

/// `data.schema_version` of both doctor payloads. Additive evolution only.
const DOCTOR_SCHEMA_VERSION: u32 = 1;

/// Version tag inside the opaque cursor. A cursor from a different listing (or
/// a hand-written `workspace_id`) therefore fails to decode instead of silently
/// paginating from an unrelated position.
const DOCTOR_CURSOR_TAG: &str = "wtdoctor1:";

fn encode_doctor_cursor(workspace_id: &str) -> String {
    use base64::Engine as _;
    base64::engine::general_purpose::URL_SAFE_NO_PAD
        .encode(format!("{DOCTOR_CURSOR_TAG}{workspace_id}"))
}

/// Decode an opaque cursor back into the `workspace_id` to resume after.
///
/// Fails closed (`LBR-WORKTREE-001`) rather than restarting at page one: a
/// caller walking the registry page by page would otherwise re-read rows it
/// already processed and believe it had seen everything.
fn decode_doctor_cursor(raw: &str) -> CliResult<String> {
    use base64::Engine as _;
    let invalid = || {
        CliError::fatal(format!(
            "pagination cursor '{raw}' was not issued by `libra worktree doctor` (or has expired)"
        ))
        .with_stable_code(StableErrorCode::WorktreeCursorInvalid)
        .with_hint("drop the cursor and re-read the first page")
    };
    let bytes = base64::engine::general_purpose::URL_SAFE_NO_PAD
        .decode(raw)
        .map_err(|_| invalid())?;
    let decoded = String::from_utf8(bytes).map_err(|_| invalid())?;
    let workspace_id = decoded
        .strip_prefix(DOCTOR_CURSOR_TAG)
        .ok_or_else(invalid)?;
    if workspace_id.is_empty() {
        return Err(invalid());
    }
    Ok(workspace_id.to_string())
}

/// A scope is corrupt or unreadable, so the report would be incomplete.
/// The doctor never degrades to a partial diagnosis (§C.13): an operator
/// acting on a silently-truncated report is worse off than one told the scope
/// cannot be read.
fn doctor_scope_corrupt(message: impl Into<String>) -> CliError {
    CliError::fatal(message.into())
        .with_stable_code(StableErrorCode::WorktreeScopeCorrupt)
        .with_hint("repair the reported scope, then re-run `libra worktree doctor`")
}

/// One finding about a workspace scope. `code` is the stable machine key;
/// `severity` is `warning` (needs attention) or `error` (blocks normal use).
#[derive(Debug, Serialize)]
struct ScopeDiagnostic {
    code: &'static str,
    severity: &'static str,
    detail: String,
}

impl ScopeDiagnostic {
    fn warning(code: &'static str, detail: impl Into<String>) -> Self {
        Self {
            code,
            severity: "warning",
            detail: detail.into(),
        }
    }

    fn error(code: &'static str, detail: impl Into<String>) -> Self {
        Self {
            code,
            severity: "error",
            detail: detail.into(),
        }
    }
}

/// The frozen per-workspace diagnostic record (§C.8 W4). `workspace_id`,
/// `repo_id`, `lease_state` and `scope_diagnostics` are the required fields;
/// the rest is additive context.
#[derive(Debug, Serialize)]
struct WorkspaceDiagnostic {
    workspace_id: String,
    repo_id: String,
    kind: &'static str,
    state: &'static str,
    path: String,
    worktree_id: Option<String>,
    /// `none` (no lease taken) | `held` | `expired`.
    lease_state: &'static str,
    lease_owner: Option<String>,
    lease_fence: i64,
    lease_expires_at: Option<i64>,
    scope_diagnostics: Vec<ScopeDiagnostic>,
}

#[derive(Debug, Serialize)]
struct WorktreeDoctorPage {
    schema_version: u32,
    diagnostics: Vec<WorkspaceDiagnostic>,
    next_cursor: Option<String>,
    /// The WORKTREE-scope findings (§C.11 W0). Carried in the same envelope
    /// as the workspace page rather than replacing it: the two halves answer
    /// different questions (is this working tree's layout/identity sound vs
    /// is this Agent workspace's lease sound) and a caller that parses one
    /// must not lose the other.
    #[serde(skip_serializing)]
    worktrees: Vec<WorktreeDiagnostic>,
}

#[derive(Debug, Serialize)]
struct WorktreeDoctorSingle {
    schema_version: u32,
    diagnostic: WorkspaceDiagnostic,
}

/// The registry entry that owns this record's scope, matched by stable id
/// first and canonical path second (a legacy v1 entry may still lack an id).
fn doctor_registry_entry<'a>(
    registry: &'a WorktreeState,
    record: &WorkspaceRecord,
) -> Option<&'a WorktreeEntry> {
    if let Some(worktree_id) = record.worktree_id.as_deref()
        && let Some(entry) = registry
            .entries
            .iter()
            .find(|entry| entry.worktree_id.as_deref() == Some(worktree_id))
    {
        return Some(entry);
    }
    registry
        .entries
        .iter()
        .find(|entry| paths_are_same(&entry.path, &record.path))
}

/// Compare two stored absolute paths, tolerating symlinked prefixes
/// (`/tmp` vs `/private/tmp`) that only differ once resolved.
fn paths_are_same(left: &str, right: &str) -> bool {
    if left == right {
        return true;
    }
    match (fs::canonicalize(left), fs::canonicalize(right)) {
        (Ok(left), Ok(right)) => left == right,
        _ => false,
    }
}

/// Build one workspace's diagnosis from state that is already in hand — pure
/// over the record, the registry snapshot and the clock, plus read-only
/// filesystem probes.
/// §C.4.1.1 diagnosability: SCOPED capture rows whose recorded fence no
/// longer matches this workspace's live fence. Every capture/import/export
/// write for them fails closed (the owner claim is immutable by trigger),
/// so without this finding a reclaimed workspace's history dead-ends with a
/// refusal pointing at doctor — and doctor said nothing.
async fn stale_fence_capture_finding(
    conn: &sea_orm::DatabaseConnection,
    record: &WorkspaceRecord,
) -> Option<ScopeDiagnostic> {
    use sea_orm::{ConnectionTrait, DbBackend, Statement};
    let mut stale = 0i64;
    for table in ["agent_session", "agent_export_job", "agent_import_identity"] {
        let row = conn
            .query_one_raw(Statement::from_sql_and_values(
                DbBackend::Sqlite,
                format!(
                    "SELECT COUNT(*) AS n FROM {table} \
                      WHERE scope_state = 'scoped' AND workspace_id = ? \
                        AND workspace_fence <> ?"
                ),
                [
                    record.workspace_id.clone().into(),
                    record.lease_fence.into(),
                ],
            ))
            .await
            .ok()??;
        stale += row.try_get_by::<i64, _>("n").unwrap_or(0);
    }
    (stale > 0).then(|| {
        ScopeDiagnostic::warning(
            "capture_rows_stale_fence",
            format!(
                "{stale} scoped capture row(s) carry an earlier lease fence of this \
                  workspace; their owner claims are immutable, so capture/import/export \
                  writes for those provider sessions fail closed under the current fence — \
                  the rows remain readable provenance"
            ),
        )
    })
}

fn diagnose_workspace(
    record: &WorkspaceRecord,
    current_repo_id: &str,
    registry: &WorktreeState,
    now_ms: i64,
) -> WorkspaceDiagnostic {
    let lease_state = match (&record.lease_owner, record.lease_expires_at) {
        (None, _) => "none",
        (Some(_), Some(deadline)) if now_ms >= deadline => "expired",
        // `release` deliberately RETAINS lease_owner (provenance) while
        // nulling the expiry and leaving the live state set — so an owner
        // with no deadline is "held" only while the record is still live; a
        // released/adopted record's retained owner is history, not a lease.
        (Some(_), None)
            if matches!(
                record.state,
                crate::internal::workspace::WorkspaceState::Released
                    | crate::internal::workspace::WorkspaceState::Orphaned
            ) =>
        {
            "none"
        }
        (Some(_), _) => "held",
    };
    let mut findings = Vec::new();

    if record.repo_id != current_repo_id {
        findings.push(ScopeDiagnostic::error(
            "foreign_repository_identity",
            format!(
                "the record was written under repository identity {} but this repository is \
                  now {current_repo_id}; it is invisible to the normal listings and blocks new \
                  workspace registrations until it is settled",
                record.repo_id
            ),
        ));
    }
    if matches!(record.state, WorkspaceState::Orphaned) {
        findings.push(ScopeDiagnostic::warning(
            "workspace_orphaned",
            "teardown failed or the owner vanished; the workspace still holds recovery state",
        ));
    }
    if lease_state == "expired" {
        findings.push(ScopeDiagnostic::warning(
            "lease_expired",
            format!(
                "the lease deadline passed (expires_at {}, now {now_ms}); the lease still \
                  belongs to its owner until it is explicitly reclaimed",
                record.lease_expires_at.unwrap_or_default()
            ),
        ));
    }
    if !Path::new(&record.path).exists() {
        findings.push(ScopeDiagnostic::warning(
            "workspace_path_missing",
            format!("no directory at the claimed path '{}'", record.path),
        ));
    }

    if matches!(record.kind, WorkspaceKind::Linked) {
        match doctor_registry_entry(registry, record) {
            None => findings.push(ScopeDiagnostic::warning(
                "registry_entry_missing",
                format!(
                    "no worktree registry entry owns this scope (worktree_id {}, path '{}')",
                    record.worktree_id.as_deref().unwrap_or("<unset>"),
                    record.path
                ),
            )),
            Some(entry) => {
                if !paths_are_same(&entry.path, &record.path) {
                    findings.push(ScopeDiagnostic::error(
                        "registry_path_mismatch",
                        format!(
                            "the registry entry for this scope lives at '{}' but the workspace \
                              record claims '{}'",
                            entry.path, record.path
                        ),
                    ));
                }
                match entry.state {
                    WorktreeEntryState::Active => {}
                    WorktreeEntryState::DetachedFromRegistry => {
                        findings.push(ScopeDiagnostic::warning(
                            "registry_entry_detached",
                            "the worktree was unregistered with `worktree remove` (keep-dir); \
                              commands inside it fail closed until it is re-added",
                        ));
                    }
                    WorktreeEntryState::Tombstone => {
                        findings.push(ScopeDiagnostic::warning(
                            "registry_entry_tombstoned",
                            "the worktree directory was deleted but its scoped rows are still \
                              pending cleanup; `libra worktree repair --confirm` retries it",
                        ));
                    }
                }
                match detect_entry_layout(Path::new(&entry.path), entry.is_main) {
                    "corrupt" => findings.push(ScopeDiagnostic::error(
                        "scope_layout_corrupt",
                        format!("the gitdir layout at '{}' is unrecognizable", entry.path),
                    )),
                    "legacy-symlink" => findings.push(ScopeDiagnostic::warning(
                        "scope_layout_legacy_symlink",
                        format!(
                            "'{}' still uses the pre-isolation shared-`.libra` symlink layout; \
                              migrate it with `libra worktree repair --migrate-layout --confirm`",
                            entry.path
                        ),
                    )),
                    _ => {}
                }
            }
        }
    }

    WorkspaceDiagnostic {
        workspace_id: record.workspace_id.clone(),
        repo_id: record.repo_id.clone(),
        kind: record.kind.as_db_value(),
        state: record.state.as_db_value(),
        path: record.path.clone(),
        worktree_id: record.worktree_id.clone(),
        lease_state,
        lease_owner: record.lease_owner.clone(),
        lease_fence: record.lease_fence,
        lease_expires_at: record.lease_expires_at,
        scope_diagnostics: findings,
    }
}

#[derive(Debug, Serialize)]
struct CaptureScopeAdoptionOutput {
    schema_version: u32,
    session_id: String,
    workspace_id: String,
    repo_id: String,
    worktree_id: String,
    workspace_fence: i64,
}

/// plan-20260715 W4-07: re-home or clear Always approvals whose opaque
/// `project_id` is not the current `libra.repoid`. Migrations never rewrite
/// those rows; this confirmed doctor action is the only supported path.
async fn adopt_or_clear_legacy_approved_project(
    legacy_project_id: &str,
    confirm: bool,
    clear: bool,
    output: &OutputConfig,
) -> CliResult<()> {
    use crate::internal::ai::permission::ApprovedRulesetStore;

    let action = if clear {
        "worktree doctor --clear-approved-project"
    } else {
        "worktree doctor --adopt-approved-project"
    };
    require_repair_confirmation(confirm, action)?;

    let db_path = crate::utils::path::database();
    let conn = crate::internal::db::get_db_conn_instance_for_path(&db_path)
        .await
        .map_err(|error| {
            CliError::fatal(format!(
                "cannot open the repository database for approved_permission recovery: {error}"
            ))
        })?;

    // Open the audit boundary first. Only if the sole blocker is a missing
    // libra.repoid do we mint one and retry — so a held control slot never
    // causes an unaudited identity write.
    let boundary = match begin_repair_operation(action, Some(legacy_project_id)).await {
        Ok(boundary) => boundary,
        Err(error)
            if error
                .to_string()
                .contains("repository identity (libra.repoid) is missing") =>
        {
            ApprovedRulesetStore::ensure_repo_identity(&conn)
                .await
                .map_err(|init_error| {
                    CliError::fatal(format!(
                        "cannot initialize libra.repoid before approved_permission recovery: \
                          {init_error}"
                    ))
                })?;
            begin_repair_operation(action, Some(legacy_project_id)).await?
        }
        Err(error) => return Err(error),
    };

    let result = async {
        if clear {
            let removed = ApprovedRulesetStore::clear_legacy_project_id(&conn, legacy_project_id)
                .await
                .map_err(|error| {
                    CliError::fatal(format!(
                        "cannot clear approved_permission project_id '{legacy_project_id}': \
                              {error}"
                    ))
                })?;
            Ok(serde_json::json!({
                "action": "clear",
                "legacy_project_id": legacy_project_id,
                "rows_affected": removed,
            }))
        } else {
            let adopted = ApprovedRulesetStore::adopt_legacy_project_id(&conn, legacy_project_id)
                .await
                .map_err(|error| {
                    CliError::fatal(format!(
                        "cannot adopt approved_permission project_id '{legacy_project_id}': \
                              {error}"
                    ))
                })?;
            Ok(serde_json::json!({
                "action": "adopt",
                "legacy_project_id": legacy_project_id,
                "rows_affected": adopted,
            }))
        }
    }
    .await;

    let payload = finish_repair_operation(boundary, result).await?;
    if output.is_json() {
        let command = if clear {
            "worktree.doctor.clear_approved_project"
        } else {
            "worktree.doctor.adopt_approved_project"
        };
        return emit_json_data(command, &payload, output);
    }
    if clear {
        println!(
            "cleared {} approved_permission row(s) under legacy project_id '{legacy_project_id}'",
            payload["rows_affected"]
        );
    } else {
        println!(
            "adopted {} approved_permission row(s) from legacy project_id '{legacy_project_id}' \
              onto libra.repoid",
            payload["rows_affected"]
        );
    }
    Ok(())
}

/// Explicit mutation for the W4 legacy boundary. Migration 2026080401 marks
/// historical capture rows `legacy_unknown` because their original scope was
/// not recorded. This command is the only supported way to assign one: it
/// requires a live workspace lease/fence, confirmation, and an audit row.
async fn adopt_legacy_capture_scope(
    workspace_id: &str,
    session_id: &str,
    confirm: bool,
    output: &OutputConfig,
) -> CliResult<()> {
    if !confirm {
        return Err(CliError::command_usage(
            "legacy capture adoption changes persistent ownership; re-run with --confirm after \
              verifying the workspace and session",
        ));
    }
    let db_path = crate::utils::path::database();
    let conn = crate::internal::db::get_db_conn_instance_for_path(&db_path)
        .await
        .map_err(|error| {
            CliError::fatal(format!(
                "cannot open the repository database for capture-scope adoption: {error}"
            ))
        })?;
    let record = WorkspaceStore::get_with_conn(&conn, workspace_id)
        .await
        .map_err(|error| {
            CliError::fatal(format!("cannot read workspace '{workspace_id}': {error}"))
        })?
        .ok_or_else(|| {
            CliError::fatal(format!(
                "no workspace matches id '{workspace_id}'; list them with `libra worktree doctor`"
            ))
            .with_stable_code(StableErrorCode::CliInvalidTarget)
        })?;
    if !record.state.holds_identity() || record.lease_owner.is_none() || record.lease_fence <= 0 {
        return Err(CliError::fatal(format!(
            "workspace '{workspace_id}' has no live lease fence; refuse to attribute capture \
              state to a released, orphaned, or unleased scope"
        )));
    }
    if record
        .lease_expires_at
        .is_none_or(|deadline| deadline <= workspace::now_ms())
    {
        return Err(CliError::fatal(format!(
            "workspace '{workspace_id}' lease has expired; refuse to attribute capture state \
              until its owner renews or an explicit reclaim issues a new fence"
        )));
    }
    let identity = RepoIdentity::resolve(&conn).await.map_err(|error| {
        CliError::fatal(format!(
            "cannot resolve this repository's identity for capture-scope adoption: {error}"
        ))
    })?;
    let target_worktree_id = record.worktree_id.clone().unwrap_or_default();
    let txn = crate::internal::db::begin_write_transaction(&conn)
        .await
        .map_err(|error| CliError::fatal(format!("begin capture-scope adoption: {error}")))?;
    let legacy = txn
         .query_one_raw(Statement::from_sql_and_values(
             txn.get_database_backend(),
             "SELECT agent_kind, provider_session_id FROM (
                  SELECT 0 AS match_priority, agent_kind, provider_session_id FROM agent_session
                   WHERE scope_state = 'legacy_unknown' AND session_id = ?
                  UNION ALL
                  SELECT 1 AS match_priority, agent_kind, provider_session_id FROM agent_session
                   WHERE scope_state = 'legacy_unknown' AND provider_session_id = ?
                  UNION ALL
                  SELECT 1 AS match_priority, agent_kind, provider_session_id FROM agent_export_job
                   WHERE scope_state = 'legacy_unknown' AND provider_session_id = ?
                  UNION ALL
                  SELECT 1 AS match_priority, agent_kind, provider_session_id FROM agent_import_identity
                   WHERE scope_state = 'legacy_unknown' AND provider_session_id = ?
              ) ORDER BY match_priority LIMIT 1",
             [
                 session_id.into(),
                 session_id.into(),
                 session_id.into(),
                 session_id.into(),
             ],
         ))
         .await
         .map_err(|error| CliError::fatal(format!("read legacy capture session: {error}")))?
         .ok_or_else(|| {
             CliError::fatal(format!(
                 "capture session identifier '{session_id}' has no legacy unscoped capture row; \
                  doctor adoption accepts an agent session id or an orphan provider session id"
             ))
         })?;
    let agent_kind: String = legacy
        .try_get_by("agent_kind")
        .map_err(|error| CliError::fatal(format!("decode legacy capture agent kind: {error}")))?;
    let provider_session_id: String =
        legacy.try_get_by("provider_session_id").map_err(|error| {
            CliError::fatal(format!("decode legacy provider session identity: {error}"))
        })?;
    let foreign_scoped = txn
        .query_one_raw(Statement::from_sql_and_values(
            txn.get_database_backend(),
            "SELECT 1 FROM (
                  SELECT provider_session_id FROM agent_session
                   WHERE provider_session_id = ? AND scope_state = 'scoped'
                  UNION ALL
                  SELECT provider_session_id FROM agent_export_job
                   WHERE provider_session_id = ? AND scope_state = 'scoped'
                  UNION ALL
                  SELECT provider_session_id FROM agent_import_identity
                   WHERE provider_session_id = ? AND scope_state = 'scoped'
              ) LIMIT 1",
            [
                provider_session_id.clone().into(),
                provider_session_id.clone().into(),
                provider_session_id.clone().into(),
            ],
        ))
        .await
        .map_err(|error| {
            CliError::fatal(format!("check existing scoped capture ownership: {error}"))
        })?;
    if foreign_scoped.is_some() {
        txn.rollback().await.ok();
        return Err(CliError::fatal(format!(
            "provider session '{provider_session_id}' already has a scoped capture claim; \
              refusing to merge a legacy row into it"
        )));
    }
    let target_repo_id = identity.as_str().to_string();
    let target_workspace_id = record.workspace_id.clone();
    let target_workspace_fence = record.lease_fence;
    let mut adopted_rows = txn
        .execute_raw(Statement::from_sql_and_values(
            txn.get_database_backend(),
            "UPDATE agent_session
              SET repo_id = ?, worktree_id = ?, workspace_id = ?, workspace_fence = ?,
                  scope_state = 'scoped'
              WHERE provider_session_id = ? AND scope_state = 'legacy_unknown'
                AND EXISTS (
                    SELECT 1 FROM workspace_record
                    WHERE workspace_id = ? AND repo_id = ? AND lease_fence = ?
                      AND state IN ('provisioning', 'active', 'releasing')
                      AND lease_owner IS NOT NULL
                      AND lease_expires_at > (unixepoch('now') * 1000)
                )",
            [
                target_repo_id.clone().into(),
                target_worktree_id.clone().into(),
                target_workspace_id.clone().into(),
                target_workspace_fence.into(),
                provider_session_id.clone().into(),
                target_workspace_id.clone().into(),
                target_repo_id.clone().into(),
                target_workspace_fence.into(),
            ],
        ))
        .await
        .map_err(|error| CliError::fatal(format!("adopt legacy capture session: {error}")))?
        .rows_affected();
    for table in ["agent_export_job", "agent_import_identity"] {
        let sql = format!(
            "UPDATE {table}
              SET repo_id = ?, worktree_id = ?, workspace_id = ?, workspace_fence = ?,
                  scope_state = 'scoped'
              WHERE provider_session_id = ? AND scope_state = 'legacy_unknown'
                AND EXISTS (
                    SELECT 1 FROM workspace_record
                    WHERE workspace_id = ? AND repo_id = ? AND lease_fence = ?
                      AND state IN ('provisioning', 'active', 'releasing')
                      AND lease_owner IS NOT NULL
                      AND lease_expires_at > (unixepoch('now') * 1000)
                )"
        );
        adopted_rows += txn
            .execute_raw(Statement::from_sql_and_values(
                txn.get_database_backend(),
                sql,
                [
                    target_repo_id.clone().into(),
                    target_worktree_id.clone().into(),
                    target_workspace_id.clone().into(),
                    target_workspace_fence.into(),
                    provider_session_id.clone().into(),
                    target_workspace_id.clone().into(),
                    target_repo_id.clone().into(),
                    target_workspace_fence.into(),
                ],
            ))
            .await
            .map_err(|error| CliError::fatal(format!("adopt legacy {table} scope: {error}")))?
            .rows_affected();
    }
    if adopted_rows == 0 {
        txn.rollback().await.ok();
        return Err(CliError::fatal(
            "capture-scope adoption was fenced out because the target workspace changed; rerun \
              doctor and choose the current live workspace",
        ));
    }
    let actor = env::var("LIBRA_ACTOR")
        .ok()
        .or_else(|| env::var("USER").ok());
    txn.execute_raw(Statement::from_sql_and_values(
        txn.get_database_backend(),
        "INSERT INTO agent_workspace_scope_audit (
             audit_id, action, agent_kind, provider_session_id, repo_id, worktree_id,
             workspace_id, workspace_fence, actor, created_at
          ) VALUES (?, 'adopt_legacy_capture_scope', ?, ?, ?, ?, ?, ?, ?, ?)",
        [
            uuid::Uuid::new_v4().to_string().into(),
            agent_kind.into(),
            provider_session_id.into(),
            target_repo_id.into(),
            target_worktree_id.clone().into(),
            target_workspace_id.into(),
            target_workspace_fence.into(),
            actor.into(),
            workspace::now_ms().into(),
        ],
    ))
    .await
    .map_err(|error| CliError::fatal(format!("audit capture-scope adoption: {error}")))?;
    txn.commit()
        .await
        .map_err(|error| CliError::fatal(format!("commit capture-scope adoption: {error}")))?;

    let payload = CaptureScopeAdoptionOutput {
        schema_version: DOCTOR_SCHEMA_VERSION,
        session_id: session_id.to_string(),
        workspace_id: record.workspace_id,
        repo_id: identity.as_str().to_string(),
        worktree_id: target_worktree_id,
        workspace_fence: record.lease_fence,
    };
    if output.is_json() {
        return emit_json_data("worktree.doctor.adopt_capture", &payload, output);
    }
    if !output.quiet {
        println!(
            "adopted legacy capture session into workspace {} at lease fence {}",
            payload.workspace_id, payload.workspace_fence
        );
    }
    Ok(())
}

/// Probe, rather than count, legacy capture rows so a diagnostic page stays
/// bounded even in repositories with a long capture history. Before the W4
/// migration these columns do not exist; doctor must remain usable on that
/// older schema and simply has no legacy-scope classification to report.
async fn legacy_capture_scope_exists(conn: &sea_orm::DatabaseConnection) -> CliResult<bool> {
    let result = conn
        .query_one_raw(Statement::from_string(
            conn.get_database_backend(),
            "SELECT 1 FROM (
                  SELECT 1 FROM agent_session WHERE scope_state = 'legacy_unknown'
                  UNION ALL
                  SELECT 1 FROM agent_export_job WHERE scope_state = 'legacy_unknown'
                  UNION ALL
                  SELECT 1 FROM agent_import_identity WHERE scope_state = 'legacy_unknown'
              ) LIMIT 1"
                .to_string(),
        ))
        .await;
    match result {
        Ok(row) => Ok(row.is_some()),
        Err(error)
            if error
                .to_string()
                .to_ascii_lowercase()
                .contains("no such column")
                || error
                    .to_string()
                    .to_ascii_lowercase()
                    .contains("no such table") =>
        {
            Ok(false)
        }
        Err(error) => Err(doctor_scope_corrupt(format!(
            "cannot inspect legacy capture scope state: {error}"
        ))),
    }
}

fn print_legacy_capture_scope_guidance(output: &OutputConfig, legacy_exists: bool) {
    if legacy_exists && !output.is_json() && !output.quiet {
        println!(
            "legacy capture scope: unscoped capture rows exist and are intentionally excluded \
              from new writes; inspect the session and adopt only its verified owner with \
              `libra worktree doctor <workspace-id> --adopt-capture-session <session-id> --confirm`"
        );
    }
}

fn print_legacy_approved_project_guidance(output: &OutputConfig, legacy_ids: &[String]) {
    if legacy_ids.is_empty() || output.quiet {
        return;
    }
    let listed = legacy_ids.join(", ");
    let message = format!(
        "legacy approved_permission project_id(s): {listed}; they are invisible to the runtime \
          until adopted with `libra worktree doctor --adopt-approved-project <id> --confirm` or \
          removed with `libra worktree doctor --clear-approved-project <id> --confirm`"
    );
    if output.is_json() {
        // Keep the frozen worktree.doctor JSON page schema untouched; surface
        // the recovery IDs on stderr so machine callers can still discover them.
        eprintln!("{message}");
    } else {
        println!("{message}");
    }
}

/// Read-only discovery of opaque Always-approval buckets. Missing table or
/// columns (pre-migration / no-migration doctor open) reports empty.
async fn list_legacy_approved_project_ids_readonly(
    conn: &sea_orm::DatabaseConnection,
) -> CliResult<Vec<String>> {
    use crate::internal::ai::permission::ApprovedRulesetStore;

    match ApprovedRulesetStore::list_legacy_project_ids(conn).await {
        Ok(ids) => Ok(ids),
        Err(error) => {
            let text = error.to_string().to_ascii_lowercase();
            if text.contains("no such table") || text.contains("no such column") {
                Ok(Vec::new())
            } else {
                Err(doctor_scope_corrupt(format!(
                    "cannot list legacy approved_permission project_id values: {error}"
                )))
            }
        }
    }
}

/// `libra worktree doctor [<workspace-id>] [--limit N] [--cursor C]`.
pub(crate) async fn run_worktree_doctor(
    workspace_id: Option<String>,
    limit: Option<u64>,
    cursor: Option<String>,
    output: &OutputConfig,
) -> CliResult<()> {
    // A single scope is not a page: rejecting the combination keeps the two
    // frozen response shapes unambiguous (Codex R19).
    if workspace_id.is_some() && (limit.is_some() || cursor.is_some()) {
        return Err(CliError::command_usage(
            "`libra worktree doctor <workspace-id>` diagnoses one scope and takes no \
              --limit/--cursor; drop the id for the paginated view",
        ));
    }

    // Read-only registry snapshot (lockless reader — never rewrites the file).
    // A registry this command cannot parse IS corrupt scope state: refuse
    // rather than report a diagnosis built on unknown ownership.
    let registry = load_state_readonly().map_err(|error| {
        doctor_scope_corrupt(format!("cannot read the worktree registry: {error}"))
    })?;

    // Open WITHOUT applying migrations (§C.11 W0): running the pending
    // migrations of the repository you are diagnosing is a write, and it
    // changes the very thing you came to observe — on a repository behind
    // schema, `doctor` is the one command you must be able to run without
    // committing to an upgrade. `cli.rs` already exempts this command from
    // the schema guard for the same reason.
    let db_path = crate::utils::path::database();
    let conn = crate::internal::db::open_database_without_migrations(&db_path)
        .await
        .map_err(|error| {
            doctor_scope_corrupt(format!(
                "cannot open the repository database at '{}': {error}",
                db_path.display()
            ))
        })?;
    let now = workspace::now_ms();
    let legacy_capture_exists = legacy_capture_scope_exists(&conn).await?;
    let legacy_approved_ids = list_legacy_approved_project_ids_readonly(&conn).await?;

    if let Some(workspace_id) = workspace_id {
        let record = match WorkspaceStore::doctor_record_with_conn(&conn, &workspace_id).await {
            Ok(record) => record,
            // Same special case as the paginated path below: a repository
            // that never ran the workspace migration has no records — its
            // TRUE state, not scope corruption, and the actionable answer
            // is "no such workspace", not LBR-WORKTREE-002.
            Err(error) if workspace_table_absent(&error) => None,
            Err(error) => {
                return Err(doctor_scope_corrupt(format!(
                    "cannot read the workspace record '{workspace_id}': {error}"
                )));
            }
        }
        .ok_or_else(|| {
            CliError::fatal(format!(
                "no workspace matches id '{workspace_id}'; list them with \
                      `libra worktree doctor`"
            ))
            .with_stable_code(StableErrorCode::CliInvalidTarget)
        })?;
        let repo_id = doctor_repo_identity(&conn).await?;
        let mut diagnostic = diagnose_workspace(&record, &repo_id, &registry, now);
        if let Some(finding) = stale_fence_capture_finding(&conn, &record).await {
            diagnostic.scope_diagnostics.push(finding);
        }
        let result = render_doctor_single(diagnostic, output);
        print_legacy_capture_scope_guidance(output, legacy_capture_exists);
        print_legacy_approved_project_guidance(output, &legacy_approved_ids);
        return result;
    }

    let after = cursor.as_deref().map(decode_doctor_cursor).transpose()?;
    let page = match WorkspaceStore::doctor_page_with_conn(&conn, limit, after.as_deref()).await {
        Ok(page) => page,
        // A repository that has not yet run the workspace migration has no
        // records to diagnose — that is its true state, not a corrupt one.
        // Every other read failure still fails closed.
        Err(error) if workspace_table_absent(&error) => workspace::WorkspacePage {
            items: Vec::new(),
            next_cursor: None,
        },
        Err(error) => {
            return Err(doctor_scope_corrupt(format!(
                "cannot read the workspace registry: {error}"
            )));
        }
    };
    // The human doctor also renders the W0 worktree-scope report on the same
    // no-migration connection. Do not build it for JSON/machine pagination:
    // it scans every registry entry, would make a capped workspace page
    // unbounded, and is not part of the frozen W4 JSON payload.
    let worktrees = if output.is_json() {
        Vec::new()
    } else {
        collect_worktree_scope_report(&conn).await?.diagnostics
    };
    if page.items.is_empty() {
        // Nothing to classify, so a missing/unreadable repository identity is
        // not worth failing on: an empty diagnosis IS the whole truth here.
        let result = render_doctor_page(
            WorktreeDoctorPage {
                schema_version: DOCTOR_SCHEMA_VERSION,
                diagnostics: Vec::new(),
                next_cursor: None,
                worktrees,
            },
            output,
        );
        print_legacy_capture_scope_guidance(output, legacy_capture_exists);
        print_legacy_approved_project_guidance(output, &legacy_approved_ids);
        return result;
    }
    let repo_id = doctor_repo_identity(&conn).await?;
    let diagnostics = page
        .items
        .iter()
        .map(|record| diagnose_workspace(record, &repo_id, &registry, now))
        .collect();
    let result = render_doctor_page(
        WorktreeDoctorPage {
            schema_version: DOCTOR_SCHEMA_VERSION,
            diagnostics,
            next_cursor: page.next_cursor.as_deref().map(encode_doctor_cursor),
            worktrees,
        },
        output,
    );
    print_legacy_capture_scope_guidance(output, legacy_capture_exists);
    print_legacy_approved_project_guidance(output, &legacy_approved_ids);
    result
}

/// Whether a workspace read failed only because the table does not exist yet.
fn workspace_table_absent(error: &workspace::WorkspaceError) -> bool {
    let text = error.to_string().to_ascii_lowercase();
    text.contains("no such table") && text.contains("workspace_record")
}

/// This repository's canonical identity, needed to tell a record of THIS
/// repository from one left behind by a previous identity.
///
/// Deliberately the strict resolver: minting an identity would be a write, and
/// the doctor's default invocation is read-only.
async fn doctor_repo_identity(conn: &sea_orm::DatabaseConnection) -> CliResult<String> {
    RepoIdentity::resolve(conn)
        .await
        .map(|identity| identity.as_str().to_string())
        .map_err(|error| {
            doctor_scope_corrupt(format!(
                "cannot read this repository's identity, so workspace records cannot be \
                  attributed: {error}"
            ))
        })
}

fn render_doctor_single(diagnostic: WorkspaceDiagnostic, output: &OutputConfig) -> CliResult<()> {
    if output.is_json() {
        let payload = WorktreeDoctorSingle {
            schema_version: DOCTOR_SCHEMA_VERSION,
            diagnostic,
        };
        return emit_json_data("worktree.doctor", &payload, output);
    }
    if output.quiet {
        return Ok(());
    }
    println!("workspace_id: {}", diagnostic.workspace_id);
    println!("repo_id:      {}", diagnostic.repo_id);
    println!("kind:         {}", diagnostic.kind);
    println!("state:        {}", diagnostic.state);
    println!("path:         {}", diagnostic.path);
    if let Some(worktree_id) = &diagnostic.worktree_id {
        println!("worktree_id:  {worktree_id}");
    }
    println!("lease_state:  {}", diagnostic.lease_state);
    if let Some(owner) = &diagnostic.lease_owner {
        println!("lease_owner:  {owner} (fence {})", diagnostic.lease_fence);
    }
    print_scope_diagnostics(&diagnostic.scope_diagnostics);
    Ok(())
}

fn render_doctor_page(page: WorktreeDoctorPage, output: &OutputConfig) -> CliResult<()> {
    if output.is_json() {
        return emit_json_data("worktree.doctor", &page, output);
    }
    if output.quiet {
        return Ok(());
    }
    print_worktree_scope_report(&WorktreeDoctorOutput {
        schema_version: page.schema_version,
        diagnostics: page.worktrees.clone(),
        next_cursor: None,
    });
    if page.diagnostics.is_empty() {
        println!("(no workspace records to diagnose)");
        return Ok(());
    }
    for diagnostic in &page.diagnostics {
        println!(
            "{}  {:12} lease={:7} {}",
            diagnostic.workspace_id, diagnostic.state, diagnostic.lease_state, diagnostic.path
        );
        print_scope_diagnostics(&diagnostic.scope_diagnostics);
    }
    if let Some(cursor) = &page.next_cursor {
        println!("(more rows: --cursor {cursor})");
    }
    Ok(())
}

fn print_scope_diagnostics(findings: &[ScopeDiagnostic]) {
    for finding in findings {
        println!(
            "  {:7} {}: {}",
            finding.severity, finding.code, finding.detail
        );
    }
}

/// Implements `worktree lock <path> [--reason <msg>]`.
///
/// Marks the specified worktree entry as locked and persists an optional
/// human-readable reason. Locking is a state-only operation and does not
/// alter directories on disk.
async fn lock_worktree(path: String, reason: Option<String>) -> WorktreeResult<WorktreeLockOutput> {
    let _registry_lock = acquire_registry_lock_async().await?;
    let mut state = load_state()?;
    let target = resolve_path(&path, "worktree path")?;
    let entry = match find_entry_mut(&mut state, &target) {
        Some(e) => e,
        None => return Err(WorktreeError::NoSuchWorktree { path }),
    };
    if entry.locked {
        return Ok(WorktreeLockOutput {
            path: target.to_string_lossy().to_string(),
            locked: true,
            lock_reason: entry.lock_reason.clone(),
            changed: false,
        });
    }
    entry.locked = true;
    entry.lock_reason = reason;
    let lock_reason = entry.lock_reason.clone();
    write_state(&state)?;
    Ok(WorktreeLockOutput {
        path: target.to_string_lossy().to_string(),
        locked: true,
        lock_reason,
        changed: true,
    })
}

fn render_lock_worktree(result: &WorktreeLockOutput, output: &OutputConfig) -> CliResult<()> {
    if output.is_json() {
        return emit_json_data("worktree.lock", result, output);
    }
    Ok(())
}

/// Implements `worktree unlock <path>`.
///
/// Clears the lock flag and reason for the specified worktree entry if it is
/// currently locked. Unlocking is idempotent and leaves the filesystem untouched.
async fn unlock_worktree(path: String) -> WorktreeResult<WorktreeUnlockOutput> {
    let _registry_lock = acquire_registry_lock_async().await?;
    let mut state = load_state()?;
    let target = resolve_path(&path, "worktree path")?;
    let entry = match find_entry_mut(&mut state, &target) {
        Some(e) => e,
        None => return Err(WorktreeError::NoSuchWorktree { path }),
    };
    if !entry.locked {
        return Ok(WorktreeUnlockOutput {
            path: target.to_string_lossy().to_string(),
            locked: false,
            changed: false,
        });
    }
    entry.locked = false;
    entry.lock_reason = None;
    write_state(&state)?;
    Ok(WorktreeUnlockOutput {
        path: target.to_string_lossy().to_string(),
        locked: false,
        changed: true,
    })
}

fn render_unlock_worktree(result: &WorktreeUnlockOutput, output: &OutputConfig) -> CliResult<()> {
    if output.is_json() {
        return emit_json_data("worktree.unlock", result, output);
    }
    Ok(())
}

/// Implements `worktree move <src> <dest>`.
///
/// This command:
/// - resolves both source and destination paths,
/// - rejects moves of the main or a locked worktree,
/// - ensures the destination does not already exist on disk or in the registry,
/// - updates the registry to point at the new path and saves it, and then
/// - renames the directory on disk, attempting to roll back registry changes
///   if the rename fails.
async fn move_worktree(src: String, dest: String) -> WorktreeResult<WorktreeMoveOutput> {
    let _registry_lock = acquire_registry_lock_async().await?;
    let mut state = load_state()?;
    let src_path = resolve_path(&src, "source worktree path")?;
    let dest_path = resolve_path(&dest, "destination worktree path")?;
    let storage = util::storage_path();

    if util::is_sub_path(&dest_path, &storage) {
        return Err(WorktreeError::InvalidTarget(format!(
            "destination cannot be inside .libra storage: {}",
            dest_path.display()
        )));
    }

    if find_entry(&state, &dest_path).is_some() {
        return Err(WorktreeError::OperationBlocked(format!(
            "destination already registered as worktree: {}",
            dest_path.display()
        )));
    }

    let index = state
        .entries
        .iter()
        .position(|w| Path::new(&w.path) == src_path)
        .ok_or(WorktreeError::NoSuchWorktree { path: src })?;

    if state.entries[index].is_main {
        return Err(WorktreeError::MainWorktree {
            action: "move",
            path: src_path.to_string_lossy().to_string(),
        });
    }
    if state.entries[index].locked {
        return Err(WorktreeError::LockedWorktree {
            action: "move",
            path: src_path.to_string_lossy().to_string(),
        });
    }

    if dest_path.exists() {
        return Err(WorktreeError::OperationBlocked(format!(
            "destination already exists: {}",
            dest_path.display()
        )));
    }

    // Durable intent BEFORE the first cross-medium mutation (§C.7): a crash
    // between the registry write and the directory rename is rolled forward
    // (or back) by `worktree repair` from this record.
    let journal_worktree_id = state.entries[index]
        .worktree_id
        .clone()
        .or_else(|| resolve_worktree_id(&src_path));
    // W3-s3: moving a worktree with an UNFINISHED layout migration would
    // strand its journal at the old path and let reconciliation unfreeze
    // the relocated (still-unmigrated) state — refuse until repair settles
    // it.
    {
        let db = crate::internal::db::get_db_conn_instance().await;
        let pending = journal_pending(&db)
            .await
            .map_err(WorktreeError::OperationBlocked)?;
        if pending.iter().any(|intent| {
            intent.op == "migrate"
                && (intent.worktree_id.as_deref() == journal_worktree_id.as_deref()
                    || intent.payload["path"].as_str() == Some(src_path.to_string_lossy().as_ref()))
        }) {
            return Err(WorktreeError::OperationBlocked(format!(
                "'{}' has an unfinished layout migration; run `libra worktree repair \
                  --confirm` first",
                src_path.display()
            )));
        }
    }
    let payload = serde_json::json!({
        "src": src_path.to_string_lossy(),
        "dest": dest_path.to_string_lossy(),
    });
    let db = crate::internal::db::get_db_conn_instance().await;
    let journal_id = journal_append(
        &db,
        WorktreeControl::Move.declare(),
        journal_worktree_id.as_deref(),
        &payload,
    )
    .await
    .map_err(WorktreeError::OperationBlocked)?;

    let old_path = state.entries[index].path.clone();
    state.entries[index].path = dest_path.to_string_lossy().to_string();
    if let Err(e) = write_state(&state) {
        state.entries[index].path = old_path;
        let _ = journal_resolve(&db, journal_id).await;
        return Err(e);
    }

    if let Err(e) = fs::rename(&src_path, &dest_path) {
        state.entries[index].path = old_path;
        write_state(&state)?;
        let _ = journal_resolve(&db, journal_id).await;
        return Err(WorktreeError::IoWrite(format!(
            "failed to move worktree directory '{}' to '{}': {e}",
            src_path.display(),
            dest_path.display()
        )));
    }
    if let Err(error) = journal_resolve(&db, journal_id).await {
        tracing::warn!(
            error,
            "move journal entry not resolved; repair will reconcile"
        );
    }

    Ok(WorktreeMoveOutput {
        source: src_path.to_string_lossy().to_string(),
        destination: dest_path.to_string_lossy().to_string(),
        registry_updated: true,
        disk_directory_moved: true,
    })
}

fn render_move_worktree(result: &WorktreeMoveOutput, output: &OutputConfig) -> CliResult<()> {
    if output.is_json() {
        return emit_json_data("worktree.move", result, output);
    }
    Ok(())
}

/// Implements `worktree prune`.
///
/// Any non-main worktree whose directory no longer exists on disk is removed
/// from the registry. Before mutating state, the function prints the set of
/// paths that will be pruned so the user can see what is being cleaned up.
/// Each pruned worktree's scoped DB rows are GC'd like `remove` does — the
/// directory is gone, so layer ownership is purged too (nothing left on disk
/// to guard); leaked rows would otherwise be re-inherited by a worktree
/// re-created at the same path (deterministic instance id).
async fn prune_worktrees() -> WorktreeResult<WorktreePruneOutput> {
    let _registry_lock = acquire_registry_lock_async().await?;
    let mut state = load_state()?;

    // §C.7: prune only handles entries whose path is PROVEN missing
    // (NotFound). Any other stat error — permissions, an unmounted volume —
    // must NOT classify the worktree as missing; those entries are kept.
    fn path_proven_missing(path: &Path) -> bool {
        matches!(
            fs::symlink_metadata(path),
            Err(ref error) if error.kind() == io::ErrorKind::NotFound
        )
    }

    let mut to_prune: Vec<(String, Option<String>)> = Vec::new();
    for entry in &state.entries {
        if entry.is_main || entry.locked || entry.state == WorktreeEntryState::Tombstone {
            // Tombstones are repair's job (the directory is already gone by
            // definition; only the scoped cleanup is pending).
            continue;
        }
        if !path_proven_missing(Path::new(&entry.path)) {
            continue;
        }
        let id = entry
            .worktree_id
            .clone()
            .or_else(|| resolve_worktree_id(Path::new(&entry.path)));
        to_prune.push((entry.path.clone(), id));
    }

    let mut pruned: Vec<String> = Vec::new();
    let mut tombstoned: Vec<String> = Vec::new();
    if !to_prune.is_empty() {
        let db = crate::internal::db::get_db_conn_instance().await;
        // An entry with ACTIVE sequencer/bisect state is never pruned —
        // its rows anchor the interrupted operation's objects.
        let mut eligible: Vec<(String, Option<String>)> = Vec::new();
        for (path, id) in to_prune {
            if let Some(id_str) = id.as_deref()
                && scoped_state_active(&db, id_str).await
            {
                continue;
            }
            // §C.7: an entry an agent still holds a LIVE LEASE on is never
            // pruned either — the path is gone but the WORKSPACE is not (an
            // in-flight re-provision, or a directory another actor removed
            // out of band), and deleting its scoped rows would pull the
            // workspace out from under a fenced owner. Queried BY WORKTREE
            // ID: the path-keyed lookup canonicalizes its query and cannot
            // match a path that no longer exists — which is every prune
            // candidate.
            if let Some(id_str) = id.as_deref() {
                match crate::internal::workspace::WorkspaceStore::find_live_linked_with_conn(
                    &db, id_str,
                )
                .await
                {
                    Ok(Some(record)) => {
                        let now = chrono::Utc::now().timestamp_millis();
                        if record.lease_expires_at.is_some_and(|expiry| expiry > now) {
                            // A live, UNEXPIRED lease: never pruned.
                            continue;
                        }
                    }
                    Ok(None) => {}
                    Err(_) => {
                        // FAIL CLOSED: an unreadable workspace store keeps
                        // the entry rather than pruning blind.
                        continue;
                    }
                }
            }
            eligible.push((path, id));
        }
        if !eligible.is_empty() {
            let payload = serde_json::json!({
                "paths": eligible.iter().map(|(p, _)| p.clone()).collect::<Vec<_>>(),
            });
            let journal_id = journal_append(&db, WorktreeControl::Prune.declare(), None, &payload)
                .await
                .map_err(WorktreeError::OperationBlocked)?;
            let mut mirror_failed = false;
            for (path, id) in &eligible {
                // STRICT per-entry cleanup: a GC failure keeps the entry as
                // a TOMBSTONE (the directory is proven missing) so the rows
                // stay visible to repair and the down-migration guard —
                // never silently orphaned.
                let cleaned = if let Some(id_str) = id.as_deref() {
                    match gc_worktree_scoped_rows_strict(&db, id_str, true).await {
                        Ok(()) => {
                            let _ = lifecycle_delete(&db, id_str).await;
                            true
                        }
                        Err(error) => {
                            tracing::warn!(
                                worktree_id = id_str,
                                error,
                                "prune cleanup failed; keeping a tombstone"
                            );
                            if let Err(mirror_error) = lifecycle_upsert(
                                &db,
                                id_str,
                                WorktreeEntryState::Tombstone.as_str(),
                                path,
                            )
                            .await
                            {
                                // Without the mirror row the down guard
                                // cannot see this tombstone: keep the
                                // journal row pending so repair retries.
                                tracing::warn!(
                                    worktree_id = id_str,
                                    error = mirror_error,
                                    "tombstone mirror write failed; journal kept for repair"
                                );
                                mirror_failed = true;
                            }
                            false
                        }
                    }
                } else {
                    true
                };
                if cleaned {
                    pruned.push(path.clone());
                } else {
                    tombstoned.push(path.clone());
                }
            }
            let pruned_set: std::collections::HashSet<&String> = pruned.iter().collect();
            for entry in &mut state.entries {
                if tombstoned.contains(&entry.path) {
                    entry.state = WorktreeEntryState::Tombstone;
                }
            }
            state.entries.retain(|w| !pruned_set.contains(&w.path));
            write_state(&state)?;
            if mirror_failed {
                tracing::warn!(
                    "prune left a tombstone whose mirror write failed; the journal row \
                      stays pending for `worktree repair`"
                );
            } else if let Err(error) = journal_resolve(&db, journal_id).await {
                tracing::warn!(
                    error,
                    "prune journal entry not resolved; repair will reconcile"
                );
            }
        }
    }

    Ok(WorktreePruneOutput {
        pruned_count: pruned.len(),
        pruned,
        tombstoned,
    })
}

fn render_prune_worktrees(result: &WorktreePruneOutput, output: &OutputConfig) -> CliResult<()> {
    if output.is_json() {
        return emit_json_data("worktree.prune", result, output);
    }
    if output.quiet {
        return Ok(());
    }
    if result.pruned.is_empty() {
        println!("No worktrees to prune");
        return Ok(());
    }
    println!("Will prune {} worktrees:", result.pruned_count);
    for path in &result.pruned {
        println!("  {}", path);
    }
    println!("Pruned {} worktrees", result.pruned_count);
    Ok(())
}

/// Implements `worktree remove <path> [--delete-dir]`.
///
/// Defaults to preserving the directory on disk (Libra's intentional
/// non-destructive behavior — see [`COMPATIBILITY.md`](../../../COMPATIBILITY.md)).
/// With `--delete-dir`, the worktree must be clean (no staged or unstaged
/// changes) and the directory is removed before the registry entry is dropped.
/// Order matters: registry last — a half-completed delete cannot silently
/// unregister a worktree whose directory is still present.
async fn remove_worktree(path: String, delete_dir: bool) -> WorktreeResult<WorktreeRemoveOutput> {
    let _registry_lock = acquire_registry_lock_async().await?;
    let mut state = load_state()?;
    let target = resolve_path(&path, "worktree path")?;

    let index = state
        .entries
        .iter()
        .position(|w| Path::new(&w.path) == target)
        .ok_or(WorktreeError::NoSuchWorktree { path })?;

    let entry = &state.entries[index];
    if entry.is_main {
        return Err(WorktreeError::MainWorktree {
            action: "remove",
            path: target.to_string_lossy().to_string(),
        });
    }
    if entry.locked {
        return Err(WorktreeError::LockedWorktree {
            action: "remove",
            path: target.to_string_lossy().to_string(),
        });
    }
    // W3-s3: a LEGACY symlink target shares main's `.libra` — any lifecycle
    // marker written into it would land in MAIN storage and freeze the main
    // repository. Refuse until the layout migration settles it.
    if detect_entry_layout(&target, false) == "legacy-symlink" {
        return Err(WorktreeError::OperationBlocked(format!(
            "'{}' uses the legacy shared-.libra symlink layout; run `libra worktree \
              repair --migrate-layout --confirm {}` first",
            target.display(),
            target.display()
        )));
    }
    // §C.7: a worktree an AGENT holds a live workspace lease on is not the
    // human's to detach or delete out from under it — the lease's fenced
    // owner is mid-work there. Release or let the lease lapse first.
    {
        let db = crate::internal::db::get_db_conn_instance().await;
        // FAIL CLOSED on a store that cannot be read: swallowing the error
        // would disable this protection exactly when the leased agent is
        // busy writing the same database.
        let lease_error = |error: crate::internal::workspace::WorkspaceError| {
            WorktreeError::OperationBlocked(format!(
                "cannot verify agent workspace leases for '{}': {error}; refusing to \
                  remove until the workspace store is readable",
                target.display()
            ))
        };
        let by_path =
            crate::internal::workspace::WorkspaceStore::find_live_by_path_with_conn(&db, &target)
                .await
                .map_err(lease_error)?;
        let by_id = match entry.worktree_id.as_deref() {
            Some(id) => {
                crate::internal::workspace::WorkspaceStore::find_live_linked_with_conn(&db, id)
                    .await
                    .map_err(lease_error)?
            }
            None => None,
        };
        // An EXPIRED lease does not block: nothing may run the scavenger for
        // a long time, and "let it expire" must actually unblock the human
        // (§C.7 acceptance: a crashed agent's worktree stays recoverable).
        let now = chrono::Utc::now().timestamp_millis();
        let unexpired = |record: &crate::internal::workspace::WorkspaceRecord| {
            record.lease_expires_at.is_some_and(|expiry| expiry > now)
        };
        if let Some(record) = by_path.into_iter().chain(by_id).find(unexpired) {
            return Err(WorktreeError::OperationBlocked(format!(
                "'{}' is held by live agent workspace '{}' (lease fence {}); release the \
                  lease or let it expire before removing the worktree",
                target.display(),
                record.workspace_id,
                record.lease_fence
            )));
        }
    }
    let entry_state = entry.state;
    if entry_state == WorktreeEntryState::Tombstone {
        return Err(WorktreeError::OperationBlocked(format!(
            "'{}' is a tombstone (directory already deleted, scoped cleanup pending); run \
              `libra worktree repair --confirm` to retry the cleanup",
            target.display()
        )));
    }
    if !delete_dir && entry_state == WorktreeEntryState::DetachedFromRegistry {
        return Err(WorktreeError::OperationBlocked(format!(
            "'{}' is already detached from the registry; re-add it or use --delete-dir",
            target.display()
        )));
    }

    // The registry's PERSISTED id is authoritative (v2); the gitdir probe is
    // only a fallback for pre-v2 rows the loader could not backfill.
    let worktree_id_for_gc = state.entries[index]
        .worktree_id
        .clone()
        .or_else(|| resolve_worktree_id(&target));

    // Resolve the pooled connection ONCE, while the cwd-based path lookup
    // is still valid — later steps may write the detached marker, after
    // which any cwd-based storage resolution inside the target would fail.
    let db = crate::internal::db::get_db_conn_instance().await;

    // §C.7: a worktree with ACTIVE sequencer/rebase/bisect state refuses
    // both remove modes — detaching would strand a half-finished operation
    // behind the fail-closed gate, deleting would destroy it.
    if let Some(id) = worktree_id_for_gc.as_deref()
        && scoped_state_active(&db, id).await
    {
        return Err(WorktreeError::OperationBlocked(format!(
            "'{}' has an in-progress rebase/cherry-pick/bisect; finish or abort it there \
              first",
            target.display()
        )));
    }
    // …and the FILE-backed half of the same rule: merge/revert sidecars and a
    // held autostash live in the target's gitdir, not in DB rows.
    refuse_active_sidecar_state(&target.join(util::ROOT_DIR), "removing the worktree")?;

    if delete_dir {
        remove_worktree_delete_dir(
            &db,
            &mut state,
            index,
            &target,
            worktree_id_for_gc,
            entry_state,
        )
        .await
    } else {
        remove_worktree_detach(&db, &mut state, index, &target, worktree_id_for_gc).await
    }
}

/// Keep-dir remove (W3-s1b, §C.7): the directory and its scoped DB rows are
/// PRESERVED — the entry moves to `detached_from_registry` and the gitdir
/// gains a marker that fail-closes every command run inside the directory
/// until re-add or `--delete-dir`. Deleting the scoped rows here would leave
/// a directory that still operates as a repository but lost its HEAD.
async fn remove_worktree_detach(
    db: &sea_orm::DatabaseConnection,
    state: &mut WorktreeState,
    index: usize,
    target: &Path,
    worktree_id: Option<String>,
) -> WorktreeResult<WorktreeRemoveOutput> {
    let Some(worktree_id) = worktree_id else {
        return Err(WorktreeError::OperationBlocked(format!(
            "cannot detach '{}': its stable worktree id is unknown; run `libra worktree \
              repair --confirm` first",
            target.display()
        )));
    };
    let payload = serde_json::json!({
        "path": target.to_string_lossy(),
        "delete_dir": false,
    });
    let journal_id = journal_append(
        db,
        WorktreeControl::Remove.declare(),
        Some(&worktree_id),
        &payload,
    )
    .await
    .map_err(WorktreeError::OperationBlocked)?;

    // Recovery-ordered: lifecycle mirror → registry state → journal → the
    // gitdir marker LAST. The marker fail-closes every storage resolution
    // inside the target directory, so all cwd-sensitive work (DB access,
    // registry writes — `remove .` runs with cwd IN the target) must finish
    // before it appears. A crash before the marker leaves the journal row
    // (or the detached entry) for `worktree repair`, whose reconcile pass
    // rewrites missing markers from the registry.
    lifecycle_upsert(
        db,
        &worktree_id,
        WorktreeEntryState::DetachedFromRegistry.as_str(),
        &target.to_string_lossy(),
    )
    .await
    .map_err(WorktreeError::OperationBlocked)?;
    state.entries[index].state = WorktreeEntryState::DetachedFromRegistry;
    write_state(state)?;
    // Marker BEFORE resolving the journal: if the marker write fails, the
    // pending row makes `worktree repair` re-freeze the directory. The DB
    // handle was resolved before the marker exists, so the resolve below
    // cannot trip the gate even when cwd is inside the target.
    write_detached_marker(target, &worktree_id)?;
    if let Err(error) = journal_resolve(db, journal_id).await {
        tracing::warn!(
            error,
            "detach journal entry not resolved; repair will reconcile"
        );
    }

    Ok(WorktreeRemoveOutput {
        path: target.to_string_lossy().into_owned(),
        registry_removed: false,
        disk_directory_deleted: false,
        detached: true,
        tombstone: false,
    })
}

/// `--delete-dir` (W3-s1b, §C.7): dirty-check → delete + fsync parent →
/// ONLY THEN clean scoped rows. A cleanup failure keeps the entry as a
/// TOMBSTONE for `worktree repair` to retry — the rows are reachability
/// roots until proven cleaned.
async fn remove_worktree_delete_dir(
    db: &sea_orm::DatabaseConnection,
    state: &mut WorktreeState,
    index: usize,
    target: &Path,
    worktree_id: Option<String>,
    entry_state: WorktreeEntryState,
) -> WorktreeResult<WorktreeRemoveOutput> {
    let payload = serde_json::json!({
        "path": target.to_string_lossy(),
        "delete_dir": true,
    });
    let journal_id = journal_append(
        db,
        WorktreeControl::Remove.declare(),
        worktree_id.as_deref(),
        &payload,
    )
    .await
    .map_err(WorktreeError::OperationBlocked)?;

    // A DETACHED worktree's marker fail-closes the in-worktree dirty check;
    // lift it for the check and restore it on refusal. Under the registry
    // lock, and journaled, so a crash mid-window is rolled forward by
    // repair (the registry entry still says detached).
    let _marker_lifted = if entry_state == WorktreeEntryState::DetachedFromRegistry {
        let marker = target.join(util::ROOT_DIR).join(DETACHED_MARKER);
        match fs::remove_file(&marker) {
            Ok(()) => true,
            Err(error) if error.kind() == io::ErrorKind::NotFound => false,
            Err(error) => {
                let _ = journal_resolve(db, journal_id).await;
                return Err(WorktreeError::IoWrite(format!(
                    "cannot lift the detached marker for the dirty check: {error}"
                )));
            }
        }
    } else {
        false
    };

    let dirty = worktree_is_dirty(target).await;
    match dirty {
        Ok(false) => {}
        Ok(true) => {
            let restored = restore_marker_on_refusal(
                entry_state == WorktreeEntryState::DetachedFromRegistry,
                target,
                worktree_id.as_deref(),
            );
            if restored {
                let _ = journal_resolve(db, journal_id).await;
            }
            return Err(WorktreeError::DirtyWorktree {
                path: target.to_string_lossy().to_string(),
            });
        }
        Err(error) => {
            let restored = restore_marker_on_refusal(
                entry_state == WorktreeEntryState::DetachedFromRegistry,
                target,
                worktree_id.as_deref(),
            );
            if restored {
                let _ = journal_resolve(db, journal_id).await;
            }
            return Err(error);
        }
    }

    // `remove --delete-dir .` runs with cwd INSIDE the target: move out
    // before deleting it, or every later cwd-based lookup (and the user's
    // shell) sits in a deleted directory.
    if let Ok(cwd) = env::current_dir()
        && cwd.starts_with(target)
        && let Some(parent) = target.parent()
    {
        let _ = env::set_current_dir(parent);
    }
    if let Err(e) = fs::remove_dir_all(target) {
        // Re-freeze a detached entry before surfacing the error — the
        // journal row stays pending either way (no resolve on this path),
        // so repair re-establishes whatever this restore could not.
        let _ = restore_marker_on_refusal(
            entry_state == WorktreeEntryState::DetachedFromRegistry,
            target,
            worktree_id.as_deref(),
        );
        return Err(WorktreeError::IoWrite(format!(
            "failed to delete worktree directory '{}': {e}",
            target.display()
        )));
    }
    // The tombstone contract says "directory DURABLY deleted" — so a real
    // fsync failure keeps the entry as a tombstone (rows preserved, repair
    // retries) rather than proceeding to destroy the scoped rows on the
    // strength of an unflushed unlink. Platforms whose directory handles
    // refuse sync (an io::ErrorKind::Unsupported open/sync) keep the old
    // best-effort behavior.
    let durable = match target.parent().map(fs::File::open) {
        Some(Ok(dir)) => match dir.sync_all() {
            Ok(()) => true,
            Err(error) if error.kind() == io::ErrorKind::Unsupported => true,
            Err(error) => {
                tracing::warn!(
                    error = %error,
                    "cannot fsync the deleted worktree's parent; keeping a tombstone"
                );
                false
            }
        },
        // An unopenable parent cannot prove durability either way — keep
        // the historical best-effort answer rather than tombstoning every
        // removal on exotic filesystems.
        _ => true,
    };

    let cleanup_failed = if !durable {
        true
    } else if let Some(id) = worktree_id.as_deref() {
        match gc_worktree_scoped_rows_strict(db, id, true).await {
            Ok(()) => {
                let _ = lifecycle_delete(db, id).await;
                false
            }
            Err(error) => {
                tracing::warn!(
                    worktree_id = id,
                    error,
                    "scoped-row cleanup failed after directory deletion; keeping a tombstone"
                );
                lifecycle_upsert(
                    db,
                    id,
                    WorktreeEntryState::Tombstone.as_str(),
                    &target.to_string_lossy(),
                )
                .await
                .map_err(WorktreeError::OperationBlocked)?;
                true
            }
        }
    } else {
        false
    };

    if cleanup_failed {
        state.entries[index].state = WorktreeEntryState::Tombstone;
    } else {
        state.entries.remove(index);
    }
    write_state(state)?;
    if let Err(error) = journal_resolve(db, journal_id).await {
        tracing::warn!(
            error,
            "remove journal entry not resolved; repair will reconcile"
        );
    }

    Ok(WorktreeRemoveOutput {
        path: target.to_string_lossy().into_owned(),
        registry_removed: !cleanup_failed,
        disk_directory_deleted: true,
        detached: false,
        tombstone: cleanup_failed,
    })
}

/// Restore the detached marker after a refused `--delete-dir`. Returns
/// whether the gate is intact again — when the restore FAILS, the caller
/// keeps the journal row pending so `worktree repair` re-freezes the
/// directory instead of leaving a detached entry unfrozen.
fn restore_marker_on_refusal(
    entry_is_detached: bool,
    target: &Path,
    worktree_id: Option<&str>,
) -> bool {
    // Restore UNCONDITIONALLY for detached entries: the marker may have
    // been lifted for the dirty check, or may have been missing already
    // (a crash between registry publication and marker creation) — either
    // way, a refused delete must leave the directory frozen again.
    if !entry_is_detached {
        return true;
    }
    let Some(id) = worktree_id else {
        return false;
    };
    match write_detached_marker(target, id) {
        Ok(()) => true,
        Err(error) => {
            tracing::warn!(
                %error,
                "could not restore the detached marker after a refused delete; the \
                 pending journal row lets `worktree repair` re-freeze the directory"
            );
            false
        }
    }
}

#[derive(Debug, Serialize)]
struct MigrateLayoutOutput {
    dry_run: bool,
    migrated: Vec<String>,
    planned: Vec<String>,
    skipped: Vec<String>,
}

/// `repair --migrate-layout [--dry-run] [<path>]` (W3-s3 §C.6.2): migrate
/// legacy shared-`.libra` symlink worktrees to the isolated layout via the
/// journaled state machine. Runs from the MAIN worktree only, under the
/// registry lock. Tracked/untracked FILES are never touched — the new
/// private index is built from the shared HEAD snapshot, so post-migration
/// they simply show as dirty/untracked against it (staged state is NOT
/// copied: the shared index cannot prove which worktree owns it).
async fn migrate_layout_run(
    filter: Option<String>,
    dry_run: bool,
) -> WorktreeResult<MigrateLayoutOutput> {
    if util::current_worktree_id().is_some() || util::is_legacy_symlink_worktree() {
        return Err(WorktreeError::OperationBlocked(
            "run `worktree repair --migrate-layout --confirm` from the MAIN worktree".to_string(),
        ));
    }
    // Dry run is READ-ONLY end to end: no lock file creation, no registry
    // upgrade, and NO database open at all — the lockless reader is enough to
    // enumerate layouts, and even a migration-applying connection would be a
    // write on the repository being previewed (§C.11, Codex R20).
    let (_registry_lock, state) = if dry_run {
        (None, load_state_readonly()?)
    } else {
        let guard = acquire_registry_lock_async().await?;
        let state = load_state()?;
        (Some(guard), state)
    };
    let filter_path = match &filter {
        Some(raw) => Some(resolve_path(raw, "worktree path")?),
        None => None,
    };

    let mut targets = Vec::new();
    let mut skipped = Vec::new();
    for (idx, entry) in state.entries.iter().enumerate() {
        if entry.is_main {
            continue;
        }
        if let Some(want) = &filter_path
            && Path::new(&entry.path) != want
        {
            continue;
        }
        match detect_entry_layout(Path::new(&entry.path), false) {
            "legacy-symlink" => targets.push(idx),
            layout => {
                if filter_path.is_some() {
                    return Err(WorktreeError::InvalidTarget(format!(
                        "'{}' is not a legacy-symlink worktree (layout: {layout})",
                        entry.path
                    )));
                }
                skipped.push(format!("{} ({layout})", entry.path));
            }
        }
    }
    if let Some(want) = &filter_path
        && targets.is_empty()
    {
        return Err(WorktreeError::NoSuchWorktree {
            path: want.to_string_lossy().to_string(),
        });
    }

    if dry_run {
        return Ok(MigrateLayoutOutput {
            dry_run: true,
            migrated: Vec::new(),
            planned: targets
                .into_iter()
                .map(|idx| state.entries[idx].path.clone())
                .collect(),
            skipped,
        });
    }

    // Confirmed execution only: the dry-run preview returned above without
    // any database access, so this connection never resolves on the read-only
    // path. (Migrations were already applied by the CLI preflight and the
    // dispatch-time open — the §C.7 contract for confirmed repair actions.)
    let db = crate::internal::db::get_db_conn_instance().await;

    // Preconditions shared by every target (§C.6.2 step 4): the SHARED
    // index must be conflict-free, no repository-global (main-scope)
    // sequencer may be active, and HEAD must be readable and born.
    let shared_index = git_internal::internal::index::Index::load(crate::utils::path::index())
        .map_err(|e| WorktreeError::IoRead(format!("cannot read the shared index: {e}")))?;
    if !crate::command::unmerged::collect(&shared_index).is_empty() {
        return Err(WorktreeError::OperationBlocked(
            "the shared index has unmerged (conflict) entries; resolve or abort the \
              conflict in the main worktree first"
                .to_string(),
        ));
    }
    if scoped_state_active(&db, "").await {
        return Err(WorktreeError::OperationBlocked(
            "an in-progress rebase/cherry-pick/bisect is active in the main worktree; \
              finish or abort it first"
                .to_string(),
        ));
    }
    // A legacy-symlink worktree SHARES common storage, so an active merge/
    // revert or held autostash there belongs to some worktree that must
    // conclude it before the layout underneath it changes.
    refuse_active_sidecar_state(&util::storage_path(), "migrating the layout")?;
    let head_commit = Head::current_commit_result()
        .await
        .map_err(|e| WorktreeError::IoRead(format!("cannot read the shared HEAD: {e}")))?
        .ok_or_else(|| {
            WorktreeError::OperationBlocked(
                "the repository has no commits yet; nothing to migrate onto".to_string(),
            )
        })?;

    let mut state = state;
    let mut migrated = Vec::new();
    for idx in targets {
        let path = state.entries[idx].path.clone();
        migrate_one_worktree(&db, &mut state, idx, head_commit).await?;
        migrated.push(path);
    }

    Ok(MigrateLayoutOutput {
        dry_run: false,
        migrated,
        planned: Vec::new(),
        skipped,
    })
}

/// One target's journaled migration (§C.6.2 steps 1-7). Every filesystem
/// step verifies IDENTITY (no-follow symlink target, our own marker file),
/// never bare existence; any mismatch keeps all materials and errors.
async fn migrate_one_worktree(
    db: &sea_orm::DatabaseConnection,
    state: &mut WorktreeState,
    index: usize,
    head_commit: git_internal::hash::ObjectHash,
) -> WorktreeResult<()> {
    let target = PathBuf::from(&state.entries[index].path);
    let gitdir = target.join(util::ROOT_DIR);
    let storage = util::storage_path();
    let canonical_storage = fs::canonicalize(&storage).unwrap_or_else(|_| storage.clone());

    // Step 1: no-follow — `.libra` must BE a symlink resolving exactly to
    // the common storage. Anything else is refused untouched.
    let meta = fs::symlink_metadata(&gitdir)
        .map_err(|e| WorktreeError::IoRead(format!("cannot stat '{}': {e}", gitdir.display())))?;
    if !meta.file_type().is_symlink()
        || fs::canonicalize(&gitdir).ok().as_deref() != Some(canonical_storage.as_path())
    {
        return Err(WorktreeError::OperationBlocked(format!(
            "'{}' is not a legacy symlink at this repository's storage; refusing",
            gitdir.display()
        )));
    }

    let worktree_id = state.entries[index].worktree_id.clone().ok_or_else(|| {
        WorktreeError::OperationBlocked(format!(
            "registry entry '{}' has no persisted worktree id; run `libra worktree repair \
                  --confirm` first",
            target.display()
        ))
    })?;
    // A target with an UNRESOLVED earlier migrate intent must be settled by
    // `worktree repair` first — starting a second migration would leave the
    // first row permanently ambiguous (recovery adopts only its own
    // journal-stamped marker).
    let pending = journal_pending(db)
        .await
        .map_err(WorktreeError::OperationBlocked)?;
    if pending.iter().any(|intent| {
        intent.op == "migrate"
            && intent.payload["path"].as_str() == Some(target.to_string_lossy().as_ref())
    }) {
        return Err(WorktreeError::OperationBlocked(format!(
            "'{}' has an unresolved earlier migration journal; run `libra worktree \
              repair --confirm` to settle it, then retry",
            target.display()
        )));
    }
    let payload = serde_json::json!({
        "path": target.to_string_lossy(),
        "head": head_commit.to_string(),
    });
    let journal_id = journal_append(
        db,
        WorktreeControl::MigrateLayout.declare(),
        Some(&worktree_id),
        &payload,
    )
    .await
    .map_err(WorktreeError::OperationBlocked)?;
    let set_stage = |stage: &'static str| {
        let db = db.clone();
        async move {
            journal_set_stage(&db, journal_id, stage)
                .await
                .map_err(WorktreeError::OperationBlocked)
        }
    };

    // Step 2: prepared gitdir with identity marker, fsynced.
    let prepared = target.join(format!(".libra.migrate-{journal_id}"));
    fs::create_dir_all(&prepared).map_err(|e| {
        WorktreeError::IoWrite(format!("cannot create '{}': {e}", prepared.display()))
    })?;
    let write = |name: &str, contents: String| -> WorktreeResult<()> {
        crate::utils::atomic_write::write_atomic(&prepared.join(name), contents.as_bytes(), true)
            .map_err(|e| {
                WorktreeError::IoWrite(format!("cannot write '{}/{name}': {e}", prepared.display()))
            })
    };
    // The MARKER is written FIRST: recovery identifies the prepared dir by
    // this marker, so a crash after `create_dir_all` but before the marker
    // used to manufacture a directory recovery could never settle (the
    // journal stayed pending forever, and a fresh migration was refused by
    // the pending-journal guard). Marker-first closes that window — every
    // crash from here on leaves an identifiable artifact.
    write("migrate-marker", format!("journal {journal_id}\n"))?;
    write("commondir", format!("{}\n", canonical_storage.display()))?;
    write("worktree_id", format!("{worktree_id}\n"))?;
    fsync_parent_best_effort(&prepared.join("commondir"));
    fsync_parent_best_effort(&prepared);
    set_stage("PreparedLocalGitdir").await?;

    // Step 5a: atomic rename of the symlink to a journal-identified backup.
    let backup = target.join(format!(".libra.legacy-backup-{journal_id}"));
    match fs::symlink_metadata(&backup) {
        Ok(_) => {
            return Err(WorktreeError::OperationBlocked(format!(
                "'{}' already exists; remove it manually, then retry",
                backup.display()
            )));
        }
        Err(error) if error.kind() == io::ErrorKind::NotFound => {}
        Err(error) => {
            return Err(WorktreeError::IoRead(format!(
                "cannot inspect '{}': {error}",
                backup.display()
            )));
        }
    }
    let _gate_bypass = util::bypass_migration_gate();
    // Atomic NO-REPLACE backup on Unix: symlink(2) fails with EEXIST if a
    // concurrent actor claimed the name after the check above, so user
    // material can never be silently overwritten by a replace-capable
    // rename. (A crash between the two steps leaves the legacy link intact
    // — the pre-backup recovery branch removes this identity-named backup.)
    #[cfg(unix)]
    {
        let dest = fs::read_link(&gitdir).map_err(|e| {
            WorktreeError::IoRead(format!("cannot read the legacy link target: {e}"))
        })?;
        std::os::unix::fs::symlink(&dest, &backup).map_err(|e| {
            WorktreeError::IoWrite(format!("cannot back up the legacy symlink: {e}"))
        })?;
        // Durability order: the backup's directory entry must be on disk
        // BEFORE the original link disappears — a power loss in between
        // must never leave neither. This sync is STRICT: if the filesystem
        // cannot prove the entry durable, abort with the backup removed and
        // the legacy link untouched.
        if let Err(error) = fsync_parent_strict(&backup) {
            let _ = fs::remove_file(&backup);
            return Err(WorktreeError::IoWrite(format!(
                "cannot durably record the backup link ({error}); migration aborted with \
                  the legacy link untouched"
            )));
        }
        fs::remove_file(&gitdir).map_err(|e| {
            WorktreeError::IoWrite(format!("cannot retire the legacy symlink: {e}"))
        })?;
    }
    #[cfg(not(unix))]
    fs::rename(&gitdir, &backup)
        .map_err(|e| WorktreeError::IoWrite(format!("cannot back up the legacy symlink: {e}")))?;
    fsync_parent_best_effort(&backup);
    set_stage("OldLinkBackedUp").await?;

    // Step 5b: install the prepared gitdir.
    fs::rename(&prepared, &gitdir).map_err(|e| {
        WorktreeError::IoWrite(format!(
            "cannot install the new gitdir (legacy backup kept at '{}'): {e}",
            backup.display()
        ))
    })?;
    fsync_parent_best_effort(&gitdir);
    validate_installed_gitdir(&gitdir, journal_id, &worktree_id, &canonical_storage).map_err(
        |error| {
            WorktreeError::OperationBlocked(format!(
                "installed gitdir failed identity validation ({error}); materials kept — \
                  investigate, then rerun `worktree repair --confirm`"
            ))
        },
    )?;
    set_stage("NewGitdirInstalled").await?;

    // Step 3: seed the scoped HEAD (detached at the shared snapshot) and
    // build the private index from that commit — WITHOUT touching files.
    seed_migrated_worktree(&target, head_commit).await?;
    set_stage("HeadIndexSeeded").await?;

    // Registry: the entry's layout is now derivable as linked-v2; persist
    // the (possibly id-backfilled) state.
    write_state(state)?;
    set_stage("RegistryCommitted").await?;

    // Step 6: verify from inside the worktree before any cleanup.
    verify_migrated_worktree(&target, head_commit).await?;
    set_stage("Verified").await?;

    // Cleanup: drop the backup symlink and the marker, resolve the journal.
    match fs::symlink_metadata(&backup) {
        Ok(meta)
            if meta.file_type().is_symlink()
                && fs::canonicalize(&backup).ok().as_deref()
                    == Some(canonical_storage.as_path()) =>
        {
            fs::remove_file(&backup).map_err(|error| {
                WorktreeError::IoWrite(format!(
                    "migrated, but the legacy backup '{}' could not be removed: {error}; \
                      remove it and rerun `worktree repair --confirm`",
                    backup.display()
                ))
            })?;
        }
        Err(error) if error.kind() == io::ErrorKind::NotFound => {}
        _ => {
            return Err(WorktreeError::OperationBlocked(format!(
                "'{}' is no longer the expected legacy symlink; not deleting it — \
                  investigate, then rerun `worktree repair --confirm`",
                backup.display()
            )));
        }
    }
    // Marker LAST: recovery adopts an install ONLY via this journal-stamped
    // marker, so it must survive until the journal resolve below has landed
    // (resolve-before-marker is what makes marker-only adoption safe).
    journal_resolve(db, journal_id)
        .await
        .map_err(WorktreeError::OperationBlocked)?;
    let _ = fs::remove_file(gitdir.join("migrate-marker"));
    Ok(())
}

/// Seed a freshly installed isolated gitdir: detached HEAD at the shared
/// snapshot, private index rebuilt from that commit's tree. Working files
/// are left exactly as they were.
async fn seed_migrated_worktree(
    target: &Path,
    head_commit: git_internal::hash::ObjectHash,
) -> WorktreeResult<()> {
    let _guard = DirGuard::change_to(target)
        .map_err(|e| WorktreeError::IoRead(format!("cannot enter '{}': {e}", target.display())))?;
    // §C.4.2: the private index this builds belongs to `target`, not to the
    // worktree the invocation pinned — re-pin so `path::index()` writes it
    // there instead of rebuilding the invoker's index from `target`'s HEAD.
    let _scope =
        crate::internal::worktree_scope::WorktreeScope::override_scope(target.to_path_buf());
    Head::update_result(Head::Detached(head_commit), None)
        .await
        .map_err(|e| {
            WorktreeError::IoWrite(format!("cannot seed HEAD for '{}': {e}", target.display()))
        })?;
    restore::execute_checked(RestoreArgs {
        overlay: false,
        no_overlay: false,
        ours: false,
        theirs: false,
        ignore_unmerged: false,
        merge: false,
        conflict: None,
        pathspec: vec![util::working_dir_string()],
        source: Some("HEAD".to_string()),
        worktree: false,
        staged: true,
        pathspec_from_file: None,
        pathspec_file_nul: false,
        no_progress: false,
    })
    .await
    .map_err(|e| {
        WorktreeError::IoWrite(format!(
            "cannot build the private index for '{}': {e}",
            target.display()
        ))
    })
}

/// Post-install verification (§C.6.2 step 6): scoped HEAD resolves to the
/// migrated snapshot, the private index exists, and the common object
/// store is reachable through the new commondir.
async fn verify_migrated_worktree(
    target: &Path,
    head_commit: git_internal::hash::ObjectHash,
) -> WorktreeResult<()> {
    let _guard = DirGuard::change_to(target)
        .map_err(|e| WorktreeError::IoRead(format!("cannot enter '{}': {e}", target.display())))?;
    let seen = Head::current_commit_result()
        .await
        .map_err(|e| WorktreeError::IoRead(format!("verification failed reading HEAD: {e}")))?;
    if seen != Some(head_commit) {
        return Err(WorktreeError::OperationBlocked(format!(
            "verification failed: '{}' resolves HEAD to {:?}, expected {head_commit}; \
              materials kept — rerun `worktree repair --confirm`",
            target.display(),
            seen
        )));
    }
    if !target.join(util::ROOT_DIR).join("index").exists() {
        return Err(WorktreeError::OperationBlocked(format!(
            "verification failed: '{}' has no private index; materials kept — rerun \
              `worktree repair --confirm`",
            target.display()
        )));
    }
    crate::command::load_object::<git_internal::internal::object::commit::Commit>(&head_commit)
        .map_err(|e| {
            WorktreeError::IoRead(format!(
                "verification failed reading the HEAD commit through the new commondir: {e}"
            ))
        })?;
    Ok(())
}

fn render_migrate_layout(result: &MigrateLayoutOutput, output: &OutputConfig) -> CliResult<()> {
    if output.is_json() {
        return emit_json_data("worktree.repair", result, output);
    }
    if output.quiet {
        return Ok(());
    }
    if result.dry_run {
        if result.planned.is_empty() {
            println!("No legacy-symlink worktrees to migrate.");
        }
        for path in &result.planned {
            println!("would migrate {path}");
        }
    } else {
        for path in &result.migrated {
            println!("migrated {path}");
        }
        if result.migrated.is_empty() {
            println!("No legacy-symlink worktrees to migrate.");
        }
    }
    for entry in &result.skipped {
        println!("skipped {entry}");
    }
    Ok(())
}

/// Full identity validation of an INSTALLED migration gitdir, run
/// immediately before seeding (post-rename): no-follow real directory, the
/// journal-stamped marker, a commondir canonicalizing to THIS storage, and
/// the expected worktree id. Post-install validation binds what was
/// actually installed, closing the check-then-rename window.
fn validate_installed_gitdir(
    gitdir: &Path,
    journal_id: i64,
    worktree_id: &str,
    canonical_storage: &Path,
) -> Result<(), String> {
    if !no_follow_real_dir(gitdir) {
        return Err(format!("'{}' is not a real directory", gitdir.display()));
    }
    let marker_ok = fs::read_to_string(gitdir.join("migrate-marker"))
        .is_ok_and(|raw| raw.trim() == format!("journal {journal_id}"));
    if !marker_ok {
        return Err(format!(
            "'{}' does not carry this migration's marker",
            gitdir.display()
        ));
    }
    let commondir_ok = fs::read_to_string(gitdir.join("commondir"))
        .ok()
        .and_then(|raw| raw.lines().next().map(str::trim).map(PathBuf::from))
        .map(|p| {
            let abs = if p.is_absolute() { p } else { gitdir.join(p) };
            fs::canonicalize(&abs).unwrap_or(abs)
        })
        .is_some_and(|p| p == canonical_storage);
    if !commondir_ok {
        return Err(format!(
            "'{}' commondir does not resolve to this repository's storage",
            gitdir.display()
        ));
    }
    let id_ok =
        fs::read_to_string(gitdir.join("worktree_id")).is_ok_and(|raw| raw.trim() == worktree_id);
    if !id_ok {
        return Err(format!(
            "'{}' worktree_id does not match the registry",
            gitdir.display()
        ));
    }
    Ok(())
}

/// No-follow check: a REAL directory (symlink_metadata never follows, so a
/// symlinked path reports the link itself).
fn no_follow_real_dir(path: &Path) -> bool {
    fs::symlink_metadata(path)
        .map(|meta| meta.is_dir())
        .unwrap_or(false)
}

/// Write the fail-closed gitdir marker for a detached worktree.
fn write_detached_marker(target: &Path, worktree_id: &str) -> WorktreeResult<()> {
    let marker = target.join(util::ROOT_DIR).join(DETACHED_MARKER);
    crate::utils::atomic_write::write_atomic(
        &marker,
        format!(
            "{worktree_id}\nremoved from the worktree registry; re-add or delete this \
              directory\n"
        )
        .as_bytes(),
        true,
    )
    .map_err(|source| {
        WorktreeError::IoWrite(format!(
            "cannot write the detached marker '{}': {source}",
            marker.display()
        ))
    })
}

/// Strict parent-directory fsync: returns the error instead of swallowing
/// it (used where a lost directory entry would open a crash window).
fn fsync_parent_strict(target: &Path) -> io::Result<()> {
    let parent = target
        .parent()
        .ok_or_else(|| io::Error::other("path has no parent"))?;
    fs::File::open(parent)?.sync_all()
}

/// Durably record the parent directory entry after a worktree deletion —
/// the tombstone contract ("directory durably deleted") depends on it.
/// Best-effort on platforms/filesystems that refuse directory fsync.
/// Whether a MARKER-LESS prepared-migration dir is safe to roll back: its
/// name already binds it to one exact journal id, and it contains AT MOST
/// the files the migration engine writes before installation. Anything else
/// inside means a human or another tool touched it — keep it and the
/// journal.
/// The `worktree_id` recorded in the gitdir AT `path`, if readable — the
/// identity check move-recovery uses before renaming or adopting a
/// directory (bare existence is not identity: a stranger's directory can
/// come to occupy the journalled path after a crash).
fn gitdir_identity_at(path: &str) -> Option<String> {
    fs::read_to_string(Path::new(path).join(util::ROOT_DIR).join("worktree_id"))
        .ok()
        .map(|raw| raw.trim().to_string())
}

fn prepared_dir_is_settled_leftover(prepared: &Path) -> bool {
    let Ok(entries) = fs::read_dir(prepared) else {
        return false;
    };
    for entry in entries {
        let Ok(entry) = entry else { return false };
        let known = matches!(
            entry.file_name().to_str(),
            Some("migrate-marker" | "commondir" | "worktree_id")
        );
        if !known {
            return false;
        }
    }
    true
}

fn fsync_parent_best_effort(target: &Path) {
    if let Some(parent) = target.parent()
        && let Ok(dir) = fs::File::open(parent)
    {
        let _ = dir.sync_all();
    }
}

/// Refuse to remove/migrate a worktree whose gitdir holds an ACTIVE
/// merge/revert or a held autostash (§C.4.3).
///
/// The DB check beside this covers rebase/cherry-pick/bisect rows; these
/// three are FILES in the target's own gitdir, and detaching writes the
/// fail-closed marker OVER them — the operation would be stranded behind a
/// gate the user cannot lift without `repair`. A sidecar that cannot be
/// STATed is treated as present: unreadable is not evidence of absence.
fn refuse_active_sidecar_state(gitdir: &Path, action: &str) -> WorktreeResult<()> {
    for name in [
        "merge-state.json",
        "revert-state.json",
        "merge-autostash.json",
        // An interrupted `stash branch`'s rollback record: removing the
        // gitdir would strand the half-created branch AND delete the only
        // instruction for undoing it.
        "stash-branch-journal.json",
    ] {
        let path = gitdir.join(name);
        match fs::symlink_metadata(&path) {
            Ok(_) => {
                return Err(WorktreeError::OperationBlocked(format!(
                    "'{}' holds in-progress state ({name}); conclude the merge/revert \
                      (or run any `libra stash` command there to finish a journaled \
                      rollback) before {action}",
                    gitdir.display()
                )));
            }
            Err(error) if error.kind() == io::ErrorKind::NotFound => {}
            Err(error) => {
                return Err(WorktreeError::IoRead(format!(
                    "cannot inspect '{}' before {action}: {error}",
                    path.display()
                )));
            }
        }
    }
    Ok(())
}

/// The in-worktree dirty check shared by `--delete-dir` (staged or real —
/// non-overlay — unstaged changes refuse the destructive delete).
async fn worktree_is_dirty(target: &Path) -> WorktreeResult<bool> {
    let _guard = DirGuard::change_to(target).map_err(|e| {
        WorktreeError::IoRead(format!("cannot enter worktree '{}': {e}", target.display()))
    })?;
    // §C.4.2: this gate decides whether a DESTRUCTIVE delete may proceed, and
    // it must judge `target`'s own index. Inheriting the invoker's pin would
    // compare `target`'s files against ANOTHER worktree's staged state — a
    // clean invoker would then read a dirty target as clean and delete it.
    let _scope =
        crate::internal::worktree_scope::WorktreeScope::override_scope(target.to_path_buf());
    // W1 §C.4.1.1: applied layer overlays are excluded from status by
    // design, so they alone are not "uncommitted changes". This is a
    // DESTRUCTIVE gate (`remove_dir_all` follows), so it must NOT consult
    // the process-global advisory exclusion snapshot — another scope's
    // refresh could hide REAL uncommitted files behind same-named overlay
    // paths. The target scope's overlay set is read straight from the DB
    // (fail-closed on error) and subtracted explicitly from the UNSTAGED
    // side only; anything staged always refuses.
    let overlay: std::collections::HashSet<String> =
        crate::internal::layer::LayerStore::materialized_paths(
            &crate::internal::worktree_scope::WorktreeScope::for_workdir(target),
        )
        .await
        .map_err(|e| {
            WorktreeError::IoRead(format!(
                "cannot verify layer-owned paths before the dirty check: {e}"
            ))
        })?
        .into_iter()
        .map(|p| p.path)
        .collect();
    let staged = crate::command::status::changes_to_be_committed_safe()
        .await
        .map_err(|e| WorktreeError::IoRead(format!("failed to inspect worktree status: {e}")))?;
    let unstaged = crate::command::status::changes_to_be_staged()
        .map_err(|e| WorktreeError::IoRead(format!("failed to inspect worktree status: {e}")))?;
    let is_real_change = |path: &std::path::PathBuf| {
        crate::internal::layer::normalize_key(path).is_none_or(|key| !overlay.contains(&key))
    };
    let unstaged_dirty = unstaged
        .new
        .iter()
        .chain(unstaged.modified.iter())
        .chain(unstaged.deleted.iter())
        .any(is_real_change)
        || !unstaged.renamed.is_empty();
    Ok(!staged.is_empty() || unstaged_dirty)
}

fn render_remove_worktree(result: &WorktreeRemoveOutput, output: &OutputConfig) -> CliResult<()> {
    if output.is_json() {
        return emit_json_data("worktree.remove", result, output);
    }
    if output.quiet {
        return Ok(());
    }
    if result.tombstone {
        println!(
            "Deleted worktree directory '{}', but the scoped-state cleanup failed — a \
              tombstone entry remains; run `libra worktree repair --confirm` to retry.",
            result.path
        );
    } else if result.disk_directory_deleted {
        println!(
            "Removed worktree '{}' from registry and deleted directory.",
            result.path
        );
    } else {
        println!(
            "Detached worktree '{}' from the registry. Directory and its state kept on \
              disk (frozen); re-add it with `libra worktree add` or delete it with \
              `--delete-dir`.",
            result.path
        );
    }
    Ok(())
}

#[cfg(unix)]
fn umount_fuse_path(path: String, cleanup: bool) -> WorktreeResult<WorktreeUmountOutput> {
    let target = resolve_path(&path, "FUSE worktree path")?;
    let mountpoint = fuse_utils::resolve_task_worktree_mountpoint_arg(&target);
    fuse_utils::force_unmount_path(&mountpoint).map_err(|source| {
        WorktreeError::IoWrite(format!(
            "failed to unmount FUSE path {}: {source}",
            mountpoint.display()
        ))
    })?;

    let mut cleanup_root = None;
    let mut cleanup_root_removed = false;
    if cleanup {
        let root = fuse_utils::fuse_task_worktree_cleanup_root(&mountpoint).ok_or_else(|| {
            WorktreeError::InvalidTarget(format!(
                "--cleanup only supports Libra task FUSE worktree paths ending in '/workspace': {}",
                mountpoint.display()
            ))
        })?;
        if root.exists() {
            fs::remove_dir_all(&root).map_err(|source| {
                WorktreeError::IoWrite(format!(
                    "failed to remove FUSE worktree root '{}': {source}",
                    root.display()
                ))
            })?;
            cleanup_root_removed = true;
        }
        cleanup_root = Some(root.to_string_lossy().to_string());
    }

    Ok(WorktreeUmountOutput {
        mountpoint: mountpoint.to_string_lossy().to_string(),
        unmounted: true,
        cleanup_requested: cleanup,
        cleanup_root,
        cleanup_root_removed,
    })
}

#[cfg(unix)]
fn render_umount_fuse_path(result: &WorktreeUmountOutput, output: &OutputConfig) -> CliResult<()> {
    if output.is_json() {
        return emit_json_data("worktree.umount", result, output);
    }
    if output.quiet {
        return Ok(());
    }
    println!("unmounted {}", result.mountpoint);
    if let Some(cleanup_root) = &result.cleanup_root {
        println!("removed {}", cleanup_root);
    }
    Ok(())
}

/// Implements `worktree repair`.
///
/// This command removes duplicate worktree entries that point to the same
/// canonical path and re-applies the invariant that there is exactly one
/// main worktree entry. The repaired state is only written back if changes
/// were actually made.
#[derive(Debug, Serialize)]
struct WorktreeRepairIdentityOutput {
    path: String,
    worktree_id: String,
    worktree_id_restored: bool,
    commondir_restored: bool,
}

/// `worktree repair <path>` (W3 §C.7): restore a linked worktree's gitdir
/// identity from the registry's PERSISTED stable id — never from guesses.
/// Rewrites `.libra/worktree_id` when missing/corrupt and `commondir` when
/// missing (pointing at this repository's common storage). Refuses for
/// unregistered paths, the main worktree, and entries whose registry row
/// carries no persisted id (pre-v2 rows: run the no-arg `worktree repair`
/// once to upgrade the registry, or re-add the worktree).
async fn repair_worktree_identity(path: String) -> WorktreeResult<WorktreeRepairIdentityOutput> {
    let _registry_lock = acquire_registry_lock_async().await?;
    // A legacy v1 registry carries NO persisted identities — refuse before
    // the locked loader would durably upgrade it (backfilling ids from the
    // possibly-damaged gitdirs this command is meant to repair). The no-arg
    // repair is the explicit, documented upgrade step.
    if let Ok(raw) = fs::read(state_path())
        && !raw.is_empty()
        && matches!(
            WorktreeState::parse_document(&raw),
            Ok((_, RegistryShape::V1))
        )
    {
        return Err(WorktreeError::OperationBlocked(
            "the worktree registry still uses the legacy v1 format with no persisted \
              identities; run `libra worktree repair --confirm` (no argument) once to upgrade \
              it, then retry"
                .to_string(),
        ));
    }
    let state = load_state()?;
    let target = resolve_path(&path, "worktree path")?;
    let entry =
        find_entry(&state, &target).ok_or(WorktreeError::NoSuchWorktree { path: path.clone() })?;
    if entry.is_main {
        return Err(WorktreeError::MainWorktree {
            action: WorktreeControl::Repair.declare(),
            path: target.to_string_lossy().to_string(),
        });
    }
    // A TOMBSTONE's directory was durably deleted; whatever exists at the
    // path now is a stranger, and stamping identity files into it would
    // adopt on the identity surface what `retry_tombstones` explicitly
    // refuses to adopt — while the entry stays in cleanup limbo.
    if entry.state == WorktreeEntryState::Tombstone {
        return Err(WorktreeError::OperationBlocked(format!(
            "'{}' is a tombstone (its directory was already deleted; only scoped-row \
              cleanup is pending) — run the no-arg `libra worktree repair --confirm` to \
              retry the cleanup instead of re-stamping identity into a recreated directory",
            target.display()
        )));
    }
    let Some(stable_id) = entry.worktree_id.clone() else {
        return Err(WorktreeError::OperationBlocked(format!(
            "the registry entry for '{}' predates registry v2 and carries no persisted \
              worktree id; run the no-arg `libra worktree repair --confirm` once to upgrade \
              the registry, then retry",
            target.display()
        )));
    };
    let gitdir = target.join(util::ROOT_DIR);
    // No-follow: a LEGACY symlink gitdir resolves to MAIN storage — writing
    // identity files through it would alter main metadata. Refuse with the
    // migration hint; only a REAL directory is repairable.
    if detect_entry_layout(&target, false) == "legacy-symlink" {
        return Err(WorktreeError::OperationBlocked(format!(
            "'{}' uses the legacy shared-.libra symlink layout; run `libra worktree \
              repair --migrate-layout --confirm {}` first",
            target.display(),
            target.display()
        )));
    }
    if !no_follow_real_dir(&gitdir) {
        return Err(WorktreeError::OperationBlocked(format!(
            "'{}' has no real .libra gitdir to repair; re-add the worktree instead",
            target.display()
        )));
    }

    // Classify the commondir pointer FIRST — a foreign-storage refusal must
    // happen before ANY write, or a failed repair would still have mutated
    // the target worktree's identity.
    let commondir_path = gitdir.join("commondir");
    let common = util::storage_path();
    // A commondir pointer needs restoring when it is MISSING or CORRUPT
    // (unreadable / empty first line — the same states the storage resolver
    // fails closed on). A VALID pointer at a DIFFERENT storage is refused:
    // that worktree belongs to another repository and silently re-homing it
    // would alias two repos' state. Relative pointers resolve against the
    // local gitdir, exactly like the storage resolver.
    let current_common = match fs::read_to_string(&commondir_path) {
        Ok(contents) => contents
            .lines()
            .next()
            .map(str::trim)
            .filter(|line| !line.is_empty())
            .map(PathBuf::from),
        Err(_) => None,
    };
    let commondir_restored = match current_common {
        Some(existing) => {
            let existing_abs = if existing.is_absolute() {
                existing.clone()
            } else {
                gitdir.join(&existing)
            };
            match fs::canonicalize(&existing_abs) {
                Ok(existing_resolved) => {
                    let common_resolved =
                        fs::canonicalize(&common).unwrap_or_else(|_| common.clone());
                    if existing_resolved != common_resolved {
                        // An EXISTING different storage: refusing is the
                        // never-re-home rule.
                        return Err(WorktreeError::OperationBlocked(format!(
                            "'{}' already points at a different common storage ('{}'); \
                              refusing to re-home the worktree — remove and re-add it if \
                              this is intended",
                            commondir_path.display(),
                            existing_resolved.display()
                        )));
                    }
                    false
                }
                // A DANGLING pointer proves nothing about a different
                // storage — the target does not exist (the classic moved-
                // repository corruption the resolver's errors send users
                // here to fix). It is CORRUPT, and corrupt pointers are
                // exactly what this command restores from the registry.
                Err(error) if error.kind() == io::ErrorKind::NotFound => true,
                Err(error) => {
                    return Err(WorktreeError::IoRead(format!(
                        "cannot inspect the commondir target '{}' recorded in '{}': {error}",
                        existing_abs.display(),
                        commondir_path.display()
                    )));
                }
            }
        }
        None => true,
    };

    let id_path = gitdir.join("worktree_id");
    let current_id = fs::read_to_string(&id_path)
        .ok()
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty());
    // §C.9: declare before the first possible write. `repair` writes identity
    // FILES rather than a journal row (there is no crash window a journal
    // could roll forward — each write is a single atomic rename), so the
    // declaration is asserted here rather than at an append site.
    let _declared = WorktreeControl::Repair.declare();
    let worktree_id_restored = current_id.as_deref() != Some(stable_id.as_str());
    if worktree_id_restored {
        crate::utils::atomic_write::write_atomic(
            &id_path,
            format!("{stable_id}\n").as_bytes(),
            true,
        )
        .map_err(|source| {
            WorktreeError::IoWrite(format!(
                "failed to restore '{}': {source}",
                id_path.display()
            ))
        })?;
    }

    if commondir_restored {
        crate::utils::atomic_write::write_atomic(
            &commondir_path,
            format!("{}\n", common.display()).as_bytes(),
            true,
        )
        .map_err(|source| {
            WorktreeError::IoWrite(format!(
                "failed to restore '{}': {source}",
                commondir_path.display()
            ))
        })?;
    }

    Ok(WorktreeRepairIdentityOutput {
        path: target.to_string_lossy().to_string(),
        worktree_id: stable_id,
        worktree_id_restored,
        commondir_restored,
    })
}

fn render_repair_identity(
    result: &WorktreeRepairIdentityOutput,
    output: &OutputConfig,
) -> CliResult<()> {
    if output.is_json() {
        return emit_json_data("worktree.repair", result, output);
    }
    if !output.quiet {
        println!(
            "repaired '{}': worktree_id {}{}{}",
            result.path,
            result.worktree_id,
            if result.worktree_id_restored {
                " (restored)"
            } else {
                " (already correct)"
            },
            if result.commondir_restored {
                "; commondir restored"
            } else {
                ""
            }
        );
    }
    Ok(())
}

pub(crate) async fn repair_worktrees() -> WorktreeResult<WorktreeRepairOutput> {
    let _registry_lock = acquire_registry_lock_async().await?;
    // The healing loader may itself rewrite the file (v1 upgrade, identity
    // invariants); report that as a change too.
    let bytes_before = fs::read(state_path()).ok();
    let mut state = load_state_for_repair()?;
    let mut changed = fs::read(state_path()).ok() != bytes_before;
    let mut notes = Vec::new();

    let mut seen = HashSet::<PathBuf>::new();
    state.entries.retain(|w| {
        let p = PathBuf::from(&w.path);
        if !seen.insert(p) {
            changed = true;
            false
        } else {
            true
        }
    });

    if ensure_main_entry(&mut state).map_err(|source| WorktreeError::StateRepair { source })? {
        changed = true;
    }
    if normalize_v2_ids(&mut state) {
        changed = true;
    }

    // §C.7 W3-s1b recovery, in dependency order: stale intents first (they
    // may settle an interrupted detach/move/re-attach), then tombstone
    // retries, then marker + lifecycle-mirror reconciliation.
    let db = crate::internal::db::get_db_conn_instance().await;
    let journal_recovered =
        recover_pending_intents(&db, &mut state, &mut changed, &mut notes).await?;
    let (tombstones_cleaned, tombstones_pending) =
        retry_tombstones(&db, &mut state, &mut changed, &mut notes).await;
    reconcile_lifecycle(&db, &mut state, &mut notes).await;

    if changed {
        let _ = normalize_v2_ids(&mut state);
        write_state(&state)?;
    }

    Ok(WorktreeRepairOutput {
        changed,
        journal_recovered,
        tombstones_cleaned,
        tombstones_pending,
        notes,
    })
}

/// Roll every stale intent-journal row forward (or back) deterministically
/// (§C.7). Recovery NEVER deletes directories, and it resolves a row ONLY
/// from a proven state: any ambiguous observation (unreadable path, failed
/// cleanup, unexpected directory combination) KEEPS the row pending with an
/// explanatory note so the next repair retries instead of guessing.
async fn recover_pending_intents(
    db: &sea_orm::DatabaseConnection,
    state: &mut WorktreeState,
    changed: &mut bool,
    notes: &mut Vec<String>,
) -> WorktreeResult<usize> {
    let pending = journal_pending(db)
        .await
        .map_err(WorktreeError::OperationBlocked)?;
    let mut recovered = 0usize;
    for intent in pending {
        let PendingIntent {
            id,
            op,
            worktree_id,
            payload,
        } = intent;
        let mut resolve_row = true;
        match op.as_str() {
            "remove" => {
                let path = payload["path"].as_str().unwrap_or_default().to_string();
                let delete_dir = payload["delete_dir"].as_bool().unwrap_or(false);
                let entry_index = state
                    .entries
                    .iter()
                    .position(|w| w.path == path && !w.is_main);
                if delete_dir {
                    let presence = probe_path(Path::new(&path));
                    if let PathPresence::Unknown(error) = &presence {
                        resolve_row = false;
                        notes.push(format!(
                            "cannot determine whether '{path}' still exists ({error}); \
                              journal kept for the next repair"
                        ));
                    }
                    if matches!(presence, PathPresence::Missing) {
                        // Deletion happened; finish the cleanup.
                        if let Some(idx) = entry_index {
                            let entry_id = state.entries[idx].worktree_id.clone();
                            if let Some(id_str) = entry_id.as_deref().or(worktree_id.as_deref()) {
                                match gc_worktree_scoped_rows_strict(db, id_str, true).await {
                                    Ok(()) => {
                                        let _ = lifecycle_delete(db, id_str).await;
                                        state.entries.remove(idx);
                                        *changed = true;
                                        notes.push(format!(
                                            "completed interrupted remove of '{path}'"
                                        ));
                                    }
                                    Err(error) => {
                                        state.entries[idx].state = WorktreeEntryState::Tombstone;
                                        if let Err(mirror_error) = lifecycle_upsert(
                                            db,
                                            id_str,
                                            WorktreeEntryState::Tombstone.as_str(),
                                            &path,
                                        )
                                        .await
                                        {
                                            resolve_row = false;
                                            notes.push(format!(
                                                "tombstone mirror write for '{path}' failed \
                                                  ({mirror_error}); journal kept for the \
                                                  next repair"
                                            ));
                                        }
                                        *changed = true;
                                        notes.push(format!(
                                            "remove of '{path}' left a tombstone (cleanup \
                                              failed again: {error})"
                                        ));
                                    }
                                }
                            }
                        } else if let Some(id_str) = worktree_id.as_deref() {
                            match gc_worktree_scoped_rows_strict(db, id_str, true).await {
                                Ok(()) => {
                                    let _ = lifecycle_delete(db, id_str).await;
                                }
                                Err(error) => {
                                    resolve_row = false;
                                    notes.push(format!(
                                        "scoped cleanup for the removed '{path}' failed \
                                          ({error}); journal kept for the next repair"
                                    ));
                                }
                            }
                        }
                    } else if matches!(presence, PathPresence::Present) {
                        // Deletion never completed. NON-DESTRUCTIVE roll
                        // back: restore the detached marker if this entry
                        // was detached (the dirty-check window lifts it).
                        if let Some(idx) = entry_index
                            && state.entries[idx].state == WorktreeEntryState::DetachedFromRegistry
                            && let Some(id_str) = state.entries[idx].worktree_id.clone().as_deref()
                            && let Err(error) = write_detached_marker(Path::new(&path), id_str)
                        {
                            resolve_row = false;
                            notes.push(format!(
                                "could not re-freeze detached '{path}' ({error}); journal \
                                  kept for the next repair"
                            ));
                        }
                        notes.push(format!(
                            "interrupted `remove --delete-dir` of '{path}' rolled back \
                              (directory still present; nothing was deleted by repair)"
                        ));
                    }
                } else {
                    // Keep-dir detach: roll FORWARD to the detached state —
                    // unless the still-unfrozen worktree started NEW work
                    // between the crash and this repair. remove itself
                    // refuses on active sequencer/sidecar state; recovery
                    // honoring less would strand that work behind the
                    // fail-closed marker. The user reruns remove when done.
                    if let Some(idx) = entry_index {
                        let entry_id = state.entries[idx].worktree_id.clone();
                        let active_now = match entry_id.as_deref().or(worktree_id.as_deref()) {
                            Some(id_str) => scoped_state_active(db, id_str).await,
                            None => false,
                        };
                        if active_now {
                            resolve_row = false;
                            notes.push(format!(
                                "interrupted detach of '{path}' NOT rolled forward: the \
                                  worktree has in-progress sequencer/bisect state that \
                                  started after the crash; finish or abort it, rerun \
                                  `libra worktree remove {path}`, then repair"
                            ));
                        } else if let Some(id_str) = entry_id.as_deref().or(worktree_id.as_deref())
                        {
                            if let Err(error) = lifecycle_upsert(
                                db,
                                id_str,
                                WorktreeEntryState::DetachedFromRegistry.as_str(),
                                &path,
                            )
                            .await
                            {
                                resolve_row = false;
                                notes.push(format!(
                                    "lifecycle mirror write for '{path}' failed ({error}); \
                                      journal kept for the next repair"
                                ));
                            }
                            if let Err(error) = write_detached_marker(Path::new(&path), id_str) {
                                resolve_row = false;
                                notes.push(format!(
                                    "could not freeze detached '{path}' ({error}); journal \
                                      kept for the next repair"
                                ));
                            }
                        }
                        if !active_now {
                            if state.entries[idx].state != WorktreeEntryState::DetachedFromRegistry
                            {
                                state.entries[idx].state = WorktreeEntryState::DetachedFromRegistry;
                                *changed = true;
                            }
                            notes.push(format!("completed interrupted detach of '{path}'"));
                        }
                    }
                }
            }
            "add" => {
                let path = payload["path"].as_str().unwrap_or_default().to_string();
                if payload["reattach"].as_bool().unwrap_or(false) {
                    // Finish a crashed re-attach ONLY for an entry already
                    // PUBLISHED as Active (the crash sat between the
                    // registry write and the marker removal). A still-
                    // detached entry is NOT rolled forward: linked ids are
                    // deterministic (path-derived), so this journal could
                    // predate a delete/re-add/re-detach cycle at the same
                    // path — unfreezing would betray that LATER detach. It
                    // stays frozen (rerun `worktree add` to re-attach) and
                    // the row resolves as rolled back.
                    if let Some(idx) = state.entries.iter().position(|w| w.path == path) {
                        match state.entries[idx].state {
                            WorktreeEntryState::Active
                                if gitdir_identity_at(&path)
                                    != state.entries[idx].worktree_id.clone() =>
                            {
                                // The directory no longer carries THIS
                                // entry's identity — it was replaced after
                                // the crash. Lifting the marker would
                                // unfreeze a stranger; reconcile_lifecycle
                                // applies the same rule.
                                resolve_row = false;
                                notes.push(format!(
                                    "re-attach of '{path}' not completed: the directory's \
                                      identity no longer matches the registry entry; \
                                      journal kept — investigate, then rerun repair"
                                ));
                            }
                            WorktreeEntryState::Active => {
                                let marker =
                                    Path::new(&path).join(util::ROOT_DIR).join(DETACHED_MARKER);
                                match fs::remove_file(&marker) {
                                    Ok(()) => {}
                                    Err(error) if error.kind() == io::ErrorKind::NotFound => {}
                                    Err(error) => {
                                        resolve_row = false;
                                        notes.push(format!(
                                            "could not lift the marker while completing \
                                              the re-attach of '{path}' ({error}); journal \
                                              kept for the next repair"
                                        ));
                                    }
                                }
                                if resolve_row {
                                    if let Some(id_str) =
                                        state.entries[idx].worktree_id.clone().as_deref()
                                    {
                                        let _ = lifecycle_delete(db, id_str).await;
                                    }
                                    notes.push(format!(
                                        "completed interrupted re-attach of '{path}'"
                                    ));
                                }
                            }
                            WorktreeEntryState::DetachedFromRegistry => {
                                notes.push(format!(
                                    "stale re-attach intent for '{path}' rolled back — \
                                      the entry is (still or again) detached and stays \
                                      frozen; rerun `libra worktree add {path}` to \
                                      re-attach it"
                                ));
                            }
                            WorktreeEntryState::Tombstone => {
                                notes.push(format!(
                                    "stale re-attach intent for '{path}' resolved — the \
                                      entry is now a tombstone"
                                ));
                            }
                        }
                    }
                    if *changed {
                        write_state(state)?;
                    }
                    if resolve_row {
                        journal_resolve(db, id)
                            .await
                            .map_err(WorktreeError::OperationBlocked)?;
                        recovered += 1;
                    }
                    continue;
                }
                let registered = state.entries.iter().any(|w| w.path == path);
                if !registered
                    && let Some(spec) = payload.get("create_branch")
                    && let Some(name) = spec["name"].as_str()
                {
                    // The `-b` branch may have been created before the
                    // crash: roll it back tip-conditionally under the
                    // attach lock — a moved tip means someone committed on
                    // it, which is not ours to delete (journal kept).
                    match spec["start"]
                        .as_str()
                        .and_then(|raw| raw.parse::<git_internal::hash::ObjectHash>().ok())
                    {
                        Some(start) => {
                            // FAIL CLOSED on lock failure: without the
                            // attach lock a concurrent scope could be
                            // attaching this branch right now. And never
                            // delete a branch ANY scope has attached, even
                            // at the original tip.
                            match util::acquire_branch_attach_lock() {
                                Err(error) => {
                                    resolve_row = false;
                                    notes.push(format!(
                                        "cannot acquire the branch-attach lock to roll \
                                          back branch '{name}' ({error}); journal kept for \
                                          the next repair"
                                    ));
                                }
                                Ok(_attach_lock) => {
                                    // Result-returning probe over EVERY
                                    // scope: a read failure fails CLOSED
                                    // (no delete, journal kept) — never
                                    // "could not check, so not attached".
                                    match Head::branch_checked_out_anywhere_result(name).await {
                                        Err(error) => {
                                            resolve_row = false;
                                            notes.push(format!(
                                                "cannot verify whether branch '{name}' is \
                                                  attached ({error}); not deleting it — \
                                                  journal kept for the next repair"
                                            ));
                                        }
                                        Ok(Some(scope)) => {
                                            resolve_row = false;
                                            notes.push(format!(
                                                "branch '{name}' from an interrupted \
                                                  `worktree add -b` is checked out at \
                                                  worktree '{scope}'; not deleting it — \
                                                  journal kept, resolve manually"
                                            ));
                                        }
                                        Ok(None) => {
                                            match Branch::delete_branch_if_tip_result(name, &start)
                                             .await
                                         {
                                 Ok(crate::internal::branch::ConditionalDeleteOutcome::Deleted) => {
                                     notes.push(format!(
                                         "rolled back branch '{name}' from an interrupted \
                                          `worktree add -b`"
                                     ));
                                 }
                                 Ok(crate::internal::branch::ConditionalDeleteOutcome::NotFound) => {
                                 }
                                 Ok(crate::internal::branch::ConditionalDeleteOutcome::TipMoved) => {
                                     resolve_row = false;
                                     notes.push(format!(
                                         "branch '{name}' from an interrupted `worktree add \
                                          -b` has NEW commits; not deleting it — journal \
                                          kept, resolve manually"
                                     ));
                                 }
                                 Err(error) => {
                                     resolve_row = false;
                                     notes.push(format!(
                                         "could not roll back branch '{name}' ({error}); \
                                          journal kept for the next repair"
                                     ));
                                 }
                                         }
                                        }
                                    }
                                }
                            }
                        }
                        None => {
                            resolve_row = false;
                            notes.push(format!(
                                "interrupted `worktree add -b {name}' journal has an \
                                  unparsable start tip; journal kept — resolve manually"
                            ));
                        }
                    }
                }
                if !registered && let Some(id_str) = worktree_id.as_deref() {
                    // The add never published; sweep the scope it may have
                    // seeded. The half-created directory (if any) is left
                    // for the user — recovery never deletes directories.
                    match gc_worktree_scoped_rows_strict(db, id_str, true).await {
                        Ok(()) => {
                            let _ = lifecycle_delete(db, id_str).await;
                            notes.push(format!(
                                "rolled back interrupted add of '{path}' (scoped rows \
                                  swept; any partial directory was left in place)"
                            ));
                        }
                        Err(error) => {
                            resolve_row = false;
                            notes.push(format!(
                                "sweep for the unpublished add of '{path}' failed \
                                  ({error}); journal kept for the next repair"
                            ));
                        }
                    }
                }
            }
            "move" => {
                let src = payload["src"].as_str().unwrap_or_default().to_string();
                let dest = payload["dest"].as_str().unwrap_or_default().to_string();
                let src_presence = probe_path(Path::new(&src));
                let dest_presence = probe_path(Path::new(&dest));
                if let PathPresence::Unknown(error) = &src_presence {
                    resolve_row = false;
                    notes.push(format!(
                        "cannot determine whether '{src}' still exists ({error}); journal \
                          kept for the next repair"
                    ));
                }
                if let PathPresence::Unknown(error) = &dest_presence {
                    resolve_row = false;
                    notes.push(format!(
                        "cannot determine whether '{dest}' exists ({error}); journal kept \
                          for the next repair"
                    ));
                }
                let src_exists = matches!(src_presence, PathPresence::Present);
                let dest_missing = matches!(dest_presence, PathPresence::Missing);
                let dest_exists = matches!(dest_presence, PathPresence::Present);
                let src_missing = matches!(src_presence, PathPresence::Missing);
                // Bind the intent to ITS worktree via the journal's persisted
                // id — matching by path could adopt an unrelated entry that
                // later came to occupy the source/destination (e.g. a
                // tombstone), and rename a stranger's directory. Anything
                // that does not line up EXACTLY is ambiguous: keep the row.
                let expected_id = worktree_id.as_deref();
                let entry_of_intent = expected_id.and_then(|journal_id| {
                    state
                        .entries
                        .iter()
                        .position(|w| w.worktree_id.as_deref() == Some(journal_id))
                });
                let dest_taken_by_other = state
                    .entries
                    .iter()
                    .any(|w| w.path == dest && w.worktree_id.as_deref() != expected_id);
                let src_taken_by_other = state
                    .entries
                    .iter()
                    .any(|w| w.path == src && w.worktree_id.as_deref() != expected_id);
                if resolve_row {
                    match entry_of_intent {
                        _ if expected_id.is_none() => {
                            resolve_row = false;
                            notes.push(format!(
                                "interrupted move '{src}' -> '{dest}' carries no worktree \
                                  id; journal kept — investigate manually"
                            ));
                        }
                        _ if dest_taken_by_other || src_taken_by_other => {
                            resolve_row = false;
                            notes.push(format!(
                                "interrupted move '{src}' -> '{dest}': another registry \
                                  entry now occupies one of the paths; journal kept — \
                                  resolve manually, then rerun repair"
                            ));
                        }
                        Some(idx) if state.entries[idx].path == dest => {
                            if src_exists
                                && dest_missing
                                && gitdir_identity_at(&src) != expected_id.map(str::to_string)
                            {
                                // Whatever occupies `src` is NOT this
                                // intent's worktree (no gitdir, or another
                                // identity) — renaming it would relocate a
                                // stranger's material. Keep the journal.
                                resolve_row = false;
                                notes.push(format!(
                                    "interrupted move '{src}' -> '{dest}': the directory \
                                      at the source no longer carries this worktree's \
                                      identity; journal kept — resolve manually, then \
                                      rerun repair"
                                ));
                            } else if src_exists && dest_missing {
                                // Registry updated, rename never happened:
                                // finish it (or roll the registry back).
                                match fs::rename(&src, &dest) {
                                    Ok(()) => {
                                        notes.push(format!(
                                            "completed interrupted move '{src}' -> '{dest}'"
                                        ));
                                    }
                                    Err(error) => {
                                        state.entries[idx].path = src.clone();
                                        *changed = true;
                                        notes.push(format!(
                                            "rolled back interrupted move '{src}' -> \
                                              '{dest}' (rename failed: {error})"
                                        ));
                                    }
                                }
                            } else if src_missing && dest_exists {
                                notes.push(format!(
                                    "interrupted move '{src}' -> '{dest}' was already \
                                      complete"
                                ));
                            } else {
                                resolve_row = false;
                                notes.push(format!(
                                    "interrupted move '{src}' -> '{dest}' is ambiguous \
                                      (src present: {src_exists}, dest present: \
                                      {dest_exists}); journal kept — resolve the \
                                      directories manually, then rerun repair"
                                ));
                            }
                        }
                        Some(idx) if state.entries[idx].path == src => {
                            if src_missing
                                && dest_exists
                                && gitdir_identity_at(&dest) != expected_id.map(str::to_string)
                            {
                                resolve_row = false;
                                notes.push(format!(
                                    "interrupted move '{src}' -> '{dest}': the directory \
                                      at the destination does not carry this worktree's \
                                      identity; journal kept — resolve manually, then \
                                      rerun repair"
                                ));
                            } else if src_missing && dest_exists {
                                // Directory moved but the registry write was
                                // lost: finish it.
                                state.entries[idx].path = dest.clone();
                                *changed = true;
                                notes.push(format!(
                                    "finished registry update for interrupted move \
                                      '{src}' -> '{dest}'"
                                ));
                            } else if src_exists && dest_missing {
                                notes.push(format!(
                                    "interrupted move '{src}' -> '{dest}' never started; \
                                      nothing to do"
                                ));
                            } else {
                                resolve_row = false;
                                notes.push(format!(
                                    "interrupted move '{src}' -> '{dest}' is ambiguous \
                                      (src present: {src_exists}, dest present: \
                                      {dest_exists}); journal kept — resolve the \
                                      directories manually, then rerun repair"
                                ));
                            }
                        }
                        Some(idx) => {
                            resolve_row = false;
                            let elsewhere = state.entries[idx].path.clone();
                            notes.push(format!(
                                "interrupted move '{src}' -> '{dest}': its worktree is \
                                  now registered at '{elsewhere}'; journal kept — \
                                  investigate manually"
                            ));
                        }
                        None => {
                            resolve_row = false;
                            notes.push(format!(
                                "interrupted move '{src}' -> '{dest}': no registry entry \
                                  carries its worktree id; journal kept — investigate \
                                  manually, then rerun repair"
                            ));
                        }
                    }
                }
            }
            "prune" => {
                if let Some(paths) = payload["paths"].as_array() {
                    for value in paths {
                        let path = value.as_str().unwrap_or_default().to_string();
                        if state.entries.iter().any(|w| w.path == path && !w.is_main)
                            && let PathPresence::Unknown(error) = probe_path(Path::new(&path))
                        {
                            resolve_row = false;
                            notes.push(format!(
                                "cannot determine whether '{path}' still exists ({error}); \
                                  journal kept for the next repair"
                            ));
                            continue;
                        }
                        if let Some(idx) = state.entries.iter().position(|w| {
                            w.path == path
                                && !w.is_main
                                && matches!(probe_path(Path::new(&w.path)), PathPresence::Missing)
                        }) {
                            let entry_id = state.entries[idx].worktree_id.clone();
                            let cleaned = if let Some(id_str) = entry_id.as_deref() {
                                match gc_worktree_scoped_rows_strict(db, id_str, true).await {
                                    Ok(()) => {
                                        let _ = lifecycle_delete(db, id_str).await;
                                        true
                                    }
                                    Err(error) => {
                                        state.entries[idx].state = WorktreeEntryState::Tombstone;
                                        if let Err(mirror_error) = lifecycle_upsert(
                                            db,
                                            id_str,
                                            WorktreeEntryState::Tombstone.as_str(),
                                            &path,
                                        )
                                        .await
                                        {
                                            resolve_row = false;
                                            notes.push(format!(
                                                "tombstone mirror write for '{path}' failed \
                                                  ({mirror_error}); journal kept for the \
                                                  next repair"
                                            ));
                                        }
                                        *changed = true;
                                        notes.push(format!(
                                            "prune of '{path}' left a tombstone (cleanup \
                                              failed: {error})"
                                        ));
                                        false
                                    }
                                }
                            } else {
                                true
                            };
                            if cleaned {
                                state.entries.remove(idx);
                                *changed = true;
                                notes.push(format!("completed interrupted prune of '{path}'"));
                            }
                        }
                    }
                }
            }
            "migrate" => {
                // §C.6.2 step 5 recovery: decide by IDENTITY (no-follow
                // symlink target, our own journal-stamped marker), never by
                // bare existence. Ambiguity keeps the row.
                let path = PathBuf::from(payload["path"].as_str().unwrap_or_default());
                let head = payload["head"]
                    .as_str()
                    .and_then(|raw| raw.parse::<git_internal::hash::ObjectHash>().ok());
                let gitdir = path.join(util::ROOT_DIR);
                let prepared = path.join(format!(".libra.migrate-{id}"));
                let backup = path.join(format!(".libra.legacy-backup-{id}"));
                let storage = util::storage_path();
                let canonical_storage =
                    fs::canonicalize(&storage).unwrap_or_else(|_| storage.clone());
                let is_our_marker = |dir: &Path| {
                    fs::read_to_string(dir.join("migrate-marker"))
                        .is_ok_and(|raw| raw.trim() == format!("journal {id}"))
                };
                let is_legacy_link = |candidate: &Path| {
                    fs::symlink_metadata(candidate)
                        .map(|meta| meta.file_type().is_symlink())
                        .unwrap_or(false)
                        && fs::canonicalize(candidate).ok().as_deref()
                            == Some(canonical_storage.as_path())
                };
                let gitdir_meta = fs::symlink_metadata(&gitdir);
                if is_legacy_link(&gitdir) {
                    // Pre-backup crash: the legacy link is untouched. Roll
                    // BACK — drop only OUR (marker-verified) prepared dir;
                    // any unexpected artifact or a failed removal keeps the
                    // journal (nothing is guessed or force-deleted).
                    match fs::symlink_metadata(&prepared) {
                        Err(error) if error.kind() == io::ErrorKind::NotFound => {
                            if is_legacy_link(&backup)
                                && let Err(remove_error) = fs::remove_file(&backup)
                            {
                                resolve_row = false;
                                notes.push(format!(
                                    "cannot remove the backup link '{}' ({remove_error}); \
                                      journal kept",
                                    backup.display()
                                ));
                            } else {
                                notes.push(format!(
                                    "rolled back interrupted layout migration of '{}' \
                                      (legacy link untouched)",
                                    path.display()
                                ));
                            }
                        }
                        Ok(meta) if meta.is_dir() && is_our_marker(&prepared) => {
                            let mut backup_leaked = false;
                            if is_legacy_link(&backup)
                                && let Err(remove_error) = fs::remove_file(&backup)
                            {
                                backup_leaked = true;
                                resolve_row = false;
                                notes.push(format!(
                                    "cannot remove the backup link '{}' ({remove_error}); \
                                      journal kept",
                                    backup.display()
                                ));
                            }
                            if backup_leaked {
                                // Keep the prepared dir too: with the journal
                                // pending, the next repair retries both.
                            } else if let Err(error) = fs::remove_dir_all(&prepared) {
                                resolve_row = false;
                                notes.push(format!(
                                    "cannot remove the prepared dir '{}' ({error}); \
                                      journal kept",
                                    prepared.display()
                                ));
                            } else {
                                notes.push(format!(
                                    "rolled back interrupted layout migration of '{}' \
                                      (legacy link untouched)",
                                    path.display()
                                ));
                            }
                        }
                        Ok(meta)
                            if meta.is_dir() && prepared_dir_is_settled_leftover(&prepared) =>
                        {
                            // Marker-less but IDENTIFIED: the directory name
                            // embeds this exact journal's id (nothing else
                            // creates that name), and it holds at most the
                            // three files the engine writes — a crash before
                            // the marker landed, or mid-rollback after the
                            // marker was already removed. Both roll back.
                            let mut backup_leaked = false;
                            if is_legacy_link(&backup)
                                && let Err(remove_error) = fs::remove_file(&backup)
                            {
                                backup_leaked = true;
                                resolve_row = false;
                                notes.push(format!(
                                    "cannot remove the backup link '{}' ({remove_error}); \
                                      journal kept",
                                    backup.display()
                                ));
                            }
                            if backup_leaked {
                                // Keep the prepared dir too: with the journal
                                // pending, the next repair retries both.
                            } else if let Err(error) = fs::remove_dir_all(&prepared) {
                                resolve_row = false;
                                notes.push(format!(
                                    "cannot remove the prepared dir '{}' ({error}); \
                                      journal kept",
                                    prepared.display()
                                ));
                            } else {
                                notes.push(format!(
                                    "rolled back interrupted layout migration of '{}' \
                                      (legacy link untouched)",
                                    path.display()
                                ));
                            }
                        }
                        _ => {
                            resolve_row = false;
                            notes.push(format!(
                                "prepared artifact '{}' does not carry this journal's \
                                  marker; journal kept — investigate manually, nothing \
                                  was deleted",
                                prepared.display()
                            ));
                        }
                    }
                } else if matches!(&gitdir_meta, Err(e) if e.kind() == io::ErrorKind::NotFound)
                    && is_legacy_link(&backup)
                    && no_follow_real_dir(&prepared)
                    && is_our_marker(&prepared)
                {
                    // Between 5a and 5b: install ours, then finish.
                    if let Err(error) = fs::rename(&prepared, &gitdir) {
                        resolve_row = false;
                        notes.push(format!(
                            "cannot finish installing the migrated gitdir for '{}' \
                              ({error}); journal kept",
                            path.display()
                        ));
                    } else {
                        fsync_parent_best_effort(&gitdir);
                        if let Err(error) = validate_installed_gitdir(
                            &gitdir,
                            id,
                            worktree_id.as_deref().unwrap_or_default(),
                            &canonical_storage,
                        ) {
                            // `continue` skips the shared tail, so the
                            // journal row stays pending by construction.
                            notes.push(format!(
                                "installed gitdir for '{}' failed identity validation \
                                  ({error}); journal kept — investigate manually",
                                path.display()
                            ));
                            if *changed {
                                write_state(state)?;
                            }
                            continue;
                        }
                        let _gate_bypass = util::bypass_migration_gate();
                        match finish_migration_recovery(&path, &backup, head).await {
                            Ok(()) => {
                                if *changed {
                                    write_state(state)?;
                                }
                                journal_resolve(db, id)
                                    .await
                                    .map_err(WorktreeError::OperationBlocked)?;
                                let _ = fs::remove_file(
                                    path.join(util::ROOT_DIR).join("migrate-marker"),
                                );
                                recovered += 1;
                                notes.push(format!(
                                    "completed interrupted layout migration of '{}'",
                                    path.display()
                                ));
                                continue;
                            }
                            Err(error) => {
                                resolve_row = false;
                                notes.push(format!(
                                    "layout migration of '{}' still incomplete ({error}); \
                                      journal kept",
                                    path.display()
                                ));
                            }
                        }
                    }
                } else if validate_installed_gitdir(
                    &gitdir,
                    id,
                    worktree_id.as_deref().unwrap_or_default(),
                    &canonical_storage,
                )
                .is_ok()
                {
                    // ONLY the journal-stamped marker may adopt an install:
                    // ids are path-derived, so a pruned/re-added worktree
                    // at the same path would satisfy a commondir/id match
                    // and be silently re-seeded from the stale snapshot.
                    // (The resolve-before-marker ordering guarantees a
                    // marker-less install never has a live journal.)
                    // Installed; finish seed/verify/cleanup idempotently.
                    // Journal resolves BEFORE the marker is lifted, so a
                    // crash in between still leaves a recognizable install.
                    let _gate_bypass = util::bypass_migration_gate();
                    match finish_migration_recovery(&path, &backup, head).await {
                        Ok(()) => {
                            if *changed {
                                write_state(state)?;
                            }
                            journal_resolve(db, id)
                                .await
                                .map_err(WorktreeError::OperationBlocked)?;
                            let _ =
                                fs::remove_file(path.join(util::ROOT_DIR).join("migrate-marker"));
                            recovered += 1;
                            notes.push(format!(
                                "completed interrupted layout migration of '{}'",
                                path.display()
                            ));
                            continue;
                        }
                        Err(error) => {
                            resolve_row = false;
                            notes.push(format!(
                                "layout migration of '{}' still incomplete ({error}); \
                                  journal kept",
                                path.display()
                            ));
                        }
                    }
                } else {
                    resolve_row = false;
                    notes.push(format!(
                        "interrupted layout migration of '{}': on-disk state matches no \
                          known stage (identity check failed); journal kept — investigate \
                          manually, nothing was deleted",
                        path.display()
                    ));
                }
            }
            other => {
                // FAIL CLOSED: an op this binary does not know was written
                // by a NEWER binary — resolving (deleting) it would destroy
                // that binary's crash-recovery anchor. Keep it pending.
                resolve_row = false;
                notes.push(format!(
                    "unknown intent op '{other}' (id {id}); journal kept — rerun repair \
                      with the binary that recorded it"
                ));
            }
        }
        // Persist the registry BEFORE resolving the intent: a crash after
        // the resolve with only in-memory registry changes would silently
        // lose the recovery (e.g. a reattach unfrozen with a still-detached
        // persisted entry).
        if *changed {
            write_state(state)?;
        }
        if resolve_row {
            journal_resolve(db, id)
                .await
                .map_err(WorktreeError::OperationBlocked)?;
            recovered += 1;
        }
    }
    Ok(recovered)
}

/// Finish an installed-but-unfinished layout migration: (re-)seed the
/// scoped HEAD + private index at the journaled snapshot, verify, and drop
/// the identity-checked backup link. Idempotent — every step overwrites or
/// re-checks rather than assuming.
async fn finish_migration_recovery(
    target: &Path,
    backup: &Path,
    head: Option<git_internal::hash::ObjectHash>,
) -> Result<(), String> {
    let Some(head_commit) = head else {
        return Err("journal carries no parsable HEAD snapshot".to_string());
    };
    seed_migrated_worktree(target, head_commit)
        .await
        .map_err(|e| e.into_cli_error().to_string())?;
    verify_migrated_worktree(target, head_commit)
        .await
        .map_err(|e| e.into_cli_error().to_string())?;
    let storage = util::storage_path();
    let canonical_storage = fs::canonicalize(&storage).unwrap_or(storage);
    match fs::symlink_metadata(backup) {
        Ok(meta) if meta.file_type().is_symlink() => {
            // Only the EXPECTED legacy link (resolving to this repo's
            // storage) may be deleted; a foreign symlink is kept.
            if fs::canonicalize(backup).ok().as_deref() != Some(canonical_storage.as_path()) {
                return Err(format!(
                    "backup '{}' does not resolve to this repository's storage; not \
                      touching it",
                    backup.display()
                ));
            }
            fs::remove_file(backup).map_err(|e| format!("cannot remove the backup: {e}"))?;
        }
        Ok(_) => {
            return Err(format!(
                "backup '{}' is not the expected symlink; not touching it",
                backup.display()
            ));
        }
        Err(error) if error.kind() == io::ErrorKind::NotFound => {}
        Err(error) => return Err(format!("cannot inspect the backup: {error}")),
    }
    Ok(())
}

/// Retry the scoped cleanup of every tombstone entry whose directory is
/// still proven gone; a recreated directory is reported, never adopted.
async fn retry_tombstones(
    db: &sea_orm::DatabaseConnection,
    state: &mut WorktreeState,
    changed: &mut bool,
    notes: &mut Vec<String>,
) -> (usize, usize) {
    let mut cleaned = 0usize;
    let mut pending = 0usize;
    let mut index = 0usize;
    while index < state.entries.len() {
        if state.entries[index].state != WorktreeEntryState::Tombstone {
            index += 1;
            continue;
        }
        let path = state.entries[index].path.clone();
        let dir_missing = matches!(
            fs::symlink_metadata(Path::new(&path)),
            Err(ref error) if error.kind() == io::ErrorKind::NotFound
        );
        if !dir_missing {
            pending += 1;
            notes.push(format!(
                "tombstone '{path}': a directory now exists at that path; not adopting it \
                  — remove or rename it, then rerun repair"
            ));
            index += 1;
            continue;
        }
        let Some(id) = state.entries[index].worktree_id.clone() else {
            pending += 1;
            index += 1;
            continue;
        };
        match gc_worktree_scoped_rows_strict(db, &id, true).await {
            Ok(()) => {
                let _ = lifecycle_delete(db, &id).await;
                state.entries.remove(index);
                *changed = true;
                cleaned += 1;
                notes.push(format!("tombstone '{path}': scoped cleanup completed"));
            }
            Err(error) => {
                pending += 1;
                notes.push(format!(
                    "tombstone '{path}': scoped cleanup failed again ({error}); will retry \
                      on the next repair"
                ));
                index += 1;
            }
        }
    }
    (cleaned, pending)
}

/// Make every lifecycle artifact consistent with the registry (the
/// authority): detached entries get their fail-closed marker and SQL
/// mirror row rewritten; ACTIVE entries whose gitdir still carries a stale
/// marker (a crashed re-attach) have it lifted when the identity matches;
/// mirror rows whose entry is gone or active are deleted so they cannot
/// block the down migration forever. Idempotent.
async fn reconcile_lifecycle(
    db: &sea_orm::DatabaseConnection,
    state: &mut WorktreeState,
    notes: &mut Vec<String>,
) {
    for entry in &state.entries {
        let Some(id) = entry.worktree_id.as_deref() else {
            continue;
        };
        let marker = Path::new(&entry.path)
            .join(util::ROOT_DIR)
            .join(DETACHED_MARKER);
        match entry.state {
            WorktreeEntryState::DetachedFromRegistry => {
                // Restore the marker only into a directory that still
                // carries THIS entry's identity — writing it into a
                // recreated stranger directory would fabricate a `.libra`
                // (write_atomic creates parents) and freeze the user's
                // unrelated material. The Active arm below applies the same
                // identity rule when LIFTING a marker.
                if !marker.exists()
                    && Path::new(&entry.path).is_dir()
                    && gitdir_identity_at(&entry.path).as_deref() == Some(id)
                {
                    match write_detached_marker(Path::new(&entry.path), id) {
                        Ok(()) => {
                            notes.push(format!("restored the detached marker for '{}'", entry.path))
                        }
                        Err(error) => notes.push(format!(
                            "FAILED to restore the detached marker for '{}' ({error}); the \
                              directory is NOT frozen — rerun repair after fixing the cause",
                            entry.path
                        )),
                    }
                }
                let _ = lifecycle_upsert(
                    db,
                    id,
                    WorktreeEntryState::DetachedFromRegistry.as_str(),
                    &entry.path,
                )
                .await;
            }
            WorktreeEntryState::Tombstone => {
                let _ =
                    lifecycle_upsert(db, id, WorktreeEntryState::Tombstone.as_str(), &entry.path)
                        .await;
            }
            WorktreeEntryState::Active => {
                if marker.exists() {
                    let gitdir_id = fs::read_to_string(
                        Path::new(&entry.path)
                            .join(util::ROOT_DIR)
                            .join("worktree_id"),
                    )
                    .ok()
                    .map(|value| value.trim().to_string());
                    if gitdir_id.as_deref() == Some(id) {
                        if fs::remove_file(&marker).is_ok() {
                            notes.push(format!(
                                "lifted a stale detached marker from active '{}'",
                                entry.path
                            ));
                        }
                    } else {
                        notes.push(format!(
                            "active '{}' carries a detached marker but its gitdir identity \
                              does not match; leaving it frozen — investigate manually",
                            entry.path
                        ));
                    }
                }
                let _ = lifecycle_delete(db, id).await;
            }
        }
    }

    // Stray migrate-markers (§C.6.2 cleanup retry): a marker whose journal
    // already RESOLVED (no pending migrate row for the path) only freezes a
    // finished worktree — clear it once the install identity is verified
    // (real dir + commondir at this storage). Unverifiable states are
    // reported, never guessed.
    if let Ok(pending) = journal_pending(db).await {
        let storage = util::storage_path();
        let canonical_storage = fs::canonicalize(&storage).unwrap_or(storage);
        for entry in &state.entries {
            let gitdir = Path::new(&entry.path).join(util::ROOT_DIR);
            let marker = gitdir.join("migrate-marker");
            if !marker.exists() {
                continue;
            }
            let has_pending = pending.iter().any(|intent| {
                intent.op == "migrate"
                    && (intent.payload["path"].as_str() == Some(entry.path.as_str())
                        || (intent.worktree_id.is_some()
                            && intent.worktree_id.as_deref() == entry.worktree_id.as_deref()))
            });
            if has_pending {
                continue;
            }
            let commondir_ok = fs::read_to_string(gitdir.join("commondir"))
                .ok()
                .and_then(|raw| raw.lines().next().map(str::trim).map(PathBuf::from))
                .map(|p| {
                    let abs = if p.is_absolute() { p } else { gitdir.join(p) };
                    fs::canonicalize(&abs).unwrap_or(abs)
                })
                .is_some_and(|p| p == canonical_storage);
            if no_follow_real_dir(&gitdir) && commondir_ok {
                match fs::remove_file(&marker) {
                    Ok(()) => notes.push(format!(
                        "cleared a resolved migration marker from '{}'",
                        entry.path
                    )),
                    Err(error) => notes.push(format!(
                        "FAILED to clear the resolved migration marker from '{}' \
                          ({error}); the worktree stays frozen — fix the cause and rerun \
                          repair",
                        entry.path
                    )),
                }
            } else {
                notes.push(format!(
                    "'{}' carries a migration marker with no pending journal but its \
                      install identity cannot be verified; leaving it frozen",
                    entry.path
                ));
            }
        }
    }

    // Sweep mirror rows with no matching non-active entry: they would block
    // the down migration with nothing left to finish.
    if let Ok(rows) = lifecycle_rows(db).await {
        for (row_id, row_state) in rows {
            let matching = state.entries.iter().find(|entry| {
                entry.worktree_id.as_deref() == Some(row_id.as_str()) && !entry.state.is_active()
            });
            if matching.is_none() {
                let _ = lifecycle_delete(db, &row_id).await;
                notes.push(format!(
                    "cleared an orphaned lifecycle row ({row_id}: {row_state})"
                ));
            }
        }
    }
}

pub(crate) fn render_repair_worktrees(
    result: &WorktreeRepairOutput,
    output: &OutputConfig,
) -> CliResult<()> {
    if output.is_json() {
        return emit_json_data("worktree.repair", result, output);
    }
    if !output.quiet {
        for note in &result.notes {
            println!("{note}");
        }
        if result.tombstones_pending > 0 {
            println!(
                "{} tombstone(s) still pending; rerun `libra worktree repair --confirm` \
                  after addressing the notes above",
                result.tombstones_pending
            );
        }
    }
    Ok(())
}

#[cfg(all(test, unix))]
mod tests {
    use tempfile::tempdir;

    use super::*;

    #[test]
    fn registry_parse_accepts_v2_shape() {
        let data = br#"{
             "schema_version": 2,
             "entries": [
                 {"path": "/m", "is_main": true, "locked": false, "lock_reason": null},
                 {"path": "/w", "is_main": false, "locked": false, "lock_reason": null,
                  "worktree_id": "abc123"}
             ]
         }"#;
        let state = WorktreeState::parse(data).expect("v2 parses");
        assert_eq!(state.schema_version, REGISTRY_SCHEMA_VERSION);
        assert_eq!(state.entries.len(), 2);
        assert_eq!(state.entries[0].worktree_id, None);
        assert_eq!(state.entries[1].worktree_id.as_deref(), Some("abc123"));
    }

    #[test]
    fn registry_parse_upgrades_v1_shape_in_memory() {
        let data = br#"{
             "worktrees": [
                 {"path": "/m", "is_main": true, "locked": false, "lock_reason": null},
                 {"path": "/w", "is_main": false, "locked": true, "lock_reason": "keep"}
             ]
         }"#;
        let state = WorktreeState::parse(data).expect("v1 upgrades in memory");
        assert_eq!(state.schema_version, REGISTRY_SCHEMA_VERSION);
        assert_eq!(state.entries.len(), 2);
        // Ids stay unfilled on the read-only path; the durable upgrade
        // (load_state) backfills them.
        assert_eq!(state.entries[1].worktree_id, None);
        assert!(state.entries[1].locked);
        assert_eq!(state.entries[1].lock_reason.as_deref(), Some("keep"));
    }

    /// Fail-closed discrimination: a document carrying any v2 key must be a
    /// fully valid v2 registry — it never falls through to the lenient v1
    /// reader, so a malformed/hybrid file cannot be misread as (and later
    /// rewritten from) a stale embedded `worktrees` array.
    #[test]
    fn registry_parse_refuses_hybrid_and_malformed_v2_shapes() {
        // v2 marker + malformed entries + a plausible legacy array: must NOT
        // fall back to reading the stale v1 array.
        let hybrid = br#"{
             "schema_version": 2,
             "entries": "corrupt",
             "worktrees": [
                 {"path": "/m", "is_main": true, "locked": false, "lock_reason": null}
             ]
         }"#;
        assert!(WorktreeState::parse(hybrid).is_err());

        // Valid v2 alongside a stray legacy array is ambiguous — refused,
        // never silently ignored.
        let ambiguous = br#"{"schema_version": 2, "entries": [], "worktrees": []}"#;
        assert!(WorktreeState::parse(ambiguous).is_err());

        // A v2-marked document with malformed entries and NO legacy array is
        // corrupt, not empty.
        let malformed = br#"{"schema_version": 2, "entries": 7}"#;
        assert!(WorktreeState::parse(malformed).is_err());

        // Neither shape at all.
        assert!(WorktreeState::parse(br#"{}"#).is_err());
        assert!(WorktreeState::parse(br#"[]"#).is_err());
    }

    /// The main-entry invariant applies to v1 documents too: read-side
    /// consumers (service gate, rerere probe, cleanup snapshot) must not
    /// consume a mainless legacy registry as an empty root set.
    #[test]
    fn registry_parse_refuses_mainless_v1_shapes() {
        let empty = br#"{"worktrees": []}"#;
        let err = WorktreeState::parse(empty).expect_err("empty v1 fails closed");
        assert!(err.contains("exactly one main"), "{err}");

        let sole_linked = br#"{
             "worktrees": [
                 {"path": "/w", "is_main": false, "locked": false, "lock_reason": null}
             ]
         }"#;
        assert!(WorktreeState::parse(sole_linked).is_err());
    }

    /// v2 must contain exactly one main entry — zero (or several) mains is
    /// corruption a lockless reader must refuse, not silently re-elect.
    #[test]
    fn registry_parse_requires_exactly_one_main() {
        let zero_main = br#"{
             "schema_version": 2,
             "entries": [
                 {"path": "/w", "is_main": false, "locked": false, "lock_reason": null,
                  "worktree_id": "abc123"}
             ]
         }"#;
        let err = WorktreeState::parse(zero_main).expect_err("zero mains fail closed");
        assert!(err.contains("exactly one main"), "{err}");

        let two_mains = br#"{
             "schema_version": 2,
             "entries": [
                 {"path": "/m", "is_main": true, "locked": false, "lock_reason": null},
                 {"path": "/n", "is_main": true, "locked": false, "lock_reason": null}
             ]
         }"#;
        let err = WorktreeState::parse(two_mains).expect_err("two mains fail closed");
        assert!(err.contains("exactly one main"), "{err}");
    }

    /// v2 identity invariants: the registry is the persisted identity
    /// authority — a linked entry with no id (or a main entry with one) is
    /// corruption and must be refused, never patched from the gitdir.
    #[test]
    fn registry_parse_enforces_v2_identity_invariants() {
        let linked_without_id = br#"{
             "schema_version": 2,
             "entries": [
                 {"path": "/m", "is_main": true, "locked": false, "lock_reason": null},
                 {"path": "/w", "is_main": false, "locked": false, "lock_reason": null}
             ]
         }"#;
        let err = WorktreeState::parse(linked_without_id).expect_err("missing id fails closed");
        assert!(err.contains("missing its persisted worktree_id"), "{err}");

        let main_with_id = br#"{
             "schema_version": 2,
             "entries": [
                 {"path": "/m", "is_main": true, "locked": false, "lock_reason": null,
                  "worktree_id": "oops"}
             ]
         }"#;
        let err = WorktreeState::parse(main_with_id).expect_err("main id fails closed");
        assert!(err.contains("must not carry a worktree_id"), "{err}");
    }

    #[test]
    fn registry_parse_refuses_future_schema_version() {
        // 3 is CURRENT since the service-fence generations (migration
        // 2026073005); 4 is the future one. A version this binary does not know
        // is refused rather than reinterpreted — the whole point of the field.
        let data = br#"{"schema_version": 4, "entries": []}"#;
        let err = WorktreeState::parse(data).expect_err("future version fails closed");
        assert!(err.contains("schema_version 4"), "{err}");

        // The refusal is about the VERSION, not the shape: the same document
        // at the current version gets past it and fails on the main-entry
        // invariant instead.
        let current = br#"{"schema_version": 3, "entries": []}"#;
        let err = WorktreeState::parse(current).expect_err("no main entry");
        assert!(
            !err.contains("schema_version"),
            "the current version is not refused for its version: {err}"
        );

        // v2 is still READ and promoted in memory — its entries simply carry
        // no service-fence generations yet.
        let v2 = br#"{"schema_version": 2, "entries":
             [{"path": "/w", "is_main": true, "locked": false}]}"#;
        let parsed = WorktreeState::parse(v2).expect("a v2 registry is read");
        assert_eq!(
            parsed.schema_version, REGISTRY_SCHEMA_VERSION,
            "and is promoted to the current version in memory"
        );
        assert_eq!(
            parsed.linked_history,
            LinkedHistory::Unknown,
            "a pre-v3 registry cannot say its history was `never` (§C.4.3)"
        );
    }

    /// The second belt (§C.7): a v1 binary's `{ worktrees: [...] }` parser
    /// must FAIL on v2 bytes (renamed top-level key) instead of silently
    /// reading an empty registry and rewriting the file.
    #[test]
    fn v1_parser_fails_on_v2_bytes() {
        let v2 = serde_json::to_vec(&WorktreeState {
            epoch_counter: 0,
            linked_history: LinkedHistory::Never,
            schema_version: REGISTRY_SCHEMA_VERSION,
            entries: vec![WorktreeEntry {
                epoch: 0,
                path: "/w".to_string(),
                is_main: false,
                locked: false,
                lock_reason: None,
                worktree_id: Some("abc123".to_string()),
                state: WorktreeEntryState::Active,
            }],
        })
        .expect("serialize v2");
        assert!(serde_json::from_slice::<WorktreeStateV1>(&v2).is_err());
    }

    #[test]
    fn umount_fuse_path_cleans_task_worktree_root_without_repo() {
        let temp = tempdir().expect("create temp dir");
        let cleanup_root = temp
            .path()
            .join("libra-task-worktree-fuse-29353-019ddec6-de60-7383");
        let workspace = cleanup_root.join("workspace");
        fs::create_dir_all(&workspace).expect("create task workspace");
        let canonical_cleanup_root = cleanup_root.canonicalize().expect("canonical cleanup root");
        let canonical_workspace = workspace.canonicalize().expect("canonical workspace");

        let output = umount_fuse_path(cleanup_root.to_string_lossy().to_string(), true)
            .expect("umount cleanup should succeed for inactive task workspace");

        assert_eq!(
            output.mountpoint,
            canonical_workspace.to_string_lossy().as_ref()
        );
        assert!(output.unmounted);
        assert!(output.cleanup_requested);
        assert_eq!(
            output.cleanup_root.as_deref(),
            Some(canonical_cleanup_root.to_string_lossy().as_ref())
        );
        assert!(output.cleanup_root_removed);
        assert!(!cleanup_root.exists());
    }

    /// W0 §C.11: a failure to CLOSE the audit boundary is a command failure —
    /// warn-and-continue would leave a `running` row while reporting a
    /// successful repair, the audit gap the boundary exists to exclude. Fault
    /// injection: the operation table is dropped between begin and finish, so
    /// the close cannot write its outcome.
    #[tokio::test]
    async fn finish_repair_operation_surfaces_close_failure() {
        use sea_orm::{ConnectionTrait, Database, DbBackend, Statement};

        use crate::{
            internal::operation_wrapper::{
                OperationMeta, OperationScope, begin_operation_with_conn,
            },
            utils::test::{ChangeDirGuard, setup_with_new_libra_in},
        };

        let repo = tempfile::tempdir().expect("tempdir");
        setup_with_new_libra_in(repo.path()).await;
        let _cwd = ChangeDirGuard::new(repo.path());

        let db = Database::connect("sqlite::memory:")
            .await
            .expect("in-memory db");
        for sql in [
            "CREATE TABLE legacy_operation(op_id TEXT PRIMARY KEY,repo_id TEXT NOT NULL,view_id TEXT NOT NULL,command_name TEXT NOT NULL,description TEXT NOT NULL,actor TEXT NOT NULL,args_digest TEXT,start_ts INTEGER NOT NULL,end_ts INTEGER,status TEXT NOT NULL,worktree_id TEXT NOT NULL DEFAULT '',scope_provenance TEXT NOT NULL DEFAULT 'declared',restorable INTEGER NOT NULL DEFAULT 1,control_slot TEXT,claim_owner TEXT,scope_kind TEXT NOT NULL DEFAULT 'main');",
            "CREATE TABLE legacy_operation_parent(op_id TEXT NOT NULL,parent_op_id TEXT NOT NULL,PRIMARY KEY (op_id,parent_op_id));",
            "CREATE TABLE config_kv(id INTEGER PRIMARY KEY AUTOINCREMENT,key TEXT NOT NULL,value TEXT NOT NULL,encrypted INTEGER NOT NULL DEFAULT 0);",
            "CREATE TABLE legacy_operation_view_ref(view_id TEXT NOT NULL,ref_kind TEXT NOT NULL,ref_name TEXT NOT NULL,ref_remote TEXT NOT NULL,target_oid TEXT NOT NULL,PRIMARY KEY (view_id,ref_kind,ref_name,ref_remote));",
            "CREATE TABLE legacy_operation_view_workspace(view_id TEXT NOT NULL,pointer_kind TEXT NOT NULL,pointer_value TEXT NOT NULL,PRIMARY KEY (view_id,pointer_kind));",
            "CREATE TABLE reference (id INTEGER PRIMARY KEY AUTOINCREMENT,name TEXT,kind TEXT NOT NULL,\"commit\" TEXT,remote TEXT,worktree_id TEXT)",
        ] {
            db.execute_raw(Statement::from_string(DbBackend::Sqlite, sql.to_string()))
                .await
                .expect("create schema");
        }

        let meta = OperationMeta {
            command_name: "worktree repair".to_string(),
            description: "repair the worktree registry".to_string(),
            actor: "test".to_string(),
            repo_id: "repo_1".to_string(),
            args_digest: None,
        };
        let boundary = begin_operation_with_conn(&db, meta, OperationScope::default())
            .await
            .expect("open the boundary");

        // Fault injection: the close cannot write its outcome row.
        db.execute_raw(Statement::from_string(
            DbBackend::Sqlite,
            "DROP TABLE legacy_operation".to_string(),
        ))
        .await
        .expect("drop the operation table");

        let err = finish_repair_operation(boundary, Ok::<_, CliError>(()))
            .await
            .expect_err("a close failure must surface as a command error");
        let text = format!("{err:?}");
        assert!(
            text.contains("could not be closed"),
            "the error names the unclosed audit record: {text}"
        );
    }

    /// plan-20260714 W1: the BLOCKING registry acquisition keeps exactly one
    /// caller — the `spawn_blocking` inside `acquire_registry_lock_async`.
    ///
    /// Privacy stops other modules; this stops THIS module from quietly
    /// growing a second inline caller. Taking that `flock` on a runtime
    /// worker strands sqlx's spawned connection-return in the worker's
    /// non-stealable LIFO slot, and with sea-orm's 1-connection SQLite pool
    /// every database user in the process then waits out the acquire
    /// timeout. That deadlock shipped once; it must not ship twice.
    #[test]
    fn blocking_registry_lock_has_a_single_caller() {
        let source = std::fs::read_to_string(
            std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src/command/worktree.rs"),
        )
        .expect("this module's source must be readable");
        // Only the PRODUCTION half: the assertion below names the very
        // literals it forbids, so scanning this test module would match
        // itself.
        let production = source
            .split("#[cfg(all(test, unix))]")
            .next()
            .expect("the module has a production half");
        let callers: Vec<&str> = production
            .lines()
            .map(str::trim)
            .filter(|line| !line.starts_with("//"))
            .filter(|line| {
                line.contains("acquire_registry_lock")
                    && !line.contains("acquire_registry_lock_async")
                    && !line.starts_with("fn acquire_registry_lock")
            })
            .collect();
        assert_eq!(
            callers,
            vec!["match tokio::task::spawn_blocking(acquire_registry_lock).await {"],
            "the blocking registry acquisition grew a caller outside \
              `acquire_registry_lock_async`: take it on the blocking pool \
              instead (plan-20260714 W1)"
        );
    }
}
