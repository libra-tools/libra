//! Task workspace preparation and synchronization for orchestrated AI execution.
//!
//! Boundary: each task receives an isolated copy or FUSE overlay of the main workspace,
//! then allowed changes are synced back after scope checks. Tests in this module cover
//! file copy, symlink handling, deletion, contract violations, and cleanup behavior.

#[cfg(target_os = "macos")]
use std::sync::Mutex;
use std::{
    collections::BTreeSet,
    fs, io,
    path::{Path, PathBuf},
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
};
#[cfg(unix)]
use std::{
    thread,
    time::{Duration, Instant},
};

#[cfg(unix)]
use libfuse_fs::{
    overlayfs::{OverlayFs, config::Config as FuseOverlayConfig},
    passthrough::{PassthroughArgs, new_passthroughfs_layer},
};
#[cfg(unix)]
use rfuse3::{MountOptions, raw::Session};
#[cfg(unix)]
use tokio::runtime::Handle;
#[cfg(unix)]
use tracing::warn;
use uuid::Uuid;

use super::{
    acl::{ScopeVerdict, cargo_lock_companion_allowed, check_scope},
    types::TaskWorkspaceBackend,
};
#[cfg(unix)]
use crate::utils::fuse as fuse_utils;
use crate::{
    internal::ai::{
        agent_run::{
            AgentRunId,
            event::{AgentRunEvent, WorkspaceStrategy},
            event_store::AgentRunEventStore,
            workspace_strategy::{
                MaterializationUnavailable, WorkspaceSizing, full_copy_fallback_warning,
                record_materialization, resolve_after_preferred_attempt, select_preferred_strategy,
            },
        },
        workspace_snapshot::{
            WorkspaceEntry, WorkspaceSnapshot, changed_paths_since_baseline, snapshot_workspace,
            workspace_entry_if_exists,
        },
    },
    utils::util,
};

pub(crate) struct TaskWorktree {
    pub(crate) root: PathBuf,
    pub(crate) baseline: WorkspaceSnapshot,
    backend: TaskWorktreeBackend,
}

impl TaskWorktree {
    pub(crate) fn backend(&self) -> TaskWorkspaceBackend {
        match &self.backend {
            TaskWorktreeBackend::Copy { .. } => TaskWorkspaceBackend::Copy,
            #[cfg(unix)]
            TaskWorktreeBackend::Fuse(_) => TaskWorkspaceBackend::Fuse,
        }
    }
}

enum TaskWorktreeBackend {
    Copy {
        cleanup_root: PathBuf,
    },
    #[cfg(unix)]
    Fuse(FuseTaskWorktreeBackend),
}

#[cfg(unix)]
struct FuseTaskWorktreeBackend {
    cleanup_root: PathBuf,
    mount_handle: rfuse3::raw::MountHandle,
}

struct TaskWorktreePaths {
    cleanup_root: PathBuf,
    workspace_root: PathBuf,
    #[cfg(unix)]
    lower_root: PathBuf,
    #[cfg(unix)]
    upper_root: PathBuf,
}

#[cfg(target_os = "macos")]
static MACOS_FUSE_MOUNT_HANDSHAKE_LOCK: Mutex<()> = Mutex::new(());

#[cfg(all(unix, test))]
const FUSE_HEALTH_CHECK_TIMEOUT: Duration = Duration::from_millis(10);
#[cfg(all(unix, not(test)))]
const FUSE_HEALTH_CHECK_TIMEOUT: Duration = Duration::from_secs(3);
#[cfg(all(unix, test))]
const FUSE_HEALTH_CHECK_INTERVAL: Duration = Duration::from_millis(1);
#[cfg(all(unix, not(test)))]
const FUSE_HEALTH_CHECK_INTERVAL: Duration = Duration::from_millis(50);

/// FUSE provisioning gate. Once `disabled` flips to `true`,
/// `prepare_task_worktree` skips FUSE entirely and goes directly to the copy
/// backend.
///
/// The flag is shared via a single `Arc<AtomicBool>` so concurrent task
/// provisioning sees consistent state. Tasks that race into the *first*
/// FUSE attempt still snapshot and materialize their lower/upper directories
/// independently; whichever attempts finish first set the flag, and every
/// subsequent task short-circuits past the FUSE path. On macOS only the
/// `mount_macfuse` handshake is serialized after materialization, avoiding
/// device allocation races without delaying baseline capture.
///
/// The orchestrator owns one `FuseProvisionState` for its entire lifetime so
/// the disable signal persists across orchestrator runs in the same process —
/// not just across replans within a single run. Without this persistence the
/// orchestrator would re-attempt (and re-fail) FUSE on every new intent the
/// user submits in the same TUI session.
#[derive(Clone, Debug)]
pub struct FuseProvisionState {
    disabled: Arc<AtomicBool>,
}

impl Default for FuseProvisionState {
    fn default() -> Self {
        Self {
            disabled: Arc::new(AtomicBool::new(fuse_disabled_by_default())),
        }
    }
}

impl FuseProvisionState {
    /// Atomically mark FUSE disabled for this session. Returns `true` iff this
    /// call was the first to flip the flag; the caller is then responsible for
    /// emitting the one-time TUI note.
    pub fn disable_first_time(&self) -> bool {
        self.disabled
            .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
            .is_ok()
    }

    pub fn is_disabled(&self) -> bool {
        self.disabled.load(Ordering::Acquire)
    }
}

fn fuse_disabled_by_default() -> bool {
    #[cfg(test)]
    {
        true
    }
    #[cfg(not(test))]
    {
        std::env::var_os(crate::utils::pager::LIBRA_TEST_ENV).is_some()
    }
}

/// Outcome of a FUSE provisioning attempt during `prepare_task_worktree`.
/// Reported back to the caller so the orchestrator can emit a single
/// user-visible note when FUSE flips from "available" to "disabled".
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FuseAttemptOutcome {
    /// FUSE overlay mounted successfully and is the active backend.
    Mounted,
    /// FUSE was already disabled session-wide before this task; copy backend used.
    Skipped,
    /// FUSE was already disabled by an earlier failure; copy backend used.
    AlreadyDisabled,
    /// This task was the first to fail FUSE — it triggered the session disable.
    /// The caller must emit the one-time "FUSE disabled" TUI note.
    JustDisabled { reason: String },
    /// Platform without FUSE support (non-unix); copy backend used.
    Unsupported,
}

impl FuseAttemptOutcome {
    pub fn disabled_reason(&self) -> Option<&str> {
        match self {
            Self::JustDisabled { reason } => Some(reason.as_str()),
            _ => None,
        }
    }
}

#[cfg(unix)]
enum FuseTaskWorktreeProvision {
    Mounted(TaskWorktreeBackend),
    Fallback { reason: String },
}

pub(crate) fn prepare_task_worktree(
    main_working_dir: &Path,
    task_id: Uuid,
    fuse_state: &FuseProvisionState,
) -> io::Result<(TaskWorktree, FuseAttemptOutcome)> {
    let baseline = snapshot_workspace(main_working_dir)?;
    #[cfg(not(unix))]
    let _ = fuse_state;

    #[cfg(unix)]
    {
        if !fuse_state.is_disabled() {
            let fuse_paths = task_worktree_paths(main_working_dir, task_id, "fuse");
            match prepare_fuse_task_worktree(main_working_dir, &fuse_paths, &baseline)? {
                FuseTaskWorktreeProvision::Mounted(backend) => {
                    return Ok((
                        TaskWorktree {
                            root: fuse_paths.workspace_root,
                            baseline,
                            backend,
                        },
                        FuseAttemptOutcome::Mounted,
                    ));
                }
                FuseTaskWorktreeProvision::Fallback { reason } => {
                    // Mount or health check failed; flip the session-wide flag.
                    let outcome = if fuse_state.disable_first_time() {
                        FuseAttemptOutcome::JustDisabled { reason }
                    } else {
                        FuseAttemptOutcome::AlreadyDisabled
                    };
                    return prepare_task_worktree_copy_fallback(
                        main_working_dir,
                        task_id,
                        baseline,
                        outcome,
                    );
                }
            }
        }
    }

    #[cfg(unix)]
    let outcome = FuseAttemptOutcome::AlreadyDisabled;
    #[cfg(not(unix))]
    let outcome = FuseAttemptOutcome::Unsupported;

    prepare_task_worktree_copy_fallback(main_working_dir, task_id, baseline, outcome)
}

fn prepare_task_worktree_copy_fallback(
    main_working_dir: &Path,
    task_id: Uuid,
    baseline: WorkspaceSnapshot,
    outcome: FuseAttemptOutcome,
) -> io::Result<(TaskWorktree, FuseAttemptOutcome)> {
    let copy_paths = task_worktree_paths(main_working_dir, task_id, "copy");
    prepare_task_worktree_root(&copy_paths.cleanup_root)?;
    // The cleanup root now exists on disk. If the copy materialization
    // fails partway through, remove it before surfacing the error so a
    // mid-copy failure does not leak a partial `libra-task-worktree-*`
    // directory (CEX-S2-11 (5): no leaked workspaces). The caller
    // receives only the error and has no handle to clean up itself.
    let backend = remove_partial_workspace_on_error(
        &copy_paths.cleanup_root,
        prepare_copy_task_worktree(main_working_dir, &copy_paths, &baseline),
    )?;

    Ok((
        TaskWorktree {
            root: copy_paths.workspace_root,
            baseline,
            backend,
        },
        outcome,
    ))
}

fn task_worktree_paths(main_working_dir: &Path, task_id: Uuid, backend: &str) -> TaskWorktreePaths {
    let cleanup_root = task_worktree_base_dir(main_working_dir).join(format!(
        "libra-task-worktree-{}-{}-{}",
        backend,
        std::process::id(),
        task_id
    ));
    TaskWorktreePaths {
        workspace_root: cleanup_root.join("workspace"),
        #[cfg(unix)]
        lower_root: cleanup_root.join("lower"),
        #[cfg(unix)]
        upper_root: cleanup_root.join("upper"),
        cleanup_root,
    }
}

fn task_worktree_base_dir(main_working_dir: &Path) -> PathBuf {
    match util::try_get_storage_path(Some(main_working_dir.to_path_buf())) {
        Ok(storage) => storage.join("worktrees").join("tasks"),
        Err(err) if err.kind() == io::ErrorKind::NotFound => task_worktree_temp_dir(),
        Err(err) => {
            tracing::warn!(
                path = %main_working_dir.display(),
                "failed to resolve Libra storage for task worktree; using temporary directory: {}",
                err
            );
            task_worktree_temp_dir()
        }
    }
}

fn task_worktree_temp_dir() -> PathBuf {
    let temp_dir = std::env::temp_dir();
    #[cfg(target_os = "macos")]
    {
        fs::canonicalize(&temp_dir).unwrap_or(temp_dir)
    }
    #[cfg(not(target_os = "macos"))]
    {
        temp_dir
    }
}

fn prepare_task_worktree_root(cleanup_root: &Path) -> io::Result<()> {
    if cleanup_root.exists() {
        fs::remove_dir_all(cleanup_root)?;
    }
    fs::create_dir_all(cleanup_root)
}

fn prepare_copy_task_worktree(
    main_working_dir: &Path,
    paths: &TaskWorktreePaths,
    baseline: &WorkspaceSnapshot,
) -> io::Result<TaskWorktreeBackend> {
    fs::create_dir_all(&paths.workspace_root)?;
    match util::try_get_storage_path(Some(main_working_dir.to_path_buf())) {
        Ok(storage) => link_repo_storage(
            &storage,
            &paths.workspace_root.join(util::ROOT_DIR),
            "copy task worktree",
        )?,
        Err(err) if err.kind() == io::ErrorKind::NotFound => {}
        Err(err) => return Err(err),
    }
    materialize_workspace(main_working_dir, &paths.workspace_root, baseline)?;
    Ok(TaskWorktreeBackend::Copy {
        cleanup_root: paths.cleanup_root.clone(),
    })
}

#[cfg(unix)]
fn prepare_fuse_task_worktree(
    main_working_dir: &Path,
    paths: &TaskWorktreePaths,
    baseline: &WorkspaceSnapshot,
) -> io::Result<FuseTaskWorktreeProvision> {
    let Ok(runtime) = Handle::try_current() else {
        return Ok(FuseTaskWorktreeProvision::Fallback {
            reason: "tokio runtime unavailable for FUSE provisioning".to_string(),
        });
    };

    prepare_task_worktree_root(&paths.cleanup_root)?;
    // The cleanup root now exists; if FUSE layer setup fails, remove it
    // before surfacing the error so a partial workspace is not leaked
    // (CEX-S2-11 (5)). The mount-failure arms below already clean up via
    // `warn_cleanup_root_failure` + `Fallback`; this covers the `?`
    // io-error path that would otherwise leak the directory. Shares the
    // same `remove_partial_workspace_on_error` helper as the copy path.
    let expect_repo_storage_link = remove_partial_workspace_on_error(
        &paths.cleanup_root,
        prepare_fuse_task_worktree_layers(main_working_dir, paths, baseline),
    )?;

    let mount_result = mount_fuse_task_worktree_on_runtime(
        &runtime,
        &paths.lower_root,
        &paths.workspace_root,
        &paths.upper_root,
    );

    match mount_result {
        Ok(mount_handle) => {
            if let Err(err) =
                verify_fuse_task_worktree_mount(&paths.workspace_root, expect_repo_storage_link)
            {
                if let Err(unmount_err) = runtime.block_on(mount_handle.unmount()) {
                    warn!(
                        mount = %paths.workspace_root.display(),
                        "failed to unmount unhealthy FUSE task worktree before fallback: {}",
                        unmount_err
                    );
                }
                warn_cleanup_root_failure(&paths.cleanup_root);
                let reason = err.to_string();
                warn!(
                    path = %main_working_dir.display(),
                    mount = %paths.workspace_root.display(),
                    "mounted FUSE task worktree failed health check, falling back to copy backend: {}",
                    reason
                );
                return Ok(FuseTaskWorktreeProvision::Fallback { reason });
            }

            Ok(FuseTaskWorktreeProvision::Mounted(
                TaskWorktreeBackend::Fuse(FuseTaskWorktreeBackend {
                    cleanup_root: paths.cleanup_root.clone(),
                    mount_handle,
                }),
            ))
        }
        Err(err) => {
            warn_cleanup_root_failure(&paths.cleanup_root);
            let reason = err.to_string();
            warn!(
                path = %main_working_dir.display(),
                mount = %paths.workspace_root.display(),
                "failed to mount FUSE task worktree, falling back to copy backend: {}",
                reason
            );
            Ok(FuseTaskWorktreeProvision::Fallback { reason })
        }
    }
}

#[cfg(unix)]
fn prepare_fuse_task_worktree_layers(
    main_working_dir: &Path,
    paths: &TaskWorktreePaths,
    baseline: &WorkspaceSnapshot,
) -> io::Result<bool> {
    fs::create_dir_all(&paths.workspace_root)?;
    fs::create_dir_all(&paths.lower_root)?;
    fs::create_dir_all(&paths.upper_root)?;

    // Keep the upper layer as a complete writable workspace. Build tools in
    // different ecosystems create and rename arbitrary generated directories;
    // relying on per-directory copy-up makes that path filesystem-specific and
    // can reject otherwise valid writes.
    materialize_workspace(main_working_dir, &paths.upper_root, baseline)?;

    match util::try_get_storage_path(Some(main_working_dir.to_path_buf())) {
        Ok(storage) => {
            link_repo_storage(
                &storage,
                &paths.upper_root.join(util::ROOT_DIR),
                "FUSE upper layer",
            )?;
            Ok(true)
        }
        Err(err) if err.kind() == io::ErrorKind::NotFound => Ok(false),
        Err(err) => Err(err),
    }
}

#[cfg(unix)]
fn mount_fuse_task_worktree_on_runtime(
    runtime: &Handle,
    lower_root: &Path,
    workspace_root: &Path,
    upper_root: &Path,
) -> io::Result<rfuse3::raw::MountHandle> {
    #[cfg(target_os = "macos")]
    {
        let _guard = MACOS_FUSE_MOUNT_HANDSHAKE_LOCK
            .lock()
            .map_err(|_| io::Error::other("macOS FUSE mount handshake lock poisoned"))?;
        runtime.block_on(mount_fuse_task_worktree(
            lower_root,
            workspace_root,
            upper_root,
        ))
    }

    #[cfg(not(target_os = "macos"))]
    {
        runtime.block_on(mount_fuse_task_worktree(
            lower_root,
            workspace_root,
            upper_root,
        ))
    }
}

#[cfg(unix)]
fn verify_fuse_task_worktree_mount(
    workspace_root: &Path,
    expect_repo_storage_link: bool,
) -> io::Result<()> {
    let started = Instant::now();
    let mut attempts = 0_u32;

    loop {
        attempts += 1;
        match verify_fuse_task_worktree_mount_once(workspace_root, expect_repo_storage_link) {
            Ok(()) => return Ok(()),
            Err(err) if started.elapsed() >= FUSE_HEALTH_CHECK_TIMEOUT => {
                return Err(io::Error::new(
                    err.kind(),
                    format!(
                        "FUSE mount health check failed after {} attempts over {:?}: workspace={}, expected_repo_storage_link={}: {}",
                        attempts,
                        started.elapsed(),
                        workspace_root.display(),
                        expect_repo_storage_link,
                        err
                    ),
                ));
            }
            Err(_) => thread::sleep(FUSE_HEALTH_CHECK_INTERVAL),
        }
    }
}

#[cfg(unix)]
fn verify_fuse_task_worktree_mount_once(
    workspace_root: &Path,
    expect_repo_storage_link: bool,
) -> io::Result<()> {
    fs::read_dir(workspace_root).map_err(|err| {
        io::Error::new(
            err.kind(),
            format!(
                "mounted workspace root is not readable at '{}': {}",
                workspace_root.display(),
                err
            ),
        )
    })?;

    if expect_repo_storage_link {
        verify_fuse_repo_storage_link(workspace_root)?;
    }

    verify_fuse_task_worktree_write_probe(workspace_root)?;

    Ok(())
}

#[cfg(unix)]
fn verify_fuse_repo_storage_link(workspace_root: &Path) -> io::Result<()> {
    let storage_link = workspace_root.join(util::ROOT_DIR);
    let metadata = fs::symlink_metadata(&storage_link).map_err(|err| {
        io::Error::new(
            err.kind(),
            format!(
                "expected .libra repository storage link is not visible at '{}': {}",
                storage_link.display(),
                err
            ),
        )
    })?;

    if metadata.file_type().is_symlink() {
        let target = fs::read_link(&storage_link).map_err(|err| {
            io::Error::new(
                err.kind(),
                format!(
                    "expected .libra repository storage link is not readable at '{}': {}",
                    storage_link.display(),
                    err
                ),
            )
        })?;
        let resolved = if target.is_absolute() {
            target
        } else {
            storage_link.parent().unwrap_or(workspace_root).join(target)
        };
        if !resolved.exists() {
            return Err(io::Error::new(
                io::ErrorKind::NotFound,
                format!(
                    "expected .libra repository storage link at '{}' points to missing target '{}'",
                    storage_link.display(),
                    resolved.display()
                ),
            ));
        }
    }

    util::try_get_storage_path(Some(workspace_root.to_path_buf())).map_err(|err| {
        io::Error::new(
            err.kind(),
            format!(
                "expected .libra repository storage link at '{}' is not usable by Libra VCS: {}",
                storage_link.display(),
                err
            ),
        )
    })?;

    Ok(())
}

#[cfg(unix)]
fn verify_fuse_task_worktree_write_probe(workspace_root: &Path) -> io::Result<()> {
    let probe_name = format!(".libra-fuse-write-probe-{}", std::process::id());
    let probe_tmp = workspace_root.join(format!("{probe_name}.tmp"));
    let probe_final = workspace_root.join(probe_name);
    let _ = fs::remove_dir_all(&probe_tmp);
    let _ = fs::remove_dir_all(&probe_final);

    let result = (|| {
        fs::create_dir(&probe_tmp).map_err(|err| {
            io::Error::new(
                err.kind(),
                format!(
                    "mounted workspace root is not writable; failed to create probe directory '{}': {}",
                    probe_tmp.display(),
                    err
                ),
            )
        })?;
        fs::write(probe_tmp.join("probe.txt"), b"ok\n").map_err(|err| {
            io::Error::new(
                err.kind(),
                format!(
                    "mounted workspace root is not writable; failed to write probe file under '{}': {}",
                    probe_tmp.display(),
                    err
                ),
            )
        })?;
        fs::rename(&probe_tmp, &probe_final).map_err(|err| {
            io::Error::new(
                err.kind(),
                format!(
                    "mounted workspace root does not support generated directory rename from '{}' to '{}': {}",
                    probe_tmp.display(),
                    probe_final.display(),
                    err
                ),
            )
        })?;
        fs::remove_dir_all(&probe_final).map_err(|err| {
            io::Error::new(
                err.kind(),
                format!(
                    "mounted workspace root is not cleanup-writable; failed to remove probe directory '{}': {}",
                    probe_final.display(),
                    err
                ),
            )
        })
    })();

    if result.is_err() {
        let _ = fs::remove_dir_all(&probe_tmp);
        let _ = fs::remove_dir_all(&probe_final);
    }

    result
}

#[cfg(unix)]
async fn mount_fuse_task_worktree(
    lower_root: &Path,
    workspace_root: &Path,
    upper_root: &Path,
) -> io::Result<rfuse3::raw::MountHandle> {
    let lower_layer = Arc::new(
        new_passthroughfs_layer(PassthroughArgs {
            root_dir: lower_root,
            mapping: None::<&str>,
        })
        .await
        .map_err(|err| {
            fuse_mount_step_error(
                format!(
                    "failed to create FUSE lower passthrough layer at {}",
                    lower_root.display()
                ),
                err,
            )
        })?,
    );
    let upper_layer = Arc::new(
        new_passthroughfs_layer(PassthroughArgs {
            root_dir: upper_root,
            mapping: None::<&str>,
        })
        .await
        .map_err(|err| {
            fuse_mount_step_error(
                format!(
                    "failed to create FUSE upper passthrough layer at {}",
                    upper_root.display()
                ),
                err,
            )
        })?,
    );

    let overlay = OverlayFs::new(
        Some(upper_layer),
        vec![lower_layer],
        FuseOverlayConfig {
            mountpoint: workspace_root.to_path_buf(),
            do_import: true,
            writeback: true,
            ..Default::default()
        },
        1,
    )
    .map_err(|err| {
        fuse_mount_step_error(
            format!(
                "failed to create FUSE overlay for mount {}",
                workspace_root.display()
            ),
            err,
        )
    })?;

    let uid = unsafe { libc::getuid() };
    let gid = unsafe { libc::getgid() };
    let mut mount_options = MountOptions::default();
    #[cfg(target_os = "linux")]
    mount_options.force_readdir_plus(true);
    mount_options
        .uid(uid)
        .gid(gid)
        .fs_name("libra-task-worktree");

    Session::new(mount_options)
        .mount_with_unprivileged(overlay, workspace_root.as_os_str())
        .await
        .map_err(|err| {
            fuse_mount_step_error(
                format!(
                    "failed to mount FUSE overlay at {}",
                    workspace_root.display()
                ),
                err,
            )
        })
}

#[cfg(unix)]
fn fuse_mount_step_error(context: String, err: io::Error) -> io::Error {
    io::Error::new(err.kind(), format!("{context}: {err}"))
}

pub(crate) fn cleanup_task_worktree(worktree: TaskWorktree) -> io::Result<()> {
    match worktree.backend {
        TaskWorktreeBackend::Copy { cleanup_root } => remove_cleanup_root(&cleanup_root),
        #[cfg(unix)]
        TaskWorktreeBackend::Fuse(fuse) => cleanup_fuse_task_worktree(fuse),
    }
}

/// A materialized isolated workspace for a sub-agent run (CEX-S2-11),
/// plus the [`WorkspaceStrategy`] that produced it. Wraps the underlying
/// [`TaskWorktree`] so callers get the run-scoped lifecycle
/// ([`SubAgentWorkspace::cleanup`]) without touching the worktree
/// internals.
///
/// Nominal visibility is `pub` (the module itself stays `pub(crate)`)
/// so the AG-22 `materialize_isolated_workspace` seam — a `pub fn` on a
/// fully public path — can return it without a `private_interfaces`
/// lint; outside the crate the type is reachable through that seam but
/// not nameable.
///
// `allow(dead_code)`: the materialization abstraction lands ahead of the
// flag-gated sub-agent dispatcher wiring that calls it (a later CEX-S2-11
// slice), matching the doc's "abstraction before runtime" sequencing.
#[allow(dead_code)]
pub struct SubAgentWorkspace {
    worktree: TaskWorktree,
    strategy: WorkspaceStrategy,
}

#[allow(dead_code)]
impl SubAgentWorkspace {
    /// Filesystem root the sub-agent should run in.
    pub fn root(&self) -> &Path {
        &self.worktree.root
    }

    /// The strategy recorded in the `workspace_materialized` event.
    pub fn strategy(&self) -> WorkspaceStrategy {
        self.strategy
    }

    /// The physical materialization backend (FUSE overlay vs full copy).
    /// Orthogonal to [`strategy`](Self::strategy): until native
    /// object-store-sharing worktrees and sparse checkout land, every
    /// strategy is physically materialized through
    /// [`prepare_task_worktree`].
    pub fn backend(&self) -> TaskWorkspaceBackend {
        self.worktree.backend()
    }

    /// Tear down the workspace (unmount FUSE / remove the copy). Per
    /// CEX-S2-11 (5) this must run on run completion so workspaces do not
    /// leak.
    pub fn cleanup(self) -> io::Result<()> {
        cleanup_task_worktree(self.worktree)
    }
}

/// Error materializing a sub-agent's isolated workspace.
///
/// `pub` (not `pub(crate)`) for the same seam-signature reason as
/// [`SubAgentWorkspace`].
#[allow(dead_code)]
#[derive(Debug, thiserror::Error)]
pub enum SubAgentWorkspaceError {
    /// The preferred strategy could not be materialized and full-copy
    /// fallback was not permitted (`agent.allow_full_copy = false`).
    /// Surfaced WITHOUT touching the filesystem.
    #[error(transparent)]
    Unavailable(#[from] MaterializationUnavailable),
    /// A filesystem error occurred while materializing the workspace or
    /// appending the `workspace_materialized` event.
    #[error("failed to materialize sub-agent workspace: {0}")]
    Io(#[from] io::Error),
}

/// Materialize an isolated workspace for a sub-agent run and append the
/// `workspace_materialized` event to the run's transcript (CEX-S2-11).
///
/// `sizing` (from [`workspace_sizing::measure_workspace_sizing`]) drives
/// [`select_preferred_strategy`]. The preferred strategy is materialized
/// through [`prepare_task_worktree`] — a FUSE overlay when available,
/// else a full copy:
///
/// - `Worktree` (small repo) → materialized directly; strategy
///   `Worktree`.
/// - `Sparse` (large repo) → **no native sparse-checkout API exists
///   yet**, so it is treated as a failed preferred attempt and resolved
///   via [`resolve_after_preferred_attempt`]: with `allow_full_copy` it
///   materializes a full copy (strategy `FullCopy`, carrying the reason
///   as the audit `fallback_reason`); otherwise it returns
///   [`SubAgentWorkspaceError::Unavailable`] **without** materializing.
///
/// On success the `workspace_materialized` event records the resolved
/// strategy, the source/materialized file count, elapsed time, and any
/// fallback reason, and is appended to
/// `.libra/sessions/{thread_id}/agents/{run_id}.jsonl` via `store`.
///
/// The physical backend (FUSE overlay vs full copy) is orthogonal to the
/// recorded strategy and is available via
/// [`SubAgentWorkspace::backend`]; the strategy field records the
/// size-based selection, which is the doc's taxonomy. Native
/// object-store-sharing `Worktree` and `Sparse` materialization remain
/// pending CEX-S2-11 slices.
///
/// [`workspace_sizing::measure_workspace_sizing`]: crate::internal::ai::agent_run::workspace_sizing::measure_workspace_sizing
#[allow(dead_code)]
pub(crate) fn materialize_sub_agent_workspace(
    main_working_dir: &Path,
    sizing: WorkspaceSizing,
    thread_id: Uuid,
    run_id: AgentRunId,
    allow_full_copy: bool,
    fuse_state: &FuseProvisionState,
    store: &AgentRunEventStore,
) -> Result<SubAgentWorkspace, SubAgentWorkspaceError> {
    let preferred = select_preferred_strategy(sizing);

    // Resolve the final strategy BEFORE any filesystem work so a
    // disallowed full-copy fallback fails fast without materializing.
    let (final_strategy, fallback_reason) = if preferred == WorkspaceStrategy::Worktree {
        (WorkspaceStrategy::Worktree, None)
    } else {
        // Sparse (the only other size-selected strategy) has no native
        // materializer yet — treat it as a failed preferred attempt.
        resolve_after_preferred_attempt(
            preferred,
            Err("native sparse checkout is not yet implemented; \
                 falling back to a full workspace copy"
                .to_string()),
            allow_full_copy,
        )?
    };

    let start = std::time::Instant::now();
    let (worktree, _outcome) = prepare_task_worktree(main_working_dir, run_id.0, fuse_state)?;
    let elapsed_ms = u64::try_from(start.elapsed().as_millis()).unwrap_or(u64::MAX);
    // The baseline is the source snapshot the workspace mirrors, so its
    // entry count is the count of files the workspace exposes. For the
    // copy backend this equals files physically written; for a lazy FUSE
    // overlay it is the logical count (the overlay exposes the same tree
    // without eagerly copying every file).
    let materialized_file_count = worktree.baseline.entries.len() as u64;

    // CEX-S2-11 (2): a full-copy fallback is opt-in-gated and expensive
    // (it duplicates the whole worktree), so flag it in the audit log
    // alongside the structured `WorkspaceMaterialized` event that carries
    // the same `fallback_reason`. Non-fallback strategies stay silent.
    if let Some(warning) = full_copy_fallback_warning(final_strategy, fallback_reason.as_deref()) {
        tracing::warn!(
            run_id = %run_id.0,
            thread_id = %thread_id,
            elapsed_ms,
            materialized_file_count,
            source_repo_size = sizing.repo_size_bytes,
            "{warning}",
        );
    }

    let materialization = record_materialization(
        final_strategy,
        sizing,
        materialized_file_count,
        elapsed_ms,
        fallback_reason,
    );

    // The workspace is already on disk. `TaskWorktree` has no `Drop`, so a
    // failed transcript append would otherwise leak it — clean up before
    // surfacing the error (CEX-S2-11 (5): no leaked workspaces).
    if let Err(append_error) = store.append(
        thread_id,
        run_id,
        &AgentRunEvent::WorkspaceMaterialized {
            agent_run_id: run_id,
            materialization,
        },
    ) {
        if let Err(cleanup_error) = cleanup_task_worktree(worktree) {
            tracing::warn!(
                run_id = %run_id.0,
                "failed to clean up sub-agent workspace after transcript append error: {cleanup_error}",
            );
        }
        return Err(SubAgentWorkspaceError::Io(append_error));
    }

    Ok(SubAgentWorkspace {
        worktree,
        strategy: final_strategy,
    })
}

#[cfg(unix)]
fn cleanup_fuse_task_worktree(worktree: FuseTaskWorktreeBackend) -> io::Result<()> {
    let workspace_root = worktree.cleanup_root.join("workspace");
    let runtime = Handle::try_current().map_err(|err| {
        io::Error::other(format!("tokio runtime unavailable for FUSE cleanup: {err}"))
    })?;
    if let Err(err) = runtime.block_on(worktree.mount_handle.unmount()) {
        if is_fuse_unmount_already_inactive_error(&err) {
            warn!(
                path = %worktree.cleanup_root.display(),
                "FUSE task worktree mount was already inactive during cleanup: {}",
                err
            );
        } else {
            fuse_utils::force_unmount_path(&workspace_root).map_err(|fallback_err| {
                io::Error::new(
                    fallback_err.kind(),
                    format!(
                        "failed to unmount FUSE task worktree at {} after mount handle error ({}): {}",
                        workspace_root.display(),
                        err,
                        fallback_err
                    ),
                )
            })?;
        }
    }

    match remove_cleanup_root(&worktree.cleanup_root) {
        Ok(()) => Ok(()),
        Err(err)
            if is_fuse_cleanup_busy_error(&err) || fuse_utils::is_mount_active(&workspace_root) =>
        {
            fuse_utils::force_unmount_path(&workspace_root).map_err(|unmount_err| {
                io::Error::new(
                    unmount_err.kind(),
                    format!(
                        "failed to unmount busy FUSE task worktree at {} before cleanup retry: {}",
                        workspace_root.display(),
                        unmount_err
                    ),
                )
            })?;
            remove_cleanup_root(&worktree.cleanup_root)
        }
        Err(err) => Err(err),
    }
}

#[cfg(unix)]
fn is_fuse_unmount_already_inactive_error(err: &io::Error) -> bool {
    matches!(
        err.raw_os_error(),
        Some(libc::EINVAL) | Some(libc::ENOENT) | Some(libc::ENOTCONN)
    )
}

#[cfg(unix)]
fn is_fuse_cleanup_busy_error(err: &io::Error) -> bool {
    matches!(err.raw_os_error(), Some(libc::EBUSY))
}

fn remove_cleanup_root(cleanup_root: &Path) -> io::Result<()> {
    if cleanup_root.exists() {
        fs::remove_dir_all(cleanup_root)?;
    }
    Ok(())
}

/// Pass through `result`, but if it is `Err`, remove the
/// already-created `cleanup_root` first so a materialization that fails
/// *after* the workspace root exists does not leak a partial
/// `libra-task-worktree-*` directory (CEX-S2-11 (5): no leaked
/// workspaces). Shared by the copy and FUSE materialization paths,
/// which both create the root and then run a fallible materializer the
/// caller has no handle to clean up.
fn remove_partial_workspace_on_error<T>(
    cleanup_root: &Path,
    result: io::Result<T>,
) -> io::Result<T> {
    if result.is_err()
        && let Err(cleanup_err) = remove_cleanup_root(cleanup_root)
    {
        tracing::warn!(
            path = %cleanup_root.display(),
            "failed to remove partial task worktree after materialization error: {cleanup_err}",
        );
    }
    result
}

#[cfg(unix)]
fn warn_cleanup_root_failure(cleanup_root: &Path) {
    if let Err(err) = remove_cleanup_root(cleanup_root) {
        warn!(
            path = %cleanup_root.display(),
            "failed to clean up abandoned task worktree root: {}",
            err
        );
    }
}

pub(crate) fn sync_task_worktree_back(
    main_working_dir: &Path,
    task_worktree_dir: &Path,
    baseline: &WorkspaceSnapshot,
    touch_files: &[String],
    in_scope: &[String],
    out_of_scope: &[String],
) -> Result<SyncBackReport, WorkspaceSyncError> {
    let task_snapshot = snapshot_workspace(task_worktree_dir)
        .map_err(|err| workspace_sync_io_error("snapshot task worktree", task_worktree_dir, err))?;
    let changed_paths = changed_paths_since_baseline(baseline, &task_snapshot);
    let changed_path_set = changed_paths.iter().cloned().collect::<BTreeSet<_>>();

    let violations =
        collect_contract_violations(&changed_paths, touch_files, in_scope, out_of_scope);
    if !violations.is_empty() {
        return Err(WorkspaceSyncError::ContractViolation(
            format_contract_violation_message(&violations),
        ));
    }

    let mut report = SyncBackReport::default();
    for rel_path in changed_paths {
        let baseline_entry = baseline.entries.get(&rel_path).cloned();
        let task_entry = task_snapshot.entries.get(&rel_path).cloned();
        let main_path = main_working_dir.join(&rel_path);
        let current_entry = workspace_entry_if_exists(&main_path)
            .map_err(|err| workspace_sync_io_error("inspect main workspace", &main_path, err))?;

        if current_entry == task_entry {
            report.already_applied.push(rel_path);
            continue;
        }

        if stale_cargo_lock_companion(&rel_path, &changed_path_set) {
            report.skipped.push(SkippedSyncPath {
                path: rel_path,
                reason: "Cargo.lock changed without a matching Cargo.toml change; treating it as a stale verification side effect".to_string(),
            });
            continue;
        }

        if current_entry == baseline_entry {
            apply_task_change(
                task_worktree_dir,
                main_working_dir,
                &rel_path,
                &task_snapshot,
            )?;
            report.applied.push(rel_path);
            continue;
        }

        if is_cargo_lock_path(&rel_path)
            && cargo_manifest_changed_for_lock(&rel_path, &changed_path_set)
        {
            return Err(WorkspaceSyncError::RetryableConflict {
                path: rel_path,
                reason: "Cargo.toml and Cargo.lock both changed, but the main workspace lockfile diverged from this task's lockfile".to_string(),
            });
        }

        if try_merge_text_change(
            main_working_dir,
            task_worktree_dir,
            baseline,
            &rel_path,
            baseline_entry.as_ref(),
            current_entry.as_ref(),
            task_entry.as_ref(),
        )? {
            report.merged.push(rel_path);
            continue;
        }

        return Err(WorkspaceSyncError::RetryableConflict {
            path: rel_path,
            reason:
                "main workspace changed concurrently and the task change could not be merged safely"
                    .to_string(),
        });
    }

    Ok(report)
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct SyncBackReport {
    pub(crate) applied: Vec<PathBuf>,
    pub(crate) already_applied: Vec<PathBuf>,
    pub(crate) merged: Vec<PathBuf>,
    pub(crate) skipped: Vec<SkippedSyncPath>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct SkippedSyncPath {
    pub(crate) path: PathBuf,
    pub(crate) reason: String,
}

#[derive(Debug, thiserror::Error)]
pub(crate) enum WorkspaceSyncError {
    #[error("{0}")]
    ContractViolation(String),
    #[error("retryable sync conflict at '{path}': {reason}")]
    RetryableConflict { path: PathBuf, reason: String },
    #[error("hard sync conflict: {reason}")]
    HardConflict {
        path: Option<PathBuf>,
        reason: String,
    },
    #[error("FUSE infrastructure failure while {stage} at '{path}': {message}")]
    FuseInfrastructure {
        stage: &'static str,
        path: PathBuf,
        message: String,
    },
    #[error("workspace sync failed while {stage} at '{path}': {source}")]
    Io {
        stage: &'static str,
        path: PathBuf,
        #[source]
        source: io::Error,
    },
}

impl WorkspaceSyncError {
    pub(crate) fn is_retryable_conflict(&self) -> bool {
        matches!(self, Self::RetryableConflict { .. })
    }

    pub(crate) fn is_fuse_infrastructure(&self) -> bool {
        matches!(self, Self::FuseInfrastructure { .. })
    }
}

fn apply_task_change(
    task_worktree_dir: &Path,
    main_working_dir: &Path,
    rel_path: &Path,
    task_snapshot: &WorkspaceSnapshot,
) -> Result<(), WorkspaceSyncError> {
    if task_snapshot.entries.contains_key(rel_path) {
        copy_workspace_entry(task_worktree_dir, main_working_dir, rel_path).map_err(|err| {
            workspace_sync_io_error("apply task change", &task_worktree_dir.join(rel_path), err)
        })?;
    } else {
        remove_workspace_entry(main_working_dir, rel_path).map_err(|err| {
            workspace_sync_io_error(
                "remove task-deleted path",
                &main_working_dir.join(rel_path),
                err,
            )
        })?;
    }
    Ok(())
}

fn stale_cargo_lock_companion(rel_path: &Path, changed_paths: &BTreeSet<PathBuf>) -> bool {
    is_cargo_lock_path(rel_path) && !cargo_manifest_changed_for_lock(rel_path, changed_paths)
}

fn is_cargo_lock_path(path: &Path) -> bool {
    path.file_name().is_some_and(|name| name == "Cargo.lock")
}

fn cargo_manifest_changed_for_lock(lock_path: &Path, changed_paths: &BTreeSet<PathBuf>) -> bool {
    if !is_cargo_lock_path(lock_path) {
        return false;
    }
    let lock_dir = lock_path.parent().unwrap_or_else(|| Path::new(""));
    changed_paths.iter().any(|path| {
        path.file_name().is_some_and(|name| name == "Cargo.toml") && path.starts_with(lock_dir)
    })
}

fn try_merge_text_change(
    main_working_dir: &Path,
    task_worktree_dir: &Path,
    baseline: &WorkspaceSnapshot,
    rel_path: &Path,
    baseline_entry: Option<&WorkspaceEntry>,
    current_entry: Option<&WorkspaceEntry>,
    task_entry: Option<&WorkspaceEntry>,
) -> Result<bool, WorkspaceSyncError> {
    if !matches!(
        (baseline_entry, current_entry, task_entry),
        (
            Some(WorkspaceEntry::File(_)),
            Some(WorkspaceEntry::File(_)),
            Some(WorkspaceEntry::File(_))
        )
    ) {
        return Ok(false);
    }

    let Some(baseline_bytes) = baseline.file_contents.get(rel_path) else {
        return Ok(false);
    };
    let main_path = main_working_dir.join(rel_path);
    let task_path = task_worktree_dir.join(rel_path);
    let current_bytes = fs::read(&main_path)
        .map_err(|err| workspace_sync_io_error("read main file for merge", &main_path, err))?;
    let task_bytes = fs::read(&task_path)
        .map_err(|err| workspace_sync_io_error("read task file for merge", &task_path, err))?;

    match diffy::merge_bytes(baseline_bytes, &current_bytes, &task_bytes) {
        Ok(merged) => {
            fs::write(&main_path, merged)
                .map_err(|err| workspace_sync_io_error("write merged file", &main_path, err))?;
            Ok(true)
        }
        Err(_) => Ok(false),
    }
}

fn workspace_sync_io_error(stage: &'static str, path: &Path, err: io::Error) -> WorkspaceSyncError {
    if is_fuse_infrastructure_io_error(&err) {
        return WorkspaceSyncError::FuseInfrastructure {
            stage,
            path: path.to_path_buf(),
            message: err.to_string(),
        };
    }

    WorkspaceSyncError::Io {
        stage,
        path: path.to_path_buf(),
        source: err,
    }
}

fn is_fuse_infrastructure_io_error(err: &io::Error) -> bool {
    matches!(err.raw_os_error(), Some(5) | Some(6))
        || is_fuse_infrastructure_error_message(&err.to_string())
}

pub(crate) fn is_fuse_infrastructure_error_message(message: &str) -> bool {
    let lower = message.to_ascii_lowercase();
    lower.contains("device not configured")
        || lower.contains("input/output error")
        || lower.contains("os error 5")
        || lower.contains("os error 6")
}

/// Snapshot the worktree at `task_worktree_dir` and report every changed path
/// that violates the task contract relative to `baseline`.
///
/// Why: the executor needs to surface these violations to the LLM *inside* the
/// retry loop instead of letting them slip through to a terminal sync-back
/// failure that would force a full replan.
pub(crate) fn detect_contract_violations(
    task_worktree_dir: &Path,
    baseline: &WorkspaceSnapshot,
    touch_files: &[String],
    in_scope: &[String],
    out_of_scope: &[String],
) -> io::Result<Vec<ContractViolation>> {
    let task_snapshot = snapshot_workspace(task_worktree_dir)?;
    let changed_paths = changed_paths_since_baseline(baseline, &task_snapshot);
    Ok(collect_contract_violations(
        &changed_paths,
        touch_files,
        in_scope,
        out_of_scope,
    ))
}

#[derive(Clone, Debug)]
pub(crate) struct ContractViolation {
    pub(crate) path: PathBuf,
    pub(crate) reason: String,
}

pub(crate) fn format_contract_violation_message(violations: &[ContractViolation]) -> String {
    let mut parts = Vec::with_capacity(violations.len());
    for violation in violations {
        parts.push(format!(
            "task worktree modified '{}' outside its declared contract: {}",
            violation.path.display(),
            violation.reason
        ));
    }
    parts.join("\n")
}

fn collect_contract_violations(
    changed_paths: &[PathBuf],
    touch_files: &[String],
    in_scope: &[String],
    out_of_scope: &[String],
) -> Vec<ContractViolation> {
    changed_paths
        .iter()
        .filter_map(|rel_path| {
            let rel_path_str = rel_path.to_string_lossy();
            sync_contract_violation(touch_files, in_scope, out_of_scope, &rel_path_str).map(
                |reason| ContractViolation {
                    path: rel_path.clone(),
                    reason,
                },
            )
        })
        .collect()
}

fn sync_contract_violation(
    touch_files: &[String],
    in_scope: &[String],
    out_of_scope: &[String],
    path: &str,
) -> Option<String> {
    if !touch_files.is_empty() {
        if let ScopeVerdict::OutOfScope(reason) = check_scope(&[], out_of_scope, path) {
            return Some(reason);
        }
        if cargo_lock_companion_allowed(touch_files, path) {
            return None;
        }
        return match check_scope(touch_files, &[], path) {
            ScopeVerdict::InScope => None,
            ScopeVerdict::OutOfScope(reason) => Some(format!("not in touchFiles: {reason}")),
        };
    }

    if let ScopeVerdict::OutOfScope(reason) = check_scope(&[], out_of_scope, path) {
        return Some(reason);
    }
    if cargo_lock_companion_allowed(in_scope, path) {
        return None;
    }
    match check_scope(in_scope, out_of_scope, path) {
        ScopeVerdict::InScope => None,
        ScopeVerdict::OutOfScope(reason) => Some(reason),
    }
}

fn materialize_workspace(
    source_root: &Path,
    target_root: &Path,
    snapshot: &WorkspaceSnapshot,
) -> io::Result<()> {
    for rel_path in snapshot.entries.keys() {
        copy_workspace_entry(source_root, target_root, rel_path)?;
    }
    Ok(())
}

fn copy_workspace_entry(source_root: &Path, target_root: &Path, rel_path: &Path) -> io::Result<()> {
    let source = source_root.join(rel_path);
    let target = target_root.join(rel_path);
    if let Some(parent) = target.parent() {
        fs::create_dir_all(parent)?;
    }

    let source_metadata = fs::symlink_metadata(&source)?;
    if source_metadata.file_type().is_symlink() {
        copy_symlink(&source, &target)?;
        return Ok(());
    }

    clone_or_copy_file(&source, &target)?;
    fs::set_permissions(&target, source_metadata.permissions())?;
    Ok(())
}

fn clone_or_copy_file(source: &Path, target: &Path) -> io::Result<()> {
    remove_existing_target(target)?;

    match try_clone_file_cow(source, target) {
        Ok(()) => Ok(()),
        Err(_) => {
            let _ = remove_existing_target(target);
            fs::copy(source, target)?;
            Ok(())
        }
    }
}

fn copy_symlink(source: &Path, target: &Path) -> io::Result<()> {
    remove_existing_target(target)?;
    let link_target = fs::read_link(source)?;
    create_symlink(&link_target, source, target)
}

#[cfg(target_os = "macos")]
fn try_clone_file_cow(source: &Path, target: &Path) -> io::Result<()> {
    use std::{ffi::CString, os::unix::ffi::OsStrExt};

    unsafe extern "C" {
        fn clonefile(
            src: *const libc::c_char,
            dst: *const libc::c_char,
            flags: libc::c_int,
        ) -> libc::c_int;
    }

    let source_cstr = CString::new(source.as_os_str().as_bytes()).map_err(|_| {
        io::Error::new(
            io::ErrorKind::InvalidInput,
            format!(
                "source path contains interior NUL byte: {}",
                source.display()
            ),
        )
    })?;
    let target_cstr = CString::new(target.as_os_str().as_bytes()).map_err(|_| {
        io::Error::new(
            io::ErrorKind::InvalidInput,
            format!(
                "target path contains interior NUL byte: {}",
                target.display()
            ),
        )
    })?;

    // SAFETY: The C strings are NUL-terminated, live for the duration of the call,
    // and `clonefile` does not retain the provided pointers after returning.
    let rc = unsafe { clonefile(source_cstr.as_ptr(), target_cstr.as_ptr(), 0) };
    if rc == 0 {
        Ok(())
    } else {
        Err(io::Error::last_os_error())
    }
}

#[cfg(target_os = "linux")]
fn try_clone_file_cow(source: &Path, target: &Path) -> io::Result<()> {
    use std::{fs::File, os::fd::AsRawFd};

    const FICLONE: libc::c_ulong = 0x4004_9409;

    let source_file = File::open(source)?;
    let target_file = File::create(target)?;
    // SAFETY: `ioctl(FICLONE)` reads the source fd value, operates on two live
    // file descriptors opened above, and does not outlive the call boundary.
    let rc = unsafe { libc::ioctl(target_file.as_raw_fd(), FICLONE, source_file.as_raw_fd()) };
    if rc == 0 {
        Ok(())
    } else {
        Err(io::Error::last_os_error())
    }
}

#[cfg(not(any(target_os = "macos", target_os = "linux")))]
fn try_clone_file_cow(_source: &Path, _target: &Path) -> io::Result<()> {
    Err(io::Error::new(
        io::ErrorKind::Unsupported,
        "copy-on-write cloning is not supported on this platform",
    ))
}

fn remove_workspace_entry(root: &Path, rel_path: &Path) -> io::Result<()> {
    let target = root.join(rel_path);
    match fs::symlink_metadata(&target) {
        Ok(_) => {
            remove_existing_target(&target)?;
            remove_empty_parents(root, target.parent());
        }
        Err(err) if err.kind() == io::ErrorKind::NotFound => {}
        Err(err) => return Err(err),
    }
    Ok(())
}

fn remove_existing_target(path: &Path) -> io::Result<()> {
    match fs::symlink_metadata(path) {
        Ok(metadata) if metadata.file_type().is_dir() && !metadata.file_type().is_symlink() => {
            fs::remove_dir_all(path)
        }
        Ok(_) => fs::remove_file(path),
        Err(err) if err.kind() == io::ErrorKind::NotFound => Ok(()),
        Err(err) => Err(err),
    }
}

fn link_repo_storage(storage: &Path, link_path: &Path, context: &str) -> io::Result<()> {
    create_storage_link(storage, link_path).map_err(|err| {
        io::Error::new(
            err.kind(),
            format!(
                "failed to link repository storage '{}' into {} at '{}': {}",
                storage.display(),
                context,
                link_path.display(),
                err
            ),
        )
    })
}

fn remove_empty_parents(root: &Path, mut current: Option<&Path>) {
    while let Some(dir) = current {
        if dir == root {
            break;
        }

        let is_empty = match fs::read_dir(dir) {
            Ok(mut entries) => entries.next().is_none(),
            Err(_) => false,
        };
        if !is_empty {
            break;
        }
        if fs::remove_dir(dir).is_err() {
            break;
        }
        current = dir.parent();
    }
}

#[cfg(unix)]
fn create_storage_link(storage: &Path, link_path: &Path) -> io::Result<()> {
    std::os::unix::fs::symlink(storage, link_path)
}

#[cfg(unix)]
fn create_symlink(link_target: &Path, _source: &Path, link_path: &Path) -> io::Result<()> {
    std::os::unix::fs::symlink(link_target, link_path)
}

#[cfg(windows)]
fn create_storage_link(storage: &Path, link_path: &Path) -> io::Result<()> {
    std::os::windows::fs::symlink_dir(storage, link_path)
}

#[cfg(windows)]
fn create_symlink(link_target: &Path, source: &Path, link_path: &Path) -> io::Result<()> {
    match fs::metadata(source) {
        Ok(metadata) if metadata.is_dir() => {
            std::os::windows::fs::symlink_dir(link_target, link_path)
        }
        _ => std::os::windows::fs::symlink_file(link_target, link_path),
    }
}

#[cfg(test)]
mod tests {
    use std::{io, path::PathBuf};

    use tempfile::tempdir;
    use uuid::Uuid;

    use super::{
        FuseAttemptOutcome, FuseProvisionState, SubAgentWorkspaceError, WorkspaceSyncError,
        cleanup_task_worktree, clone_or_copy_file, detect_contract_violations,
        materialize_sub_agent_workspace, materialize_workspace, prepare_copy_task_worktree,
        prepare_task_worktree, prepare_task_worktree_copy_fallback, prepare_task_worktree_root,
        remove_partial_workspace_on_error, sync_task_worktree_back, task_worktree_paths,
    };
    use crate::{
        internal::ai::{
            agent_run::{
                AgentRunId,
                event::{AgentRunEvent, WorkspaceStrategy},
                event_store::AgentRunEventStore,
                workspace_strategy::{SPARSE_REPO_SIZE_THRESHOLD_BYTES, WorkspaceSizing},
            },
            workspace_snapshot::{
                WorkspaceEntry, snapshot_workspace, snapshot_workspace_with_contents,
            },
        },
        utils::{test, util},
    };

    /// A `tracing` writer that captures every emitted line into a shared
    /// buffer so a test can assert what was (or was not) logged. Used to
    /// pin the CEX-S2-11 (2) audit-log warning at its real emission site
    /// in `materialize_sub_agent_workspace`, not just the pure helper.
    #[derive(Clone, Default)]
    struct CapturedWriter(std::sync::Arc<std::sync::Mutex<Vec<u8>>>);

    impl io::Write for CapturedWriter {
        fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
            self.0
                .lock()
                .expect("captured-log buffer poisoned")
                .extend_from_slice(buf);
            Ok(buf.len())
        }

        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }

    impl<'a> tracing_subscriber::fmt::MakeWriter<'a> for CapturedWriter {
        type Writer = CapturedWriter;

        fn make_writer(&'a self) -> Self::Writer {
            self.clone()
        }
    }

    /// Run `body` with a thread-local `tracing` subscriber that captures
    /// WARN-and-above events, and return everything it logged. The
    /// subscriber is scoped to this thread (via `with_default`), so it
    /// only sees events the synchronous `body` emits here — parallel
    /// tests on other threads cannot pollute the buffer.
    fn capture_warnings(body: impl FnOnce()) -> String {
        let buffer = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
        let subscriber = tracing_subscriber::fmt()
            .with_writer(CapturedWriter(buffer.clone()))
            .with_max_level(tracing::Level::WARN)
            .with_ansi(false)
            .finish();
        tracing::subscriber::with_default(subscriber, body);
        let bytes = buffer.lock().expect("captured-log buffer poisoned").clone();
        String::from_utf8(bytes).expect("captured logs must be valid UTF-8")
    }

    #[cfg(unix)]
    fn symlink_path(target: &std::path::Path, link: &std::path::Path) -> io::Result<()> {
        std::os::unix::fs::symlink(target, link)
    }

    #[cfg(windows)]
    fn symlink_path(target: &std::path::Path, link: &std::path::Path) -> io::Result<()> {
        match std::fs::metadata(target) {
            Ok(metadata) if metadata.is_dir() => std::os::windows::fs::symlink_dir(target, link),
            _ => std::os::windows::fs::symlink_file(target, link),
        }
    }

    #[cfg(target_os = "macos")]
    #[tokio::test]
    async fn fuse_passthrough_lookup_handles_symlink_entries_on_macos() {
        use std::ffi::OsStr;

        use libfuse_fs::passthrough::{PassthroughArgs, new_passthroughfs_layer};
        use rfuse3::raw::{Filesystem, Request};

        let root = tempdir().unwrap();
        std::fs::write(root.path().join("target.txt"), "target").unwrap();
        symlink_path(
            std::path::Path::new("target.txt"),
            &root.path().join("link.txt"),
        )
        .unwrap();

        let fs = new_passthroughfs_layer(PassthroughArgs {
            root_dir: root.path(),
            mapping: None::<&str>,
        })
        .await
        .unwrap();

        let entry = fs
            .lookup(Request::default(), 1, OsStr::new("link.txt"))
            .await
            .unwrap();

        assert_eq!(entry.attr.kind, rfuse3::FileType::Symlink);
    }

    #[test]
    fn clone_or_copy_file_preserves_contents() {
        let temp = tempdir().unwrap();
        let source = temp.path().join("source.txt");
        let target = temp.path().join("target.txt");
        std::fs::write(&source, "cow me maybe\n").unwrap();

        clone_or_copy_file(&source, &target).unwrap();

        assert_eq!(std::fs::read_to_string(&target).unwrap(), "cow me maybe\n");
    }

    #[test]
    fn snapshot_records_directory_symlink_without_recursing() {
        let temp = tempdir().unwrap();
        let root = temp.path().join("root");
        let external = temp.path().join("external");
        std::fs::create_dir_all(root.join("nested")).unwrap();
        std::fs::create_dir_all(&external).unwrap();
        std::fs::write(external.join("secret.txt"), "outside\n").unwrap();
        symlink_path(&external, &root.join("nested").join("external-link")).unwrap();

        let snapshot = snapshot_workspace(&root).unwrap();

        assert_eq!(
            snapshot
                .entries
                .get(std::path::Path::new("nested/external-link")),
            Some(&WorkspaceEntry::Symlink(external))
        );
        assert!(
            !snapshot
                .entries
                .contains_key(std::path::Path::new("nested/external-link/secret.txt"))
        );
    }

    #[test]
    fn materialize_and_sync_preserve_symlink_entries() {
        let temp = tempdir().unwrap();
        let main = temp.path().join("main");
        let task = temp.path().join("task");
        std::fs::create_dir_all(&main).unwrap();
        std::fs::write(main.join("target.txt"), "base\n").unwrap();
        symlink_path(std::path::Path::new("target.txt"), &main.join("link.txt")).unwrap();

        let baseline = snapshot_workspace(&main).unwrap();
        std::fs::create_dir_all(&task).unwrap();
        materialize_workspace(&main, &task, &baseline).unwrap();
        assert!(
            std::fs::symlink_metadata(task.join("link.txt"))
                .unwrap()
                .file_type()
                .is_symlink()
        );

        std::fs::remove_file(task.join("link.txt")).unwrap();
        symlink_path(std::path::Path::new("updated.txt"), &task.join("link.txt")).unwrap();

        sync_task_worktree_back(&main, &task, &baseline, &[], &[], &[]).unwrap();

        assert!(
            std::fs::symlink_metadata(main.join("link.txt"))
                .unwrap()
                .file_type()
                .is_symlink()
        );
        assert_eq!(
            std::fs::read_link(main.join("link.txt")).unwrap(),
            PathBuf::from("updated.txt")
        );
    }

    #[test]
    fn sync_rejects_changes_outside_touch_files_contract() {
        let temp = tempdir().unwrap();
        let main = temp.path().join("main");
        let task = temp.path().join("task");
        std::fs::create_dir_all(main.join("src")).unwrap();
        std::fs::write(main.join("src/allowed.rs"), "base\n").unwrap();
        std::fs::write(main.join("src/other.rs"), "base\n").unwrap();

        let baseline = snapshot_workspace(&main).unwrap();
        std::fs::create_dir_all(&task).unwrap();
        materialize_workspace(&main, &task, &baseline).unwrap();
        std::fs::write(task.join("src/other.rs"), "changed\n").unwrap();

        let err = sync_task_worktree_back(
            &main,
            &task,
            &baseline,
            &["src/allowed.rs".to_string()],
            &["src/".to_string()],
            &[],
        )
        .unwrap_err();

        assert!(err.to_string().contains("outside its declared contract"));
        assert_eq!(
            std::fs::read_to_string(main.join("src/other.rs")).unwrap(),
            "base\n"
        );
    }

    #[test]
    fn workspace_sync_skips_stale_cargo_lock_companion_and_ignores_target_outputs() {
        let temp = tempdir().unwrap();
        let main = temp.path().join("main");
        let task = temp.path().join("task");
        std::fs::create_dir_all(main.join("libra/src")).unwrap();
        std::fs::write(
            main.join("libra/Cargo.toml"),
            "[package]\nname = \"libra\"\n",
        )
        .unwrap();
        std::fs::write(main.join("libra/src/main.rs"), "fn main() {}\n").unwrap();

        let baseline = snapshot_workspace(&main).unwrap();
        std::fs::create_dir_all(&task).unwrap();
        materialize_workspace(&main, &task, &baseline).unwrap();
        std::fs::write(task.join("libra/Cargo.lock"), "# generated lockfile\n").unwrap();
        std::fs::create_dir_all(task.join("libra/target")).unwrap();
        std::fs::write(task.join("libra/target/.rustc_info.json"), "{}\n").unwrap();

        let report = sync_task_worktree_back(
            &main,
            &task,
            &baseline,
            &[
                "libra/Cargo.toml".to_string(),
                "libra/src/main.rs".to_string(),
            ],
            &["libra/".to_string()],
            &[],
        )
        .unwrap();

        assert_eq!(report.skipped.len(), 1);
        assert_eq!(report.skipped[0].path, PathBuf::from("libra/Cargo.lock"));
        assert!(!main.join("libra/Cargo.lock").exists());
        assert!(!main.join("libra/target/.rustc_info.json").exists());
    }

    #[test]
    fn workspace_sync_applies_cargo_lock_when_manifest_changed_from_baseline() {
        let temp = tempdir().unwrap();
        let main = temp.path().join("main");
        let task = temp.path().join("task");
        std::fs::create_dir_all(main.join("libra")).unwrap();
        std::fs::write(
            main.join("libra/Cargo.toml"),
            "[package]\nname = \"libra\"\n\n[dependencies]\n",
        )
        .unwrap();
        std::fs::write(main.join("libra/Cargo.lock"), "# base lockfile\n").unwrap();

        let baseline = snapshot_workspace(&main).unwrap();
        std::fs::create_dir_all(&task).unwrap();
        materialize_workspace(&main, &task, &baseline).unwrap();
        std::fs::write(
            task.join("libra/Cargo.toml"),
            "[package]\nname = \"libra\"\n\n[dependencies]\nserde = \"1\"\n",
        )
        .unwrap();
        std::fs::write(task.join("libra/Cargo.lock"), "# updated lockfile\n").unwrap();

        let report = sync_task_worktree_back(
            &main,
            &task,
            &baseline,
            &["libra/Cargo.toml".to_string()],
            &["libra/".to_string()],
            &[],
        )
        .unwrap();

        assert!(report.applied.contains(&PathBuf::from("libra/Cargo.lock")));
        assert_eq!(
            std::fs::read_to_string(main.join("libra/Cargo.lock")).unwrap(),
            "# updated lockfile\n"
        );
    }

    #[test]
    fn workspace_sync_applies_root_cargo_lock_when_member_manifest_changed() {
        let temp = tempdir().unwrap();
        let main = temp.path().join("main");
        let task = temp.path().join("task");
        std::fs::create_dir_all(main.join("crates/app")).unwrap();
        std::fs::write(
            main.join("Cargo.toml"),
            "[workspace]\nmembers = [\"crates/app\"]\n",
        )
        .unwrap();
        std::fs::write(
            main.join("crates/app/Cargo.toml"),
            "[package]\nname = \"app\"\n\n[dependencies]\n",
        )
        .unwrap();
        std::fs::write(main.join("Cargo.lock"), "# base lockfile\n").unwrap();

        let baseline = snapshot_workspace(&main).unwrap();
        std::fs::create_dir_all(&task).unwrap();
        materialize_workspace(&main, &task, &baseline).unwrap();
        std::fs::write(
            task.join("crates/app/Cargo.toml"),
            "[package]\nname = \"app\"\n\n[dependencies]\nserde = \"1\"\n",
        )
        .unwrap();
        std::fs::write(task.join("Cargo.lock"), "# updated workspace lockfile\n").unwrap();

        let report = sync_task_worktree_back(
            &main,
            &task,
            &baseline,
            &["crates/app/Cargo.toml".to_string()],
            &[],
            &[],
        )
        .unwrap();

        assert!(report.applied.contains(&PathBuf::from("Cargo.lock")));
        assert_eq!(
            std::fs::read_to_string(main.join("Cargo.lock")).unwrap(),
            "# updated workspace lockfile\n"
        );
    }

    #[test]
    fn workspace_sync_skips_already_applied_path() {
        let temp = tempdir().unwrap();
        let main = temp.path().join("main");
        let task = temp.path().join("task");
        std::fs::create_dir_all(main.join("src")).unwrap();
        std::fs::write(main.join("src/lib.rs"), "base\n").unwrap();

        let baseline = snapshot_workspace(&main).unwrap();
        std::fs::create_dir_all(&task).unwrap();
        materialize_workspace(&main, &task, &baseline).unwrap();
        std::fs::write(task.join("src/lib.rs"), "updated\n").unwrap();
        std::fs::write(main.join("src/lib.rs"), "updated\n").unwrap();

        let report = sync_task_worktree_back(
            &main,
            &task,
            &baseline,
            &["src/lib.rs".to_string()],
            &[],
            &[],
        )
        .unwrap();

        assert_eq!(report.already_applied, vec![PathBuf::from("src/lib.rs")]);
        assert!(report.applied.is_empty());
    }

    #[test]
    fn workspace_sync_three_way_merges_non_overlapping_text_edits() {
        let temp = tempdir().unwrap();
        let main = temp.path().join("main");
        let task = temp.path().join("task");
        std::fs::create_dir_all(main.join("src")).unwrap();
        std::fs::write(main.join("src/lib.rs"), "one\ntwo\nthree\n").unwrap();

        let baseline = snapshot_workspace_with_contents(&main).unwrap();
        std::fs::create_dir_all(&task).unwrap();
        materialize_workspace(&main, &task, &baseline).unwrap();
        std::fs::write(task.join("src/lib.rs"), "ONE\ntwo\nthree\n").unwrap();
        std::fs::write(main.join("src/lib.rs"), "one\ntwo\nTHREE\n").unwrap();

        let report = sync_task_worktree_back(
            &main,
            &task,
            &baseline,
            &["src/lib.rs".to_string()],
            &[],
            &[],
        )
        .unwrap();

        assert_eq!(report.merged, vec![PathBuf::from("src/lib.rs")]);
        assert_eq!(
            std::fs::read_to_string(main.join("src/lib.rs")).unwrap(),
            "ONE\ntwo\nTHREE\n"
        );
    }

    #[test]
    fn workspace_sync_three_way_conflict_does_not_overwrite_main_workspace() {
        let temp = tempdir().unwrap();
        let main = temp.path().join("main");
        let task = temp.path().join("task");
        std::fs::create_dir_all(main.join("src")).unwrap();
        std::fs::write(main.join("src/lib.rs"), "one\ntwo\nthree\n").unwrap();

        let baseline = snapshot_workspace_with_contents(&main).unwrap();
        std::fs::create_dir_all(&task).unwrap();
        materialize_workspace(&main, &task, &baseline).unwrap();
        std::fs::write(task.join("src/lib.rs"), "one\nTASK\nthree\n").unwrap();
        std::fs::write(main.join("src/lib.rs"), "one\nMAIN\nthree\n").unwrap();

        let err = sync_task_worktree_back(
            &main,
            &task,
            &baseline,
            &["src/lib.rs".to_string()],
            &[],
            &[],
        )
        .unwrap_err();

        assert!(matches!(err, WorkspaceSyncError::RetryableConflict { .. }));
        assert_eq!(
            std::fs::read_to_string(main.join("src/lib.rs")).unwrap(),
            "one\nMAIN\nthree\n"
        );
    }

    #[test]
    fn workspace_sync_manifest_and_lock_divergence_returns_retryable_conflict() {
        let temp = tempdir().unwrap();
        let main = temp.path().join("main");
        let task = temp.path().join("task");
        std::fs::create_dir_all(&main).unwrap();
        std::fs::write(main.join("Cargo.toml"), "[dependencies]\n").unwrap();
        std::fs::write(main.join("Cargo.lock"), "# base\n").unwrap();

        let baseline = snapshot_workspace(&main).unwrap();
        std::fs::create_dir_all(&task).unwrap();
        materialize_workspace(&main, &task, &baseline).unwrap();
        std::fs::write(task.join("Cargo.toml"), "[dependencies]\nserde = \"1\"\n").unwrap();
        std::fs::write(task.join("Cargo.lock"), "# task lock\n").unwrap();
        std::fs::write(main.join("Cargo.lock"), "# concurrent lock\n").unwrap();

        let err = sync_task_worktree_back(
            &main,
            &task,
            &baseline,
            &["Cargo.toml".to_string()],
            &[],
            &[],
        )
        .unwrap_err();

        assert!(matches!(
            err,
            WorkspaceSyncError::RetryableConflict { path, .. }
                if path == std::path::Path::new("Cargo.lock")
        ));
        assert_eq!(
            std::fs::read_to_string(main.join("Cargo.lock")).unwrap(),
            "# concurrent lock\n"
        );
    }

    #[test]
    fn detect_contract_violations_reports_path_outside_touch_files() {
        let temp = tempdir().unwrap();
        let main = temp.path().join("main");
        let task = temp.path().join("task");
        std::fs::create_dir_all(main.join("src")).unwrap();
        std::fs::write(main.join("src/allowed.rs"), "base\n").unwrap();
        std::fs::write(main.join("src/other.rs"), "base\n").unwrap();

        let baseline = snapshot_workspace(&main).unwrap();
        std::fs::create_dir_all(&task).unwrap();
        materialize_workspace(&main, &task, &baseline).unwrap();
        std::fs::write(task.join("src/other.rs"), "changed\n").unwrap();

        let violations = detect_contract_violations(
            &task,
            &baseline,
            &["src/allowed.rs".to_string()],
            &["src/".to_string()],
            &[],
        )
        .unwrap();

        assert_eq!(violations.len(), 1);
        assert_eq!(violations[0].path, std::path::PathBuf::from("src/other.rs"));
        assert!(violations[0].reason.contains("not in touchFiles"));
    }

    #[test]
    fn detect_contract_violations_accepts_cargo_lock_companion_with_absolute_touch_file() {
        let temp = tempdir().unwrap();
        let main = temp.path().join("main");
        let task = temp.path().join("task");
        std::fs::create_dir_all(main.join("src")).unwrap();
        std::fs::write(main.join("Cargo.toml"), "[package]\nname = \"libra\"\n").unwrap();
        std::fs::write(main.join("src/main.rs"), "fn main() {}\n").unwrap();

        let baseline = snapshot_workspace(&main).unwrap();
        std::fs::create_dir_all(&task).unwrap();
        materialize_workspace(&main, &task, &baseline).unwrap();
        std::fs::write(task.join("Cargo.lock"), "# generated lockfile\n").unwrap();

        // Simulates touch_files coming straight from the LLM with absolute paths;
        // the cargo-lock companion match should still tolerate it.
        let violations = detect_contract_violations(
            &task,
            &baseline,
            &[
                "/some/abs/Cargo.toml".to_string(),
                "/some/abs/src/main.rs".to_string(),
            ],
            &[],
            &[],
        )
        .unwrap();

        assert!(
            violations.is_empty(),
            "expected no violations, got {:?}",
            violations
        );
    }

    #[test]
    fn sync_rejects_changes_outside_write_scope_when_touch_files_absent() {
        let temp = tempdir().unwrap();
        let main = temp.path().join("main");
        let task = temp.path().join("task");
        std::fs::create_dir_all(main.join("src")).unwrap();
        std::fs::create_dir_all(main.join("docs")).unwrap();
        std::fs::write(main.join("src/allowed.rs"), "base\n").unwrap();
        std::fs::write(main.join("docs/readme.md"), "base\n").unwrap();

        let baseline = snapshot_workspace(&main).unwrap();
        std::fs::create_dir_all(&task).unwrap();
        materialize_workspace(&main, &task, &baseline).unwrap();
        std::fs::write(task.join("docs/readme.md"), "changed\n").unwrap();

        let err = sync_task_worktree_back(&main, &task, &baseline, &[], &["src/".to_string()], &[])
            .unwrap_err();

        assert!(
            err.to_string()
                .contains("path 'docs/readme.md' not in any in-scope pattern")
        );
        assert_eq!(
            std::fs::read_to_string(main.join("docs/readme.md")).unwrap(),
            "base\n"
        );
    }

    #[test]
    fn prepare_task_worktree_supports_plain_directories_without_repo_storage() {
        let temp = tempdir().unwrap();
        let main = temp.path().join("workspace");
        std::fs::create_dir_all(main.join("src")).unwrap();
        std::fs::write(main.join("src/lib.rs"), "fn main() {}\n").unwrap();

        let (task_worktree, _) =
            prepare_task_worktree(&main, Uuid::new_v4(), &FuseProvisionState::default()).unwrap();

        assert_eq!(
            std::fs::read_to_string(task_worktree.root.join("src/lib.rs")).unwrap(),
            "fn main() {}\n"
        );
        assert!(!task_worktree.root.join(util::ROOT_DIR).exists());

        cleanup_task_worktree(task_worktree).unwrap();
    }

    #[tokio::test]
    #[serial_test::serial]
    async fn task_worktree_paths_use_repo_storage_when_available() {
        let temp = tempdir().unwrap();
        let repo = temp.path().join("repo");
        std::fs::create_dir_all(&repo).unwrap();
        test::setup_with_new_libra_in(&repo).await;

        let storage = util::try_get_storage_path(Some(repo.clone())).unwrap();
        let expected_base = storage.join("worktrees").join("tasks");
        let task_id = Uuid::new_v4();
        let paths = task_worktree_paths(&repo, task_id, "fuse");

        assert_eq!(paths.cleanup_root.parent(), Some(expected_base.as_path()));
        assert_eq!(paths.workspace_root, paths.cleanup_root.join("workspace"));
        #[cfg(unix)]
        assert_eq!(paths.lower_root, paths.cleanup_root.join("lower"));
        #[cfg(unix)]
        assert_eq!(paths.upper_root, paths.cleanup_root.join("upper"));

        let cleanup_name = paths.cleanup_root.file_name().unwrap().to_string_lossy();
        assert!(cleanup_name.starts_with("libra-task-worktree-fuse-"));
        assert!(cleanup_name.ends_with(&task_id.to_string()));
    }

    #[cfg(unix)]
    #[test]
    fn fuse_task_worktree_materializes_workspace_into_upper_layer() {
        let temp = tempdir().unwrap();
        let main = temp.path().join("workspace");
        std::fs::create_dir_all(main.join("src")).unwrap();
        std::fs::create_dir_all(main.join("target/debug")).unwrap();
        std::fs::write(main.join("src/lib.rs"), "pub fn upper() {}\n").unwrap();
        std::fs::write(main.join("target/debug/app"), "compiled\n").unwrap();
        let baseline = snapshot_workspace(&main).unwrap();
        let paths = task_worktree_paths(&main, Uuid::new_v4(), "fuse-test");

        let has_repo_storage = super::prepare_fuse_task_worktree_layers(&main, &paths, &baseline)
            .expect("prepare FUSE layer roots");

        assert!(!has_repo_storage);
        assert_eq!(
            std::fs::read_to_string(paths.upper_root.join("src/lib.rs")).unwrap(),
            "pub fn upper() {}\n"
        );
        assert!(
            !paths.lower_root.join("src/lib.rs").exists(),
            "baseline files must not require lower-layer copy-up"
        );
        assert!(
            !paths.upper_root.join("target/debug/app").exists(),
            "generated build outputs should stay out of the task baseline"
        );

        super::remove_cleanup_root(&paths.cleanup_root).unwrap();
    }

    #[test]
    fn prepare_task_worktree_skips_gitignored_build_outputs() {
        let temp = tempdir().unwrap();
        let main = temp.path().join("workspace");
        std::fs::create_dir_all(main.join("src")).unwrap();
        std::fs::create_dir_all(main.join("target/debug")).unwrap();
        std::fs::write(main.join(".gitignore"), "target/\n").unwrap();
        std::fs::write(main.join("src/lib.rs"), "fn main() {}\n").unwrap();
        std::fs::write(main.join("target/debug/app"), "compiled\n").unwrap();

        let (task_worktree, _) =
            prepare_task_worktree(&main, Uuid::new_v4(), &FuseProvisionState::default()).unwrap();

        assert!(task_worktree.root.join("src/lib.rs").exists());
        assert!(!task_worktree.root.join("target").exists());

        cleanup_task_worktree(task_worktree).unwrap();
    }

    #[test]
    fn fuse_provision_state_defaults_disabled_in_unit_tests() {
        assert!(FuseProvisionState::default().is_disabled());
    }

    #[test]
    fn device_not_configured_is_classified_as_fuse_infrastructure_error() {
        assert!(super::is_fuse_infrastructure_error_message(
            "Tool 'read_file' failed: Device not configured (os error 6)"
        ));
        assert!(super::is_fuse_infrastructure_error_message(
            "failed to snapshot worktree: os error 6"
        ));
    }

    #[test]
    fn input_output_error_is_classified_as_fuse_infrastructure_error() {
        assert!(super::is_fuse_infrastructure_error_message(
            "failed to snapshot workspace: Input/output error (os error 5)"
        ));
        assert!(super::is_fuse_infrastructure_io_error(
            &io::Error::from_raw_os_error(5)
        ));
    }

    #[cfg(unix)]
    #[test]
    fn fuse_unmount_treats_einval_and_enoent_as_already_inactive() {
        assert!(super::is_fuse_unmount_already_inactive_error(
            &io::Error::from_raw_os_error(libc::EINVAL)
        ));
        assert!(super::is_fuse_unmount_already_inactive_error(
            &io::Error::from_raw_os_error(libc::ENOENT)
        ));
        assert!(super::is_fuse_unmount_already_inactive_error(
            &io::Error::from_raw_os_error(libc::ENOTCONN)
        ));
        assert!(!super::is_fuse_unmount_already_inactive_error(
            &io::Error::from_raw_os_error(libc::EIO)
        ));
    }

    #[cfg(unix)]
    #[test]
    fn fuse_cleanup_detects_resource_busy_errors() {
        assert!(super::is_fuse_cleanup_busy_error(
            &io::Error::from_raw_os_error(libc::EBUSY)
        ));
        assert!(!super::is_fuse_cleanup_busy_error(
            &io::Error::from_raw_os_error(libc::ENOENT)
        ));
    }

    #[cfg(unix)]
    #[test]
    fn fuse_health_check_allows_plain_workspace_without_repo_storage() {
        let temp = tempdir().unwrap();
        let workspace = temp.path().join("workspace");
        std::fs::create_dir_all(&workspace).unwrap();

        super::verify_fuse_task_worktree_mount(&workspace, false).unwrap();
        assert!(
            std::fs::read_dir(&workspace).unwrap().next().is_none(),
            "health-check write probe should clean up after itself"
        );
    }

    #[cfg(unix)]
    #[test]
    fn fuse_health_check_rejects_non_writable_workspace() {
        let temp = tempdir().unwrap();
        let workspace = temp.path().join("not-a-directory");
        std::fs::write(&workspace, "not writable\n").unwrap();

        let err = super::verify_fuse_task_worktree_mount(&workspace, false).unwrap_err();

        let message = err.to_string();
        assert!(message.contains("mounted workspace root is not readable"));
        assert!(message.contains(workspace.to_string_lossy().as_ref()));
    }

    #[cfg(unix)]
    #[test]
    fn fuse_health_check_reports_missing_repo_storage_context() {
        let temp = tempdir().unwrap();
        let workspace = temp.path().join("workspace");
        std::fs::create_dir_all(&workspace).unwrap();

        let err = super::verify_fuse_task_worktree_mount(&workspace, true).unwrap_err();

        let message = err.to_string();
        assert!(message.contains("FUSE mount health check failed after"));
        assert!(message.contains("expected .libra repository storage link is not visible"));
        assert!(message.contains(workspace.to_string_lossy().as_ref()));
    }

    #[cfg(unix)]
    #[test]
    fn fuse_health_check_accepts_usable_repo_storage_link() {
        let temp = tempdir().unwrap();
        let workspace = temp.path().join("workspace");
        let storage = temp.path().join("storage");
        std::fs::create_dir_all(&workspace).unwrap();
        std::fs::create_dir_all(storage.join("objects")).unwrap();
        std::fs::create_dir_all(storage.join("hooks")).unwrap();
        std::os::unix::fs::symlink(&storage, workspace.join(util::ROOT_DIR)).unwrap();

        super::verify_fuse_task_worktree_mount_once(&workspace, true).unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn fuse_health_check_rejects_dangling_repo_storage_link() {
        let temp = tempdir().unwrap();
        let workspace = temp.path().join("workspace");
        std::fs::create_dir_all(&workspace).unwrap();
        let missing = temp.path().join("missing-storage");
        std::os::unix::fs::symlink(&missing, workspace.join(util::ROOT_DIR)).unwrap();

        let err = super::verify_fuse_task_worktree_mount_once(&workspace, true).unwrap_err();

        let message = err.to_string();
        assert!(message.contains("points to missing target"));
        assert!(message.contains(missing.to_string_lossy().as_ref()));
    }

    #[test]
    fn prepare_copy_task_worktree_includes_untracked_workspace_files() {
        let temp = tempdir().unwrap();
        let main = temp.path().join("workspace");
        std::fs::create_dir_all(main.join("src")).unwrap();
        std::fs::write(main.join("src/lib.rs"), "fn main() {}\n").unwrap();
        std::fs::write(main.join("task_a.txt"), "base\n").unwrap();
        std::fs::write(main.join("task_b.txt"), "base\n").unwrap();

        let baseline = snapshot_workspace(&main).unwrap();
        let paths = task_worktree_paths(&main, Uuid::new_v4(), "copy-test");
        prepare_task_worktree_root(&paths.cleanup_root).unwrap();

        let backend = prepare_copy_task_worktree(&main, &paths, &baseline).unwrap();

        assert!(matches!(backend, super::TaskWorktreeBackend::Copy { .. }));
        assert_eq!(
            std::fs::read_to_string(paths.workspace_root.join("task_a.txt")).unwrap(),
            "base\n"
        );
        assert_eq!(
            std::fs::read_to_string(paths.workspace_root.join("task_b.txt")).unwrap(),
            "base\n"
        );

        cleanup_task_worktree(super::TaskWorktree {
            root: paths.workspace_root.clone(),
            baseline,
            backend,
        })
        .unwrap();
    }

    #[tokio::test]
    #[serial_test::serial]
    async fn prepare_task_worktree_keeps_repo_storage_visible_in_runtime() {
        let temp = tempdir().unwrap();
        let repo = temp.path().join("repo");
        std::fs::create_dir_all(&repo).unwrap();
        test::setup_with_new_libra_in(&repo).await;
        std::fs::create_dir_all(repo.join("src")).unwrap();
        std::fs::write(repo.join("src/lib.rs"), "pub fn worktree() {}\n").unwrap();

        let repo_for_prepare = repo.clone();
        let (task_worktree, _) = tokio::task::spawn_blocking(move || {
            prepare_task_worktree(
                &repo_for_prepare,
                Uuid::new_v4(),
                &FuseProvisionState::default(),
            )
        })
        .await
        .unwrap()
        .unwrap();

        assert!(task_worktree.root.join(util::ROOT_DIR).exists());
        assert_eq!(
            std::fs::read_to_string(task_worktree.root.join("src/lib.rs")).unwrap(),
            "pub fn worktree() {}\n"
        );

        tokio::task::spawn_blocking(move || cleanup_task_worktree(task_worktree))
            .await
            .unwrap()
            .unwrap();
    }

    fn event_store(temp_root: &std::path::Path) -> AgentRunEventStore {
        AgentRunEventStore::new(temp_root.join(".libra").join("sessions"))
    }

    /// A small repo (under both thresholds) selects `Worktree` and
    /// materializes via `prepare_task_worktree`: the source files appear
    /// in the workspace, the strategy is `Worktree`, and exactly one
    /// `workspace_materialized` event is appended to the run transcript.
    #[test]
    fn materialize_sub_agent_worktree_for_small_repo_emits_event() {
        let temp = tempdir().unwrap();
        let main = temp.path().join("workspace");
        std::fs::create_dir_all(main.join("src")).unwrap();
        std::fs::write(main.join("src/lib.rs"), "fn main() {}\n").unwrap();

        let store = event_store(temp.path());
        let thread_id = Uuid::new_v4();
        let run_id = AgentRunId::new();
        let sizing = WorkspaceSizing {
            repo_size_bytes: 4 * 1024,
            worktree_file_count: 1,
        };

        let workspace = materialize_sub_agent_workspace(
            &main,
            sizing,
            thread_id,
            run_id,
            false, // allow_full_copy irrelevant for the Worktree path
            &FuseProvisionState::default(),
            &store,
        )
        .expect("worktree materialization should succeed");

        assert_eq!(workspace.strategy(), WorkspaceStrategy::Worktree);
        assert_eq!(
            std::fs::read_to_string(workspace.root().join("src/lib.rs")).unwrap(),
            "fn main() {}\n",
        );

        let events = store.read(thread_id, run_id).expect("read transcript");
        assert_eq!(events.len(), 1, "exactly one workspace_materialized event");
        match events[0].known() {
            Some(AgentRunEvent::WorkspaceMaterialized {
                materialization, ..
            }) => {
                assert_eq!(materialization.strategy, WorkspaceStrategy::Worktree);
                assert!(materialization.fallback_reason.is_empty());
                assert!(materialization.materialized_file_count >= 1);
            }
            other => panic!("expected WorkspaceMaterialized, got {other:?}"),
        }

        workspace.cleanup().expect("cleanup must not leak");
    }

    /// CEX-S2-11 (5): if the copy materialization fails AFTER the
    /// cleanup root has been created, the partial workspace must be
    /// removed before the error is surfaced — the caller receives only
    /// the error and has no handle to clean up. Inject a failure by
    /// snapshotting a file and then deleting it, so `copy_workspace_entry`
    /// hits `NotFound` on the source mid-copy.
    #[test]
    fn copy_fallback_removes_partial_workspace_on_materialization_error() {
        let temp = tempdir().unwrap();
        let main = temp.path().join("workspace");
        std::fs::create_dir_all(&main).unwrap();
        std::fs::write(main.join("real.txt"), "content\n").unwrap();

        let baseline = snapshot_workspace(&main).expect("snapshot");
        // Delete the snapshotted source so the copy step fails partway.
        std::fs::remove_file(main.join("real.txt")).unwrap();

        let task_id = Uuid::new_v4();
        let cleanup_root = task_worktree_paths(&main, task_id, "copy").cleanup_root;

        let result = prepare_task_worktree_copy_fallback(
            &main,
            task_id,
            baseline,
            FuseAttemptOutcome::AlreadyDisabled,
        );
        assert!(
            result.is_err(),
            "copying a deleted source file must surface an error",
        );
        assert!(
            !cleanup_root.exists(),
            "the partial workspace must be removed on a copy materialization error \
             (no leak), but {} still exists",
            cleanup_root.display(),
        );
    }

    /// Backend-agnostic coverage of the shared cleanup-on-error helper
    /// used by BOTH the copy and FUSE materialization paths
    /// (CEX-S2-11 (5)): an `Err` removes the already-created cleanup
    /// root; an `Ok` leaves it intact and passes the value through. This
    /// exercises the FUSE path's leak-prevention logic (the FUSE backend
    /// itself needs a real mount and is not reachable under `#[cfg(test)]`).
    #[test]
    fn remove_partial_workspace_on_error_cleans_up_only_on_err() {
        let temp = tempdir().unwrap();

        let err_root = temp.path().join("err-root");
        std::fs::create_dir_all(err_root.join("workspace")).unwrap();
        let result: io::Result<()> =
            remove_partial_workspace_on_error(&err_root, Err(io::Error::other("boom")));
        assert!(result.is_err(), "the original error must be propagated");
        assert!(
            !err_root.exists(),
            "an Err must remove the already-created cleanup root (no leak)",
        );

        let ok_root = temp.path().join("ok-root");
        std::fs::create_dir_all(&ok_root).unwrap();
        let result = remove_partial_workspace_on_error(&ok_root, io::Result::Ok(7u8));
        assert_eq!(result.expect("Ok passes through"), 7);
        assert!(
            ok_root.exists(),
            "an Ok must leave the workspace root intact",
        );
    }

    /// A large repo selects `Sparse`; with no native sparse API and
    /// `allow_full_copy = false`, materialization fails fast with
    /// `Unavailable` and — critically — touches neither the filesystem
    /// (no workspace) nor the transcript (no event).
    #[test]
    fn materialize_sub_agent_sparse_without_opt_in_is_unavailable_and_inert() {
        let temp = tempdir().unwrap();
        let main = temp.path().join("workspace");
        std::fs::create_dir_all(&main).unwrap();
        std::fs::write(main.join("a.txt"), "x").unwrap();

        let store = event_store(temp.path());
        let thread_id = Uuid::new_v4();
        let run_id = AgentRunId::new();
        let sizing = WorkspaceSizing {
            repo_size_bytes: 2 * SPARSE_REPO_SIZE_THRESHOLD_BYTES, // forces Sparse
            worktree_file_count: 1,
        };

        // `SubAgentWorkspace` is intentionally not `Debug` (it wraps FUSE
        // handles), so match instead of `expect_err`.
        let err = match materialize_sub_agent_workspace(
            &main,
            sizing,
            thread_id,
            run_id,
            false,
            &FuseProvisionState::default(),
            &store,
        ) {
            Ok(workspace) => {
                let _ = workspace.cleanup();
                panic!("sparse without opt-in must be unavailable");
            }
            Err(err) => err,
        };
        assert!(matches!(err, SubAgentWorkspaceError::Unavailable(_)));

        // No event was appended — the failure happened before any I/O.
        assert!(
            store.read(thread_id, run_id).unwrap().is_empty(),
            "a rejected materialization must not append a transcript event",
        );
    }

    /// A large repo with `allow_full_copy = true` falls back to a full
    /// copy: strategy `FullCopy`, a non-empty `fallback_reason`, and the
    /// event recorded.
    #[test]
    fn materialize_sub_agent_sparse_with_opt_in_falls_back_to_full_copy() {
        let temp = tempdir().unwrap();
        let main = temp.path().join("workspace");
        std::fs::create_dir_all(&main).unwrap();
        std::fs::write(main.join("a.txt"), "hello").unwrap();

        let store = event_store(temp.path());
        let thread_id = Uuid::new_v4();
        let run_id = AgentRunId::new();
        let sizing = WorkspaceSizing {
            repo_size_bytes: 2 * SPARSE_REPO_SIZE_THRESHOLD_BYTES,
            worktree_file_count: 1,
        };

        let workspace = materialize_sub_agent_workspace(
            &main,
            sizing,
            thread_id,
            run_id,
            true,
            &FuseProvisionState::default(),
            &store,
        )
        .expect("full-copy fallback should succeed when opted in");

        assert_eq!(workspace.strategy(), WorkspaceStrategy::FullCopy);
        assert_eq!(
            std::fs::read_to_string(workspace.root().join("a.txt")).unwrap(),
            "hello",
        );

        let events = store.read(thread_id, run_id).expect("read transcript");
        assert_eq!(events.len(), 1);
        match events[0].known() {
            Some(AgentRunEvent::WorkspaceMaterialized {
                materialization, ..
            }) => {
                assert_eq!(materialization.strategy, WorkspaceStrategy::FullCopy);
                assert!(
                    !materialization.fallback_reason.is_empty(),
                    "a fallback must record a reason",
                );
            }
            other => panic!("expected WorkspaceMaterialized, got {other:?}"),
        }

        workspace.cleanup().expect("cleanup must not leak");
    }

    /// CEX-S2-11 (2): the full-copy fallback MUST write a warning to the
    /// audit log at its real emission site. Captures `tracing` output
    /// around the opted-in fallback and asserts exactly one WARN names
    /// the full copy and the `agent.allow_full_copy` opt-in — a guard the
    /// pure `full_copy_fallback_warning` unit tests cannot give, since
    /// they don't exercise the `tracing::warn!` wiring in
    /// `materialize_sub_agent_workspace`.
    #[test]
    fn full_copy_fallback_emits_audit_log_warning() {
        let temp = tempdir().unwrap();
        let main = temp.path().join("workspace");
        std::fs::create_dir_all(&main).unwrap();
        std::fs::write(main.join("a.txt"), "hello").unwrap();

        let store = event_store(temp.path());
        let thread_id = Uuid::new_v4();
        let run_id = AgentRunId::new();
        let sizing = WorkspaceSizing {
            repo_size_bytes: 2 * SPARSE_REPO_SIZE_THRESHOLD_BYTES,
            worktree_file_count: 1,
        };

        let logs = capture_warnings(|| {
            let workspace = materialize_sub_agent_workspace(
                &main,
                sizing,
                thread_id,
                run_id,
                true,
                &FuseProvisionState::default(),
                &store,
            )
            .expect("full-copy fallback should succeed when opted in");
            assert_eq!(workspace.strategy(), WorkspaceStrategy::FullCopy);
            workspace.cleanup().expect("cleanup must not leak");
        });

        let warn_lines: Vec<&str> = logs
            .lines()
            .filter(|line| line.contains("full repository copy"))
            .collect();
        assert_eq!(
            warn_lines.len(),
            1,
            "exactly one full-copy fallback warning must be logged, got: {logs}",
        );
        assert!(
            warn_lines[0].contains("WARN"),
            "the fallback notice must be emitted at WARN level: {}",
            warn_lines[0],
        );
        assert!(
            warn_lines[0].contains("agent.allow_full_copy = true"),
            "the warning must name the opt-in flag: {}",
            warn_lines[0],
        );
    }

    /// The normal `Worktree` path (no fallback) must stay silent — no
    /// full-copy warning is logged for a small repo. Pins the
    /// `full_copy_fallback_warning` `None` branch at the emission site so
    /// a refactor that always-warns trips here.
    #[test]
    fn worktree_materialization_emits_no_full_copy_warning() {
        let temp = tempdir().unwrap();
        let main = temp.path().join("workspace");
        std::fs::create_dir_all(&main).unwrap();
        std::fs::write(main.join("a.txt"), "hello").unwrap();

        let store = event_store(temp.path());
        let thread_id = Uuid::new_v4();
        let run_id = AgentRunId::new();
        let sizing = WorkspaceSizing {
            repo_size_bytes: 4 * 1024,
            worktree_file_count: 1,
        };

        let logs = capture_warnings(|| {
            let workspace = materialize_sub_agent_workspace(
                &main,
                sizing,
                thread_id,
                run_id,
                true,
                &FuseProvisionState::default(),
                &store,
            )
            .expect("small repo must materialize as a worktree");
            assert_eq!(workspace.strategy(), WorkspaceStrategy::Worktree);
            workspace.cleanup().expect("cleanup must not leak");
        });

        assert!(
            !logs.contains("full repository copy"),
            "the worktree path must not log a full-copy fallback warning: {logs}",
        );
    }

    /// CEX-S2-11 (5) leak-free: if the workspace materializes but the
    /// transcript append fails, the just-created worktree must be cleaned
    /// up before the error propagates. Forces the append to fail (a file
    /// where the sessions root should be) and asserts the repo's
    /// `worktrees/tasks` dir is left empty — no leaked workspace.
    #[tokio::test]
    #[serial_test::serial]
    async fn materialize_cleans_up_workspace_when_event_append_fails() {
        let temp = tempdir().unwrap();
        let repo = temp.path().join("repo");
        std::fs::create_dir_all(&repo).unwrap();
        test::setup_with_new_libra_in(&repo).await;
        std::fs::write(repo.join("a.txt"), "x").unwrap();

        let storage = util::try_get_storage_path(Some(repo.clone())).unwrap();
        let worktrees_tasks = storage.join("worktrees").join("tasks");

        // A FILE where the sessions root should be makes `create_dir_all`
        // inside `store.append` fail, exercising the cleanup-on-error path.
        let bad_root = temp.path().join("sessions-as-file");
        std::fs::write(&bad_root, "not a dir").unwrap();
        let store = AgentRunEventStore::new(&bad_root);

        let result = materialize_sub_agent_workspace(
            &repo,
            WorkspaceSizing {
                repo_size_bytes: 4 * 1024,
                worktree_file_count: 1,
            },
            Uuid::new_v4(),
            AgentRunId::new(),
            false,
            &FuseProvisionState::default(),
            &store,
        );

        match result {
            Ok(workspace) => {
                let _ = workspace.cleanup();
                panic!("a transcript append failure must surface an error");
            }
            Err(err) => assert!(matches!(err, SubAgentWorkspaceError::Io(_))),
        }

        // The materialized worktree must have been cleaned up: no leftover
        // `libra-task-worktree-*` entry under the repo's tasks dir.
        let leaked: Vec<PathBuf> = std::fs::read_dir(&worktrees_tasks)
            .map(|rd| rd.filter_map(Result::ok).map(|e| e.path()).collect())
            .unwrap_or_default();
        assert!(
            leaked.is_empty(),
            "workspace leaked after transcript append failure: {leaked:?}",
        );
    }
}
