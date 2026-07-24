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
    internal::head::Head,
    utils::{
        error::{CliError, CliResult, StableErrorCode},
        output::{OutputConfig, emit_json_data},
        util,
    },
};

/// `--help` examples shown in `libra worktree --help` output.
pub const WORKTREE_EXAMPLES: &str = "\
EXAMPLES:
    libra worktree add ../feature-x                Create a linked worktree
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
}

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
}

#[derive(Debug, Serialize)]
struct WorktreeAddOutput {
    path: String,
    already_exists: bool,
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
}

#[derive(Debug, Serialize)]
struct WorktreeRemoveOutput {
    path: String,
    registry_removed: bool,
    disk_directory_deleted: bool,
}

#[derive(Debug, Serialize)]
struct WorktreeRepairOutput {
    changed: bool,
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
        WorktreeSubcommand::Add { path } => {
            let result = add_worktree(path)
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
            let result = move_worktree(src, dest).map_err(WorktreeError::into_cli_error)?;
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
            let result = repair_worktrees().map_err(WorktreeError::into_cli_error)?;
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
async fn add_worktree(path: String) -> WorktreeResult<WorktreeAddOutput> {
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
    if state
        .entries
        .iter()
        .any(|w| Path::new(&w.path) == canonical_target)
    {
        return Ok(WorktreeAddOutput {
            path: canonical_target.to_string_lossy().to_string(),
            already_exists: true,
        });
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
    gc_worktree_scoped_rows_strict(&worktree_id, true)
        .await
        .map_err(|e| {
            WorktreeError::IoWrite(format!(
                "cannot register worktree '{}': failed to clear stale scoped rows for its \
                 instance id: {e}",
                target.display()
            ))
        })?;
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

    // The main worktree's current commit (cwd is still the main repo here).
    // Codex P1: use the RESULT-returning API so a storage error is not silently
    // downgraded to "unborn repo" — only a genuinely unborn HEAD (Ok(None))
    // skips seeding; a real read error rolls back the half-created worktree.
    let seed_commit = match Head::current_commit_result().await {
        Ok(commit) => commit,
        Err(e) => {
            rollback_partial_add();
            return Err(WorktreeError::IoRead(format!(
                "failed to read HEAD while creating worktree '{}': {e}",
                target.display()
            )));
        }
    };
    if let Some(commit) = seed_commit {
        let _guard = match DirGuard::change_to(&target) {
            Ok(g) => g,
            Err(e) => {
                rollback_partial_add();
                return Err(WorktreeError::IoRead(format!(
                    "failed to enter worktree directory '{}': {e}",
                    target.display()
                )));
            }
        };
        // lore.md 2.1: cwd is now the new worktree, so `current_worktree_id()`
        // resolves to its private id. Seed its OWN HEAD as DETACHED at the main
        // worktree's commit (v1 detaches to avoid a same-branch collision), so
        // `Head::current()` resolves here and the populate below can read it.
        // Codex P1: a seed-update failure must roll back, not silently leave the
        // worktree without a private HEAD.
        if let Err(e) = Head::update_result(Head::Detached(commit), None).await {
            drop(_guard);
            rollback_partial_add();
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
            rollback_partial_add();
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
    });
    if let Err(e) = write_state(&state) {
        rollback_partial_add();
        return Err(e);
    }

    Ok(WorktreeAddOutput {
        path: canonical_target.to_string_lossy().to_string(),
        already_exists: false,
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
async fn gc_worktree_scoped_rows(worktree_id: &str, directory_gone: bool) {
    // Best-effort on the REMOVAL side only: `worktree add` re-sweeps the
    // same scope STRICTLY before seeding (deterministic ids), so a failure
    // logged here can never silently leak stale state into a future
    // worktree at the same path.
    if let Err(e) = gc_worktree_scoped_rows_strict(worktree_id, directory_gone).await {
        tracing::warn!(worktree_id, error = %e, "failed to GC per-worktree rows");
    }
}

/// Strict variant of [`gc_worktree_scoped_rows`]: the first failed DELETE
/// aborts and surfaces the error. `worktree add` uses this as its pre-seed
/// sweep — inheriting another (removed) worktree's rows must fail the add,
/// not proceed with a polluted scope.
async fn gc_worktree_scoped_rows_strict(
    worktree_id: &str,
    directory_gone: bool,
) -> Result<(), String> {
    use sea_orm::{ConnectionTrait, DbBackend, Statement};
    let db = crate::internal::db::get_db_conn_instance().await;
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
fn move_worktree(src: String, dest: String) -> WorktreeResult<WorktreeMoveOutput> {
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

    let old_path = state.entries[index].path.clone();
    state.entries[index].path = dest_path.to_string_lossy().to_string();
    if let Err(e) = write_state(&state) {
        state.entries[index].path = old_path;
        return Err(e);
    }

    if let Err(e) = fs::rename(&src_path, &dest_path) {
        state.entries[index].path = old_path;
        write_state(&state)?;
        return Err(WorktreeError::IoWrite(format!(
            "failed to move worktree directory '{}' to '{}': {e}",
            src_path.display(),
            dest_path.display()
        )));
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
    let to_prune: Vec<_> = state
        .entries
        .iter()
        .filter(|w| {
            let path = Path::new(&w.path);
            !path.exists() && !w.is_main && !w.locked
        })
        .map(|w| w.path.clone())
        .collect();

    if !to_prune.is_empty() {
        for path in &to_prune {
            if let Some(id) = resolve_worktree_id(Path::new(path)) {
                gc_worktree_scoped_rows(&id, true).await;
            }
        }
        state.entries.retain(|w| {
            let path = Path::new(&w.path);
            path.exists() || w.is_main || w.locked
        });
        write_state(&state)?;
    }

    Ok(WorktreePruneOutput {
        pruned_count: to_prune.len(),
        pruned: to_prune,
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

    // Resolve the instance id BEFORE any directory removal so its private
    // HEAD/reflog rows can be GC'd (lore.md 2.1).
    let worktree_id_for_gc = resolve_worktree_id(&target);

    if delete_dir {
        // Dirty-check: refuse on staged or unstaged changes. The check runs
        // inside the target worktree so the ignore policy and storage path
        // resolution match what the user would see if they ran `libra status`
        // there.
        let _guard = DirGuard::change_to(&target).map_err(|e| {
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
                &crate::internal::worktree_scope::WorktreeScope::for_workdir(&target),
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
            .map_err(|e| {
                WorktreeError::IoRead(format!("failed to inspect worktree status: {e}"))
            })?;
        let unstaged = crate::command::status::changes_to_be_staged().map_err(|e| {
            WorktreeError::IoRead(format!("failed to inspect worktree status: {e}"))
        })?;
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
        if !staged.is_empty() || unstaged_dirty {
            return Err(WorktreeError::DirtyWorktree {
                path: target.to_string_lossy().to_string(),
            });
        }
        // Drop the guard so the cwd is restored before we rm -rf the target.
        drop(_guard);
        fs::remove_dir_all(&target).map_err(|e| {
            WorktreeError::IoWrite(format!(
                "failed to delete worktree directory '{}': {e}",
                target.display()
            ))
        })?;
    }

    if let Some(id) = worktree_id_for_gc {
        gc_worktree_scoped_rows(&id, delete_dir || !target.exists()).await;
    }
    state.entries.remove(index);
    write_state(&state)?;

    Ok(WorktreeRemoveOutput {
        path: target.to_string_lossy().into_owned(),
        registry_removed: true,
        disk_directory_deleted: delete_dir,
    })
}

fn render_remove_worktree(result: &WorktreeRemoveOutput, output: &OutputConfig) -> CliResult<()> {
    if output.is_json() {
        return emit_json_data("worktree.remove", result, output);
    }
    if output.quiet {
        return Ok(());
    }
    if result.disk_directory_deleted {
        println!(
            "Removed worktree '{}' from registry and deleted directory.",
            result.path
        );
    } else {
        println!(
            "Removed worktree '{}' from registry. Directory kept on disk.",
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

fn repair_worktrees() -> WorktreeResult<WorktreeRepairOutput> {
    let _registry_lock = acquire_registry_lock()?;
    // The healing loader may itself rewrite the file (v1 upgrade, identity
    // invariants); report that as a change too.
    let bytes_before = fs::read(state_path()).ok();
    let mut state = load_state_for_repair()?;
    let mut changed = fs::read(state_path()).ok() != bytes_before;

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

    if changed {
        let _ = normalize_v2_ids(&mut state);
        write_state(&state)?;
    }

    Ok(WorktreeRepairOutput { changed })
}

fn render_repair_worktrees(result: &WorktreeRepairOutput, output: &OutputConfig) -> CliResult<()> {
    if output.is_json() {
        return emit_json_data("worktree.repair", result, output);
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
