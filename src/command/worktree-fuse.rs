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
    time::{Duration, Instant},
};

use clap::{Parser, Subcommand};
use libfuse_fs::overlayfs::{OverlayArgs, mount_fs};
use rfuse3::raw::MountHandle;
use serde::{Deserialize, Serialize};
use tokio::time::{sleep, timeout};
use uuid::Uuid;

#[path = "worktree.rs"]
mod legacy;

// Re-export the shared `--help` examples constant so the cli definition can
// reference `command::worktree::WORKTREE_EXAMPLES` regardless of whether the
// `worktree-fuse` feature routed compilation through this file or directly
// through `worktree.rs`.
pub use legacy::WORKTREE_EXAMPLES;
pub(crate) use legacy::{
    WorktreeError, WorktreeState, acquire_registry_lock_async, registered_fuse_worktree_paths,
    registry_knows_linked_worktree, registry_knows_linked_worktree_in_storage,
    reject_bare_repository, run_list_worktrees, run_list_worktrees_at,
};

const FUSE_MOUNT_TIMEOUT: Duration = Duration::from_secs(15);
const FUSE_UNMOUNT_TIMEOUT: Duration = Duration::from_secs(15);
const FUSE_HEALTH_TIMEOUT: Duration = Duration::from_secs(5);
const FUSE_REGISTRY_MAX_BYTES: usize = 16 * 1024 * 1024;

use crate::{
    command::{
        branch,
        restore::{self, RestoreArgs},
    },
    internal::head::Head,
    utils::{
        beneath,
        error::{CliError, CliResult, StableErrorCode},
        fuse as fuse_utils,
        output::{OutputConfig, emit_json_data},
        util, worktree_layout,
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
        /// Repository-wide name for the new worktree.
        name: String,
        /// Create the worktree at an explicit filesystem path.
        #[clap(long)]
        path: Option<String>,
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
            long = "new-branch",
            visible_alias = "create-branch",
            help = "Create and checkout a new branch"
        )]
        create_branch: Option<String>,
        #[clap(
            long,
            requires = "create_branch",
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
        /// JSON data schema selector (§C.8) — see the non-FUSE build's doc.
        #[clap(long = "schema-version", default_value_t = 2)]
        schema_version: u32,
    },
    /// Diagnose worktree and Agent-workspace scopes. STRICTLY READ-ONLY
    /// (W0 §C.11 / W4).
    ///
    /// Kept in parity with the non-FUSE build's subcommand set: this file
    /// re-declares the whole enum, so a command added there is simply absent
    /// here unless it is added here too.
    Doctor {
        /// Diagnose exactly one workspace. Cannot be combined with
        /// `--limit`/`--cursor`.
        #[clap(value_name = "WORKSPACE_ID")]
        workspace_id: Option<String>,
        /// Maximum diagnostics per page (default 50, capped at 500).
        #[clap(long, value_name = "N")]
        limit: Option<u64>,
        /// Keyset cursor: the `next_cursor` of the previous page, verbatim.
        #[clap(long, value_name = "CURSOR")]
        cursor: Option<String>,
        /// Explicitly attribute one legacy unscoped capture session (same
        /// contract as the non-FUSE build; delegated to the shared
        /// implementation).
        #[clap(
            long,
            value_name = "SESSION_ID",
            requires = "workspace_id",
            conflicts_with_all = ["limit", "cursor"]
        )]
        adopt_capture_session: Option<String>,
        /// Copy the common `info/exclude`/`info/attributes` into ONE linked
        /// worktree's local gitdir (W0 §C.4.1.1; requires --confirm).
        #[clap(
            long,
            value_name = "WORKTREE_PATH",
            conflicts_with_all = [
                "workspace_id", "limit", "cursor", "adopt_capture_session",
                "adopt_approved_project", "clear_approved_project"
            ]
        )]
        adopt_info_to: Option<String>,
        /// Delete the common `.libra/info/exclude` and `info/attributes`
        /// (requires --confirm).
        #[clap(
            long,
            conflicts_with_all = [
                "workspace_id", "limit", "cursor", "adopt_capture_session", "adopt_info_to",
                "adopt_approved_project", "clear_approved_project"
            ]
        )]
        clear_common_info: bool,
        /// Re-home Always approvals under a legacy `project_id` onto
        /// `libra.repoid` (requires --confirm).
        #[clap(
            long,
            value_name = "LEGACY_PROJECT_ID",
            conflicts_with_all = [
                "workspace_id", "limit", "cursor", "adopt_capture_session", "adopt_info_to",
                "clear_common_info", "clear_approved_project"
            ]
        )]
        adopt_approved_project: Option<String>,
        /// Delete Always approvals under a legacy `project_id` (requires
        /// --confirm).
        #[clap(
            long,
            value_name = "LEGACY_PROJECT_ID",
            conflicts_with_all = [
                "workspace_id", "limit", "cursor", "adopt_capture_session", "adopt_info_to",
                "clear_common_info", "adopt_approved_project"
            ]
        )]
        clear_approved_project: Option<String>,
        /// Confirm a mutating doctor action.
        #[clap(long)]
        confirm: bool,
    },
    Lock {
        /// Registered worktree name.
        #[clap(required_unless_present = "path", conflicts_with = "path")]
        name: Option<String>,
        /// Select an existing worktree by its explicit filesystem path.
        #[clap(long)]
        path: Option<String>,
        /// Optional explanation shown in `worktree list` while the worktree is locked.
        #[clap(long)]
        reason: Option<String>,
    },
    Unlock {
        /// Registered worktree name.
        #[clap(required_unless_present = "path", conflicts_with = "path")]
        name: Option<String>,
        #[clap(long)]
        path: Option<String>,
    },
    Move {
        /// Registered worktree name.
        #[clap(required_unless_present = "from_path", conflicts_with = "from_path")]
        name: Option<String>,
        /// Select the source by explicit filesystem path.
        #[clap(long)]
        from_path: Option<String>,
        /// New filesystem path (ordinary linked worktrees only).
        #[clap(long)]
        path: String,
    },
    Prune,
    Remove {
        /// Registered worktree name.
        #[clap(required_unless_present = "path", conflicts_with = "path")]
        name: Option<String>,
        #[clap(long)]
        path: Option<String>,
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
        #[clap(conflicts_with = "path")]
        name: Option<String>,
        #[clap(long)]
        path: Option<String>,
        /// Migrate legacy shared-`.libra` symlink worktrees to the isolated
        /// layout (W3-s3 §C.6.2).
        #[clap(long)]
        migrate_layout: bool,
        /// With --migrate-layout: report what would be migrated, write
        /// nothing.
        #[clap(long, requires = "migrate_layout")]
        dry_run: bool,
        /// Confirm a MUTATING repair action (W0 §C.11): required for every
        /// repair that writes; the read-only `--migrate-layout --dry-run`
        /// never needs it.
        #[clap(long)]
        confirm: bool,
    },
}

#[derive(Serialize, Deserialize, Debug, Clone)]
struct FuseWorktreeEntry {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    name: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    mount_id: Option<String>,
    path: String,
    branch: String,
    upper_dir: String,
    lower_dirs: Vec<String>,
    locked: bool,
    lock_reason: Option<String>,
    privileged: bool,
    allow_other: bool,
}

#[derive(Serialize, Deserialize, Debug, Clone)]
struct FuseWorktreeState {
    schema_version: u32,
    #[serde(rename = "entries")]
    worktrees: Vec<FuseWorktreeEntry>,
}

impl Default for FuseWorktreeState {
    fn default() -> Self {
        Self {
            schema_version: 1,
            worktrees: Vec::new(),
        }
    }
}

impl FuseWorktreeState {
    fn parse(bytes: &[u8]) -> io::Result<Self> {
        let value: serde_json::Value = serde_json::from_slice(bytes)
            .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error))?;
        let object = value.as_object().ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::InvalidData,
                "FUSE registry must be a JSON object",
            )
        })?;
        if object.contains_key("entries") && !object.contains_key("schema_version")
            || object.contains_key("schema_version") && object.contains_key("worktrees")
        {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "FUSE registry contains mixed or unversioned entries",
            ));
        }
        let state = if let Some(version) = value.get("schema_version") {
            if version.as_u64() != Some(1) {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    format!("unsupported FUSE worktree registry version {version}; upgrade Libra"),
                ));
            }
            serde_json::from_value::<Self>(value)
                .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error))?
        } else {
            #[derive(Deserialize)]
            struct LegacyState {
                worktrees: Vec<FuseWorktreeEntry>,
            }
            let legacy: LegacyState = serde_json::from_value(value)
                .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error))?;
            Self {
                schema_version: 1,
                worktrees: legacy.worktrees,
            }
        };
        let mut names = std::collections::HashSet::new();
        let mut paths = HashMap::new();
        for entry in &state.worktrees {
            if let Some(mount_id) = &entry.mount_id {
                Uuid::parse_str(mount_id).map_err(|error| {
                    io::Error::new(
                        io::ErrorKind::InvalidData,
                        format!("FUSE registry entry has an invalid mount binding: {error}"),
                    )
                })?;
            }
            if !Path::new(&entry.path).is_absolute() {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    "FUSE registry entry has a non-absolute path",
                ));
            }
            if let Some(previous_named) =
                paths.insert(PathBuf::from(&entry.path), entry.name.is_some())
                && (previous_named || entry.name.is_some())
            {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    "multiple FUSE entries share a named worktree path; restore an unambiguous registry before continuing",
                ));
            }
            if let Some(name) = &entry.name {
                if !names.insert(name) {
                    return Err(io::Error::new(
                        io::ErrorKind::InvalidData,
                        format!("FUSE worktree name '{name}' is registered more than once"),
                    ));
                }
                crate::utils::worktree_layout::validate_name(name)
                    .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error))?;
            }
        }
        Ok(state)
    }
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

/// W0 (§C.11, Codex R19 follow-up), in parity with
/// `legacy::repair_invocation_refused_without_confirmation`: an unconfirmed
/// mutating repair is refused before any side effect, so it must skip the
/// migration-applying open above. The FUSE surface never exposes
/// `--resolve-identity`, so the predicate collapses to "unconfirmed and not
/// the dry-run preview".
fn repair_invocation_refused_without_confirmation(command: &WorktreeSubcommand) -> bool {
    match command {
        WorktreeSubcommand::Repair {
            migrate_layout,
            dry_run,
            confirm,
            ..
        } => !(*confirm || *migrate_layout && *dry_run),
        _ => false,
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
    }
    // W0 §C.11: `doctor` is read-only, and applying migrations is a write.
    // Kept in parity with the non-FUSE entry point, which makes the same
    // exclusion — this file re-declares the dispatch, so the exclusion has
    // to be re-stated or the FUSE build quietly upgrades what it diagnoses.
    // An UNCONFIRMED mutating repair is excluded for the same reason (Codex
    // R19 follow-up): its refusal is documented as zero-side-effect. So is
    // the `--migrate-layout --dry-run` preview (Codex R20 follow-up): it is
    // documented as read-only end to end, and the legacy arm it delegates to
    // never resolves a database connection.
    let refused_repair = repair_invocation_refused_without_confirmation(&command);
    let readonly_layout_preview = matches!(
        &command,
        WorktreeSubcommand::Repair {
            migrate_layout: true,
            dry_run: true,
            ..
        }
    );
    if !matches!(
        &command,
        WorktreeSubcommand::Umount { .. } | WorktreeSubcommand::Doctor { .. }
    ) && !refused_repair
        && !readonly_layout_preview
    {
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
    } else if refused_repair || readonly_layout_preview {
        // Bare boundary precedes even the confirmation refusal (same as the
        // legacy entry point), classified through the no-migration open so
        // the refused repair and the read-only layout preview stay
        // zero-side-effect.
        legacy::reject_bare_repository_without_migrations().await?;
    }

    match command {
        WorktreeSubcommand::Add {
            name,
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
                            name,
                            path,
                            target,
                            detach,
                            new_branch: create_branch,
                            force: false,
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
                let common = worktree_layout::current_common_storage()
                    .map_err(|error| CliError::fatal(error.to_string()))?;
                let _registry_guard = legacy::acquire_registry_lock_async()
                    .await
                    .map_err(legacy::WorktreeError::into_cli_error)?;
                let target = add_fuse_worktree(
                    &common,
                    FuseAddOptions {
                        name,
                        path,
                        branch_name: branch,
                        create_branch_name: create_branch,
                        from,
                        privileged,
                        allow_other,
                    },
                )
                .await
                .map_err(|error| CliError::fatal(error.to_string()))?;
                if output.is_json() {
                    emit_json_data(
                        "worktree.add",
                        &serde_json::json!({"path":target, "fuse":true}),
                        output,
                    )
                } else {
                    if !output.quiet {
                        println!("{}", target.display());
                    }
                    Ok(())
                }
            }
        }
        WorktreeSubcommand::List {
            porcelain,
            schema_version,
        } => list_all_worktrees(output, porcelain, schema_version).await,
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
            // Delegate the WHOLE doctor surface (read-only pages and the
            // confirmed mutations alike) to the shared implementation, so
            // the FUSE build cannot drift from the non-FUSE contract.
            legacy::execute_safe(
                legacy::WorktreeArgs {
                    command: legacy::WorktreeSubcommand::Doctor {
                        workspace_id,
                        limit,
                        cursor,
                        adopt_capture_session,
                        adopt_info_to,
                        clear_common_info,
                        adopt_approved_project,
                        clear_approved_project,
                        confirm,
                    },
                },
                output,
            )
            .await
        }
        WorktreeSubcommand::Lock { name, path, reason } => {
            let common = worktree_layout::current_common_storage()
                .map_err(|error| CliError::fatal(error.to_string()))?;
            let _registry_guard = legacy::acquire_registry_lock_async()
                .await
                .map_err(legacy::WorktreeError::into_cli_error)?;
            let location = legacy::resolve_worktree_selector(name.as_deref(), path.as_deref())
                .map_err(legacy::WorktreeError::into_cli_error)?;
            let target = location.path.to_string_lossy().to_string();
            if location.is_fuse {
                lock_fuse_worktree(&common, &target, reason)
                    .map_err(|error| CliError::fatal(error.to_string()))?;
                Ok(())
            } else {
                let result = legacy::lock_worktree_locked(target, reason)
                    .await
                    .map_err(legacy::WorktreeError::into_cli_error)?;
                legacy::render_lock_worktree(&result, output)
            }
        }
        WorktreeSubcommand::Unlock { name, path } => {
            let common = worktree_layout::current_common_storage()
                .map_err(|error| CliError::fatal(error.to_string()))?;
            let _registry_guard = legacy::acquire_registry_lock_async()
                .await
                .map_err(legacy::WorktreeError::into_cli_error)?;
            let location = legacy::resolve_worktree_selector(name.as_deref(), path.as_deref())
                .map_err(legacy::WorktreeError::into_cli_error)?;
            let target = location.path.to_string_lossy().to_string();
            if location.is_fuse {
                unlock_fuse_worktree(&common, &target)
                    .map_err(|error| CliError::fatal(error.to_string()))?;
                Ok(())
            } else {
                let result = legacy::unlock_worktree_locked(target)
                    .await
                    .map_err(legacy::WorktreeError::into_cli_error)?;
                legacy::render_unlock_worktree(&result, output)
            }
        }
        WorktreeSubcommand::Remove {
            name,
            path,
            delete_dir,
        } => {
            let common = worktree_layout::current_common_storage()
                .map_err(|error| CliError::fatal(error.to_string()))?;
            let _registry_guard = legacy::acquire_registry_lock_async()
                .await
                .map_err(legacy::WorktreeError::into_cli_error)?;
            let location = legacy::resolve_worktree_selector(name.as_deref(), path.as_deref())
                .map_err(legacy::WorktreeError::into_cli_error)?;
            let target = location.path.to_string_lossy().to_string();
            if location.is_fuse {
                remove_fuse_worktree(&common, &target)
                    .await
                    .map_err(|error| CliError::fatal(error.to_string()))?;
                Ok(())
            } else {
                let result = legacy::remove_worktree_locked(target, delete_dir, false)
                    .await
                    .map_err(legacy::WorktreeError::into_cli_error)?;
                legacy::render_remove_worktree(&result, output)
            }
        }
        WorktreeSubcommand::Umount { path, cleanup } => {
            let result = umount_fuse_path(path, cleanup)
                .await
                .map_err(FuseUmountError::into_cli_error)?;
            render_umount_fuse_path(&result, output)
        }
        WorktreeSubcommand::Move {
            name,
            from_path,
            path,
        } => {
            let _registry_guard = legacy::acquire_registry_lock_async()
                .await
                .map_err(legacy::WorktreeError::into_cli_error)?;
            let location = legacy::resolve_worktree_selector(name.as_deref(), from_path.as_deref())
                .map_err(legacy::WorktreeError::into_cli_error)?;
            if location.is_fuse {
                return Err(CliError::fatal(
                    "moving a FUSE worktree is not supported; remove it and create a new worktree at the desired path",
                ));
            }
            let result =
                legacy::move_worktree_locked(location.path.to_string_lossy().to_string(), path)
                    .await
                    .map_err(legacy::WorktreeError::into_cli_error)?;
            legacy::render_move_worktree(&result, output)
        }
        WorktreeSubcommand::Prune => {
            let common = worktree_layout::current_common_storage()
                .map_err(|error| CliError::fatal(error.to_string()))?;
            let _registry_guard = legacy::acquire_registry_lock_async()
                .await
                .map_err(legacy::WorktreeError::into_cli_error)?;
            legacy::registered_worktree_locations()
                .map_err(legacy::WorktreeError::into_cli_error)?;
            let result = legacy::prune_worktrees_locked()
                .await
                .map_err(legacy::WorktreeError::into_cli_error)?;
            prune_fuse_worktrees(&common).map_err(|error| CliError::fatal(error.to_string()))?;
            legacy::render_prune_worktrees(&result, output)
        }
        WorktreeSubcommand::Repair {
            name,
            path,
            migrate_layout,
            dry_run,
            confirm,
        } => {
            // Core registry first: a corrupt/zero-byte core registry must
            // fail the command BEFORE any fuse metadata is mutated.
            let run_fuse_repair = name.is_none() && path.is_none() && !migrate_layout;
            if run_fuse_repair {
                // W0 §C.11: ONE audit boundary covering BOTH repairs — the
                // core registry repair and the FUSE state repair. Delegating
                // to the legacy arm would close the boundary before the FUSE
                // mutation: a FUSE failure would then follow a row already
                // closed as successful, and a FUSE success would go
                // unrecorded.
                legacy::require_repair_confirmation(confirm, "worktree repair")?;
                let common = worktree_layout::current_common_storage()
                    .map_err(|error| CliError::fatal(error.to_string()))?;
                let _registry_guard = legacy::acquire_registry_lock_async()
                    .await
                    .map_err(legacy::WorktreeError::into_cli_error)?;
                let boundary = legacy::begin_repair_operation("worktree repair", None).await?;
                let result = legacy::repair_worktrees_locked()
                    .await
                    .map_err(|e| e.into_cli_error())
                    .and_then(|repaired| {
                        repair_fuse_worktrees(&common)
                            .map(|()| repaired)
                            .map_err(|e| CliError::fatal(e.to_string()))
                    });
                let result = legacy::finish_repair_operation(boundary, result).await?;
                return legacy::render_repair_worktrees(&result, output);
            }
            if name.is_some() || path.is_some() {
                let location = legacy::resolve_worktree_selector(name.as_deref(), path.as_deref())
                    .map_err(legacy::WorktreeError::into_cli_error)?;
                if location.is_fuse {
                    return Err(CliError::fatal(
                        "scoped identity repair is not supported for FUSE worktrees; use 'worktree repair --confirm' to repair the registries",
                    ));
                }
            }
            legacy::execute_safe(
                legacy::WorktreeArgs {
                    command: legacy::WorktreeSubcommand::Repair {
                        name,
                        path,
                        migrate_layout,
                        dry_run,
                        confirm,
                        // Not exposed on the FUSE surface; the identity-repair
                        // flow needs the canonical layout.
                        resolve_identity: false,
                        yes: false,
                    },
                },
                output,
            )
            .await
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

fn fuse_state_path(common: &Path) -> PathBuf {
    common.join("worktrees-fuse.json")
}

fn load_fuse_state(common: &Path) -> io::Result<FuseWorktreeState> {
    let root = beneath::open_root(common)?;
    load_fuse_state_from(&root)
}

fn load_fuse_state_from(root: &fs::File) -> io::Result<FuseWorktreeState> {
    match worktree_layout::read_regular_metadata_from(
        root,
        Path::new("worktrees-fuse.json"),
        FUSE_REGISTRY_MAX_BYTES,
    ) {
        Ok(data) => FuseWorktreeState::parse(&data),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(FuseWorktreeState::default()),
        Err(error) => Err(error),
    }
}

fn save_fuse_state_to(root: &fs::File, state: &FuseWorktreeState) -> io::Result<()> {
    use std::{ffi::CString, os::unix::io::AsRawFd};

    let data = serde_json::to_vec_pretty(state).map_err(io::Error::other)?;
    if data.len() > FUSE_REGISTRY_MAX_BYTES {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "FUSE registry exceeds the 16 MiB metadata limit; remove stale entries before adding another worktree",
        ));
    }
    let temporary = format!("worktrees-fuse.{}.tmp", Uuid::new_v4().simple());
    let temporary_c = CString::new(temporary.as_str()).map_err(io::Error::other)?;
    beneath::write_regular_file_beneath(root, Path::new(&temporary), &data, true)?;
    beneath::open_file_beneath(root, Path::new(&temporary))?.sync_all()?;
    // SAFETY: both names are fixed/UUID-derived single components and root
    // stays open throughout the atomic replacement. No pathname is followed.
    let renamed = unsafe {
        libc::renameat(
            root.as_raw_fd(),
            temporary_c.as_ptr(),
            root.as_raw_fd(),
            c"worktrees-fuse.json".as_ptr(),
        )
    };
    if renamed != 0 {
        let error = io::Error::last_os_error();
        // SAFETY: remove only our unique temporary entry beneath the pinned root.
        unsafe { libc::unlinkat(root.as_raw_fd(), temporary_c.as_ptr(), 0) };
        return Err(error);
    }
    root.sync_all()
}

async fn verify_mount_health(mountpoint: &Path, mount_id: &str) -> io::Result<()> {
    let deadline = Instant::now() + FUSE_HEALTH_TIMEOUT;
    loop {
        let remaining = deadline.saturating_duration_since(Instant::now());
        if remaining.is_zero() {
            return Err(io::Error::new(
                io::ErrorKind::TimedOut,
                "FUSE mount did not become ready within five seconds",
            ));
        }
        let mountpoint = mountpoint.to_path_buf();
        let mount_id = mount_id.to_owned();
        let result = timeout(
            remaining,
            tokio::task::spawn_blocking(move || verify_mount_health_once(&mountpoint, &mount_id)),
        )
        .await
        .map_err(|_| io::Error::new(io::ErrorKind::TimedOut, "FUSE health check timed out"))?
        .map_err(|error| io::Error::other(format!("FUSE health check worker failed: {error}")))?;
        match result {
            Ok(()) => return Ok(()),
            Err(error)
                if error.kind() == io::ErrorKind::NotFound
                    || matches!(error.raw_os_error(), Some(libc::ENOTCONN | libc::ENXIO)) =>
            {
                sleep(Duration::from_millis(50)).await;
            }
            Err(error) => return Err(error),
        }
    }
}

fn verify_mount_health_once(mountpoint: &Path, mount_id: &str) -> io::Result<()> {
    use std::{
        ffi::CString,
        io::{Read, Write},
        os::unix::io::{AsRawFd, FromRawFd},
    };
    let expected_binding = format!("{mount_id}\n").into_bytes();

    let root = beneath::open_root(mountpoint)?;
    if worktree_layout::read_regular_metadata_from(
        &root,
        Path::new(".libra/worktree-mount-id"),
        128,
    )? != expected_binding
    {
        return Err(io::Error::other(
            "FUSE mounted metadata does not match its registered upper directory",
        ));
    }
    let mask = beneath::open_beneath(&root, Path::new(".libra/worktree"))?;
    if beneath::read_dir_fd(mask)?.next().transpose()?.is_some() {
        return Err(io::Error::other(
            "FUSE worktree opacity mask exposed the managed container; refusing recursive mount",
        ));
    }
    let probe = format!(".libra-fuse-health-{}", Uuid::new_v4().simple());
    let probe_c = CString::new(probe.as_str()).map_err(io::Error::other)?;
    // SAFETY: the probe is a UUID-derived single component and root is
    // pinned. O_EXCL/O_NOFOLLOW prevent touching an existing user file.
    let raw = unsafe {
        libc::openat(
            root.as_raw_fd(),
            probe_c.as_ptr(),
            libc::O_RDWR | libc::O_CREAT | libc::O_EXCL | libc::O_NOFOLLOW | libc::O_CLOEXEC,
            0o600,
        )
    };
    if raw < 0 {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: openat returned a new owned descriptor.
    let mut file = unsafe { fs::File::from_raw_fd(raw) };
    let result = (|| {
        file.write_all(b"libra-fuse-health")?;
        let mut read = beneath::open_file_beneath(&root, Path::new(&probe))?;
        let mut bytes = Vec::new();
        read.read_to_end(&mut bytes)?;
        if bytes != b"libra-fuse-health" {
            return Err(io::Error::other(
                "FUSE worktree normal-file I/O health check failed",
            ));
        }
        Ok(())
    })();
    // SAFETY: unlink the owned probe through the same pinned directory.
    let removed = unsafe { libc::unlinkat(root.as_raw_fd(), probe_c.as_ptr(), 0) };
    result?;
    if removed != 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(())
}

fn validate_fuse_target(
    target: &Path,
    lower_dirs: &[PathBuf],
    upper: &Path,
    common: &Path,
    managed: bool,
) -> io::Result<()> {
    let container = worktree_layout::managed_root(common);
    if target == upper || target.starts_with(upper) || upper.starts_with(target) {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "FUSE mountpoint and writable upper directory must not overlap",
        ));
    }
    if !managed
        && (target.starts_with(&container)
            || container.starts_with(target)
            || target.starts_with(common)
            || lower_dirs
                .iter()
                .any(|lower| target.starts_with(lower) || lower.starts_with(target)))
    {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "explicit FUSE --path must be outside every source worktree, the upper directory, and .libra/worktree; omit --path to use the protected managed layout",
        ));
    }
    Ok(())
}

struct FuseAddOptions {
    name: String,
    path: Option<String>,
    branch_name: Option<String>,
    create_branch_name: Option<String>,
    from: Option<String>,
    privileged: bool,
    allow_other: bool,
}

async fn add_fuse_worktree(common: &Path, options: FuseAddOptions) -> io::Result<PathBuf> {
    let FuseAddOptions {
        name,
        path,
        branch_name,
        create_branch_name,
        from,
        privileged,
        allow_other,
    } = options;
    worktree_layout::validate_name(&name)?;
    let registry_root = beneath::open_root(common)?;
    let locations = legacy::registered_worktree_locations().map_err(io::Error::other)?;
    if locations
        .iter()
        .any(|entry| entry.name.as_deref() == Some(name.as_str()))
    {
        return Err(io::Error::new(
            io::ErrorKind::AlreadyExists,
            format!("worktree name '{name}' is already registered; choose another name"),
        ));
    }
    let source_scope = match crate::internal::worktree_scope::WorktreeScope::request_scope() {
        Some(scope) => scope,
        None => crate::internal::worktree_scope::RequestScope::try_resolve(util::cur_dir())?
            .ok_or_else(|| io::Error::other("cannot resolve the invoking FUSE source worktree"))?,
    };
    if source_scope.storage != common {
        return Err(io::Error::other(
            "FUSE source and registry belong to different repositories",
        ));
    }
    let managed = path.is_none();
    let desired = match path {
        Some(path) => PathBuf::from(path),
        None => worktree_layout::fuse_workspace(common, &name)?,
    };
    let root_path = worktree_layout::fuse_root(common, &name)?;
    let upper_path = worktree_layout::fuse_upper(common, &name)?;
    worktree_layout::validate_managed_path(common, &root_path)?;
    let target_spec = if managed {
        fuse_utils::managed::Target::beneath(common, &desired)?
    } else {
        fuse_utils::managed::Target::resolve(&desired, &source_scope.workdir)?
    };
    let root_spec = fuse_utils::managed::Target::beneath(common, &root_path)?;
    let target = target_spec.path.clone();
    if fs::symlink_metadata(&root_spec.path).is_ok() {
        return Err(io::Error::new(
            io::ErrorKind::AlreadyExists,
            format!(
                "managed FUSE directory '{}' already exists; choose another name or inspect the retained directory",
                root_spec.path.display()
            ),
        ));
    }
    let lower_dir = fs::canonicalize(&source_scope.worktree_root)?;
    let lower_root = beneath::open_root(&lower_dir)?;
    let lower_dirs = vec![lower_dir.clone()];
    validate_fuse_target(&target, &lower_dirs, &upper_path, common, managed)?;
    if locations.iter().any(|entry| entry.path == target) {
        return Err(io::Error::new(
            io::ErrorKind::AlreadyExists,
            format!("FUSE target '{}' is already registered", target.display()),
        ));
    }
    // This v4 gate also protects names introduced only in the FUSE registry
    // from older ordinary builds that would otherwise miss their occupancy.
    legacy::ensure_registry_v4_locked().map_err(io::Error::other)?;
    let mount_id = Uuid::new_v4().to_string();
    let root = root_spec.create(true)?;
    let setup = (|| {
        let upper = root.child_target(Path::new("upper"))?.create(true)?;
        fuse_utils::managed::prepare_worktree_mask(&upper)?;
        fuse_utils::managed::prepare_mount_binding(&upper, &mount_id)?;
        let mount = if managed {
            root.child_target(Path::new("workspace"))?.create(true)?
        } else {
            target_spec.create(false)?
        };
        let checked = (|| {
            root.validate_path()?;
            upper.validate_path()?;
            mount.validate_path()?;
            if !mount.is_empty()? {
                return Err(io::Error::new(
                    io::ErrorKind::AlreadyExists,
                    "FUSE target changed and is no longer empty",
                ));
            }
            Ok(())
        })();
        if let Err(error) = checked {
            if !managed {
                mount.rollback()?;
            }
            return Err(error);
        }
        Ok((upper, mount))
    })();
    let (upper, mount) = match setup {
        Ok(directories) => directories,
        Err(error) => {
            root.rollback().map_err(|rollback| {
                io::Error::other(format!("{error}; FUSE setup rollback failed: {rollback}"))
            })?;
            return Err(error);
        }
    };
    let creation = async {
        if let Some(new_branch) = create_branch_name.as_ref() {
            branch::create_branch_safe(new_branch.clone(), from.clone())
                .await
                .map_err(|error| io::Error::other(format!("failed to create branch: {error}")))?;
        }
        let explicit_branch = branch_name.is_some() || create_branch_name.is_some();
        let checkout_branch = if let Some(name) = create_branch_name.clone().or(branch_name) {
            name
        } else {
            match Head::current().await {
                Head::Branch(name) => name,
                _ => "HEAD".to_string(),
            }
        };
        // Hold source and destination directory descriptors through mounting.
        // Check their pathname identities immediately before the external
        // mount API, which necessarily consumes filesystem paths.
        let lower_now = beneath::open_root(&lower_dir)?;
        use std::os::unix::fs::MetadataExt;
        if lower_root.metadata()?.dev() != lower_now.metadata()?.dev()
            || lower_root.metadata()?.ino() != lower_now.metadata()?.ino()
        {
            return Err(io::Error::other(
                "FUSE source worktree changed during creation",
            ));
        }
        root.validate_path()?;
        upper.validate_path()?;
        mount.validate_path()?;
        let mount_args = OverlayArgs {
            mountpoint: &target,
            upperdir: &upper.path,
            lowerdir: lower_dirs.clone(),
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
                    format!("FUSE mount timed out for '{}'", target.display()),
                )
            })?
            .into_mount_handle_result()?;
        active_mounts()
            .lock()
            .map_err(|_| io::Error::other("FUSE active-mount lock is poisoned"))?
            .insert(target.to_string_lossy().to_string(), mount_handle);
        verify_mount_health(&target, &mount_id).await?;
        if explicit_branch
            || Head::current_commit_result()
                .await
                .map_err(io::Error::other)?
                .is_some()
        {
            // Bind only the file destination to the overlay. Source HEAD,
            // index, database and object reads retain the invoking scope;
            // this does not create or change a persistent FUSE identity.
            let mut destination_scope = source_scope.clone();
            destination_scope.workdir = target.clone();
            destination_scope.worktree_root = target.clone();
            crate::internal::worktree_scope::with_request_scope(
                Some(destination_scope),
                restore::execute_checked_typed_into(
                    RestoreArgs {
                        overlay: false,
                        no_overlay: false,
                        ours: false,
                        theirs: false,
                        ignore_unmerged: false,
                        merge: false,
                        conflict: None,
                        pathspec: vec![":/".to_string()],
                        source: Some(checkout_branch.clone()),
                        worktree: true,
                        staged: false,
                        pathspec_from_file: None,
                        pathspec_file_nul: false,
                        no_progress: false,
                    },
                    &target,
                ),
            )
            .await
            .map_err(|error| {
                io::Error::other(format!(
                    "failed to populate FUSE worktree from '{checkout_branch}': {error}"
                ))
            })?;
        }
        root.validate_path()?;
        let _guard = fuse_state_lock()
            .lock()
            .map_err(|_| io::Error::other("FUSE state lock is poisoned"))?;
        let mut current = load_fuse_state_from(&registry_root)?;
        current.worktrees.push(FuseWorktreeEntry {
            name: Some(name.clone()),
            mount_id: Some(mount_id),
            path: target.to_string_lossy().to_string(),
            branch: checkout_branch,
            upper_dir: upper.path.to_string_lossy().to_string(),
            lower_dirs: lower_dirs
                .iter()
                .map(|path| path.to_string_lossy().to_string())
                .collect(),
            locked: false,
            lock_reason: None,
            privileged,
            allow_other,
        });
        save_fuse_state_to(&registry_root, &current)?;
        Ok::<_, io::Error>(())
    }
    .await;
    if let Err(error) = creation {
        // A directory fsync may fail after atomic registry replacement. Keep
        // its live mount and data if the entry is already visible on disk.
        if load_fuse_state_from(&registry_root).is_ok_and(|state| {
            state.worktrees.iter().any(|entry| {
                entry.name.as_deref() == Some(name.as_str()) && Path::new(&entry.path) == target
            })
        }) {
            return Err(io::Error::other(format!(
                "FUSE worktree was registered at '{}', but durability could not be confirmed: {error}",
                target.display()
            )));
        }
        if let Err(unmount) = unmount_path(&target).await {
            return Err(io::Error::other(format!(
                "{error}; cannot safely roll back the mounted worktree: {unmount}"
            )));
        }
        if !managed {
            mount.rollback().map_err(|rollback| {
                io::Error::other(format!(
                    "{error}; external target cleanup failed: {rollback}"
                ))
            })?;
        }
        root.rollback().map_err(|rollback| {
            io::Error::other(format!(
                "{error}; managed worktree cleanup failed: {rollback}"
            ))
        })?;
        return Err(error);
    }
    Ok(target)
}

async fn list_all_worktrees(
    output: &OutputConfig,
    porcelain: bool,
    schema_version: u32,
) -> CliResult<()> {
    if schema_version != 2 {
        return Err(CliError::failure(format!(
            "unsupported worktree list schema version {schema_version}: the shipped shape is \
             version 2"
        ))
        .with_exit_code(129)
        .with_stable_code(StableErrorCode::CliInvalidArguments));
    }
    let mut result = legacy::run_list_worktrees().map_err(legacy::WorktreeError::into_cli_error)?;
    let common = worktree_layout::current_common_storage()
        .map_err(|error| CliError::fatal(error.to_string()))?;
    let state = load_fuse_state(&common).map_err(|error| fuse_state_read_error(&common, error))?;
    if output.is_json() {
        for entry in state.worktrees {
            result.worktrees.push(legacy::WorktreeListEntry {
                name: entry.name.clone(),
                kind: "worktree",
                worktree_id: legacy::resolve_entry_worktree_id(&entry.path, false),
                path: entry.path.clone(),
                is_main: false,
                locked: entry.locked,
                lock_reason: entry.lock_reason.clone(),
                exists: Path::new(&entry.path).exists(),
                state: "active",
                layout: "task-fuse",
                // FUSE worktrees live outside the registry, so no
                // registration generation exists to fence on.
                epoch: 0,
            });
        }
        return emit_json_data(
            "worktree.list",
            &legacy::VersionedWorktreeList {
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
        // Combine the registry worktrees with the FUSE-mounted ones, then emit
        // the shared porcelain format.
        let mut all = result.worktrees;
        for entry in &state.worktrees {
            all.push(legacy::WorktreeListEntry {
                name: entry.name.clone(),
                kind: "worktree",
                worktree_id: legacy::resolve_entry_worktree_id(&entry.path, false),
                path: entry.path.clone(),
                is_main: false,
                locked: entry.locked,
                lock_reason: entry.lock_reason.clone(),
                exists: Path::new(&entry.path).exists(),
                state: "active",
                layout: "task-fuse",
                // FUSE worktrees live outside the registry — no generation.
                epoch: 0,
            });
        }
        let porcelain = legacy::format_worktree_porcelain(&all)
            .await
            .map_err(|error| {
                CliError::fatal(format!("cannot list worktrees: {error}"))
                    .with_stable_code(crate::utils::error::StableErrorCode::RepoStateInvalid)
                    .with_hint("a stored HEAD reference is corrupt; repair it and retry")
            })?;
        print!("{porcelain}");
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
        if let Some(name) = &entry.name {
            line.push_str(&format!(" [name: {name}]"));
        }
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

fn fuse_state_read_error(common: &Path, source: io::Error) -> CliError {
    let path = fuse_state_path(common);
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

fn lock_fuse_worktree(common: &Path, path: &str, reason: Option<String>) -> io::Result<bool> {
    let registry_root = beneath::open_root(common)?;
    let _state_guard = fuse_state_lock()
        .lock()
        .map_err(|_| io::Error::other("fuse state lock poisoned"))?;
    let target = PathBuf::from(path);
    let mut state = load_fuse_state_from(&registry_root)?;
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
        save_fuse_state_to(&registry_root, &state)?;
    }
    Ok(found)
}

fn unlock_fuse_worktree(common: &Path, path: &str) -> io::Result<bool> {
    let registry_root = beneath::open_root(common)?;
    let _state_guard = fuse_state_lock()
        .lock()
        .map_err(|_| io::Error::other("fuse state lock poisoned"))?;
    let target = PathBuf::from(path);
    let mut state = load_fuse_state_from(&registry_root)?;
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
        save_fuse_state_to(&registry_root, &state)?;
    }
    Ok(found)
}

async fn remove_fuse_worktree(common: &Path, path: &str) -> io::Result<bool> {
    let registry_root = beneath::open_root(common)?;
    let target = PathBuf::from(path);
    let entry = {
        let _state_guard = fuse_state_lock()
            .lock()
            .map_err(|_| io::Error::other("fuse state lock poisoned"))?;
        let state = load_fuse_state_from(&registry_root)?;
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
    // Stored paths are canonical. A symlink introduced at any component
    // after registration must not redirect an unmount to another worktree.
    let filesystem_root = beneath::open_root(Path::new("/"))?;
    let relative_target = target.strip_prefix("/").map_err(io::Error::other)?;
    match beneath::open_beneath(&filesystem_root, relative_target) {
        Ok(_) => {}
        Err(error) if error.kind() == io::ErrorKind::NotFound => {}
        Err(error)
            if error.raw_os_error() == Some(libc::ENOTCONN)
                && fuse_utils::is_mount_active(&target) =>
        {
            // A disconnected FUSE root cannot be opened, but still needs
            // unmounting. NOFOLLOW traversal above rejects a symlink instead.
        }
        Err(error) => {
            return Err(io::Error::other(format!(
                "cannot safely access registered FUSE mountpoint '{}': {error}; restore its original directory path before removing it",
                target.display()
            )));
        }
    }

    if let Err(err) = unmount_path(&target).await
        && fuse_utils::is_mount_active(&target)
    {
        return Err(err);
    }
    // Mount-table strings can miss path aliases. Prove that the pinned
    // workspace is back on its parent's device and empty before deleting
    // its upper layer; a live overlay must never lose its writable data.
    use std::os::unix::fs::MetadataExt;
    let parent_relative = relative_target
        .parent()
        .ok_or_else(|| io::Error::other("FUSE mountpoint has no parent"))?;
    let parent = beneath::open_beneath(&filesystem_root, parent_relative)?;
    let workspace = beneath::open_beneath(&filesystem_root, relative_target).map_err(|error| {
        io::Error::other(format!(
            "cannot verify the unmounted FUSE workspace '{}': {error}; retain its upper directory and registration",
            target.display()
        ))
    })?;
    if workspace.metadata()?.dev() != parent.metadata()?.dev() {
        return Err(io::Error::other(format!(
            "FUSE workspace '{}' is still mounted; refusing to delete its upper directory",
            target.display()
        )));
    }
    if beneath::read_dir_fd(workspace)?
        .next()
        .transpose()?
        .is_some()
    {
        return Err(io::Error::other(format!(
            "FUSE workspace '{}' is not empty after unmount; retain its upper directory and inspect it",
            target.display()
        )));
    }
    let upper = Path::new(&entry.upper_dir);
    let upper_relative = upper.strip_prefix(common).map_err(|_| io::Error::other(
        "refusing to delete FUSE upper directory outside the invoking repository; repair its registration first"))?;
    let managed_root = if let Some(name) = &entry.name {
        let expected_upper = worktree_layout::fuse_upper(common, name)?;
        if upper != expected_upper {
            return Err(io::Error::other(
                "FUSE upper directory does not match its registered name; refusing cleanup",
            ));
        }
        worktree_layout::validate_managed_path(common, upper)?;
        Some(worktree_layout::fuse_root(common, name)?)
    } else {
        let parts: Vec<_> = upper_relative.components().collect();
        if parts.len() != 3
            || parts[0].as_os_str() != "worktrees-fuse"
            || parts[2].as_os_str() != "upper"
            || !matches!(parts[1], std::path::Component::Normal(_))
        {
            return Err(io::Error::other(
                "legacy FUSE upper directory is outside its owned worktrees-fuse container; refusing cleanup",
            ));
        }
        None
    };
    fuse_utils::managed::remove_registered_tree(&registry_root, upper_relative)?;
    if let Some(root) = managed_root {
        let default_workspace = root.join("workspace");
        if target == default_workspace {
            let relative = default_workspace
                .strip_prefix(common)
                .map_err(io::Error::other)?;
            fuse_utils::managed::remove_empty_registered_directory(&registry_root, relative)?;
        }
        let relative = root.strip_prefix(common).map_err(io::Error::other)?;
        fuse_utils::managed::remove_empty_registered_directory(&registry_root, relative)?;
    }
    {
        let _state_guard = fuse_state_lock()
            .lock()
            .map_err(|_| io::Error::other("fuse state lock poisoned"))?;
        let mut state = load_fuse_state_from(&registry_root)?;
        if let Some(index) = state
            .worktrees
            .iter()
            .position(|w| Path::new(&w.path) == target)
        {
            state.worktrees.remove(index);
            save_fuse_state_to(&registry_root, &state)?;
        }
    }
    Ok(true)
}

fn prune_fuse_worktrees(common: &Path) -> io::Result<()> {
    let registry_root = beneath::open_root(common)?;
    let _state_guard = fuse_state_lock()
        .lock()
        .map_err(|_| io::Error::other("fuse state lock poisoned"))?;
    let mut state = load_fuse_state_from(&registry_root)?;
    for entry in &state.worktrees {
        if !Path::new(&entry.path).exists()
            && !entry.locked
            && let Some(name) = &entry.name
        {
            let root = worktree_layout::fuse_root(common, name)?;
            match fs::symlink_metadata(&root) {
                Ok(_) => {
                    return Err(io::Error::other(format!(
                        "FUSE worktree '{name}' is missing its mountpoint but its owned container remains; use `libra worktree remove {name}` to finish cleanup before pruning"
                    )));
                }
                Err(error) if error.kind() == io::ErrorKind::NotFound => {}
                Err(error) => return Err(error),
            }
        }
    }
    let before = state.worktrees.len();
    state.worktrees.retain(|entry| {
        let path = Path::new(&entry.path);
        path.exists() || entry.locked
    });
    if state.worktrees.len() != before {
        save_fuse_state_to(&registry_root, &state)?;
    }
    Ok(())
}

fn repair_fuse_worktrees(common: &Path) -> io::Result<()> {
    let registry_root = beneath::open_root(common)?;
    let _state_guard = fuse_state_lock()
        .lock()
        .map_err(|_| io::Error::other("fuse state lock poisoned"))?;
    let mut state = load_fuse_state_from(&registry_root)?;
    let mut seen = std::collections::HashSet::<PathBuf>::new();
    let before = state.worktrees.len();
    state.worktrees.retain(|entry| {
        let p = PathBuf::from(&entry.path);
        seen.insert(p)
    });
    if state.worktrees.len() != before {
        save_fuse_state_to(&registry_root, &state)?;
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
                    // The in-process handle may keep the mount busy until
                    // its failed unmount has consumed and dropped the handle.
                    // Retry through the platform unmount command only after
                    // that ownership has been released.
                    return fuse_utils::force_unmount_path(&path).map_err(|fallback| {
                        io::Error::other(format!(
                            "failed to unmount FUSE worktree: {ioe}; retry after releasing its handle also failed: {fallback}"
                        ))
                    });
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
    let planned_cleanup_root = if cleanup {
        Some(fuse_utils::fuse_task_worktree_cleanup_root(&mountpoint).ok_or_else(|| {
            FuseUmountError::InvalidTarget(format!(
                "--cleanup only supports legacy Libra task FUSE paths ending in '/workspace', not managed named worktrees: {}",
                mountpoint.display()
            ))
        })?)
    } else {
        None
    };
    unmount_path(&mountpoint).await.map_err(|source| {
        FuseUmountError::IoWrite(format!(
            "failed to unmount FUSE path {}: {source}",
            mountpoint.display()
        ))
    })?;

    let mut cleanup_root = None;
    let mut cleanup_root_removed = false;
    if let Some(root) = planned_cleanup_root {
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

    fn registry_entry() -> serde_json::Value {
        serde_json::json!({"path":"/tmp/fuse", "branch":"main", "upper_dir":"/repo/.libra/worktrees-fuse/id/upper", "lower_dirs":["/repo"], "locked":false, "lock_reason":null, "privileged":false, "allow_other":false})
    }

    #[test]
    fn fuse_registry_reads_legacy_and_writes_versioned_entries() {
        let legacy = serde_json::json!({"worktrees":[registry_entry()]});
        let parsed =
            FuseWorktreeState::parse(&serde_json::to_vec(&legacy).expect("json")).expect("legacy");
        assert!(parsed.worktrees[0].name.is_none());
        let written = serde_json::to_value(&parsed).expect("serialize");
        assert_eq!(written["schema_version"], 1);
        assert!(
            written.get("worktrees").is_none(),
            "legacy writers must not silently rewrite named entries"
        );
        assert_eq!(written["entries"].as_array().expect("entries").len(), 1);
        assert!(FuseWorktreeState::parse(&serde_json::to_vec(&written).expect("json")).is_ok());
    }

    #[test]
    fn fuse_registry_rejects_unknown_mixed_or_missing_schema_arrays() {
        for malformed in [
            serde_json::json!({"schema_version":2, "entries":[]}),
            serde_json::json!({"schema_version":1, "worktrees":[]}),
            serde_json::json!({"schema_version":1, "entries":[], "worktrees":[]}),
            serde_json::json!({"entries":[]}),
            serde_json::json!({}),
        ] {
            assert!(
                FuseWorktreeState::parse(&serde_json::to_vec(&malformed).expect("json")).is_err(),
                "accepted {malformed}"
            );
        }
        assert!(FuseWorktreeState::parse(b"").is_err());
        let mut invalid_binding = registry_entry();
        invalid_binding["mount_id"] = serde_json::json!("not-a-uuid");
        let document = serde_json::json!({"schema_version":1, "entries":[invalid_binding]});
        assert!(FuseWorktreeState::parse(&serde_json::to_vec(&document).expect("json")).is_err());
    }

    #[test]
    fn fuse_registry_preserves_names_when_legacy_paths_alias_named_entries() {
        for alias in ["/tmp/fuse", "/tmp/fuse/", "/tmp/./fuse"] {
            let mut unnamed = registry_entry();
            unnamed["path"] = serde_json::json!(alias);
            let mut named = registry_entry();
            named["name"] = serde_json::json!("topic");
            for entries in [vec![unnamed.clone(), named.clone()], vec![named, unnamed]] {
                let state = serde_json::json!({"schema_version":1, "entries":entries});
                assert!(
                    FuseWorktreeState::parse(&serde_json::to_vec(&state).expect("json")).is_err(),
                    "repair must not silently discard either a named or aliased registration"
                );
            }
        }
    }

    #[test]
    fn fuse_registry_reader_rejects_symlinks_fifos_and_oversized_files() {
        use std::{
            ffi::CString,
            os::unix::{ffi::OsStrExt, fs::symlink},
        };

        let temp = tempfile::tempdir().expect("tempdir");
        let root = beneath::open_root(temp.path()).expect("pin storage");
        let registry = temp.path().join("worktrees-fuse.json");
        symlink(temp.path().join("missing"), &registry).expect("dangling registry link");
        assert!(
            load_fuse_state_from(&root).is_err(),
            "a symlink is never an absent registry"
        );
        fs::remove_file(&registry).expect("remove symlink");

        let path = CString::new(registry.as_os_str().as_bytes()).expect("fifo path");
        // SAFETY: path is a valid NUL-terminated path in our isolated tempdir.
        assert_eq!(unsafe { libc::mkfifo(path.as_ptr(), 0o600) }, 0);
        assert!(
            load_fuse_state_from(&root).is_err(),
            "FIFO metadata must fail without waiting for a writer"
        );
        fs::remove_file(&registry).expect("remove FIFO");

        fs::File::create(&registry)
            .expect("oversized registry")
            .set_len(FUSE_REGISTRY_MAX_BYTES as u64 + 1)
            .expect("sparse size");
        assert!(
            load_fuse_state_from(&root).is_err(),
            "metadata size limit must precede allocation"
        );
    }

    #[test]
    fn fuse_explicit_paths_cannot_overlap_lower_upper_or_managed_container() {
        let common = Path::new("/repo/.libra");
        let upper = Path::new("/repo/.libra/worktree/fuse/topic/upper");
        let lower = vec![PathBuf::from("/repo"), PathBuf::from("/other-source")];
        for path in [
            "/repo",
            "/repo/nested",
            "/other-source/child",
            "/repo/.libra/worktree/fuse/topic/upper",
            "/repo/.libra/worktree/fuse/topic/workspace",
            "/",
        ] {
            assert!(
                validate_fuse_target(Path::new(path), &lower, upper, common, false).is_err(),
                "accepted unsafe target {path}"
            );
        }
        assert!(
            validate_fuse_target(Path::new("/outside/topic"), &lower, upper, common, false).is_ok()
        );
        assert!(
            validate_fuse_target(
                Path::new("/repo/.libra/worktree/fuse/topic/workspace"),
                &lower,
                upper,
                common,
                true
            )
            .is_ok()
        );
    }

    #[test]
    fn fuse_cli_accepts_named_controls_and_branch_alias() {
        for flag in ["--new-branch", "--create-branch"] {
            assert!(
                WorktreeArgs::try_parse_from([
                    "worktree",
                    "add",
                    "topic",
                    "--fuse",
                    flag,
                    "feature/topic",
                    "--path",
                    "/outside/topic"
                ])
                .is_ok()
            );
        }
        assert!(WorktreeArgs::try_parse_from(["worktree", "remove", "topic"]).is_ok());
        assert!(
            WorktreeArgs::try_parse_from(["worktree", "remove", "--path", "/outside/topic"])
                .is_ok()
        );
        assert!(
            WorktreeArgs::try_parse_from(["worktree", "move", "topic", "--path", "/outside/moved"])
                .is_ok()
        );
        assert!(
            WorktreeArgs::try_parse_from([
                "worktree",
                "remove",
                "topic",
                "--path",
                "/outside/topic"
            ])
            .is_err()
        );
    }

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
    /// (the retired terminal-control error / CherryPickError / RevertError /
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
