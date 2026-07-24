//! `libra worktree` command implementation for mounting worktree overlays.
//!
//! Boundary: this command is Unix-only and focuses on FUSE mount lifecycle; generic
//! worktree management remains in `command::worktree`. Worktree-fuse command tests
//! cover argument parsing and unsupported-platform behavior.

use std::{
    collections::HashMap,
    fs, io,
    path::{Path, PathBuf},
    sync::{Mutex, OnceLock},
    time::Duration,
};

use clap::{Parser, Subcommand};
use libfuse_fs::overlayfs::{OverlayArgs, mount_fs};
use rfuse3::raw::MountHandle;
use serde::{Deserialize, Serialize};
use tokio::time::{Instant, sleep, timeout};
use uuid::Uuid;

#[path = "worktree.rs"]
mod legacy;

// Re-export the shared `--help` examples constant so the cli definition can
// reference `command::worktree::WORKTREE_EXAMPLES` regardless of whether the
// `worktree-fuse` feature routed compilation through this file or directly
// through `worktree.rs`.
pub use legacy::WORKTREE_EXAMPLES;
pub(crate) use legacy::{WorktreeError, WorktreeState, acquire_registry_lock, run_list_worktrees};

const FUSE_MOUNT_TIMEOUT: Duration = Duration::from_secs(15);
const FUSE_UNMOUNT_TIMEOUT: Duration = Duration::from_secs(15);
const FUSE_HEALTH_TIMEOUT: Duration = Duration::from_secs(5);

use crate::{
    command::{
        branch,
        restore::{self, RestoreArgs},
    },
    internal::head::Head,
    utils::{
        error::{CliError, CliResult, StableErrorCode},
        fuse as fuse_utils,
        output::{OutputConfig, emit_json_data},
        util,
    },
};

#[derive(Parser, Debug)]
pub struct WorktreeArgs {
    #[clap(subcommand)]
    pub command: WorktreeSubcommand,
}

#[derive(Subcommand, Debug)]
pub enum WorktreeSubcommand {
    Add {
        /// Filesystem path at which to create the new worktree.
        path: String,
        /// Existing branch to check out, or a commit-ish for a detached
        /// HEAD (canonical non-FUSE surface, W3 §C.7).
        target: Option<String>,
        /// Detach HEAD in the new worktree even for a branch target
        /// (non-FUSE surface).
        #[clap(long)]
        detach: bool,
        #[clap(short = 'f', long, help = "Use FUSE overlay worktree mode (Unix only)")]
        fuse: bool,
        #[clap(long, help = "Checkout this branch in the new worktree (FUSE mode)")]
        branch: Option<String>,
        #[clap(
            short = 'b',
            long = "create-branch",
            help = "Create and checkout a new branch"
        )]
        create_branch: Option<String>,
        #[clap(
            long,
            conflicts_with = "create_branch",
            help = "Base ref for --create-branch (FUSE mode)"
        )]
        from: Option<String>,
        #[clap(long, help = "Use privileged mount mode")]
        privileged: bool,
        #[clap(long, help = "Allow other users to access the mounted worktree")]
        allow_other: bool,
    },
    List {
        /// Emit a stable, machine-readable porcelain format (one attribute per
        /// line, blank line between worktrees).
        #[clap(long)]
        porcelain: bool,
    },
    Lock {
        /// Filesystem path of the worktree to lock.
        path: String,
        /// Optional explanation shown in `worktree list` while the worktree is locked.
        #[clap(long)]
        reason: Option<String>,
    },
    Unlock {
        /// Filesystem path of the worktree to unlock.
        path: String,
    },
    Move {
        /// Current filesystem path of the worktree.
        src: String,
        /// New filesystem path for the worktree.
        dest: String,
    },
    Prune,
    Remove {
        /// Filesystem path of the worktree to unregister.
        path: String,
        #[clap(long, help = "Also delete the worktree directory on disk")]
        delete_dir: bool,
    },
    #[clap(alias = "unmount", about = "Unmount a FUSE worktree mountpoint")]
    Umount {
        /// Filesystem path of the FUSE mountpoint or its task worktree root.
        path: String,
        #[clap(
            long,
            help = "Remove the Libra task worktree root after unmounting its workspace mountpoint"
        )]
        cleanup: bool,
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
        /// layout (W3-s3 §C.6.2).
        #[clap(long)]
        migrate_layout: bool,
        /// With --migrate-layout: report what would be migrated, write
        /// nothing.
        #[clap(long, requires = "migrate_layout")]
        dry_run: bool,
    },
}

#[derive(Serialize, Deserialize, Debug, Clone)]
struct FuseWorktreeEntry {
    path: String,
    branch: String,
    upper_dir: String,
    lower_dirs: Vec<String>,
    locked: bool,
    lock_reason: Option<String>,
    privileged: bool,
    allow_other: bool,
}

#[derive(Serialize, Deserialize, Debug, Default, Clone)]
struct FuseWorktreeState {
    worktrees: Vec<FuseWorktreeEntry>,
}

#[derive(Debug, Serialize)]
struct WorktreeUmountOutput {
    mountpoint: String,
    unmounted: bool,
    cleanup_requested: bool,
    cleanup_root: Option<String>,
    cleanup_root_removed: bool,
}

#[derive(Debug)]
enum FuseUmountError {
    InvalidTarget(String),
    IoRead(String),
    IoWrite(String),
}

impl FuseUmountError {
    fn stable_code(&self) -> StableErrorCode {
        match self {
            Self::InvalidTarget(_) => StableErrorCode::CliInvalidTarget,
            Self::IoRead(_) => StableErrorCode::IoReadFailed,
            Self::IoWrite(_) => StableErrorCode::IoWriteFailed,
        }
    }

    fn into_cli_error(self) -> CliError {
        CliError::fatal(self.to_string()).with_stable_code(self.stable_code())
    }
}

impl std::fmt::Display for FuseUmountError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::InvalidTarget(message) | Self::IoRead(message) | Self::IoWrite(message) => {
                f.write_str(message)
            }
        }
    }
}

impl std::error::Error for FuseUmountError {}

trait IntoMountHandleResult {
    fn into_mount_handle_result(self) -> io::Result<MountHandle>;
}

impl IntoMountHandleResult for MountHandle {
    fn into_mount_handle_result(self) -> io::Result<MountHandle> {
        Ok(self)
    }
}

impl<E> IntoMountHandleResult for Result<MountHandle, E>
where
    E: std::fmt::Display,
{
    fn into_mount_handle_result(self) -> io::Result<MountHandle> {
        self.map_err(|e| io::Error::other(format!("failed to mount FUSE worktree: {e}")))
    }
}

fn active_mounts() -> &'static Mutex<HashMap<String, MountHandle>> {
    static ACTIVE: OnceLock<Mutex<HashMap<String, MountHandle>>> = OnceLock::new();
    ACTIVE.get_or_init(|| Mutex::new(HashMap::new()))
}

fn fuse_state_lock() -> &'static Mutex<()> {
    static STATE_LOCK: OnceLock<Mutex<()>> = OnceLock::new();
    STATE_LOCK.get_or_init(|| Mutex::new(()))
}

/// Executes the worktree command in user-facing mode.
///
/// This wrapper delegates to [`execute_safe`] and prints any returned
/// [`CliError`] to stderr instead of propagating it to the caller.
/// Use this entry when the command is invoked from normal CLI dispatch.
pub async fn execute(args: WorktreeArgs) {
    if let Err(e) = execute_safe(args, &OutputConfig::default()).await {
        e.print_stderr();
    }
}

/// Executes the worktree command and returns structured errors.
///
/// Behavior summary:
/// - Ensures the current directory is a Libra repository.
/// - Routes `add --fuse`, `list`, `lock`, `unlock`, `remove`, `prune`, and
///   `repair` through FUSE-aware logic.
/// - Falls back to legacy worktree implementation for non-FUSE paths and
///   operations not implemented in the FUSE layer.
/// - Validates that `--branch`/`--create-branch`/`--from` are used only with
///   `--fuse`.
///
/// Returns [`CliResult<()>`] so callers can decide whether to bubble up,
/// map, or render failures.
pub async fn execute_safe(args: WorktreeArgs, output: &OutputConfig) -> CliResult<()> {
    let command = args.command;
    if !matches!(&command, WorktreeSubcommand::Umount { .. }) {
        util::require_repo().map_err(|_| CliError::repo_not_found())?;
        // §C.7 ordering (same as the legacy entry point): apply pending
        // migrations — including the registry-v2 capability marker — before
        // any registry IO, and refuse a future-schema database gracefully.
        // The fuse-native list path reads the legacy registry directly, so
        // the guarantee must hold here too, not only via legacy delegation.
        crate::internal::db::get_db_conn_instance_for_path(&crate::utils::path::database())
            .await
            .map_err(|source| {
                CliError::fatal(format!(
                    "cannot open the repository database before touching the worktree \
                     registry: {source}"
                ))
                .with_stable_code(StableErrorCode::IoReadFailed)
            })?;
        // Bare boundary (config-first, same as legacy): refused before any
        // registry IO.
        legacy::reject_bare_repository().await?;
    }

    match command {
        WorktreeSubcommand::Add {
            path,
            target,
            detach,
            fuse,
            branch,
            create_branch,
            from,
            privileged,
            allow_other,
        } => {
            if !fuse {
                // --branch/--from stay FUSE-only; the canonical surface uses
                // the positional target and -b/--create-branch (forwarded).
                if branch.is_some() || from.is_some() {
                    return Err(CliError::command_usage("--branch/--from require --fuse"));
                }
                legacy::execute_safe(
                    legacy::WorktreeArgs {
                        command: legacy::WorktreeSubcommand::Add {
                            path,
                            target,
                            detach,
                            new_branch: create_branch,
                        },
                    },
                    output,
                )
                .await
            } else {
                if target.is_some() || detach {
                    return Err(CliError::command_usage(
                        "the positional <BRANCH-OR-COMMIT> and --detach are not supported \
                         with --fuse; use --branch/--create-branch/--from",
                    ));
                }
                add_fuse_worktree(path, branch, create_branch, from, privileged, allow_other)
                    .await
                    .map_err(|e| CliError::fatal(e.to_string()))
            }
        }
        WorktreeSubcommand::List { porcelain } => list_all_worktrees(output, porcelain).await,
        WorktreeSubcommand::Lock { path, reason } => {
            if lock_fuse_worktree(&path, reason.clone())
                .map_err(|e| CliError::fatal(e.to_string()))?
            {
                return Ok(());
            }
            legacy::execute_safe(
                legacy::WorktreeArgs {
                    command: legacy::WorktreeSubcommand::Lock { path, reason },
                },
                output,
            )
            .await
        }
        WorktreeSubcommand::Unlock { path } => {
            if unlock_fuse_worktree(&path).map_err(|e| CliError::fatal(e.to_string()))? {
                return Ok(());
            }
            legacy::execute_safe(
                legacy::WorktreeArgs {
                    command: legacy::WorktreeSubcommand::Unlock { path },
                },
                output,
            )
            .await
        }
        WorktreeSubcommand::Remove { path, delete_dir } => {
            if remove_fuse_worktree(&path)
                .await
                .map_err(|e| CliError::fatal(e.to_string()))?
            {
                return Ok(());
            }
            legacy::execute_safe(
                legacy::WorktreeArgs {
                    command: legacy::WorktreeSubcommand::Remove { path, delete_dir },
                },
                output,
            )
            .await
        }
        WorktreeSubcommand::Umount { path, cleanup } => {
            let result = umount_fuse_path(path, cleanup)
                .await
                .map_err(FuseUmountError::into_cli_error)?;
            render_umount_fuse_path(&result, output)
        }
        WorktreeSubcommand::Move { src, dest } => {
            legacy::execute_safe(
                legacy::WorktreeArgs {
                    command: legacy::WorktreeSubcommand::Move { src, dest },
                },
                output,
            )
            .await
        }
        WorktreeSubcommand::Prune => {
            prune_fuse_worktrees().map_err(|e| CliError::fatal(e.to_string()))?;
            legacy::execute_safe(
                legacy::WorktreeArgs {
                    command: legacy::WorktreeSubcommand::Prune,
                },
                output,
            )
            .await
        }
        WorktreeSubcommand::Repair {
            path,
            migrate_layout,
            dry_run,
        } => {
            // Core registry first: a corrupt/zero-byte core registry must
            // fail the command BEFORE any fuse metadata is mutated.
            let run_fuse_repair = path.is_none() && !migrate_layout;
            legacy::execute_safe(
                legacy::WorktreeArgs {
                    command: legacy::WorktreeSubcommand::Repair {
                        path,
                        migrate_layout,
                        dry_run,
                    },
                },
                output,
            )
            .await?;
            if run_fuse_repair {
                repair_fuse_worktrees().map_err(|e| CliError::fatal(e.to_string()))?;
            }
            Ok(())
        }
    }
}

fn canonicalize_like_worktree<P: AsRef<Path>>(path: P) -> io::Result<PathBuf> {
    let p = path.as_ref();
    let joined = if p.is_absolute() {
        p.to_path_buf()
    } else {
        util::cur_dir().join(p)
    };
    let normalized = fuse_utils::normalize_abs_path(&joined);
    if normalized.exists() {
        fs::canonicalize(normalized)
    } else {
        Ok(normalized)
    }
}

fn fuse_state_path() -> PathBuf {
    util::storage_path().join("worktrees-fuse.json")
}

fn fuse_data_root() -> PathBuf {
    util::storage_path().join("worktrees-fuse")
}

fn load_fuse_state() -> io::Result<FuseWorktreeState> {
    let path = fuse_state_path();
    if !path.exists() {
        return Ok(FuseWorktreeState::default());
    }
    let data = fs::read(&path)?;
    if data.is_empty() {
        return Ok(FuseWorktreeState::default());
    }
    let state: FuseWorktreeState = serde_json::from_slice(&data)
        .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e.to_string()))?;
    Ok(state)
}

fn save_fuse_state(state: &FuseWorktreeState) -> io::Result<()> {
    let path = fuse_state_path();
    let tmp = path.with_extension("json.tmp");
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    let data = serde_json::to_vec_pretty(state).map_err(|e| io::Error::other(e.to_string()))?;
    fs::write(&tmp, data)?;
    #[cfg(windows)]
    {
        if path.exists() {
            let _ = fs::remove_file(&path);
        }
    }
    fs::rename(tmp, path)
}

async fn verify_mount_health(mountpoint: &Path) -> io::Result<()> {
    let mountpoint = mountpoint.to_path_buf();
    let deadline = Instant::now() + FUSE_HEALTH_TIMEOUT;

    loop {
        let probe_path = mountpoint.clone();
        match timeout(
            FUSE_HEALTH_TIMEOUT,
            tokio::task::spawn_blocking(move || fs::read_dir(probe_path)),
        )
        .await
        {
            Ok(Ok(Ok(_))) => return Ok(()),
            Ok(Ok(Err(err))) => {
                if Instant::now() >= deadline {
                    return Err(io::Error::new(
                        io::ErrorKind::TimedOut,
                        format!(
                            "FUSE mount did not become ready within {} seconds: {err}",
                            FUSE_HEALTH_TIMEOUT.as_secs()
                        ),
                    ));
                }
            }
            Ok(Err(err)) => {
                return Err(io::Error::other(format!(
                    "FUSE mount health check task failed: {err}"
                )));
            }
            Err(_) => {
                return Err(io::Error::new(
                    io::ErrorKind::TimedOut,
                    format!(
                        "FUSE mount health check timed out after {} seconds",
                        FUSE_HEALTH_TIMEOUT.as_secs()
                    ),
                ));
            }
        }

        sleep(Duration::from_millis(50)).await;
    }
}

async fn add_fuse_worktree(
    path: String,
    branch_name: Option<String>,
    create_branch_name: Option<String>,
    from: Option<String>,
    privileged: bool,
    allow_other: bool,
) -> io::Result<()> {
    let storage = util::storage_path();
    let target = canonicalize_like_worktree(&path)?;

    if util::is_sub_path(&target, &storage) {
        return Err(io::Error::other(
            "worktree path cannot be inside .libra storage",
        ));
    }

    let target_exists = target.exists();
    if target_exists && !target.is_dir() {
        return Err(io::Error::other("target exists and is not a directory"));
    }
    if target_exists && fs::read_dir(&target)?.next().transpose()?.is_some() {
        return Err(io::Error::other("target directory exists and is not empty"));
    }

    let state = load_fuse_state()?;
    if state.worktrees.iter().any(|w| Path::new(&w.path) == target) {
        println!("worktree already exists at {}", target.display());
        return Ok(());
    }

    if let Some(new_branch) = create_branch_name.as_ref() {
        branch::create_branch_safe(new_branch.clone(), from.clone())
            .await
            .map_err(|e| io::Error::other(format!("failed to create branch: {e}")))?;
    }

    let checkout_branch = if let Some(name) = create_branch_name.clone().or(branch_name) {
        name
    } else {
        match Head::current().await {
            Head::Branch(name) => name,
            _ => "HEAD".to_string(),
        }
    };

    let mut created_target = false;
    if !target.exists() {
        fs::create_dir_all(&target)?;
        created_target = true;
    }

    let id = Uuid::new_v4().simple().to_string();
    let upper_dir = fuse_data_root().join(id).join("upper");
    fs::create_dir_all(&upper_dir)?;

    let lower_dir = canonicalize_like_worktree(util::working_dir())?;
    let mount_args = OverlayArgs {
        mountpoint: &target,
        upperdir: &upper_dir,
        lowerdir: vec![lower_dir.clone()],
        privileged,
        mapping: None::<&str>,
        name: Some("libra-worktree-fuse"),
        allow_other,
    };
    let mount_handle = timeout(FUSE_MOUNT_TIMEOUT, mount_fs(mount_args))
        .await
        .map_err(|_| {
            io::Error::new(
                io::ErrorKind::TimedOut,
                format!(
                    "FUSE mount timed out after {} seconds for {}",
                    FUSE_MOUNT_TIMEOUT.as_secs(),
                    target.display()
                ),
            )
        })?
        .into_mount_handle_result()?;

    if let Err(err) = verify_mount_health(&target).await {
        let _ = timeout(FUSE_UNMOUNT_TIMEOUT, mount_handle.unmount()).await;
        let _ = fs::remove_dir_all(&upper_dir);
        if created_target {
            let _ = fs::remove_dir_all(&target);
        }
        return Err(io::Error::other(format!(
            "FUSE mount health check failed: {err}"
        )));
    }

    let mut rollback_needed = true;
    if Head::current_commit().await.is_some()
        && let Err(err) = restore::execute_checked(RestoreArgs {
            overlay: false,
            no_overlay: false,
            ours: false,
            theirs: false,
            ignore_unmerged: false,
            merge: false,
            conflict: None,
            pathspec: vec![target.to_string_lossy().to_string()],
            source: Some(checkout_branch.clone()),
            worktree: true,
            staged: false,
            pathspec_from_file: None,
            pathspec_file_nul: false,
            no_progress: false,
        })
        .await
    {
        let _ = timeout(FUSE_UNMOUNT_TIMEOUT, mount_handle.unmount()).await;
        let _ = fs::remove_dir_all(&upper_dir);
        if created_target {
            let _ = fs::remove_dir_all(&target);
        }
        return Err(io::Error::other(format!(
            "failed to populate FUSE worktree from '{}': {err}",
            checkout_branch
        )));
    }

    if let Ok(mut mounts) = active_mounts().lock() {
        mounts.insert(target.to_string_lossy().to_string(), mount_handle);
    } else {
        rollback_needed = false;
    }

    let save_result = {
        let _guard = fuse_state_lock()
            .lock()
            .map_err(|_| io::Error::other("fuse state lock poisoned"))?;
        let mut current = load_fuse_state()?;
        if current
            .worktrees
            .iter()
            .any(|w| Path::new(&w.path) == target)
        {
            Ok(())
        } else {
            current.worktrees.push(FuseWorktreeEntry {
                path: target.to_string_lossy().to_string(),
                branch: checkout_branch,
                upper_dir: upper_dir.to_string_lossy().to_string(),
                lower_dirs: vec![lower_dir.to_string_lossy().to_string()],
                locked: false,
                lock_reason: None,
                privileged,
                allow_other,
            });
            save_fuse_state(&current)
        }
    };

    if let Err(err) = save_result {
        if rollback_needed {
            let _ = unmount_path(&target).await;
        }
        let _ = fs::remove_dir_all(&upper_dir);
        if created_target {
            let _ = fs::remove_dir_all(&target);
        }
        return Err(err);
    }

    println!("{}", target.display());
    Ok(())
}

async fn list_all_worktrees(output: &OutputConfig, porcelain: bool) -> CliResult<()> {
    let mut result = legacy::run_list_worktrees().map_err(legacy::WorktreeError::into_cli_error)?;
    let state = load_fuse_state().map_err(fuse_state_read_error)?;
    if output.is_json() {
        for entry in state.worktrees {
            result.worktrees.push(legacy::WorktreeListEntry {
                kind: "worktree",
                worktree_id: legacy::resolve_entry_worktree_id(&entry.path, false),
                path: entry.path.clone(),
                is_main: false,
                locked: entry.locked,
                lock_reason: entry.lock_reason.clone(),
                exists: Path::new(&entry.path).exists(),
                state: "active",
                layout: "task-fuse",
            });
        }
        return emit_json_data("worktree.list", &result, output);
    }
    if output.quiet {
        return Ok(());
    }

    if porcelain {
        // Combine the registry worktrees with the FUSE-mounted ones, then emit
        // the shared porcelain format.
        let mut all = result.worktrees;
        for entry in &state.worktrees {
            all.push(legacy::WorktreeListEntry {
                kind: "worktree",
                worktree_id: legacy::resolve_entry_worktree_id(&entry.path, false),
                path: entry.path.clone(),
                is_main: false,
                locked: entry.locked,
                lock_reason: entry.lock_reason.clone(),
                exists: Path::new(&entry.path).exists(),
                state: "active",
                layout: "task-fuse",
            });
        }
        print!("{}", legacy::format_worktree_porcelain(&all).await);
        return Ok(());
    }

    for entry in result.worktrees {
        let mut line = String::new();
        if entry.is_main {
            line.push_str("main ");
        } else {
            line.push_str("worktree ");
        }
        line.push_str(&entry.path);
        if entry.locked {
            line.push_str(" [locked");
            if let Some(reason) = entry.lock_reason.as_ref()
                && !reason.is_empty()
            {
                line.push_str(": ");
                line.push_str(reason);
            }
            line.push(']');
        }
        println!("{}", line);
    }

    for entry in state.worktrees {
        let mounted = if fuse_utils::is_mount_active(Path::new(&entry.path)) {
            "mounted"
        } else {
            "unmounted"
        };
        let mut line = format!(
            "worktree {} [branch: {}] [fuse: {}]",
            entry.path, entry.branch, mounted
        );
        if entry.locked {
            line.push_str(" [locked");
            if let Some(reason) = entry.lock_reason.as_ref()
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

fn fuse_state_read_error(source: io::Error) -> CliError {
    let path = fuse_state_path();
    let message = if source.kind() == io::ErrorKind::InvalidData {
        format!(
            "FUSE worktree state '{}' is corrupt: {source}",
            path.display()
        )
    } else {
        format!(
            "failed to read FUSE worktree state '{}': {source}",
            path.display()
        )
    };
    let code = if source.kind() == io::ErrorKind::InvalidData {
        StableErrorCode::RepoCorrupt
    } else {
        StableErrorCode::IoReadFailed
    };
    CliError::fatal(message).with_stable_code(code)
}

fn lock_fuse_worktree(path: &str, reason: Option<String>) -> io::Result<bool> {
    let _state_guard = fuse_state_lock()
        .lock()
        .map_err(|_| io::Error::other("fuse state lock poisoned"))?;
    let target = canonicalize_like_worktree(path)?;
    let mut state = load_fuse_state()?;
    let mut changed = false;
    let mut found = false;
    for worktree in &mut state.worktrees {
        if Path::new(&worktree.path) == target {
            found = true;
            if !worktree.locked {
                worktree.locked = true;
                worktree.lock_reason = reason;
                changed = true;
            }
            break;
        }
    }
    if found && changed {
        save_fuse_state(&state)?;
    }
    Ok(found)
}

fn unlock_fuse_worktree(path: &str) -> io::Result<bool> {
    let _state_guard = fuse_state_lock()
        .lock()
        .map_err(|_| io::Error::other("fuse state lock poisoned"))?;
    let target = canonicalize_like_worktree(path)?;
    let mut state = load_fuse_state()?;
    let mut changed = false;
    let mut found = false;
    for worktree in &mut state.worktrees {
        if Path::new(&worktree.path) == target {
            found = true;
            if worktree.locked {
                worktree.locked = false;
                worktree.lock_reason = None;
                changed = true;
            }
            break;
        }
    }
    if found && changed {
        save_fuse_state(&state)?;
    }
    Ok(found)
}

async fn remove_fuse_worktree(path: &str) -> io::Result<bool> {
    let target = canonicalize_like_worktree(path)?;
    let entry = {
        let _state_guard = fuse_state_lock()
            .lock()
            .map_err(|_| io::Error::other("fuse state lock poisoned"))?;
        let state = load_fuse_state()?;
        let Some(found) = state
            .worktrees
            .iter()
            .find(|w| Path::new(&w.path) == target)
            .cloned()
        else {
            return Ok(false);
        };
        found
    };

    if entry.locked {
        return Err(io::Error::other("cannot remove locked worktree"));
    }

    if let Err(err) = unmount_path(&target).await
        && fuse_utils::is_mount_active(&target)
    {
        return Err(err);
    }
    if Path::new(&entry.upper_dir).exists() {
        fs::remove_dir_all(&entry.upper_dir)?;
    }
    {
        let _state_guard = fuse_state_lock()
            .lock()
            .map_err(|_| io::Error::other("fuse state lock poisoned"))?;
        let mut state = load_fuse_state()?;
        if let Some(index) = state
            .worktrees
            .iter()
            .position(|w| Path::new(&w.path) == target)
        {
            state.worktrees.remove(index);
            save_fuse_state(&state)?;
        }
    }
    Ok(true)
}

fn prune_fuse_worktrees() -> io::Result<()> {
    let _state_guard = fuse_state_lock()
        .lock()
        .map_err(|_| io::Error::other("fuse state lock poisoned"))?;
    let mut state = load_fuse_state()?;
    let before = state.worktrees.len();
    state.worktrees.retain(|entry| {
        let path = Path::new(&entry.path);
        path.exists() || entry.locked
    });
    if state.worktrees.len() != before {
        save_fuse_state(&state)?;
    }
    Ok(())
}

fn repair_fuse_worktrees() -> io::Result<()> {
    let _state_guard = fuse_state_lock()
        .lock()
        .map_err(|_| io::Error::other("fuse state lock poisoned"))?;
    let mut state = load_fuse_state()?;
    let mut seen = std::collections::HashSet::<PathBuf>::new();
    let before = state.worktrees.len();
    state.worktrees.retain(|entry| {
        let p = PathBuf::from(&entry.path);
        seen.insert(p)
    });
    if state.worktrees.len() != before {
        save_fuse_state(&state)?;
    }
    Ok(())
}

async fn unmount_path(path: &Path) -> io::Result<()> {
    let path = fuse_utils::normalize_abs_path(path);
    let handle = active_mounts()
        .lock()
        .ok()
        .and_then(|mut mounts| mounts.remove(&path.to_string_lossy().to_string()));
    if let Some(handle) = handle {
        match timeout(FUSE_UNMOUNT_TIMEOUT, handle.unmount()).await {
            Ok(Ok(())) => return Ok(()),
            Ok(Err(e)) => {
                let ioe: io::Error = e;
                if matches!(
                    ioe.raw_os_error(),
                    Some(libc::ENOTCONN | libc::EINVAL | libc::ENOENT | libc::EPERM)
                ) {
                    if !fuse_utils::is_mount_active(&path) {
                        return Ok(());
                    }
                } else {
                    return Err(io::Error::other(format!(
                        "failed to unmount FUSE worktree: {ioe}"
                    )));
                }
            }
            Err(_) => {
                if !fuse_utils::is_mount_active(&path) {
                    return Ok(());
                }
                return Err(io::Error::new(
                    io::ErrorKind::TimedOut,
                    format!(
                        "failed to unmount FUSE worktree {} within {} seconds",
                        path.display(),
                        FUSE_UNMOUNT_TIMEOUT.as_secs()
                    ),
                ));
            }
        }
    }

    fuse_utils::force_unmount_path(&path)
}

async fn umount_fuse_path(
    path: String,
    cleanup: bool,
) -> Result<WorktreeUmountOutput, FuseUmountError> {
    let target = canonicalize_like_worktree(&path).map_err(|source| {
        FuseUmountError::IoRead(format!(
            "failed to resolve FUSE worktree path '{}': {source}",
            path
        ))
    })?;
    let mountpoint = fuse_utils::resolve_task_worktree_mountpoint_arg(&target);
    unmount_path(&mountpoint).await.map_err(|source| {
        FuseUmountError::IoWrite(format!(
            "failed to unmount FUSE path {}: {source}",
            mountpoint.display()
        ))
    })?;

    let mut cleanup_root = None;
    let mut cleanup_root_removed = false;
    if cleanup {
        let root = fuse_utils::fuse_task_worktree_cleanup_root(&mountpoint).ok_or_else(|| {
            FuseUmountError::InvalidTarget(format!(
                "--cleanup only supports Libra task FUSE worktree paths ending in '/workspace': {}",
                mountpoint.display()
            ))
        })?;
        if root.exists() {
            fs::remove_dir_all(&root).map_err(|source| {
                FuseUmountError::IoWrite(format!(
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

#[cfg(test)]
mod tests {
    use super::*;

    /// Pin the `stable_code()` mapping for every variant of
    /// [`FuseUmountError`]. JSON consumers branch on the
    /// [`StableErrorCode`] in the error envelope for `libra worktree
    /// umount`. The Display body just echoes the inner string
    /// verbatim regardless of variant, so this is the only
    /// public surface contract worth pinning per-variant — a future
    /// refactor that flipped (say) `IoRead` from `IoReadFailed` to
    /// the catch-all `Other` code would silently change the wire
    /// surface unless every variant has its own guard.
    ///
    /// Continuation of the post-v0.17.700 surface-contract sweep
    /// (TuiControlError / CherryPickError / RevertError /
    /// RestoreError / StashError / ResetError).
    #[test]
    fn fuse_umount_error_stable_code_pins_each_variant() {
        assert_eq!(
            FuseUmountError::InvalidTarget("ignored".to_string()).stable_code(),
            StableErrorCode::CliInvalidTarget,
        );
        assert_eq!(
            FuseUmountError::IoRead("ignored".to_string()).stable_code(),
            StableErrorCode::IoReadFailed,
        );
        assert_eq!(
            FuseUmountError::IoWrite("ignored".to_string()).stable_code(),
            StableErrorCode::IoWriteFailed,
        );
    }

    /// Pin the `Display` echo contract for [`FuseUmountError`]. The
    /// impl at `:152-160` collapses every variant into a verbatim
    /// echo of the inner string — clients building error envelopes
    /// rely on this exact passthrough (the `into_cli_error()` call
    /// at `:148` uses `self.to_string()` as the CliError message
    /// body). A future refactor that prefixed variants with
    /// "io read: " / "io write: " would change the user-visible
    /// stderr and break automation that greps the raw inner string.
    #[test]
    fn fuse_umount_error_display_echoes_inner_string_verbatim() {
        assert_eq!(
            FuseUmountError::InvalidTarget("/not/a/path".to_string()).to_string(),
            "/not/a/path",
        );
        assert_eq!(
            FuseUmountError::IoRead("permission denied reading state".to_string()).to_string(),
            "permission denied reading state",
        );
        assert_eq!(
            FuseUmountError::IoWrite("disk full while persisting state".to_string()).to_string(),
            "disk full while persisting state",
        );
    }
}
