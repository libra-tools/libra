//! `libra worktree` command implementation.
//!
//! Boundary: manages linked worktree metadata and filesystem layout while preserving
//! main-worktree safety invariants. Command tests cover add/list/remove, duplicate
//! paths, and main-worktree protection.

use std::{
    collections::HashSet,
    env, fs, io,
    path::{Component, Path, PathBuf},
};

use clap::{Parser, Subcommand};
use serde::{Deserialize, Serialize};

#[cfg(unix)]
use crate::utils::fuse as fuse_utils;
use crate::{
    command::restore::{self, RestoreArgs},
    internal::{branch::Branch, head::Head},
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
    libra worktree repair                          Fix stale or duplicate registry rows
    libra worktree repair ../feature-x             Restore that worktree's gitdir identity
                                                   from the registry (registry v2)";

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
    /// Repair worktree metadata, attempting to recover from inconsistencies.
    /// With a path, restores that linked worktree's gitdir identity
    /// (`.libra/worktree_id` + `commondir`) from the registry's persisted
    /// stable id (registry v2, W3 §C.7).
    Repair {
        /// Linked worktree whose gitdir identity should be restored.
        path: Option<String>,
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
const REGISTRY_SCHEMA_VERSION: u32 = 2;

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
}

impl Default for WorktreeState {
    fn default() -> Self {
        Self {
            schema_version: REGISTRY_SCHEMA_VERSION,
            entries: Vec::new(),
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
            if state.schema_version != REGISTRY_SCHEMA_VERSION {
                return Err(format!(
                    "unsupported registry schema_version {}",
                    state.schema_version
                ));
            }
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
                        })
                        .collect(),
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
}

#[derive(Debug, Serialize)]
pub(crate) struct WorktreeListOutput {
    pub(crate) worktrees: Vec<WorktreeListEntry>,
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
struct WorktreeRepairOutput {
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
    use crate::internal::config::parse_git_bool;
    // FAIL CLOSED on read failures and unparseable values — a bare boundary
    // that cannot be determined must refuse, not fall through to the
    // basename heuristic (which a `.libra`-named bare directory defeats).
    let is_bare = match crate::internal::config::ConfigKv::get("core.bare").await {
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

    if needs_repo {
        util::require_repo().map_err(|_| CliError::repo_not_found())?;
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
        WorktreeSubcommand::List { porcelain } => list_worktrees(output, porcelain).await,
        WorktreeSubcommand::Lock { path, reason } => {
            let result = lock_worktree(path, reason).map_err(WorktreeError::into_cli_error)?;
            render_lock_worktree(&result, output)
        }
        WorktreeSubcommand::Unlock { path } => {
            let result = unlock_worktree(path).map_err(WorktreeError::into_cli_error)?;
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
        WorktreeSubcommand::Repair { path } => {
            if let Some(path) = path {
                let result =
                    repair_worktree_identity(path).map_err(WorktreeError::into_cli_error)?;
                return render_repair_identity(&result, output);
            }
            let result = repair_worktrees()
                .await
                .map_err(WorktreeError::into_cli_error)?;
            render_repair_worktrees(&result, output)
        }
    }
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

pub(crate) fn acquire_registry_lock() -> WorktreeResult<RegistryLockGuard> {
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
    load_state_impl(false)
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
                source: format!("{source}; run `libra worktree repair` to heal it"),
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
fn load_state_readonly() -> WorktreeResult<WorktreeState> {
    let path = state_path();
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
                    source: format!("{source}; run `libra worktree repair` to heal it"),
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
fn normalize_abs_path(path: &Path) -> PathBuf {
    let mut out = PathBuf::new();
    for comp in path.components() {
        match comp {
            Component::Prefix(prefix) => out.push(prefix.as_os_str()),
            Component::RootDir => out.push(Path::new(comp.as_os_str())),
            Component::CurDir => {}
            Component::ParentDir => {
                if matches!(out.components().next_back(), Some(Component::Normal(_))) {
                    out.pop();
                }
            }
            Component::Normal(part) => out.push(part),
        }
    }
    out
}

fn canonicalize<P: AsRef<Path>>(path: P) -> io::Result<PathBuf> {
    let p = path.as_ref();
    let joined = if p.is_absolute() {
        p.to_path_buf()
    } else {
        util::cur_dir().join(p)
    };
    let normalized = normalize_abs_path(&joined);

    let mut current = normalized.as_path();
    let mut remainder = PathBuf::new();
    loop {
        if current.exists() {
            let mut canonical = fs::canonicalize(current)?;
            if !remainder.as_os_str().is_empty() {
                canonical.push(&remainder);
            }
            return Ok(canonical);
        }

        let Some(parent) = current.parent() else {
            break;
        };

        if let Some(name) = current.file_name() {
            remainder = if remainder.as_os_str().is_empty() {
                PathBuf::from(name)
            } else {
                PathBuf::from(name).join(remainder)
            };
            current = parent;
            continue;
        }

        break;
    }

    Ok(normalized)
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
    let _registry_lock = acquire_registry_lock()?;
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
                     repair` first, then add",
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
    let add_journal_id = journal_append(&db, "add", Some(&worktree_id), &add_payload)
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
                        return Err(WorktreeError::IoRead(format!(
                            "cannot verify whether branch '{name}' is checked out: {error}"
                        )));
                    }
                    Ok(Some(scope)) => {
                        rollback_partial_add();
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
                        return Err(WorktreeError::InvalidTarget(format!(
                            "branch '{name}' disappeared while creating the worktree"
                        )));
                    }
                    Err(e) => {
                        rollback_partial_add();
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
                        return Err(WorktreeError::OperationBlocked(format!(
                            "branch '{name}' was created concurrently; pick another name"
                        )));
                    }
                    Err(e) => {
                        rollback_partial_add();
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
                return Err(WorktreeError::IoRead(format!(
                    "failed to enter worktree directory '{}': {e}",
                    target.display()
                )));
            }
        };
        let created_branch = created_branch.clone();
        // lore.md 2.1: cwd is now the new worktree, so `current_worktree_id()`
        // resolves to its private id. Seed its OWN HEAD per the resolved
        // checkout (detached commit, attached branch, or the just-created
        // `-b` branch), so `Head::current()` resolves here and the populate
        // below can read it. A seed-update failure rolls EVERYTHING back —
        // including a `-b` branch row.
        if let Err(e) = Head::update_result(seed_head, None).await {
            drop(_guard);
            rollback_partial_add();
            rollback_created_branch(created_branch).await;
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
            // Restore the invoker's cwd BEFORE the rollbacks: deleting the
            // target while it is the cwd would break the branch rollback's
            // storage resolution (and strand the shell in a removed dir).
            drop(_guard);
            rollback_partial_add();
            rollback_created_branch(created_branch).await;
            return Err(WorktreeError::IoWrite(format!(
                "failed to populate worktree '{}': {e}",
                target.display()
            )));
        }
    }

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
    });
    if let Err(e) = write_state(&state) {
        rollback_partial_add();
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

    Ok(WorktreeAddOutput {
        path: canonical_target.to_string_lossy().to_string(),
        already_exists: false,
        reattached: false,
    })
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
             `libra worktree repair` first",
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
             persisted id ({expected_id}); run `libra worktree repair {}` first",
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
             different repository's storage; run `libra worktree repair {}` first",
            target.display(),
            target.display()
        )));
    }

    let payload = serde_json::json!({
        "path": target.to_string_lossy(),
        "reattach": true,
    });
    let journal_id = journal_append(&db, "add", Some(&expected_id), &payload)
        .await
        .map_err(WorktreeError::OperationBlocked)?;

    // Publish Active FIRST, then lift the marker: a crash in between leaves
    // an Active entry whose gitdir still carries the marker — repair's
    // reconcile pass removes a stale marker whose entry is Active with a
    // matching id (and the pending journal row rolls the re-attach
    // forward). The reverse order would leave an UNFROZEN detached entry.
    state.entries[index].state = WorktreeEntryState::Active;
    write_state(state)?;
    let marker = gitdir.join(DETACHED_MARKER);
    match fs::remove_file(&marker) {
        Ok(()) => {}
        Err(error) if error.kind() == io::ErrorKind::NotFound => {}
        Err(error) => {
            // Journal kept: repair finishes lifting the marker.
            return Err(WorktreeError::IoWrite(format!(
                "cannot remove the detached marker '{}' (run `libra worktree repair` to \
                 finish the re-attach): {error}",
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
    db.execute(Statement::from_sql_and_values(
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
    db.execute(Statement::from_sql_and_values(
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
        .query_all(Statement::from_string(
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
        .execute(Statement::from_sql_and_values(
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

/// Resolve (delete) a journal row after the mutation is fully published.
async fn journal_resolve(db: &sea_orm::DatabaseConnection, id: i64) -> Result<(), String> {
    use sea_orm::{ConnectionTrait, DbBackend, Statement};
    db.execute(Statement::from_sql_and_values(
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
        .query_all(Statement::from_string(
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
            .query_one(Statement::from_sql_and_values(
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
        .query_one(Statement::from_string(
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
        db.execute(Statement::from_sql_and_values(
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
    let state = load_state_readonly()?;
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
            WorktreeListEntry {
                kind: if w.is_main { "main" } else { "worktree" },
                exists: Path::new(&w.path).exists(),
                worktree_id,
                state: w.state.as_str(),
                path: w.path,
                is_main: w.is_main,
                locked: w.locked,
                lock_reason: w.lock_reason,
            }
        })
        .collect();
    Ok(WorktreeListOutput { worktrees })
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
pub(crate) async fn format_worktree_porcelain(worktrees: &[WorktreeListEntry]) -> String {
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
            Err(_) => {}
        }
        if w.locked {
            match w.lock_reason.as_deref() {
                Some(reason) if !reason.is_empty() => out.push_str(&format!("locked {reason}\n")),
                _ => out.push_str("locked\n"),
            }
        }
        out.push('\n');
    }
    out
}

async fn list_worktrees(output: &OutputConfig, porcelain: bool) -> CliResult<()> {
    let result = run_list_worktrees().map_err(WorktreeError::into_cli_error)?;
    if output.is_json() {
        return emit_json_data("worktree.list", &result, output);
    }
    if output.quiet {
        return Ok(());
    }
    if porcelain {
        print!("{}", format_worktree_porcelain(&result.worktrees).await);
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

/// Implements `worktree lock <path> [--reason <msg>]`.
///
/// Marks the specified worktree entry as locked and persists an optional
/// human-readable reason. Locking is a state-only operation and does not
/// alter directories on disk.
fn lock_worktree(path: String, reason: Option<String>) -> WorktreeResult<WorktreeLockOutput> {
    let _registry_lock = acquire_registry_lock()?;
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
fn unlock_worktree(path: String) -> WorktreeResult<WorktreeUnlockOutput> {
    let _registry_lock = acquire_registry_lock()?;
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
    let _registry_lock = acquire_registry_lock()?;
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
    let payload = serde_json::json!({
        "src": src_path.to_string_lossy(),
        "dest": dest_path.to_string_lossy(),
    });
    let db = crate::internal::db::get_db_conn_instance().await;
    let journal_id = journal_append(&db, "move", journal_worktree_id.as_deref(), &payload)
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
    let _registry_lock = acquire_registry_lock()?;
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
            eligible.push((path, id));
        }
        if !eligible.is_empty() {
            let payload = serde_json::json!({
                "paths": eligible.iter().map(|(p, _)| p.clone()).collect::<Vec<_>>(),
            });
            let journal_id = journal_append(&db, "prune", None, &payload)
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
    let _registry_lock = acquire_registry_lock()?;
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
    let entry_state = entry.state;
    if entry_state == WorktreeEntryState::Tombstone {
        return Err(WorktreeError::OperationBlocked(format!(
            "'{}' is a tombstone (directory already deleted, scoped cleanup pending); run \
             `libra worktree repair` to retry the cleanup",
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
             repair` first",
            target.display()
        )));
    };
    let payload = serde_json::json!({
        "path": target.to_string_lossy(),
        "delete_dir": false,
    });
    let journal_id = journal_append(db, "remove", Some(&worktree_id), &payload)
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
    let journal_id = journal_append(db, "remove", worktree_id.as_deref(), &payload)
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
    fsync_parent_best_effort(target);

    let cleanup_failed = if let Some(id) = worktree_id.as_deref() {
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

/// Durably record the parent directory entry after a worktree deletion —
/// the tombstone contract ("directory durably deleted") depends on it.
/// Best-effort on platforms/filesystems that refuse directory fsync.
fn fsync_parent_best_effort(target: &Path) {
    if let Some(parent) = target.parent()
        && let Ok(dir) = fs::File::open(parent)
    {
        let _ = dir.sync_all();
    }
}

/// The in-worktree dirty check shared by `--delete-dir` (staged or real —
/// non-overlay — unstaged changes refuse the destructive delete).
async fn worktree_is_dirty(target: &Path) -> WorktreeResult<bool> {
    let _guard = DirGuard::change_to(target).map_err(|e| {
        WorktreeError::IoRead(format!("cannot enter worktree '{}': {e}", target.display()))
    })?;
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
             tombstone entry remains; run `libra worktree repair` to retry.",
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
fn repair_worktree_identity(path: String) -> WorktreeResult<WorktreeRepairIdentityOutput> {
    let _registry_lock = acquire_registry_lock()?;
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
             identities; run `libra worktree repair` (no argument) once to upgrade it, \
             then retry"
                .to_string(),
        ));
    }
    let state = load_state()?;
    let target = resolve_path(&path, "worktree path")?;
    let entry =
        find_entry(&state, &target).ok_or(WorktreeError::NoSuchWorktree { path: path.clone() })?;
    if entry.is_main {
        return Err(WorktreeError::MainWorktree {
            action: "repair",
            path: target.to_string_lossy().to_string(),
        });
    }
    let Some(stable_id) = entry.worktree_id.clone() else {
        return Err(WorktreeError::OperationBlocked(format!(
            "the registry entry for '{}' predates registry v2 and carries no persisted \
             worktree id; run the no-arg `libra worktree repair` once to upgrade the \
             registry, then retry",
            target.display()
        )));
    };
    let gitdir = target.join(util::ROOT_DIR);
    if !gitdir.is_dir() {
        return Err(WorktreeError::OperationBlocked(format!(
            "'{}' has no .libra gitdir to repair; re-add the worktree instead",
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
                existing
            } else {
                gitdir.join(existing)
            };
            let existing_resolved = fs::canonicalize(&existing_abs).unwrap_or(existing_abs);
            let common_resolved = fs::canonicalize(&common).unwrap_or_else(|_| common.clone());
            if existing_resolved != common_resolved {
                return Err(WorktreeError::OperationBlocked(format!(
                    "'{}' already points at a different common storage ('{}'); refusing to \
                     re-home the worktree — remove and re-add it if this is intended",
                    commondir_path.display(),
                    existing_resolved.display()
                )));
            }
            false
        }
        None => true,
    };

    let id_path = gitdir.join("worktree_id");
    let current_id = fs::read_to_string(&id_path)
        .ok()
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty());
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

async fn repair_worktrees() -> WorktreeResult<WorktreeRepairOutput> {
    let _registry_lock = acquire_registry_lock()?;
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
                    // Keep-dir detach: roll FORWARD to the detached state.
                    if let Some(idx) = entry_index {
                        let entry_id = state.entries[idx].worktree_id.clone();
                        if let Some(id_str) = entry_id.as_deref().or(worktree_id.as_deref()) {
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
                        if state.entries[idx].state != WorktreeEntryState::DetachedFromRegistry {
                            state.entries[idx].state = WorktreeEntryState::DetachedFromRegistry;
                            *changed = true;
                        }
                        notes.push(format!("completed interrupted detach of '{path}'"));
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
                            if src_exists && dest_missing {
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
                            if src_missing && dest_exists {
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
            other => {
                notes.push(format!("unknown intent op '{other}' (id {id}) resolved"));
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
                if !marker.exists() && Path::new(&entry.path).is_dir() {
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

fn render_repair_worktrees(result: &WorktreeRepairOutput, output: &OutputConfig) -> CliResult<()> {
    if output.is_json() {
        return emit_json_data("worktree.repair", result, output);
    }
    if !output.quiet {
        for note in &result.notes {
            println!("{note}");
        }
        if result.tombstones_pending > 0 {
            println!(
                "{} tombstone(s) still pending; rerun `libra worktree repair` after \
                 addressing the notes above",
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
        let data = br#"{"schema_version": 3, "entries": []}"#;
        let err = WorktreeState::parse(data).expect_err("future version fails closed");
        assert!(err.contains("schema_version 3"), "{err}");
    }

    /// The second belt (§C.7): a v1 binary's `{ worktrees: [...] }` parser
    /// must FAIL on v2 bytes (renamed top-level key) instead of silently
    /// reading an empty registry and rewriting the file.
    #[test]
    fn v1_parser_fails_on_v2_bytes() {
        let v2 = serde_json::to_vec(&WorktreeState {
            schema_version: REGISTRY_SCHEMA_VERSION,
            entries: vec![WorktreeEntry {
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
}
