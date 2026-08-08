//! Execution environment boundary for scheduler-owned task attempts.

use std::{
    io,
    path::{Path, PathBuf},
    time::Duration,
};

use uuid::Uuid;

use crate::{
    internal::{
        ai::{
            orchestrator::{
                types::TaskWorkspaceBackend,
                workspace::{
                    FuseAttemptOutcome, FuseProvisionState, LeaseFence, SyncBackReport,
                    TaskWorktree, UnfencedLease, WorkspaceSyncError, cleanup_task_worktree,
                    prepare_task_worktree, sync_task_worktree_back,
                },
            },
            workspace_snapshot::WorkspaceSnapshot,
        },
        db,
        workspace::{
            AcquireRequest, WorkspaceError, WorkspaceKind, WorkspaceLease, WorkspaceState,
            WorkspaceStore, now_ms,
        },
    },
    utils::util,
};

/// Lease lifetime for a task worktree (§C.8): long enough to survive a stalled
/// tool call, short enough that a machine that died with the run in flight
/// becomes recoverable the same hour.
const TASK_LEASE_TTL_MS: i64 = 30 * 60 * 1000;

/// How often the heartbeat renews. A LIVE run must never look reclaimable just
/// because the task is long — renewing only at sync-back would let a task that
/// outruns the TTL be taken over while it is still writing — so the lease is
/// extended continuously for as long as this process is alive, several times
/// per TTL so one missed tick is not fatal.
#[cfg(not(test))]
const TASK_LEASE_HEARTBEAT: Duration = Duration::from_secs(5 * 60);
/// Tests drive the real timer, just faster — the same pattern the FUSE health
/// check uses, so the heartbeat path under test is the production one.
#[cfg(test)]
const TASK_LEASE_HEARTBEAT: Duration = Duration::from_millis(30);

/// How many times a sync-back's lease check retries a transient store failure
/// before giving up, and the base delay between attempts. A completed task's
/// work is worth far more than a fast failure on a momentarily locked database.
const LEASE_CHECK_ATTEMPTS: usize = 3;
const LEASE_CHECK_BACKOFF: Duration = Duration::from_millis(150);
/// Hard ceiling on the whole check. The sync-back mutex is held across it, and
/// a pooled connection carries a 30-second busy timeout, so without this an
/// unrelated writer holding the database could stall every other task's
/// sync-back for minutes. Exceeding it is never a verdict on the lease — see
/// [`ExecutionEnvironmentProvider::renew_lease`] for what happens next.
const LEASE_CHECK_DEADLINE: Duration = Duration::from_secs(5);

/// Safety margin used when the store cannot be reached at sync-back time. A
/// takeover is only possible once the lease deadline has PASSED, so a lease
/// renewed less than `TTL - margin` ago provably still belongs to this run and
/// the sync-back may proceed on that proof rather than on a fresh read.
///
/// The margin has to cover the OPERATION the proof authorizes, not just its
/// first instruction: a sync-back snapshots the tree and copies every changed
/// path, which on a large workspace is minutes rather than seconds. Half the
/// TTL means the proof is only used when the deadline is at least that far
/// away, so exhausting it requires the store to stay unreachable — for the
/// heartbeat too, since it keeps renewing throughout — for the entire window.
const LEASE_PROOF_MARGIN: Duration = Duration::from_millis(TASK_LEASE_TTL_MS as u64 / 2);

/// Keeps a task worktree's lease alive for as long as the run is.
///
/// Stopping it (teardown, or dropping the environment) is what makes the lease
/// start ageing towards reclaimable — a crashed process stops renewing by
/// definition, which is exactly the signal the scavenger needs.
struct LeaseHeartbeat {
    handle: tokio::task::JoinHandle<()>,
}

/// Wall-clock milliseconds of the last renewal known to have SUCCEEDED, shared
/// between the heartbeat and the sync-back check.
type RenewedAt = std::sync::Arc<std::sync::atomic::AtomicI64>;

impl LeaseHeartbeat {
    fn spawn(storage_db: PathBuf, lease: WorkspaceLease, renewed_at: RenewedAt) -> Self {
        let handle = tokio::spawn(async move {
            let mut ticker = tokio::time::interval(TASK_LEASE_HEARTBEAT);
            ticker.tick().await; // the first tick completes immediately
            loop {
                ticker.tick().await;
                match renew_task_lease(&storage_db, &lease).await {
                    Ok(()) => {
                        renewed_at.store(now_ms(), std::sync::atomic::Ordering::Relaxed);
                    }
                    Err(WorkspaceError::LeaseLost { .. }) => {
                        tracing::warn!(
                            target: "libra::workspace",
                            workspace_id = %lease.workspace_id,
                            "task workspace lease was reclaimed; stopping the heartbeat"
                        );
                        return;
                    }
                    Err(error) => tracing::debug!(
                        target: "libra::workspace",
                        workspace_id = %lease.workspace_id,
                        error = %error,
                        "workspace lease renewal failed; retrying on the next tick"
                    ),
                }
            }
        });
        Self { handle }
    }
}

impl Drop for LeaseHeartbeat {
    fn drop(&mut self) {
        self.handle.abort();
    }
}

#[derive(Clone, Debug, Default)]
pub struct ExecutionEnvironmentProvider;

pub struct TaskExecutionEnvironment {
    worktree: TaskWorktree,
    fuse_outcome: FuseAttemptOutcome,
    /// The workspace record published for this task worktree (§C.8 W4). Held
    /// for the life of the environment: sync-back renews it, teardown releases
    /// it, and a failed teardown orphans it for the scavenger.
    ///
    /// `None` only when the main working directory is not a Libra repository —
    /// `prepare_task_worktree` supports that (it links repository storage only
    /// "when available"), and there is then no repository to associate the
    /// workspace WITH. Whenever a repository does exist, a failure to publish
    /// the record fails the provisioning: the association layer is a fact
    /// source, not a best-effort log.
    lease: Option<WorkspaceLease>,
    /// Repository database backing the lease, resolved once at provisioning so
    /// teardown cannot fail on a path lookup.
    storage_db: Option<PathBuf>,
    /// Renews the lease while the run is alive; aborted on teardown or drop.
    heartbeat: Option<LeaseHeartbeat>,
    /// When the lease was last renewed successfully. Lets a sync-back proceed
    /// on proof-of-freshness when the store is momentarily unreachable.
    renewed_at: RenewedAt,
}

impl TaskExecutionEnvironment {
    pub fn root(&self) -> &Path {
        &self.worktree.root
    }

    pub(crate) fn baseline_snapshot(&self) -> WorkspaceSnapshot {
        self.worktree.baseline.clone()
    }

    pub(crate) fn backend(&self) -> TaskWorkspaceBackend {
        self.worktree.backend()
    }

    /// Outcome of the FUSE mount attempt during provisioning. Callers use
    /// `JustDisabled` to emit a one-time TUI hint after the first failure.
    pub fn fuse_outcome(&self) -> &FuseAttemptOutcome {
        &self.fuse_outcome
    }

    /// Id of this workspace's record, for run/operation metadata (§C.8 keeps
    /// only association ids — never payloads). `None` outside a repository.
    pub fn workspace_id(&self) -> Option<&str> {
        self.lease.as_ref().map(|lease| lease.workspace_id.as_str())
    }
}

#[derive(Clone, Debug)]
pub struct SyncBackRequest {
    pub main_working_dir: PathBuf,
    pub touch_files: Vec<String>,
    pub scope_in: Vec<String>,
    pub scope_out: Vec<String>,
}

impl ExecutionEnvironmentProvider {
    pub async fn provision_task_worktree(
        &self,
        main_working_dir: PathBuf,
        task_id: Uuid,
        fuse_state: FuseProvisionState,
    ) -> io::Result<TaskExecutionEnvironment> {
        // The record is published only after the directory really exists
        // (§C.8: a failed provision must never leave a fake active record), so
        // the repository database is located first. A working directory that
        // is not a repository has nothing to associate the workspace with —
        // that is an absent subject, not a failure — while a repository whose
        // database cannot be claimed fails the provisioning below.
        let storage_db = util::try_get_storage_path(Some(main_working_dir.clone()))
            .ok()
            .map(|storage| storage.join(util::DATABASE));

        let worktree_dir = main_working_dir.clone();
        let (worktree, fuse_outcome) = tokio::task::spawn_blocking(move || {
            prepare_task_worktree(&worktree_dir, task_id, &fuse_state)
        })
        .await
        .map_err(|err| io::Error::other(format!("failed to prepare task worktree: {err}")))??;

        let lease = match &storage_db {
            Some(storage_db) => match Self::publish_workspace(storage_db, &worktree, task_id).await
            {
                Ok(lease) => Some(lease),
                Err(error) => {
                    // Never leave a materialized directory behind a failed
                    // claim — and if the removal ALSO fails there is no record
                    // to orphan, so the leak has to be in the error itself.
                    let root = worktree.root.clone();
                    let removed =
                        tokio::task::spawn_blocking(move || cleanup_task_worktree(worktree))
                            .await
                            .map_err(|err| {
                                io::Error::other(format!("cleanup worker failed: {err}"))
                            })
                            .and_then(|result| result);
                    return Err(match removed {
                        Ok(()) => error,
                        Err(cleanup_error) => io::Error::other(format!(
                            "{error}; the unregistered task worktree at {} could not be removed \
                             either ({cleanup_error}) and must be deleted by hand",
                            root.display()
                        )),
                    });
                }
            },
            None => None,
        };

        let renewed_at: RenewedAt =
            std::sync::Arc::new(std::sync::atomic::AtomicI64::new(now_ms()));
        let heartbeat = match (&storage_db, &lease) {
            (Some(storage_db), Some(lease)) => Some(LeaseHeartbeat::spawn(
                storage_db.clone(),
                lease.clone(),
                renewed_at.clone(),
            )),
            _ => None,
        };

        Ok(TaskExecutionEnvironment {
            worktree,
            fuse_outcome,
            lease,
            storage_db,
            heartbeat,
            renewed_at,
        })
    }

    /// Publish the `active` workspace record for a materialized task worktree.
    async fn publish_workspace(
        storage_db: &Path,
        worktree: &TaskWorktree,
        task_id: Uuid,
    ) -> io::Result<WorkspaceLease> {
        let conn = db::get_db_conn_instance_for_path(storage_db)
            .await
            .map_err(|error| {
                io::Error::other(format!(
                    "cannot open the repository database {}: {error}",
                    storage_db.display()
                ))
            })?;
        let kind = match worktree.backend() {
            TaskWorkspaceBackend::Fuse => WorkspaceKind::TaskFuse,
            // `Shared` never reaches here — a shared-workspace task runs in the
            // main worktree and never materializes a `TaskWorktree` — but a
            // materialized directory is a copy either way, so classify it as
            // one rather than refusing a run over a taxonomy mismatch.
            TaskWorkspaceBackend::Copy | TaskWorkspaceBackend::Shared => WorkspaceKind::TaskCopy,
        };
        WorkspaceStore::acquire(
            &conn,
            // Published straight as `active`: the directory is materialized by
            // the time this runs, which is exactly the §C.8 rule ("publish the
            // active record only after provisioning succeeds"). A crash before
            // this point leaves no record at all — never a fake active one.
            &AcquireRequest::task(
                kind,
                worktree.root.clone(),
                task_lease_owner(task_id),
                TASK_LEASE_TTL_MS,
            )
            .with_initial_state(WorkspaceState::Active)
            .with_association(Some(task_id.to_string()), None),
            now_ms(),
        )
        .await
        .map_err(workspace_io_error)
    }

    pub(crate) async fn sync_back(
        &self,
        environment: &TaskExecutionEnvironment,
        request: SyncBackRequest,
    ) -> Result<SyncBackReport, WorkspaceSyncError> {
        // Renew first: writing the task worktree's changes back into the main
        // workspace is exactly the moment a lease must still be ours. If a
        // doctor reclaimed it (this runtime was presumed dead), another owner
        // may already be acting on the same directory, so the sync-back fails
        // closed rather than replaying on top of it.
        self.renew_lease(environment).await?;

        let task_worktree_dir = environment.root().to_path_buf();
        let baseline = environment.worktree.baseline.clone();
        // The worker is fenced by the lease DEADLINE, checked before every
        // applied path. The heartbeat pushes that deadline forward while this
        // process lives, so a healthy run never notices it; a run whose lease
        // has actually lapsed stops writing into main instead of racing
        // whoever reclaimed it.
        let fence: Box<dyn LeaseFence> = match &environment.lease {
            Some(lease) => Box::new(LeaseDeadlineFence {
                workspace_id: lease.workspace_id.clone(),
                renewed_at: environment.renewed_at.clone(),
            }),
            None => Box::new(UnfencedLease),
        };
        tokio::task::spawn_blocking(move || {
            sync_task_worktree_back(
                &request.main_working_dir,
                &task_worktree_dir,
                &baseline,
                &request.touch_files,
                &request.scope_in,
                &request.scope_out,
                fence.as_ref(),
            )
        })
        .await
        .map_err(|err| WorkspaceSyncError::HardConflict {
            path: None,
            reason: format!("sync worker failed: {err}"),
        })?
    }

    async fn renew_lease(
        &self,
        environment: &TaskExecutionEnvironment,
    ) -> Result<(), WorkspaceSyncError> {
        let (Some(storage_db), Some(lease)) = (&environment.storage_db, &environment.lease) else {
            return Ok(());
        };
        // A LOST lease is a hard conflict — someone else may be writing to this
        // workspace, so nothing may be replayed. A database that is merely
        // unavailable (locked, briefly unreadable) is retryable: discarding a
        // completed task's work over a transient SQLite error would be a far
        // worse outcome than trying the lease check again.
        // A momentary SQLITE_BUSY must not cost a finished task its work: the
        // check retries in place with a short backoff before it gives up, and
        // only then reports a retryable conflict. A LOST lease is not retried —
        // it will not come back, and nothing may be replayed.
        let checked = tokio::time::timeout(LEASE_CHECK_DEADLINE, async {
            let mut last: Option<WorkspaceError> = None;
            for attempt in 0..LEASE_CHECK_ATTEMPTS {
                if attempt > 0 {
                    tokio::time::sleep(LEASE_CHECK_BACKOFF * attempt as u32).await;
                }
                match renew_task_lease(storage_db, lease).await {
                    Ok(()) => return Ok(()),
                    Err(error @ WorkspaceError::LeaseLost { .. }) => return Err(Some(error)),
                    Err(error) => last = Some(error),
                }
            }
            Err(last)
        })
        .await;

        match checked {
            Ok(Ok(())) => {
                environment
                    .renewed_at
                    .store(now_ms(), std::sync::atomic::Ordering::Relaxed);
                Ok(())
            }
            Ok(Err(Some(error @ WorkspaceError::LeaseLost { .. }))) => {
                Err(WorkspaceSyncError::HardConflict {
                    path: None,
                    reason: format!("workspace lease is no longer held by this run: {error}"),
                })
            }
            // The store could not be reached. That is NOT evidence the lease
            // was lost — and refusing here would make the executor discard a
            // finished task's work over a momentarily busy database. A
            // takeover can only happen after the deadline passes, so a lease
            // renewed recently enough is proof this run still owns it.
            Ok(Err(last)) => Self::proceed_on_proof_of_freshness(
                environment,
                storage_db,
                format!(
                    "cannot verify the workspace lease after {LEASE_CHECK_ATTEMPTS} attempts: {}",
                    last.map(|error| error.to_string()).unwrap_or_default()
                ),
            ),
            Err(_) => Self::proceed_on_proof_of_freshness(
                environment,
                storage_db,
                format!(
                    "the workspace lease check did not complete within {LEASE_CHECK_DEADLINE:?}"
                ),
            ),
        }
    }

    /// Decide an unverifiable lease check from the last renewal that DID
    /// succeed: while `now + margin` is still inside `renewed_at + TTL`, no
    /// reclaim can have taken this workspace, so the sync-back proceeds.
    /// Past that point nothing can vouch for the lease and it fails closed.
    fn proceed_on_proof_of_freshness(
        environment: &TaskExecutionEnvironment,
        storage_db: &Path,
        reason: String,
    ) -> Result<(), WorkspaceSyncError> {
        let renewed_at = environment
            .renewed_at
            .load(std::sync::atomic::Ordering::Relaxed);
        let deadline = renewed_at.saturating_add(TASK_LEASE_TTL_MS);
        let margin = LEASE_PROOF_MARGIN.as_millis() as i64;
        if now_ms().saturating_add(margin) < deadline {
            tracing::debug!(
                target: "libra::workspace",
                reason = %reason,
                "proceeding with sync-back on a still-valid lease deadline"
            );
            return Ok(());
        }
        Err(WorkspaceSyncError::RetryableConflict {
            path: storage_db.to_path_buf(),
            reason,
        })
    }

    pub async fn cleanup(&self, environment: TaskExecutionEnvironment) -> io::Result<()> {
        let TaskExecutionEnvironment {
            worktree,
            lease,
            storage_db,
            heartbeat,
            ..
        } = environment;
        let (Some(storage_db), Some(lease)) = (storage_db, lease) else {
            drop(heartbeat);
            // No repository, no record: just remove the directory.
            return tokio::task::spawn_blocking(move || cleanup_task_worktree(worktree))
                .await
                .map_err(|err| io::Error::other(format!("cleanup worker failed: {err}")))?;
        };
        let conn = db::get_db_conn_instance_for_path(&storage_db)
            .await
            .map_err(|error| {
                io::Error::other(format!(
                    "cannot open the repository database {}: {error}",
                    storage_db.display()
                ))
            })?;

        // Extend the claim across the removal itself. `releasing` still holds
        // the identity, but it is also still RECLAIMABLE once the deadline
        // passes — without this, a doctor could take the workspace over while
        // this process is mid-`remove_dir_all` and have its directory deleted
        // underneath it. A refusal here means the lease is already gone, so
        // the directory is no longer ours to remove.
        WorkspaceStore::renew_with_conn(&conn, &lease, TASK_LEASE_TTL_MS, now_ms())
            .await
            .map_err(workspace_io_error)?;

        // Announce the teardown before touching the filesystem: the identity
        // stays reserved while `releasing`, so nothing else claims the
        // directory mid-removal.
        WorkspaceStore::begin_release_with_conn(&conn, &lease, now_ms())
            .await
            .map_err(workspace_io_error)?;

        // The heartbeat stays ALIVE across the removal: unmounting a wedged
        // FUSE mount or deleting a very large copy can outlast any single
        // deadline, and `releasing` is still reclaimable, so a one-shot renewal
        // would leave a window in which a doctor takes the workspace over while
        // this process is still deleting it.
        // Flattened deliberately: a JoinError (runtime shutting down, worker
        // panic) must NOT return early through `?`. The record is already
        // `releasing`, so an early return would strand it live with no
        // recovery state — exactly the case the orphan path exists for.
        let removed =
            match tokio::task::spawn_blocking(move || cleanup_task_worktree(worktree)).await {
                Ok(result) => result,
                Err(join_error) => Err(io::Error::other(format!(
                    "cleanup worker failed: {join_error}"
                ))),
            };

        // Settling comes next, so renewing further would only log noise.
        drop(heartbeat);

        match removed {
            Ok(()) => match WorkspaceStore::release_with_conn(&conn, &lease, now_ms()).await {
                Ok(()) => Ok(()),
                Err(error) => {
                    // The directory is gone but the record is still live. Left
                    // as `releasing` it would reserve a path that no longer
                    // exists until the lease expires, so hand it to the
                    // scavenger instead of leaving a phantom cleanup.
                    Err(orphan_after_failure(&conn, &lease, workspace_io_error(error)).await)
                }
            },
            Err(error) => {
                // The directory may still be there. Orphan the record so the
                // scavenger/doctor can finish it instead of losing the fact
                // that this workspace was never cleaned up.
                Err(orphan_after_failure(&conn, &lease, error).await)
            }
        }
    }
}

/// Mark a workspace `orphaned` after a failed teardown step, preserving the
/// original failure. A failure to orphan is appended rather than replacing it:
/// the caller still needs to know what went wrong first.
async fn orphan_after_failure<C: sea_orm::ConnectionTrait>(
    conn: &C,
    lease: &WorkspaceLease,
    error: io::Error,
) -> io::Error {
    match WorkspaceStore::abandon_with_conn(conn, lease, now_ms()).await {
        Ok(()) => error,
        Err(orphan_error) => {
            tracing::warn!(
                target: "libra::workspace",
                workspace_id = %lease.workspace_id,
                error = %orphan_error,
                "failed to mark a workspace orphaned after a cleanup failure"
            );
            io::Error::other(format!(
                "{error}; the workspace record {} could not be marked orphaned either \
                 ({orphan_error}) — run `libra worktree doctor`",
                lease.workspace_id
            ))
        }
    }
}

/// Fences a sync-back on the lease deadline this run last proved.
///
/// The sync worker runs on a blocking thread where the workspace store cannot
/// be awaited, so ownership is checked against the wall clock instead: writes
/// are allowed only while the deadline established by the last successful
/// renewal is still in the future. The heartbeat keeps pushing that deadline
/// out for as long as the process is alive and the store is reachable, so this
/// only ever fires when the run genuinely can no longer prove it owns the
/// workspace — at which point another owner may already be acting on it.
struct LeaseDeadlineFence {
    workspace_id: String,
    renewed_at: RenewedAt,
}

impl LeaseFence for LeaseDeadlineFence {
    fn check(&self, stage: &'static str) -> Result<(), WorkspaceSyncError> {
        let renewed_at = self.renewed_at.load(std::sync::atomic::Ordering::Relaxed);
        let deadline = renewed_at.saturating_add(TASK_LEASE_TTL_MS);
        if now_ms() < deadline {
            return Ok(());
        }
        Err(WorkspaceSyncError::HardConflict {
            path: None,
            reason: format!(
                "workspace lease {} expired before this run could {stage}; another owner may \
                 already hold this workspace",
                self.workspace_id
            ),
        })
    }
}

/// One renewal of a task worktree's lease — the step the heartbeat repeats and
/// the one teardown performs before it starts removing files.
async fn renew_task_lease(storage_db: &Path, lease: &WorkspaceLease) -> Result<(), WorkspaceError> {
    let conn = db::get_db_conn_instance_for_path(storage_db)
        .await
        .map_err(|error| {
            WorkspaceError::ReadFailed(format!(
                "cannot open the repository database {}: {error}",
                storage_db.display()
            ))
        })?;
    WorkspaceStore::renew_with_conn(&conn, lease, TASK_LEASE_TTL_MS, now_ms())
        .await
        .map(|_| ())
}

/// Lease owner identity for a task worktree: the task it belongs to, qualified
/// by this process so two runtimes never share an owner string.
fn task_lease_owner(task_id: Uuid) -> String {
    format!("task:{task_id}:pid:{}", std::process::id())
}

fn workspace_io_error(error: WorkspaceError) -> io::Error {
    io::Error::other(format!("workspace lease: {error}"))
}

#[cfg(test)]
mod tests {
    use tempfile::TempDir;

    use super::*;
    use crate::{internal::workspace::WorkspaceListQuery, utils::test};

    async fn repo_db(main: &Path) -> sea_orm::DatabaseConnection {
        let path = util::try_get_storage_path(Some(main.to_path_buf()))
            .expect("storage path")
            .join(util::DATABASE);
        db::get_db_conn_instance_for_path(&path)
            .await
            .expect("repository database")
    }

    #[tokio::test]
    #[serial_test::serial]
    async fn provider_provisions_and_cleans_task_worktree() {
        let main = TempDir::new().unwrap();
        test::setup_with_new_libra_in(main.path()).await;
        std::fs::write(main.path().join("README.md"), "hello").unwrap();
        let provider = ExecutionEnvironmentProvider;

        let environment = provider
            .provision_task_worktree(
                main.path().to_path_buf(),
                Uuid::new_v4(),
                FuseProvisionState::default(),
            )
            .await
            .expect("provision task worktree");
        assert!(environment.root().join("README.md").exists());
        let root = environment.root().to_path_buf();
        let workspace_id = environment
            .workspace_id()
            .expect("a repository-backed task worktree publishes a record")
            .to_string();

        // The workspace is published as active, associated with its task, and
        // pointing at the materialized directory.
        let conn = repo_db(main.path()).await;
        let record =
            crate::internal::workspace::WorkspaceStore::get_with_conn(&conn, &workspace_id)
                .await
                .expect("read record")
                .expect("the task worktree published a record");
        assert_eq!(record.state, WorkspaceState::Active);
        assert_eq!(
            record.path,
            std::fs::canonicalize(&root)
                .expect("canonical root")
                .to_string_lossy()
        );

        provider
            .cleanup(environment)
            .await
            .expect("cleanup worktree");
        assert!(!root.exists());

        let settled =
            crate::internal::workspace::WorkspaceStore::get_with_conn(&conn, &workspace_id)
                .await
                .expect("read record")
                .expect("the record survives teardown for audit");
        assert_eq!(settled.state, WorkspaceState::Released);
    }

    /// The heartbeat itself — not a hand-called helper — keeps a long run's
    /// lease out of reach of the scavenger. The tick interval is compressed
    /// under `cfg(test)`, so this drives the REAL timer inside
    /// `LeaseHeartbeat`; the deadline must move strictly forward without
    /// minting a new fence.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    #[serial_test::serial]
    async fn heartbeat_timer_extends_the_lease_deadline() {
        let main = TempDir::new().unwrap();
        test::setup_with_new_libra_in(main.path()).await;
        std::fs::write(main.path().join("README.md"), "hello").unwrap();
        let provider = ExecutionEnvironmentProvider;

        let environment = provider
            .provision_task_worktree(
                main.path().to_path_buf(),
                Uuid::new_v4(),
                FuseProvisionState::default(),
            )
            .await
            .expect("provision task worktree");
        let workspace_id = environment
            .workspace_id()
            .expect("record published")
            .to_string();
        assert!(
            environment.heartbeat.is_some(),
            "a repository-backed workspace must be kept alive by a heartbeat"
        );

        let conn = repo_db(main.path()).await;
        let before =
            crate::internal::workspace::WorkspaceStore::get_with_conn(&conn, &workspace_id)
                .await
                .expect("read")
                .expect("record");

        // Wait for the heartbeat's own timer — bounded, and a failure to fire
        // is a test failure rather than a silent pass.
        for _ in 0..100 {
            tokio::time::sleep(TASK_LEASE_HEARTBEAT).await;
            let now =
                crate::internal::workspace::WorkspaceStore::get_with_conn(&conn, &workspace_id)
                    .await
                    .expect("read")
                    .expect("record");
            if now.lease_expires_at > before.lease_expires_at {
                assert_eq!(
                    now.lease_fence, before.lease_fence,
                    "renewal is not a takeover"
                );
                assert_eq!(now.state, WorkspaceState::Active);
                let _ = provider.cleanup(environment).await;
                return;
            }
        }
        panic!("the heartbeat timer never renewed the lease");
    }

    /// The renewal step reports a takeover instead of resurrecting the claim.
    #[tokio::test]
    #[serial_test::serial]
    async fn lease_renewal_reports_a_takeover_instead_of_resurrecting_the_claim() {
        let main = TempDir::new().unwrap();
        test::setup_with_new_libra_in(main.path()).await;
        std::fs::write(main.path().join("README.md"), "hello").unwrap();
        let provider = ExecutionEnvironmentProvider;

        let environment = provider
            .provision_task_worktree(
                main.path().to_path_buf(),
                Uuid::new_v4(),
                FuseProvisionState::default(),
            )
            .await
            .expect("provision task worktree");
        let workspace_id = environment
            .workspace_id()
            .expect("record published")
            .to_string();
        let storage_db = environment
            .storage_db
            .clone()
            .expect("repository-backed environment");
        let lease = environment.lease.clone().expect("lease");

        let conn = repo_db(main.path()).await;
        crate::internal::workspace::WorkspaceStore::reclaim_expired(
            &conn,
            &workspace_id,
            "doctor",
            TASK_LEASE_TTL_MS,
            now_ms() + TASK_LEASE_TTL_MS + 1,
        )
        .await
        .expect("reclaim");

        assert!(matches!(
            renew_task_lease(&storage_db, &lease)
                .await
                .expect_err("a reclaimed lease cannot be renewed"),
            WorkspaceError::LeaseLost { .. }
        ));

        let _ = provider.cleanup(environment).await;
    }

    /// A working directory that is not a repository still gets a task worktree
    /// — there is simply nothing to associate it with, so no record is
    /// published and teardown stays a plain directory removal.
    #[tokio::test]
    #[serial_test::serial]
    async fn task_worktree_outside_a_repository_has_no_record() {
        let main = TempDir::new().unwrap();
        std::fs::write(main.path().join("README.md"), "hello").unwrap();
        let provider = ExecutionEnvironmentProvider;

        let environment = provider
            .provision_task_worktree(
                main.path().to_path_buf(),
                Uuid::new_v4(),
                FuseProvisionState::default(),
            )
            .await
            .expect("provision task worktree");
        assert!(environment.workspace_id().is_none());
        let root = environment.root().to_path_buf();

        provider
            .cleanup(environment)
            .await
            .expect("cleanup worktree");
        assert!(!root.exists());
    }

    /// Sync-back is the moment the task worktree's changes land in the main
    /// workspace, so it renews the lease first and FAILS CLOSED if this run no
    /// longer holds it: a doctor that presumed this runtime dead may have
    /// handed the workspace to someone else who is writing to it now.
    #[tokio::test]
    #[serial_test::serial]
    async fn sync_back_refuses_once_the_lease_has_been_reclaimed() {
        let main = TempDir::new().unwrap();
        test::setup_with_new_libra_in(main.path()).await;
        std::fs::write(main.path().join("README.md"), "hello").unwrap();
        let provider = ExecutionEnvironmentProvider;

        let environment = provider
            .provision_task_worktree(
                main.path().to_path_buf(),
                Uuid::new_v4(),
                FuseProvisionState::default(),
            )
            .await
            .expect("provision task worktree");
        let workspace_id = environment
            .workspace_id()
            .expect("record published")
            .to_string();

        // The lease expires and a doctor reclaims it with a new fence.
        let conn = repo_db(main.path()).await;
        crate::internal::workspace::WorkspaceStore::reclaim_expired(
            &conn,
            &workspace_id,
            "doctor",
            TASK_LEASE_TTL_MS,
            now_ms() + TASK_LEASE_TTL_MS + 1,
        )
        .await
        .expect("doctor reclaims the expired lease");

        let error = provider
            .sync_back(
                &environment,
                SyncBackRequest {
                    main_working_dir: main.path().to_path_buf(),
                    touch_files: vec!["README.md".to_string()],
                    scope_in: Vec::new(),
                    scope_out: Vec::new(),
                },
            )
            .await
            .expect_err("a reclaimed lease must not replay changes into main");
        assert!(
            format!("{error}").contains("lease"),
            "the refusal names the lost lease: {error}"
        );

        // Teardown of a reclaimed workspace still removes the directory, and
        // reports that the record was no longer ours to settle.
        let root = environment.root().to_path_buf();
        assert!(provider.cleanup(environment).await.is_err());
        assert!(
            root.exists(),
            "the reclaimed workspace is the doctor's to remove"
        );
    }

    /// The sync worker itself is fenced: once the lease deadline this run last
    /// proved has passed, no further path may be written into main, even
    /// mid-operation. Without this, a long sync started on a still-valid proof
    /// could keep writing after another owner reclaimed the workspace.
    #[tokio::test]
    #[serial_test::serial]
    async fn expired_lease_stops_the_sync_worker_mid_operation() {
        let main = TempDir::new().unwrap();
        test::setup_with_new_libra_in(main.path()).await;
        std::fs::write(main.path().join("README.md"), "hello").unwrap();
        let provider = ExecutionEnvironmentProvider;

        let environment = provider
            .provision_task_worktree(
                main.path().to_path_buf(),
                Uuid::new_v4(),
                FuseProvisionState::default(),
            )
            .await
            .expect("provision task worktree");
        let lease = environment.lease.clone().expect("lease");

        let fence = LeaseDeadlineFence {
            workspace_id: lease.workspace_id.clone(),
            renewed_at: environment.renewed_at.clone(),
        };
        fence
            .check("apply a change")
            .expect("a freshly renewed lease allows writes");

        // The deadline lapses (the process stopped being able to renew).
        environment.renewed_at.store(
            now_ms() - TASK_LEASE_TTL_MS - 1,
            std::sync::atomic::Ordering::Relaxed,
        );
        let error = fence
            .check("apply a change")
            .expect_err("an expired lease must stop the worker");
        assert!(
            matches!(error, WorkspaceSyncError::HardConflict { .. }),
            "expected a hard conflict, got {error:?}"
        );

        let _ = provider.cleanup(environment).await;
    }

    /// An unreachable store at sync-back time must not cost a completed task
    /// its work: while the last successful renewal proves the deadline has not
    /// passed, the sync-back proceeds. Once that proof ages out, it fails
    /// closed instead of guessing.
    #[tokio::test]
    #[serial_test::serial]
    async fn unverifiable_lease_proceeds_while_the_deadline_still_holds() {
        let main = TempDir::new().unwrap();
        test::setup_with_new_libra_in(main.path()).await;
        std::fs::write(main.path().join("README.md"), "hello").unwrap();
        let provider = ExecutionEnvironmentProvider;

        let environment = provider
            .provision_task_worktree(
                main.path().to_path_buf(),
                Uuid::new_v4(),
                FuseProvisionState::default(),
            )
            .await
            .expect("provision task worktree");
        let storage_db = environment
            .storage_db
            .clone()
            .expect("repository-backed environment");

        // Freshly renewed: an unverifiable check still lets the run finish.
        ExecutionEnvironmentProvider::proceed_on_proof_of_freshness(
            &environment,
            &storage_db,
            "database unavailable".to_string(),
        )
        .expect("a lease renewed moments ago cannot have been reclaimed");

        // The margin must cover a long sync-back, not just its first step.
        assert!(
            LEASE_PROOF_MARGIN.as_millis() as i64 >= TASK_LEASE_TTL_MS / 2,
            "the freshness proof must leave room for the whole operation it authorizes"
        );

        // Aged past the deadline: nothing vouches for the lease any more.
        environment.renewed_at.store(
            now_ms() - TASK_LEASE_TTL_MS / 2 - 1,
            std::sync::atomic::Ordering::Relaxed,
        );
        let error = ExecutionEnvironmentProvider::proceed_on_proof_of_freshness(
            &environment,
            &storage_db,
            "database unavailable".to_string(),
        )
        .expect_err("an expired proof must fail closed");
        assert!(matches!(
            error,
            WorkspaceSyncError::RetryableConflict { .. }
        ));

        let _ = provider.cleanup(environment).await;
    }

    /// Two task attempts from the same repository get independent workspaces,
    /// leases and owners — the §C.8 acceptance case for parallel agents.
    ///
    /// What this proves is COEXISTENCE, which is what the acceptance criterion
    /// asks for: two runs driven concurrently end up holding two live
    /// workspaces at the same time, with distinct ids, roots, owners and
    /// recorded paths. It deliberately does NOT claim their internals overlap
    /// instant-for-instant — publications cannot, since every caller in a
    /// process shares one pooled SQLite connection (parking both inside
    /// `acquire` through the store failpoint deadlocks for exactly that
    /// reason), and the concurrent-publication guarantee is pinned at the
    /// store level by
    /// `workspace_lease_test::concurrent_acquire_overlap_is_decided_by_the_unique_index`,
    /// which uses two independent connections.
    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    #[serial_test::serial]
    async fn parallel_task_worktrees_get_independent_leases() {
        let main = TempDir::new().unwrap();
        test::setup_with_new_libra_in(main.path()).await;
        std::fs::write(main.path().join("README.md"), "hello").unwrap();
        let provider = ExecutionEnvironmentProvider;

        let (first, second) = tokio::join!(
            provider.provision_task_worktree(
                main.path().to_path_buf(),
                Uuid::new_v4(),
                FuseProvisionState::default(),
            ),
            provider.provision_task_worktree(
                main.path().to_path_buf(),
                Uuid::new_v4(),
                FuseProvisionState::default(),
            )
        );
        let first = first.expect("provision first");
        let second = second.expect("provision second");

        assert!(first.workspace_id().is_some() && second.workspace_id().is_some());
        assert_ne!(first.workspace_id(), second.workspace_id());
        assert_ne!(first.root(), second.root());

        let conn = repo_db(main.path()).await;
        let page = crate::internal::workspace::WorkspaceStore::list_with_conn(
            &conn,
            &WorkspaceListQuery::new().with_states(vec![WorkspaceState::Active]),
        )
        .await
        .expect("list workspaces");
        assert_eq!(page.items.len(), 2);
        let owners: std::collections::HashSet<_> = page
            .items
            .iter()
            .filter_map(|record| record.lease_owner.clone())
            .collect();
        assert_eq!(owners.len(), 2, "each run owns its own lease");
        let paths: std::collections::HashSet<_> = page
            .items
            .iter()
            .map(|record| record.path.clone())
            .collect();
        assert_eq!(paths.len(), 2, "two runs never share a directory");
        assert!(
            first.root().exists() && second.root().exists(),
            "both workspaces are live at the same time"
        );

        provider.cleanup(first).await.expect("cleanup first");
        provider.cleanup(second).await.expect("cleanup second");
    }
}
