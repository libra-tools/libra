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

/// A single worktree entry persisted in `worktrees.json`.
///
/// `path` is always stored as a canonical absolute path.
///
/// `pub(crate)` so the service dirty-mark gate deserializes the registry with
/// this exact schema (a drifting mirror would fail open on missing fields).
#[derive(Serialize, Deserialize, Debug, Clone)]
pub(crate) struct WorktreeEntry {
    path: String,
    is_main: bool,
    locked: bool,
    lock_reason: Option<String>,
}

/// Top-level state persisted in `worktrees.json`.
///
/// The state contains the main worktree and any number of linked worktrees.
#[derive(Serialize, Deserialize, Debug, Default, Clone)]
pub(crate) struct WorktreeState {
    pub(crate) worktrees: Vec<WorktreeEntry>,
}

impl WorktreeState {
    /// True when the registry holds exactly the main worktree entry — the
    /// only shape under which a scope-less service dirty-mark may default
    /// to the main scope. Anything else (empty, multi-entry, or a sole
    /// non-main entry) is indistinguishable from corruption or a
    /// multi-worktree layout and must fail closed.
    pub(crate) fn is_single_main(&self) -> bool {
        matches!(self.worktrees.as_slice(), [entry] if entry.is_main)
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
/// RAII guard over the worktree REGISTRY mutation lock (`worktrees.lock` in
/// the common storage). Serializes every registry mutator's
/// load → check → mutate → write sequence across processes: without it, a
/// concurrent `worktree add`'s strict pre-seed sweep could delete rows
/// another add just seeded for the same deterministic instance id, and two
/// load/modify/write registry updates could drop each other's entries. The
/// flock is BLOCKING (concurrent mutators queue rather than fail) and
/// released on drop (or process exit). Read-only paths (`list`) stay
/// lock-free.
struct RegistryLockGuard {
    #[allow(dead_code)] // held for its flock; released on drop
    file: fs::File,
}

#[cfg(unix)]
impl Drop for RegistryLockGuard {
    fn drop(&mut self) {
        use std::os::unix::io::AsRawFd;
        unsafe {
            libc::flock(self.file.as_raw_fd(), libc::LOCK_UN);
        }
    }
}

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
    #[cfg(unix)]
    {
        use std::os::unix::io::AsRawFd;
        let rc = unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX) };
        if rc != 0 {
            return Err(WorktreeError::IoWrite(format!(
                "cannot lock the worktree registry '{}': {}",
                lock_path.display(),
                std::io::Error::last_os_error()
            )));
        }
    }
    Ok(RegistryLockGuard { file })
}

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
/// Each pruned worktree's scoped DB rows are GC'd like `remove` does — the
/// directory is gone, so layer ownership is purged too (nothing left on disk
/// to guard); leaked rows would otherwise be re-inherited by a worktree
/// re-created at the same path (deterministic instance id).
async fn prune_worktrees() -> WorktreeResult<WorktreePruneOutput> {
    let _registry_lock = acquire_registry_lock()?;
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
        for path in &to_prune {
            if let Some(id) = resolve_worktree_id(Path::new(path)) {
                gc_worktree_scoped_rows(&id, true).await;
            }
        }
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
    let _registry_lock = acquire_registry_lock()?;
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
    let _registry_lock = acquire_registry_lock()?;
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
