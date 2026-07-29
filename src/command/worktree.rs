//! `libra worktree` command implementation.
//!
//! Boundary: manages linked worktree metadata and filesystem layout while preserving
//! main-worktree safety invariants. Command tests cover add/list/remove, duplicate
//! paths, and main-worktree protection.

use std::{
    collections::HashSet,
    env, fs, io,
    path::{Component, Path, PathBuf},
    time::Duration,
};

use clap::{Parser, Subcommand};
use serde::{Deserialize, Serialize};

#[cfg(unix)]
use crate::utils::fuse as fuse_utils;
use crate::{
    command::restore::{self, RestoreArgs},
    internal::{
        head::Head,
        scorpiofs_backend::{
            CAPABILITY_CHANGES_V1, CAPABILITY_MOUNT_V1, CAPABILITY_READY_V1, HttpScorpioFsClient,
            MountRequest, MountResponse, ScorpioFsBackendRecord, ScorpioFsControl, ScorpioFsDriver,
        },
        worktree_backend::{
            BackendKind as WorktreeBackendKind, BackendMountRequest as StorageMountRequest,
            BackendMountSession as StorageMountSession, BackendMountSource, WorktreeBackendDriver,
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
    libra worktree add ../feature-x                Create a linked worktree
    libra worktree scorpiofs attach --remote-path /project/crate --job-id dev-crate
                                                   Attach a ScorpioFS remote worktree
    libra worktree scorpiofs detach <mountpoint>   Detach the remote worktree
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
    libra worktree repair                          Fix stale or duplicate registry rows";

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
    /// Manage a linked worktree backed by a ScorpioFS Antares mount.
    Scorpiofs {
        #[clap(subcommand)]
        command: ScorpioFsSubcommand,
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
    Repair,
}

#[derive(Subcommand, Debug)]
pub enum ScorpioFsSubcommand {
    /// Mount a remote monorepo path and attach it as a persistent linked worktree.
    Attach {
        /// Antares API base URL.
        ///
        /// When omitted on Linux, Libra starts and owns an embedded ScorpioFS
        /// worker. Supplying this option selects the compatibility HTTP mode.
        #[clap(long)]
        endpoint: Option<String>,
        /// ScorpioFS configuration used by the Libra-owned worker.
        #[clap(long, default_value = "scorpio.toml")]
        config_path: PathBuf,
        /// Absolute path inside the remote monorepo.
        #[clap(long)]
        remote_path: String,
        /// Stable job identity used for idempotent mount creation and recovery.
        #[clap(long)]
        job_id: String,
        /// Optional Mega changelist layer.
        #[clap(long)]
        cl: Option<String>,
        /// Maximum number of seconds to wait for mount readiness.
        #[clap(long, default_value_t = 120)]
        ready_timeout_secs: u64,
    },
    /// Unmount and unregister a ScorpioFS-backed linked worktree.
    Detach {
        /// Mounted worktree path.
        path: String,
    },
}

/// A single worktree entry persisted in `worktrees.json`.
///
/// `path` is always stored as a canonical absolute path.
#[derive(Serialize, Deserialize, Debug, Clone)]
struct WorktreeEntry {
    path: String,
    is_main: bool,
    locked: bool,
    lock_reason: Option<String>,
}

/// Top-level state persisted in `worktrees.json`.
///
/// The state contains the main worktree and any number of linked worktrees.
#[derive(Serialize, Deserialize, Debug, Default, Clone)]
struct WorktreeState {
    worktrees: Vec<WorktreeEntry>,
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
struct ScorpioFsAttachOutput {
    path: String,
    worktree_id: String,
    mount_id: String,
    job_id: String,
    already_exists: bool,
}

#[derive(Debug, Serialize)]
struct ScorpioFsDetachOutput {
    path: String,
    worktree_id: String,
    job_id: String,
    unmounted: bool,
    registry_removed: bool,
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
pub async fn execute_safe(args: WorktreeArgs, output: &OutputConfig) -> CliResult<()> {
    let command = args.command;
    #[cfg(unix)]
    let needs_repo = !matches!(&command, WorktreeSubcommand::Umount { .. });
    #[cfg(not(unix))]
    let needs_repo = true;

    if needs_repo {
        util::require_repo().map_err(|_| CliError::repo_not_found())?;
    }

    match command {
        WorktreeSubcommand::Add { path } => {
            let result = add_worktree(path)
                .await
                .map_err(WorktreeError::into_cli_error)?;
            render_add_worktree(&result, output)
        }
        WorktreeSubcommand::Scorpiofs { command } => match command {
            ScorpioFsSubcommand::Attach {
                endpoint,
                config_path,
                remote_path,
                job_id,
                cl,
                ready_timeout_secs,
            } => {
                let result = attach_scorpiofs_worktree(
                    endpoint,
                    config_path,
                    remote_path,
                    job_id,
                    cl,
                    ready_timeout_secs,
                )
                .await
                .map_err(WorktreeError::into_cli_error)?;
                render_scorpiofs_attach(&result, output)
            }
            ScorpioFsSubcommand::Detach { path } => {
                let result = detach_scorpiofs_worktree(path)
                    .await
                    .map_err(WorktreeError::into_cli_error)?;
                render_scorpiofs_detach(&result, output)
            }
        },
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
            let result = prune_worktrees().map_err(WorktreeError::into_cli_error)?;
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
        WorktreeSubcommand::Repair => {
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
/// If the state file does not exist or is empty, this function initializes a
/// fresh state with a single main worktree derived from the storage path, then
/// persists it before returning.
fn load_state() -> WorktreeResult<WorktreeState> {
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
        let mut state = WorktreeState::default();
        let _ = ensure_main_entry(&mut state)
            .map_err(|source| WorktreeError::StateRepair { source })?;
        write_state(&state)?;
        return Ok(state);
    }
    let mut state: WorktreeState =
        serde_json::from_slice(&data).map_err(|source| WorktreeError::StateCorrupt {
            path: path.clone(),
            source: source.to_string(),
        })?;
    if ensure_main_entry(&mut state).map_err(|source| WorktreeError::StateRepair { source })? {
        write_state(&state)?;
    }
    Ok(state)
}

/// Atomically writes the given `WorktreeState` to disk.
///
/// The state is first written to a temporary file and then moved into place.
/// On Windows, the existing file is removed before `rename` to avoid platform
/// specific failures when the destination already exists.
fn save_state(state: &WorktreeState) -> io::Result<()> {
    let path = state_path();
    let tmp = path.with_extension("json.tmp");
    let data = serde_json::to_vec_pretty(state).map_err(|e| io::Error::other(e.to_string()))?;
    if let Some(parent) = tmp.parent() {
        fs::create_dir_all(parent)?;
    }
    fs::write(&tmp, data)?;
    #[cfg(windows)]
    {
        if path.exists() {
            match fs::remove_file(&path) {
                Ok(()) => {}
                Err(e) if e.kind() == io::ErrorKind::NotFound => {}
                Err(e) => {
                    let _ = fs::remove_file(&tmp);
                    return Err(e);
                }
            }
        }
        fs::rename(&tmp, &path)?;
    }

    #[cfg(not(windows))]
    {
        fs::rename(&tmp, &path)?;
    }
    Ok(())
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

/// Ensures that the main worktree entry exists and is unique.
///
/// If the current `is_main` marker is invalid or duplicated, this function
/// repairs it by preferring a valid existing worktree path and then enforcing
/// uniqueness. Only when no entries exist does it infer a new main path from
/// repository layout.
///
/// Returns `true` when the state is mutated.
fn ensure_main_entry(state: &mut WorktreeState) -> io::Result<bool> {
    fn is_valid_worktree_path(path: &Path) -> bool {
        path.join(util::ROOT_DIR).exists()
    }

    fn apply_unique_main(state: &mut WorktreeState, idx: usize) -> bool {
        let mut changed = false;
        for (i, w) in state.worktrees.iter_mut().enumerate() {
            let should_be_main = i == idx;
            if w.is_main != should_be_main {
                w.is_main = should_be_main;
                changed = true;
            }
        }
        changed
    }

    // First prefer a currently marked main entry if it points to an actual
    // worktree root.
    if let Some(idx) =
        state.worktrees.iter().enumerate().find_map(|(i, w)| {
            (w.is_main && is_valid_worktree_path(Path::new(&w.path))).then_some(i)
        })
    {
        return Ok(apply_unique_main(state, idx));
    }

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

    // No valid main marker exists. Prefer an existing real worktree path so
    // the original main is stable even when running from linked worktrees.
    if let Some(idx) = inferred_standard_main
        .as_ref()
        .and_then(|p| {
            state
                .worktrees
                .iter()
                .position(|w| Path::new(&w.path) == p && is_valid_worktree_path(Path::new(&w.path)))
        })
        .or_else(|| {
            state
                .worktrees
                .iter()
                .position(|w| is_valid_worktree_path(Path::new(&w.path)))
        })
        .or_else(|| (!state.worktrees.is_empty()).then_some(0))
    {
        return Ok(apply_unique_main(state, idx));
    }

    // Empty state fallback: infer a new main entry.
    let inferred_main = if let Some(p) = inferred_standard_main {
        p
    } else {
        canonicalize(util::working_dir())?
    };

    if let Some(idx) = state
        .worktrees
        .iter()
        .position(|w| Path::new(&w.path) == inferred_main)
    {
        Ok(apply_unique_main(state, idx))
    } else {
        for w in &mut *state.worktrees {
            w.is_main = false;
        }
        state.worktrees.push(WorktreeEntry {
            path: inferred_main.to_string_lossy().to_string(),
            is_main: true,
            locked: false,
            lock_reason: None,
        });
        Ok(true)
    }
}

/// Finds a mutable worktree entry by canonical path.
fn find_entry_mut<'a>(state: &'a mut WorktreeState, path: &Path) -> Option<&'a mut WorktreeEntry> {
    state
        .worktrees
        .iter_mut()
        .find(|w| Path::new(&w.path) == path)
}

/// Finds an immutable worktree entry by canonical path.
fn find_entry<'a>(state: &'a WorktreeState, path: &Path) -> Option<&'a WorktreeEntry> {
    state.worktrees.iter().find(|w| Path::new(&w.path) == path)
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
        .worktrees
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

    state.worktrees.push(WorktreeEntry {
        path: canonical_target.to_string_lossy().to_string(),
        is_main: false,
        locked: false,
        lock_reason: None,
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

async fn attach_scorpiofs_worktree(
    endpoint: Option<String>,
    config_path: PathBuf,
    remote_path: String,
    job_id: String,
    cl: Option<String>,
    ready_timeout_secs: u64,
) -> WorktreeResult<ScorpioFsAttachOutput> {
    let storage = util::storage_path();
    let _state_lock = crate::internal::scorpiofs_backend::ScorpioFsStateLock::acquire(&storage)
        .map_err(|error| WorktreeError::IoWrite(error.to_string()))?;
    let (endpoint, transport) = match endpoint {
        Some(endpoint) => (
            endpoint,
            crate::internal::scorpiofs_backend::BackendTransport::ExternalHttp,
        ),
        None => (
            ensure_managed_scorpiofs_worker(
                &storage,
                &config_path,
                Duration::from_secs(ready_timeout_secs),
            )
            .await?,
            crate::internal::scorpiofs_backend::BackendTransport::ManagedCrate,
        ),
    };
    let seed_commit = Head::current_commit_result().await.map_err(|error| {
        WorktreeError::IoRead(format!(
            "failed to read HEAD before attaching ScorpioFS worktree: {error}"
        ))
    })?;
    let client = HttpScorpioFsClient::new(&endpoint)
        .map_err(|error| WorktreeError::IoRead(error.to_string()))?;
    let request = MountRequest {
        job_id: job_id.clone(),
        path: remote_path,
        cl,
        base_oid: None,
    };
    let mut desired_state = crate::internal::scorpiofs_backend::LibraScorpioFsState::load(&storage)
        .map_err(|error| WorktreeError::IoRead(error.to_string()))?;
    let previous_mountpoint = desired_state
        .mounts
        .get(&job_id)
        .and_then(|desired| desired.mountpoint.clone());
    desired_state
        .begin_mount(request.clone(), transport, endpoint.clone())
        .map_err(|error| WorktreeError::OperationBlocked(error.to_string()))?;
    desired_state
        .save(&storage)
        .map_err(|error| WorktreeError::IoWrite(error.to_string()))?;

    let driver = ScorpioFsDriver::new(client.clone());
    let backend_request = StorageMountRequest {
        instance_id: job_id.clone(),
        worktree_id: job_id.clone(),
        source: BackendMountSource::RemoteProjection {
            remote_path: request.path.clone(),
            base_oid: request.base_oid.clone(),
            change_layer: request.cl.clone(),
        },
        mountpoint_hint: None,
        ready_timeout_secs,
    };
    let backend_session = match driver.mount(&backend_request).await {
        Ok(session) => session,
        Err(error) => {
            record_scorpiofs_attach_error(&storage, &job_id, error.to_string());
            return Err(WorktreeError::IoWrite(error.to_string()));
        }
    };
    let mount = MountResponse {
        mount_id: backend_session.session_id,
        mountpoint: backend_session.mountpoint.to_string_lossy().into_owned(),
        base_oid: backend_session.base_oid,
        ready: Some(true),
    };

    let target = match resolve_path(&mount.mountpoint, "ScorpioFS mountpoint") {
        Ok(target) => target,
        Err(error) => {
            let _ = client.delete_by_job(&job_id).await;
            record_scorpiofs_attach_error(&storage, &job_id, format!("{error:?}"));
            return Err(error);
        }
    };
    if !target.is_dir() {
        let _ = client.delete_by_job(&job_id).await;
        let error = WorktreeError::InvalidTarget(format!(
            "ScorpioFS mountpoint is not a directory: {}",
            target.display()
        ));
        record_scorpiofs_attach_error(&storage, &job_id, format!("{error:?}"));
        return Err(error);
    }
    if util::is_sub_path(&target, &storage) {
        let _ = client.delete_by_job(&job_id).await;
        let error = WorktreeError::InvalidTarget(format!(
            "ScorpioFS mountpoint cannot be inside .libra storage: {}",
            target.display()
        ));
        record_scorpiofs_attach_error(&storage, &job_id, format!("{error:?}"));
        return Err(error);
    }

    let mut state = load_state()?;
    if let Some(previous_mountpoint) = previous_mountpoint.as_deref() {
        if Path::new(previous_mountpoint) != target {
            state
                .worktrees
                .retain(|entry| Path::new(&entry.path) != Path::new(previous_mountpoint));
        }
    }
    if state
        .worktrees
        .iter()
        .any(|entry| Path::new(&entry.path) == target)
    {
        let gitdir = util::try_get_worktree_gitdir(Some(target.clone())).map_err(|error| {
            WorktreeError::IoRead(format!(
                "registered ScorpioFS worktree '{}' has invalid metadata: {error}",
                target.display()
            ))
        })?;
        let record = ScorpioFsBackendRecord::load(&gitdir)
            .map_err(|error| WorktreeError::IoRead(error.to_string()))?;
        if record.job_id != job_id || record.remote_path != request.path {
            return Err(WorktreeError::OperationBlocked(format!(
                "worktree '{}' is already registered for ScorpioFS job '{}' and path '{}'",
                target.display(),
                record.job_id,
                record.remote_path
            )));
        }
        desired_state
            .mark_ready(&job_id, &mount, &read_worktree_id(&gitdir)?)
            .map_err(|error| WorktreeError::IoWrite(error.to_string()))?;
        desired_state
            .save(&storage)
            .map_err(|error| WorktreeError::IoWrite(error.to_string()))?;
        return Ok(ScorpioFsAttachOutput {
            path: target.to_string_lossy().into_owned(),
            worktree_id: read_worktree_id(&gitdir)?,
            mount_id: mount.mount_id,
            job_id,
            already_exists: true,
        });
    }

    let backend_identity = match transport {
        crate::internal::scorpiofs_backend::BackendTransport::ManagedCrate => "managed-crate",
        crate::internal::scorpiofs_backend::BackendTransport::ExternalHttp => {
            client.endpoint().as_str()
        }
    };
    let worktree_id = scorpiofs_worktree_id(backend_identity, &job_id, &request.path);
    let gitdir = storage
        .join("worktrees")
        .join("scorpiofs")
        .join(&worktree_id);
    let pointer = target.join(util::ROOT_DIR);
    let created_gitdir = !gitdir.exists();
    if created_gitdir {
        create_worktree_gitdir(&storage, &gitdir, &worktree_id).map_err(|source| {
            WorktreeError::IoWrite(format!(
                "failed to create persistent ScorpioFS worktree gitdir '{}': {source}",
                gitdir.display()
            ))
        })?;
    }

    let record = match ScorpioFsBackendRecord::new_with_transport(
        client.endpoint(),
        &mount,
        &request,
        transport,
    ) {
        Ok(record) => record,
        Err(error) => {
            rollback_scorpiofs_attach(
                &client,
                &job_id,
                &pointer,
                false,
                &gitdir,
                created_gitdir,
                &worktree_id,
            )
            .await;
            return Err(WorktreeError::IoWrite(error.to_string()));
        }
    };

    if !created_gitdir {
        let existing = ScorpioFsBackendRecord::load(&gitdir).map_err(|error| {
            WorktreeError::OperationBlocked(format!(
                "persistent ScorpioFS worktree '{}' cannot be reused: {error}",
                gitdir.display()
            ))
        })?;
        if existing.job_id != job_id
            || existing.remote_path != request.path
            || existing.transport != record.transport
            || (transport == crate::internal::scorpiofs_backend::BackendTransport::ExternalHttp
                && existing.endpoint != record.endpoint)
        {
            let _ = client.delete_by_job(&job_id).await;
            return Err(WorktreeError::OperationBlocked(format!(
                "persistent ScorpioFS worktree id '{}' belongs to another backend attachment",
                worktree_id
            )));
        }
    }
    if let Err(error) = record.save(&gitdir) {
        rollback_scorpiofs_attach(
            &client,
            &job_id,
            &pointer,
            false,
            &gitdir,
            created_gitdir,
            &worktree_id,
        )
        .await;
        return Err(WorktreeError::IoWrite(error.to_string()));
    }

    let created_pointer = match attach_worktree_pointer(&pointer, &gitdir) {
        Ok(created) => created,
        Err(source) => {
            rollback_scorpiofs_attach(
                &client,
                &job_id,
                &pointer,
                false,
                &gitdir,
                created_gitdir,
                &worktree_id,
            )
            .await;
            return Err(WorktreeError::IoWrite(format!(
                "failed to attach persistent metadata at '{}': {source}",
                pointer.display()
            )));
        }
    };

    if let Some(commit) = seed_commit {
        let guard = match DirGuard::change_to(&target) {
            Ok(guard) => guard,
            Err(error) => {
                rollback_scorpiofs_attach(
                    &client,
                    &job_id,
                    &pointer,
                    created_pointer,
                    &gitdir,
                    created_gitdir,
                    &worktree_id,
                )
                .await;
                return Err(WorktreeError::IoRead(format!(
                    "failed to enter ScorpioFS worktree '{}': {error}",
                    target.display()
                )));
            }
        };
        if let Err(error) = Head::update_result(Head::Detached(commit), None).await {
            drop(guard);
            rollback_scorpiofs_attach(
                &client,
                &job_id,
                &pointer,
                created_pointer,
                &gitdir,
                created_gitdir,
                &worktree_id,
            )
            .await;
            return Err(WorktreeError::IoWrite(format!(
                "failed to seed ScorpioFS worktree HEAD: {error}"
            )));
        }
        if let Err(error) = restore::execute_checked(RestoreArgs {
            overlay: false,
            no_overlay: false,
            ours: false,
            theirs: false,
            ignore_unmerged: false,
            merge: false,
            conflict: None,
            pathspec: vec![".".to_string()],
            source: Some("HEAD".to_string()),
            worktree: false,
            staged: true,
            pathspec_from_file: None,
            pathspec_file_nul: false,
            no_progress: false,
        })
        .await
        {
            drop(guard);
            rollback_scorpiofs_attach(
                &client,
                &job_id,
                &pointer,
                created_pointer,
                &gitdir,
                created_gitdir,
                &worktree_id,
            )
            .await;
            return Err(WorktreeError::IoWrite(format!(
                "failed to seed ScorpioFS worktree index: {error}"
            )));
        }
    } else {
        let guard = match DirGuard::change_to(&target) {
            Ok(guard) => guard,
            Err(error) => {
                rollback_scorpiofs_attach(
                    &client,
                    &job_id,
                    &pointer,
                    created_pointer,
                    &gitdir,
                    created_gitdir,
                    &worktree_id,
                )
                .await;
                return Err(WorktreeError::IoRead(format!(
                    "failed to enter ScorpioFS worktree '{}': {error}",
                    target.display()
                )));
            }
        };
        let unborn_branch = format!("scorpiofs/{worktree_id}");
        if let Err(error) = Head::update_result(Head::Branch(unborn_branch), None).await {
            drop(guard);
            rollback_scorpiofs_attach(
                &client,
                &job_id,
                &pointer,
                created_pointer,
                &gitdir,
                created_gitdir,
                &worktree_id,
            )
            .await;
            return Err(WorktreeError::IoWrite(format!(
                "failed to seed unborn ScorpioFS worktree HEAD: {error}"
            )));
        }
    }

    state.worktrees.push(WorktreeEntry {
        path: target.to_string_lossy().into_owned(),
        is_main: false,
        locked: false,
        lock_reason: None,
    });
    if let Err(error) = write_state(&state) {
        rollback_scorpiofs_attach(
            &client,
            &job_id,
            &pointer,
            created_pointer,
            &gitdir,
            created_gitdir,
            &worktree_id,
        )
        .await;
        return Err(error);
    }

    let mut desired_state = crate::internal::scorpiofs_backend::LibraScorpioFsState::load(&storage)
        .map_err(|error| WorktreeError::IoRead(error.to_string()))?;
    desired_state
        .mark_ready(&job_id, &mount, &worktree_id)
        .map_err(|error| WorktreeError::IoWrite(error.to_string()))?;
    if let Err(error) = desired_state.save(&storage) {
        rollback_scorpiofs_attach(
            &client,
            &job_id,
            &pointer,
            created_pointer,
            &gitdir,
            created_gitdir,
            &worktree_id,
        )
        .await;
        return Err(WorktreeError::IoWrite(error.to_string()));
    }

    Ok(ScorpioFsAttachOutput {
        path: target.to_string_lossy().into_owned(),
        worktree_id,
        mount_id: mount.mount_id,
        job_id,
        already_exists: false,
    })
}

async fn detach_scorpiofs_worktree(path: String) -> WorktreeResult<ScorpioFsDetachOutput> {
    let storage = util::storage_path();
    let _state_lock = crate::internal::scorpiofs_backend::ScorpioFsStateLock::acquire(&storage)
        .map_err(|error| WorktreeError::IoWrite(error.to_string()))?;
    let target = resolve_path(&path, "ScorpioFS worktree path")?;
    let mut state = load_state()?;
    let index = state
        .worktrees
        .iter()
        .position(|entry| Path::new(&entry.path) == target)
        .ok_or_else(|| WorktreeError::NoSuchWorktree { path: path.clone() })?;
    let entry = state.worktrees[index].clone();
    if entry.is_main {
        return Err(WorktreeError::MainWorktree {
            action: "detach",
            path: target.to_string_lossy().into_owned(),
        });
    }
    if entry.locked {
        return Err(WorktreeError::LockedWorktree {
            action: "detach",
            path: target.to_string_lossy().into_owned(),
        });
    }

    let gitdir = util::try_get_worktree_gitdir(Some(target.clone())).map_err(|error| {
        WorktreeError::IoRead(format!(
            "failed to resolve ScorpioFS worktree metadata for '{}': {error}",
            target.display()
        ))
    })?;
    let managed_root = storage.join("worktrees").join("scorpiofs");
    if !util::is_sub_path(&gitdir, &managed_root) {
        return Err(WorktreeError::InvalidTarget(format!(
            "worktree '{}' is not managed by the ScorpioFS backend",
            target.display()
        )));
    }
    let record = ScorpioFsBackendRecord::load(&gitdir)
        .map_err(|error| WorktreeError::IoRead(error.to_string()))?;
    let worktree_id = read_worktree_id(&gitdir)?;

    {
        let guard = DirGuard::change_to(&target).map_err(|error| {
            WorktreeError::IoRead(format!(
                "cannot enter ScorpioFS worktree '{}': {error}",
                target.display()
            ))
        })?;
        let staged = crate::command::status::changes_to_be_committed_safe()
            .await
            .map_err(|error| {
                WorktreeError::IoRead(format!("failed to inspect staged changes: {error}"))
            })?;
        let backend_changes = crate::internal::scorpiofs_backend::current_worktree_changes()
            .await
            .map_err(|error| {
                WorktreeError::IoRead(format!(
                    "failed to query ScorpioFS worktree changes before detach: {error}"
                ))
            })?;
        let unstaged = if let Some(changes) = backend_changes {
            let ignore_case = crate::utils::path_case::effective_ignore_case_for_dir_sync(&target)
                .map_err(|error| {
                    WorktreeError::IoRead(format!(
                        "failed to resolve ScorpioFS worktree path case policy: {error}"
                    ))
                })?;
            let (visible, _) =
                crate::command::status::changes_to_be_staged_split_for_paths_with_ignore_case(
                    ignore_case,
                    &changes.candidate_paths(),
                )
                .map_err(|error| {
                    WorktreeError::IoRead(format!("failed to inspect ScorpioFS changes: {error}"))
                })?;
            visible
        } else {
            crate::command::status::changes_to_be_staged().map_err(|error| {
                WorktreeError::IoRead(format!("failed to inspect unstaged changes: {error}"))
            })?
        };
        if !staged.is_empty() || !unstaged.is_empty() {
            return Err(WorktreeError::DirtyWorktree {
                path: target.to_string_lossy().into_owned(),
            });
        }
        drop(guard);
    }

    let mut desired_state = crate::internal::scorpiofs_backend::LibraScorpioFsState::load(&storage)
        .map_err(|error| WorktreeError::IoRead(error.to_string()))?;
    if let Some(desired) = desired_state.mounts.get_mut(&record.job_id) {
        desired.lifecycle = crate::internal::scorpiofs_backend::BackendLifecycle::Unmounting;
        desired.last_error = None;
    }
    desired_state
        .save(&storage)
        .map_err(|error| WorktreeError::IoWrite(error.to_string()))?;

    state.worktrees.remove(index);
    write_state(&state)?;

    let client = HttpScorpioFsClient::new(&record.endpoint)
        .map_err(|error| WorktreeError::IoRead(error.to_string()))?;
    let driver = ScorpioFsDriver::new(client);
    let backend_session = StorageMountSession {
        backend: WorktreeBackendKind::ScorpioFs,
        session_id: record.mount_id.clone(),
        mountpoint: target.clone(),
        cleanup_key: record.job_id.clone(),
        base_oid: record.base_oid.clone(),
    };
    if let Err(error) = driver.unmount(&backend_session).await {
        if let Some(desired) = desired_state.mounts.get_mut(&record.job_id) {
            desired.lifecycle = crate::internal::scorpiofs_backend::BackendLifecycle::Ready;
            desired.last_error = Some(error.to_string());
        }
        let _ = desired_state.save(&storage);
        state.worktrees.insert(index, entry);
        if let Err(restore_error) = write_state(&state) {
            return Err(WorktreeError::StateWrite {
                path: state_path(),
                source: io::Error::other(format!(
                    "ScorpioFS unmount failed ({error}); registry rollback also failed: \
                     {restore_error:?}"
                )),
            });
        }
        return Err(WorktreeError::IoWrite(error.to_string()));
    }

    let pointer = target.join(util::ROOT_DIR);
    match fs::remove_file(&pointer) {
        Ok(()) => {}
        Err(source) if source.kind() == io::ErrorKind::NotFound => {}
        Err(source) => {
            return Err(WorktreeError::IoWrite(format!(
                "ScorpioFS mount was removed but worktree pointer '{}' could not be deleted: \
                 {source}; run `libra worktree repair`",
                pointer.display()
            )));
        }
    }

    gc_worktree_scoped_rows(&worktree_id).await;
    fs::remove_dir_all(&gitdir).map_err(|source| {
        WorktreeError::IoWrite(format!(
            "ScorpioFS mount was removed but persistent worktree metadata '{}' could not be \
             deleted: {source}; run `libra worktree repair`",
            gitdir.display()
        ))
    })?;

    desired_state.mounts.remove(&record.job_id);
    if desired_state.mounts.is_empty() {
        stop_managed_scorpiofs_worker(&mut desired_state);
    }
    desired_state
        .save(&storage)
        .map_err(|error| WorktreeError::IoWrite(error.to_string()))?;

    Ok(ScorpioFsDetachOutput {
        path: target.to_string_lossy().into_owned(),
        worktree_id,
        job_id: record.job_id,
        unmounted: true,
        registry_removed: true,
    })
}

async fn rollback_scorpiofs_attach(
    client: &HttpScorpioFsClient,
    job_id: &str,
    pointer: &Path,
    created_pointer: bool,
    gitdir: &Path,
    created_gitdir: bool,
    worktree_id: &str,
) {
    if let Some(storage) = gitdir.ancestors().nth(3) {
        record_scorpiofs_attach_error(storage, job_id, "attach transaction rolled back");
    }
    if created_pointer {
        let _ = fs::remove_file(pointer);
    }
    if created_gitdir {
        let _ = fs::remove_dir_all(gitdir);
    }
    gc_worktree_scoped_rows(worktree_id).await;
    if let Err(error) = client.delete_by_job(job_id).await {
        tracing::warn!(job_id, error = %error, "failed to roll back ScorpioFS mount");
    }
}

fn record_scorpiofs_attach_error(storage: &Path, job_id: &str, error: impl Into<String>) {
    match crate::internal::scorpiofs_backend::LibraScorpioFsState::load(storage) {
        Ok(mut state) => {
            state.mark_error(job_id, error);
            if let Err(save_error) = state.save(storage) {
                tracing::warn!(job_id, error = %save_error, "failed to persist ScorpioFS error state");
            }
        }
        Err(load_error) => {
            tracing::warn!(job_id, error = %load_error, "failed to load Libra ScorpioFS state");
        }
    }
}

async fn ensure_managed_scorpiofs_worker(
    storage: &Path,
    config_path: &Path,
    timeout: Duration,
) -> WorktreeResult<String> {
    #[cfg(not(all(target_os = "linux", feature = "scorpiofs-direct")))]
    {
        let _ = (storage, config_path, timeout);
        return Err(WorktreeError::OperationBlocked(
            "direct ScorpioFS requires Linux and the scorpiofs-direct feature; use --endpoint for compatibility mode"
                .to_string(),
        ));
    }

    #[cfg(all(target_os = "linux", feature = "scorpiofs-direct"))]
    {
        use crate::internal::scorpiofs_backend::{
            LibraScorpioFsState, ManagedWorkerRecord, ScorpioFsControl,
        };

        let mut state = LibraScorpioFsState::load(storage)
            .map_err(|error| WorktreeError::IoRead(error.to_string()))?;
        if let Some(worker) = state.worker.as_ref() {
            if let Ok(client) = HttpScorpioFsClient::new(&worker.endpoint) {
                if client.service_info().await.is_ok() {
                    return Ok(worker.endpoint.clone());
                }
            }
            for desired in state.mounts.values_mut() {
                if desired.transport
                    == crate::internal::scorpiofs_backend::BackendTransport::ManagedCrate
                {
                    desired.lifecycle =
                        crate::internal::scorpiofs_backend::BackendLifecycle::RecoverableError;
                    desired.last_error = Some(
                        "Libra-owned ScorpioFS worker stopped; reattach this job to recover"
                            .to_string(),
                    );
                }
            }
            state.worker = None;
            state
                .save(storage)
                .map_err(|error| WorktreeError::IoWrite(error.to_string()))?;
        }

        let listener = std::net::TcpListener::bind(("127.0.0.1", 0)).map_err(|source| {
            WorktreeError::IoWrite(format!("failed to reserve ScorpioFS worker port: {source}"))
        })?;
        let port = listener
            .local_addr()
            .map_err(|source| {
                WorktreeError::IoRead(format!(
                    "failed to inspect reserved ScorpioFS worker port: {source}"
                ))
            })?
            .port();
        drop(listener);

        let runtime_key = {
            use std::hash::{Hash, Hasher};

            let mut hasher = std::collections::hash_map::DefaultHasher::new();
            storage.to_string_lossy().hash(&mut hasher);
            format!("{:016x}", hasher.finish())
        };
        let runtime_root = env::temp_dir().join("libra-scorpiofs").join(runtime_key);
        fs::create_dir_all(&runtime_root).map_err(|source| {
            WorktreeError::IoWrite(format!(
                "failed to create ScorpioFS runtime directory '{}': {source}",
                runtime_root.display()
            ))
        })?;
        let config_path = if config_path.is_absolute() {
            config_path.to_path_buf()
        } else {
            env::current_dir()
                .map_err(|source| {
                    WorktreeError::IoRead(format!(
                        "failed to resolve ScorpioFS config path: {source}"
                    ))
                })?
                .join(config_path)
        };
        let endpoint = format!("http://127.0.0.1:{port}");
        let log_root = storage.join("scorpiofs");
        fs::create_dir_all(&log_root).map_err(|source| {
            WorktreeError::IoWrite(format!(
                "failed to create ScorpioFS log directory '{}': {source}",
                log_root.display()
            ))
        })?;
        let log_path = log_root.join("worker.log");
        let stdout = fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&log_path)
            .map_err(|source| {
                WorktreeError::IoWrite(format!(
                    "failed to open ScorpioFS worker log '{}': {source}",
                    log_path.display()
                ))
            })?;
        let stderr = stdout.try_clone().map_err(|source| {
            WorktreeError::IoWrite(format!("failed to clone ScorpioFS worker log: {source}"))
        })?;
        let executable = env::current_exe().map_err(|source| {
            WorktreeError::IoRead(format!("failed to locate the Libra executable: {source}"))
        })?;
        let child = std::process::Command::new(executable)
            .arg("scorpiofs-worker")
            .arg("--config-path")
            .arg(&config_path)
            .arg("--bind")
            .arg(format!("127.0.0.1:{port}"))
            .arg("--upper-root")
            .arg(runtime_root.join("upper"))
            .arg("--cl-root")
            .arg(runtime_root.join("cl"))
            .arg("--mount-root")
            .arg(runtime_root.join("mounts"))
            .arg("--runtime-state-file")
            .arg(log_root.join("ignored-runtime-state.toml"))
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::from(stdout))
            .stderr(std::process::Stdio::from(stderr))
            .spawn()
            .map_err(|source| {
                WorktreeError::IoWrite(format!(
                    "failed to start Libra-owned ScorpioFS worker: {source}"
                ))
            })?;

        state.worker = Some(ManagedWorkerRecord {
            pid: child.id(),
            endpoint: endpoint.clone(),
            config_path: config_path.to_string_lossy().into_owned(),
        });
        state
            .save(storage)
            .map_err(|error| WorktreeError::IoWrite(error.to_string()))?;

        let client = HttpScorpioFsClient::new(&endpoint)
            .map_err(|error| WorktreeError::IoRead(error.to_string()))?;
        let deadline = tokio::time::Instant::now() + timeout;
        loop {
            if let Ok(service) = client.service_info().await {
                for capability in [
                    CAPABILITY_MOUNT_V1,
                    CAPABILITY_READY_V1,
                    CAPABILITY_CHANGES_V1,
                ] {
                    service
                        .require(capability)
                        .map_err(|error| WorktreeError::OperationBlocked(error.to_string()))?;
                }
                return Ok(endpoint);
            }
            if tokio::time::Instant::now() >= deadline {
                stop_managed_scorpiofs_worker(&mut state);
                let _ = state.save(storage);
                return Err(WorktreeError::IoRead(format!(
                    "Libra-owned ScorpioFS worker did not become ready; inspect '{}'",
                    log_path.display()
                )));
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
    }
}

fn stop_managed_scorpiofs_worker(
    state: &mut crate::internal::scorpiofs_backend::LibraScorpioFsState,
) {
    let Some(worker) = state.worker.take() else {
        return;
    };
    #[cfg(unix)]
    unsafe {
        libc::kill(worker.pid as i32, libc::SIGINT);
    }
}

fn attach_worktree_pointer(pointer: &Path, gitdir: &Path) -> io::Result<bool> {
    let expected = format!("gitdir: {}\n", gitdir.display());
    match fs::read_to_string(pointer) {
        Ok(existing) if existing == expected => Ok(false),
        Ok(_) => Err(io::Error::new(
            io::ErrorKind::AlreadyExists,
            format!(
                "worktree already contains a different .libra pointer at '{}'",
                pointer.display()
            ),
        )),
        Err(error) if error.kind() == io::ErrorKind::NotFound => {
            fs::write(pointer, expected)?;
            Ok(true)
        }
        Err(error) => Err(error),
    }
}

fn read_worktree_id(gitdir: &Path) -> WorktreeResult<String> {
    let path = gitdir.join("worktree_id");
    let worktree_id = fs::read_to_string(&path).map_err(|source| {
        WorktreeError::IoRead(format!(
            "failed to read ScorpioFS worktree id '{}': {source}",
            path.display()
        ))
    })?;
    let worktree_id = worktree_id.trim();
    if worktree_id.is_empty() {
        return Err(WorktreeError::IoRead(format!(
            "ScorpioFS worktree id '{}' is empty",
            path.display()
        )));
    }
    Ok(worktree_id.to_string())
}

fn scorpiofs_worktree_id(endpoint: &str, job_id: &str, remote_path: &str) -> String {
    let key = format!("{endpoint}\0{job_id}\0{remote_path}");
    let mut hash: u64 = 0xcbf29ce484222325;
    for byte in key.as_bytes() {
        hash ^= *byte as u64;
        hash = hash.wrapping_mul(0x100000001b3);
    }
    let label: String = job_id
        .chars()
        .map(|character| {
            if character.is_ascii_alphanumeric() {
                character
            } else {
                '-'
            }
        })
        .collect();
    format!("{label}-{hash:016x}")
}

fn render_scorpiofs_attach(result: &ScorpioFsAttachOutput, output: &OutputConfig) -> CliResult<()> {
    if output.is_json() {
        return emit_json_data("worktree.scorpiofs.attach", result, output);
    }
    if !output.quiet {
        println!("{}", result.path);
    }
    Ok(())
}

fn render_scorpiofs_detach(result: &ScorpioFsDetachOutput, output: &OutputConfig) -> CliResult<()> {
    if output.is_json() {
        return emit_json_data("worktree.scorpiofs.detach", result, output);
    }
    if !output.quiet {
        println!("Detached ScorpioFS worktree '{}'.", result.path);
    }
    Ok(())
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
    if let Ok(gitdir) = util::try_get_worktree_gitdir(Some(target.to_path_buf()))
        && let Ok(id) = fs::read_to_string(gitdir.join("worktree_id"))
    {
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
async fn gc_worktree_scoped_rows(worktree_id: &str) {
    use sea_orm::{ConnectionTrait, DbBackend, Statement};
    let db = crate::internal::db::get_db_conn_instance().await;
    let mut stmts = vec![
        "DELETE FROM reference WHERE worktree_id = ? AND kind = 'Head'",
        "DELETE FROM reflog WHERE worktree_id = ?",
        "DELETE FROM sequence_state WHERE worktree_id = ?",
        "DELETE FROM rebase_state WHERE worktree_id = ?",
    ];
    // `bisect_state` is created lazily on first bisect use — only purge when
    // the table exists (a DELETE on a missing table would log a spurious warn).
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
        if let Err(e) = db
            .execute(Statement::from_sql_and_values(
                DbBackend::Sqlite,
                sql,
                [worktree_id.into()],
            ))
            .await
        {
            tracing::warn!(worktree_id, error = %e, "failed to GC per-worktree rows");
        }
    }
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
    let state = load_state()?;
    let worktrees = state
        .worktrees
        .into_iter()
        .map(|w| {
            let worktree_id = resolve_entry_worktree_id(&w.path, w.is_main);
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
    let gitdir = util::try_get_worktree_gitdir(Some(PathBuf::from(path))).ok()?;
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
        .worktrees
        .iter()
        .position(|w| Path::new(&w.path) == src_path)
        .ok_or(WorktreeError::NoSuchWorktree { path: src })?;

    if state.worktrees[index].is_main {
        return Err(WorktreeError::MainWorktree {
            action: "move",
            path: src_path.to_string_lossy().to_string(),
        });
    }
    if state.worktrees[index].locked {
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

    let old_path = state.worktrees[index].path.clone();
    state.worktrees[index].path = dest_path.to_string_lossy().to_string();
    if let Err(e) = write_state(&state) {
        state.worktrees[index].path = old_path;
        return Err(e);
    }

    if let Err(e) = fs::rename(&src_path, &dest_path) {
        state.worktrees[index].path = old_path;
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
fn prune_worktrees() -> WorktreeResult<WorktreePruneOutput> {
    let mut state = load_state()?;
    let to_prune: Vec<_> = state
        .worktrees
        .iter()
        .filter(|w| {
            let path = Path::new(&w.path);
            !path.exists() && !w.is_main && !w.locked
        })
        .map(|w| w.path.clone())
        .collect();

    if !to_prune.is_empty() {
        state.worktrees.retain(|w| {
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
    let mut state = load_state()?;
    let target = resolve_path(&path, "worktree path")?;

    let index = state
        .worktrees
        .iter()
        .position(|w| Path::new(&w.path) == target)
        .ok_or(WorktreeError::NoSuchWorktree { path })?;

    let entry = &state.worktrees[index];
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
        let staged = crate::command::status::changes_to_be_committed_safe()
            .await
            .map_err(|e| {
                WorktreeError::IoRead(format!("failed to inspect worktree status: {e}"))
            })?;
        let unstaged = crate::command::status::changes_to_be_staged().map_err(|e| {
            WorktreeError::IoRead(format!("failed to inspect worktree status: {e}"))
        })?;
        if !staged.is_empty() || !unstaged.is_empty() {
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
        gc_worktree_scoped_rows(&id).await;
    }
    state.worktrees.remove(index);
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
fn repair_worktrees() -> WorktreeResult<WorktreeRepairOutput> {
    let mut state = load_state()?;
    let mut changed = false;

    let mut seen = HashSet::<PathBuf>::new();
    state.worktrees.retain(|w| {
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
