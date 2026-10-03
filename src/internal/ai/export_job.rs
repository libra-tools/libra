//! OpenCode export-bridge job coordination (plan-20260713 DR-04b, ADR-DR-11).
//!
//! One `agent_export_job` row per `(agent_kind, provider_session_id)` makes
//! per-idle `opencode export` runs CONVERGENT without a queue or resident
//! worker:
//!
//! - every `session.idle` atomically bumps `observed_generation`
//!   ([`observe_idle`]); the caller that also wins the lease becomes the
//!   runner, everyone else returns immediately;
//! - the runner exports + gates turns through the coverage claim, then
//!   advances `processed_generation` to its target under owner+fence
//!   ([`advance_processed`]) — a fenced-out stale runner cannot advance,
//!   release, or mark anything clean;
//! - `observed > processed` after an advance means more idles arrived while
//!   exporting: the runner loops within its deadline or leaves the job
//!   `dirty` for the next idle/takeover ([`release`]);
//! - rows expire by TTL (clean --gc / retention / startup scavenging), never
//!   by session cascade — the provider session may not exist locally.

use std::time::{Duration, Instant};

use anyhow::{Context, Result, anyhow, bail};
use sea_orm::{ConnectionTrait, DatabaseConnection, DatabaseTransaction, Statement};

use super::capture_scope::{
    CaptureCommitDeadline, CaptureFinalCommitAuthorizationError, CaptureScope,
    authorize_final_capture_commit,
};

/// Lease length for one export run: must cover the export subprocess
/// deadline (≤3s, GC-DR-04) plus parse/redact/claim with margin.
const EXPORT_LEASE_MS: i64 = 30_000;
/// Job-row TTL: quiet jobs are scavenged after a day.
const EXPORT_JOB_TTL_MS: i64 = 24 * 60 * 60 * 1_000;
/// Releasing an already-owned export lease after normal capture work fails is
/// recovery-only work. It gets a short bounded writer-acquisition window and
/// must not re-anchor a still-live primary SQLite authorization deadline.
const EXPORT_LEASE_RELEASE_RECOVERY_GRACE: Duration = Duration::from_millis(250);

/// The managed export deadline remains authoritative at the final SQLite
/// boundary. A workspace fence is asynchronous, so checking only before it
/// can still make a stale generation bump or runner lease durable.
fn ensure_observed_idle_before_deadline(deadline: Option<CaptureCommitDeadline>) -> Result<()> {
    if deadline.is_some_and(|deadline| Instant::now() >= deadline.monotonic()) {
        bail!("OpenCode export idle observation exceeded its managed execution deadline");
    }
    Ok(())
}

/// Bound only work that can be safely abandoned before this observation has
/// made any durable change. Callers must pass writer acquisition or an
/// explicitly read-only preflight, never DML, final authorization, rollback,
/// or a COMMIT acknowledgement.
async fn run_observed_idle_preflight_until<T>(
    deadline: Option<CaptureCommitDeadline>,
    operation: &'static str,
    operation_future: impl std::future::Future<Output = Result<T>>,
) -> Result<T> {
    ensure_observed_idle_before_deadline(deadline)?;
    let result = match deadline {
        Some(deadline) => tokio::time::timeout_at(
            tokio::time::Instant::from_std(deadline.monotonic()),
            operation_future,
        )
        .await
        .map_err(|_| {
            anyhow!(
                "OpenCode export idle observation exceeded its managed execution deadline while {operation}"
            )
        })?,
        None => operation_future.await,
    };
    ensure_observed_idle_before_deadline(deadline)?;
    result
}

/// Acquire SQLite's writer slot before any export-job mutation. The paired
/// SQLite deadline is still checked by final authorization immediately before
/// the non-cancellable COMMIT.
async fn begin_observed_idle_transaction_until(
    conn: &DatabaseConnection,
    deadline: Option<CaptureCommitDeadline>,
) -> Result<DatabaseTransaction> {
    run_observed_idle_preflight_until(deadline, "begin export generation transaction", async {
        crate::internal::db::begin_write_transaction(conn)
            .await
            .context("begin export generation transaction")
    })
    .await
}

/// Stop a normal export-job state mutation before its paired execution budget
/// has elapsed. The SQLite half is checked only by final authorization: it
/// must never be re-derived from the remaining monotonic duration.
fn ensure_export_job_mutation_before_deadline(deadline: CaptureCommitDeadline) -> Result<()> {
    if Instant::now() >= deadline.monotonic() {
        bail!("OpenCode export job mutation exceeded its managed execution deadline");
    }
    Ok(())
}

/// Acquire SQLite's writer slot for a normal export-job mutation. This is the
/// only part of the mutation transaction that is safe to cancel at a deadline;
/// DML, final authorization, rollback, and COMMIT acknowledgement are not.
async fn begin_export_job_mutation_transaction_until(
    conn: &DatabaseConnection,
    deadline: CaptureCommitDeadline,
    operation: &'static str,
) -> Result<DatabaseTransaction> {
    ensure_export_job_mutation_before_deadline(deadline)?;
    let txn = tokio::time::timeout_at(
        tokio::time::Instant::from_std(deadline.monotonic()),
        crate::internal::db::begin_write_transaction(conn),
    )
    .await
    .map_err(|_| anyhow!("OpenCode export job mutation exceeded its managed execution deadline"))?
    .context(operation)?;
    if let Err(error) = ensure_export_job_mutation_before_deadline(deadline) {
        txn.rollback().await.ok();
        return Err(error).context(operation);
    }
    Ok(txn)
}

/// Make the paired SQLite authorization the last statement before a
/// non-cancellable export-job COMMIT acknowledgement. Once DML has begun, a
/// timeout must not drop the transaction because SQLite may have dispatched
/// work before cancellation is observed.
async fn commit_export_job_mutation_transaction_until(
    txn: DatabaseTransaction,
    scope: &CaptureScope,
    deadline: CaptureCommitDeadline,
    operation: &'static str,
) -> Result<()> {
    if let Err(error) = ensure_export_job_mutation_before_deadline(deadline) {
        txn.rollback().await.ok();
        return Err(error).context(operation);
    }
    let authorization = authorize_final_capture_commit(Some(scope), &txn, Some(deadline)).await;
    if let Err(error) = authorization {
        txn.rollback().await.ok();
        return Err(match error {
            CaptureFinalCommitAuthorizationError::DeadlineElapsed => {
                anyhow!("OpenCode export job mutation exceeded its managed execution deadline")
            }
            error @ (CaptureFinalCommitAuthorizationError::WorkspaceFenceRejected
            | CaptureFinalCommitAuthorizationError::Database(_)) => anyhow::Error::new(error),
        })
        .context(operation);
    }
    txn.commit().await.context(operation)
}

/// Derive the short paired deadline permitted solely for retiring an existing
/// export lease after the foreground capture stops. A live primary deadline
/// remains the ceiling in both clock domains; only an already-expired primary
/// may use a fresh recovery pair.
fn export_lease_release_recovery_deadline(
    primary_deadline: CaptureCommitDeadline,
) -> Result<CaptureCommitDeadline> {
    if primary_deadline.monotonic() <= Instant::now() {
        return CaptureCommitDeadline::from_budget(EXPORT_LEASE_RELEASE_RECOVERY_GRACE)
            .context("establish OpenCode export-lease recovery deadline");
    }

    let recovery_deadline = CaptureCommitDeadline::from_budget(EXPORT_LEASE_RELEASE_RECOVERY_GRACE)
        .context("establish OpenCode export-lease recovery deadline")?;
    Ok(CaptureCommitDeadline::from_established_pair(
        primary_deadline
            .monotonic()
            .min(recovery_deadline.monotonic()),
        primary_deadline
            .sqlite_not_after_millis()
            .min(recovery_deadline.sqlite_not_after_millis()),
    ))
}

/// The runner's view after [`observe_idle`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum IdleOutcome {
    /// This caller holds the lease and must export up to `target_generation`.
    Runner {
        job_id: String,
        fence_token: i64,
        target_generation: i64,
    },
    /// Another runner holds an unexpired lease; the bump was recorded and
    /// that runner (or a later idle) will pick it up.
    RecordedOnly,
}

/// The stable owner key for one export job. Keeping the provider identity and
/// its capture scope together prevents callers from advancing a job under a
/// mismatched scope, and keeps the mutation APIs from growing positional
/// argument lists as ownership checks evolve.
#[derive(Debug, Clone, Copy)]
pub struct ExportJobTarget<'a> {
    agent_kind: &'a str,
    provider_session_id: &'a str,
    scope: &'a CaptureScope,
}

impl<'a> ExportJobTarget<'a> {
    pub fn new(agent_kind: &'a str, provider_session_id: &'a str, scope: &'a CaptureScope) -> Self {
        Self {
            agent_kind,
            provider_session_id,
            scope,
        }
    }
}

fn now_or(now_ms: i64, delta: i64) -> i64 {
    now_ms.saturating_add(delta)
}

/// Close one multi-statement export observation under the workspace lease that
/// authorized its generation bump. Every individual job statement carries an
/// inline lease predicate, but only this final DML prevents a lease expiry
/// after the last job write from committing the earlier bump or runner lease.
async fn commit_observed_idle_transaction(
    txn: DatabaseTransaction,
    scope: &CaptureScope,
    deadline: Option<CaptureCommitDeadline>,
    operation: &'static str,
) -> Result<()> {
    // The final SQLite statement is the durable deadline/lease decision
    // point. Do not time out the following COMMIT: SQLx can dispatch it
    // before a cancellation is observed, which would make rollback unsafe.
    let authorization = match ensure_observed_idle_before_deadline(deadline) {
        Ok(()) => match authorize_final_capture_commit(Some(scope), &txn, deadline).await {
            Ok(()) => Ok(()),
            Err(CaptureFinalCommitAuthorizationError::DeadlineElapsed) => Err(anyhow!(
                "OpenCode export idle observation exceeded its managed execution deadline"
            )),
            Err(error) => Err(anyhow::Error::new(error).context(
                "verify capture workspace lease before final export observation authorization",
            )),
        },
        Err(error) => Err(error),
    };
    if let Err(error) = authorization {
        txn.rollback().await.ok();
        return Err(error).context(operation);
    }
    txn.commit().await.context(operation)
}

/// Reject a legacy or foreign job before it can receive another generation
/// bump. The global provider-session unique key stays in place deliberately:
/// this check turns a cross-workspace observation into an actionable refusal
/// instead of a silent overwrite or a second export runner.
async fn assert_export_job_scope<C: ConnectionTrait>(
    conn: &C,
    agent_kind: &str,
    provider_session_id: &str,
    scope: &CaptureScope,
) -> Result<()> {
    let row = conn
        .query_one_raw(Statement::from_sql_and_values(
            conn.get_database_backend(),
            "SELECT scope_state, repo_id, worktree_id, workspace_id, workspace_fence \
             FROM agent_export_job WHERE agent_kind = ? AND provider_session_id = ?",
            [agent_kind.into(), provider_session_id.into()],
        ))
        .await
        .context("read export-job workspace ownership")?;
    let Some(row) = row else {
        return Ok(());
    };
    let scope_state: String = row.try_get_by("scope_state")?;
    let repo_id: Option<String> = row.try_get_by("repo_id")?;
    let worktree_id: Option<String> = row.try_get_by("worktree_id")?;
    let workspace_id: Option<String> = row.try_get_by("workspace_id")?;
    let workspace_fence: Option<i64> = row.try_get_by("workspace_fence")?;
    if scope_state != "scoped"
        || repo_id.as_deref() != Some(scope.repo_id.as_str())
        || worktree_id.as_deref() != Some(scope.worktree_id.as_str())
        || workspace_id != scope.workspace_id
        || workspace_fence != scope.workspace_fence
    {
        bail!(
            "OpenCode export job belongs to a legacy or different workspace scope; refusing to \
             advance it — inspect it with `libra worktree doctor` before retrying"
        );
    }
    Ok(())
}

/// Record one `session.idle` and try to become the runner (ADR-DR-11).
///
/// Atomicity: the generation bump and the lease attempt are separate
/// conditional writes, each checked via `rows_affected == 1`; losing any
/// race degrades to [`IdleOutcome::RecordedOnly`], never to a double-runner.
pub async fn observe_idle(
    conn: &DatabaseConnection,
    agent_kind: &str,
    provider_session_id: &str,
    scope: &CaptureScope,
    owner: &str,
    now_ms: i64,
) -> Result<IdleOutcome> {
    observe_idle_until(
        conn,
        agent_kind,
        provider_session_id,
        scope,
        owner,
        now_ms,
        None,
    )
    .await
}

/// Deadline-aware variant of [`observe_idle`]. The deadline is deliberately
/// non-persistent: it limits only this observation transaction and cannot
/// alter the durable job's retry or lease semantics.
pub async fn observe_idle_until(
    conn: &DatabaseConnection,
    agent_kind: &str,
    provider_session_id: &str,
    scope: &CaptureScope,
    owner: &str,
    now_ms: i64,
    deadline: Option<CaptureCommitDeadline>,
) -> Result<IdleOutcome> {
    ensure_observed_idle_before_deadline(deadline)?;
    let txn = begin_observed_idle_transaction_until(conn, deadline).await?;
    // Check cross-table ownership only after acquiring SQLite's writer slot.
    // Otherwise an import/hook writer in another scope can commit its claim
    // between this preflight and the export-job insert below.
    if let Err(error) = run_observed_idle_preflight_until(
        deadline,
        "verify export provider-session scope",
        scope.assert_provider_session_compatible(&txn, provider_session_id),
    )
    .await
    {
        txn.rollback().await.ok();
        return Err(error);
    }
    if let Err(error) = run_observed_idle_preflight_until(
        deadline,
        "read export-job workspace ownership",
        assert_export_job_scope(&txn, agent_kind, provider_session_id, scope),
    )
    .await
    {
        txn.rollback().await.ok();
        return Err(error);
    }
    let tombstoned =
        match run_observed_idle_preflight_until(deadline, "check export tombstone barrier", async {
            txn.query_one_raw(Statement::from_sql_and_values(
                txn.get_database_backend(),
                "SELECT 1 FROM agent_import_tombstone
                 WHERE agent_kind = ? AND provider_session_id = ?",
                [agent_kind.into(), provider_session_id.into()],
            ))
            .await
            .context("check export tombstone barrier")
        })
        .await
        {
            Ok(tombstoned) => tombstoned,
            Err(error) => {
                txn.rollback().await.ok();
                return Err(error);
            }
        };
    if tombstoned.is_some() {
        txn.rollback().await.ok();
        anyhow::bail!("erased agent session cannot create or advance an export job");
    }
    // Ensure the row exists (first idle creates it).
    if let Err(error) = ensure_observed_idle_before_deadline(deadline) {
        txn.rollback().await.ok();
        return Err(error).context("authorize export job creation before mutation");
    }
    txn.execute_raw(Statement::from_sql_and_values(
        txn.get_database_backend(),
        "INSERT INTO agent_export_job (
            job_id, agent_kind, provider_session_id, observed_generation,
            processed_generation, state, created_at, updated_at, ttl_expires_at,
            repo_id, worktree_id, workspace_id, workspace_fence, scope_state
         ) SELECT ?, ?, ?, 0, 0, 'idle', ?, ?, ?, ?, ?, ?, ?, 'scoped'
         WHERE (? IS NULL OR EXISTS (
             SELECT 1 FROM workspace_record
             WHERE workspace_id = ? AND repo_id = ? AND lease_fence = ?
               AND state IN ('provisioning', 'active', 'releasing')
               AND lease_owner IS NOT NULL
               AND lease_expires_at > (unixepoch('now') * 1000)
         ))
         ON CONFLICT(agent_kind, provider_session_id) DO NOTHING",
        [
            uuid::Uuid::new_v4().to_string().into(),
            agent_kind.into(),
            provider_session_id.into(),
            now_ms.into(),
            now_ms.into(),
            now_or(now_ms, EXPORT_JOB_TTL_MS).into(),
            scope.repo_id.clone().into(),
            scope.worktree_id.clone().into(),
            scope.workspace_id.clone().into(),
            scope.workspace_fence.into(),
            scope.workspace_id.clone().into(),
            scope.workspace_id.clone().into(),
            scope.repo_id.clone().into(),
            scope.workspace_fence.into(),
        ],
    ))
    .await
    .context("insert agent_export_job row")?;

    // Unconditional observed bump — every idle counts exactly once.
    if let Err(error) = ensure_observed_idle_before_deadline(deadline) {
        txn.rollback().await.ok();
        return Err(error).context("authorize export generation bump before mutation");
    }
    let bumped = txn
        .execute_raw(Statement::from_sql_and_values(
            txn.get_database_backend(),
            "UPDATE agent_export_job
         SET observed_generation = observed_generation + 1, updated_at = ?,
             ttl_expires_at = ?
         WHERE agent_kind = ? AND provider_session_id = ?
           AND scope_state = 'scoped' AND repo_id = ? AND worktree_id = ?
           AND workspace_id IS ? AND workspace_fence IS ?
           AND (? IS NULL OR EXISTS (
               SELECT 1 FROM workspace_record
               WHERE workspace_id = ? AND repo_id = ? AND lease_fence = ?
                 AND state IN ('provisioning', 'active', 'releasing')
                 AND lease_owner IS NOT NULL
                 AND lease_expires_at > (unixepoch('now') * 1000)
           ))",
            [
                now_ms.into(),
                now_or(now_ms, EXPORT_JOB_TTL_MS).into(),
                agent_kind.into(),
                provider_session_id.into(),
                scope.repo_id.clone().into(),
                scope.worktree_id.clone().into(),
                scope.workspace_id.clone().into(),
                scope.workspace_fence.into(),
                scope.workspace_id.clone().into(),
                scope.workspace_id.clone().into(),
                scope.repo_id.clone().into(),
                scope.workspace_fence.into(),
            ],
        ))
        .await
        .context("bump observed_generation")?;
    if bumped.rows_affected() != 1 {
        txn.rollback().await.ok();
        bail!(
            "export job could not be advanced because its workspace lease fence changed or another scope owns this provider session"
        );
    }

    // Lease attempt: only when no live lease exists (expired or absent) AND
    // pending work remains (processed < observed) — a delayed contender whose
    // bump was already processed by another runner must NOT re-export a
    // clean generation (Codex M3 R1 P1-4).
    if let Err(error) = ensure_observed_idle_before_deadline(deadline) {
        txn.rollback().await.ok();
        return Err(error).context("authorize export lease acquisition before mutation");
    }
    let acquired = txn
        .execute_raw(Statement::from_sql_and_values(
            txn.get_database_backend(),
            "UPDATE agent_export_job
             SET owner = ?, lease_expires_at = ?,
                 fence_token = COALESCE(fence_token, 0) + 1,
                 state = 'inflight', updated_at = ?
             WHERE agent_kind = ? AND provider_session_id = ?
               AND scope_state = 'scoped' AND repo_id = ? AND worktree_id = ?
               AND workspace_id IS ? AND workspace_fence IS ?
               AND (? IS NULL OR EXISTS (
                   SELECT 1 FROM workspace_record
                   WHERE workspace_id = ? AND repo_id = ? AND lease_fence = ?
                     AND state IN ('provisioning', 'active', 'releasing')
                     AND lease_owner IS NOT NULL
                     AND lease_expires_at > (unixepoch('now') * 1000)
               ))
               AND (owner IS NULL OR lease_expires_at IS NULL OR lease_expires_at <= ?)
               AND processed_generation < observed_generation",
            [
                owner.into(),
                now_or(now_ms, EXPORT_LEASE_MS).into(),
                now_ms.into(),
                agent_kind.into(),
                provider_session_id.into(),
                scope.repo_id.clone().into(),
                scope.worktree_id.clone().into(),
                scope.workspace_id.clone().into(),
                scope.workspace_fence.into(),
                scope.workspace_id.clone().into(),
                scope.workspace_id.clone().into(),
                scope.repo_id.clone().into(),
                scope.workspace_fence.into(),
                now_ms.into(),
            ],
        ))
        .await
        .context("acquire export lease")?;
    if acquired.rows_affected() != 1 {
        commit_observed_idle_transaction(
            txn,
            scope,
            deadline,
            "commit recorded export generation under its workspace lease",
        )
        .await?;
        return Ok(IdleOutcome::RecordedOnly);
    }

    // This read follows the generation and lease DML, so it deliberately is
    // not cancellation-wrapped. The final authorization below rolls the
    // transaction back if its deadline has elapsed.
    if let Err(error) = ensure_observed_idle_before_deadline(deadline) {
        txn.rollback().await.ok();
        return Err(error).context("read acquired export job");
    }
    let row = txn
        .query_one_raw(Statement::from_sql_and_values(
            txn.get_database_backend(),
            "SELECT job_id, fence_token, observed_generation FROM agent_export_job
             WHERE agent_kind = ? AND provider_session_id = ? AND owner = ?
               AND scope_state = 'scoped' AND repo_id = ? AND worktree_id = ?
               AND workspace_id IS ? AND workspace_fence IS ?",
            [
                agent_kind.into(),
                provider_session_id.into(),
                owner.into(),
                scope.repo_id.clone().into(),
                scope.worktree_id.clone().into(),
                scope.workspace_id.clone().into(),
                scope.workspace_fence.into(),
            ],
        ))
        .await
        .context("read acquired export job")?
        .ok_or_else(|| anyhow!("export job vanished after lease acquisition"))?;
    let outcome = IdleOutcome::Runner {
        job_id: row.try_get_by("job_id")?,
        fence_token: row
            .try_get_by::<Option<i64>, _>("fence_token")?
            .context("acquired export job has no fence token")?,
        target_generation: row.try_get_by("observed_generation")?,
    };
    commit_observed_idle_transaction(
        txn,
        scope,
        deadline,
        "commit export generation lease under its workspace lease",
    )
    .await?;
    Ok(outcome)
}

/// Advance `processed_generation` to `target` under owner+fence. Returns
/// whether more work arrived meanwhile (`observed > processed`): the runner
/// loops (within its deadline) or releases dirty. Zero rows = fenced out —
/// the stale runner must stop without touching anything else.
async fn advance_processed_inner<C: ConnectionTrait>(
    conn: &C,
    job: &ExportJobTarget<'_>,
    owner: &str,
    fence_token: i64,
    target: i64,
    now_ms: i64,
) -> Result<AdvanceOutcome> {
    let advanced = conn
        .execute_raw(Statement::from_sql_and_values(
            conn.get_database_backend(),
            "UPDATE agent_export_job
             SET processed_generation = ?, updated_at = ?
         WHERE agent_kind = ? AND provider_session_id = ?
               AND scope_state = 'scoped' AND repo_id = ? AND worktree_id = ?
               AND workspace_id IS ? AND workspace_fence IS ?
               AND owner = ? AND fence_token = ?
               AND processed_generation < ?
               AND (agent_export_job.workspace_id IS NULL OR EXISTS (
                   SELECT 1 FROM workspace_record
                   WHERE workspace_id = agent_export_job.workspace_id
                     AND repo_id = agent_export_job.repo_id
                     AND lease_fence = agent_export_job.workspace_fence
                     AND state IN ('provisioning', 'active', 'releasing')
                     AND lease_owner IS NOT NULL
                     AND lease_expires_at > (unixepoch('now') * 1000)
               ))
               AND NOT EXISTS (
                 SELECT 1 FROM agent_import_tombstone t
                 WHERE t.agent_kind = agent_export_job.agent_kind
                   AND t.provider_session_id = agent_export_job.provider_session_id
               )",
            [
                target.into(),
                now_ms.into(),
                job.agent_kind.into(),
                job.provider_session_id.into(),
                job.scope.repo_id.clone().into(),
                job.scope.worktree_id.clone().into(),
                job.scope.workspace_id.clone().into(),
                job.scope.workspace_fence.into(),
                owner.into(),
                fence_token.into(),
                target.into(),
            ],
        ))
        .await
        .context("advance processed_generation")?;
    if advanced.rows_affected() != 1 {
        return Ok(AdvanceOutcome::FencedOut);
    }
    let row = conn
        .query_one_raw(Statement::from_sql_and_values(
            conn.get_database_backend(),
            "SELECT observed_generation, processed_generation FROM agent_export_job
             WHERE agent_kind = ? AND provider_session_id = ?
               AND scope_state = 'scoped' AND repo_id = ? AND worktree_id = ?
               AND workspace_id IS ? AND workspace_fence IS ?",
            [
                job.agent_kind.into(),
                job.provider_session_id.into(),
                job.scope.repo_id.clone().into(),
                job.scope.worktree_id.clone().into(),
                job.scope.workspace_id.clone().into(),
                job.scope.workspace_fence.into(),
            ],
        ))
        .await
        .context("re-read export job generations")?
        .ok_or_else(|| anyhow!("export job vanished after advance"))?;
    let observed: i64 = row.try_get_by("observed_generation")?;
    let processed: i64 = row.try_get_by("processed_generation")?;
    Ok(if observed > processed {
        AdvanceOutcome::MoreWork {
            target_generation: observed,
        }
    } else {
        AdvanceOutcome::Clean
    })
}

pub async fn advance_processed(
    conn: &DatabaseConnection,
    job: &ExportJobTarget<'_>,
    owner: &str,
    fence_token: i64,
    target: i64,
    now_ms: i64,
) -> Result<AdvanceOutcome> {
    advance_processed_inner(conn, job, owner, fence_token, target, now_ms).await
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AdvanceOutcome {
    /// Everything observed is processed.
    Clean,
    /// New idles arrived while exporting; keep looping (bounded) or release
    /// dirty.
    MoreWork { target_generation: i64 },
    /// Reservation was taken over; this runner must stop entirely.
    FencedOut,
}

/// Advance the runner's processed generation and release its lease with a
/// state that matches the observed generation. This keeps callers from
/// accidentally marking a job `idle` after [`AdvanceOutcome::MoreWork`]. A
/// fenced-out runner never attempts a release because the new owner's state
/// must remain untouched.
pub async fn advance_and_release(
    conn: &DatabaseConnection,
    job: &ExportJobTarget<'_>,
    owner: &str,
    fence_token: i64,
    target: i64,
    now_ms: i64,
) -> Result<AdvanceOutcome> {
    let outcome = advance_processed(conn, job, owner, fence_token, target, now_ms).await?;
    let state = match outcome {
        AdvanceOutcome::Clean => Some("idle"),
        AdvanceOutcome::MoreWork { .. } => Some("dirty"),
        AdvanceOutcome::FencedOut => None,
    };
    if let Some(state) = state {
        release(conn, job, owner, fence_token, state, None, now_ms).await?;
    }
    Ok(outcome)
}

/// Advance and release under one normal capture transaction. Keeping the
/// conditional advance, generation read, and release together prevents a
/// later idle from being incorrectly marked clean between independent
/// statements, while the primary paired deadline remains authoritative at the
/// final authorization boundary.
pub(crate) async fn advance_and_release_until(
    conn: &DatabaseConnection,
    job: &ExportJobTarget<'_>,
    owner: &str,
    fence_token: i64,
    target: i64,
    now_ms: i64,
    deadline: CaptureCommitDeadline,
) -> Result<AdvanceOutcome> {
    let txn = begin_export_job_mutation_transaction_until(
        conn,
        deadline,
        "begin export generation settlement",
    )
    .await?;
    if let Err(error) = ensure_export_job_mutation_before_deadline(deadline) {
        txn.rollback().await.ok();
        return Err(error).context("authorize export generation settlement before mutation");
    }
    let outcome = match advance_processed_inner(&txn, job, owner, fence_token, target, now_ms).await
    {
        Ok(outcome) => outcome,
        Err(error) => {
            txn.rollback().await.ok();
            return Err(error);
        }
    };
    let state = match outcome {
        AdvanceOutcome::Clean => "idle",
        AdvanceOutcome::MoreWork { .. } => "dirty",
        AdvanceOutcome::FencedOut => {
            txn.rollback().await.ok();
            return Ok(AdvanceOutcome::FencedOut);
        }
    };
    if let Err(error) = ensure_export_job_mutation_before_deadline(deadline) {
        txn.rollback().await.ok();
        return Err(error).context("authorize export lease release before mutation");
    }
    let released = match release_inner(&txn, job, owner, fence_token, state, None, now_ms).await {
        Ok(released) => released,
        Err(error) => {
            txn.rollback().await.ok();
            return Err(error);
        }
    };
    if !released {
        txn.rollback().await.ok();
        return Ok(AdvanceOutcome::FencedOut);
    }
    commit_export_job_mutation_transaction_until(
        txn,
        job.scope,
        deadline,
        "commit export generation settlement",
    )
    .await?;
    Ok(outcome)
}

/// Release the lease under owner+fence, marking the terminal state honestly:
/// `dirty` when work remains, `failed` with a stable code, else `idle`. A
/// fenced-out release is a silent no-op (the new owner's state wins).
async fn release_inner<C: ConnectionTrait>(
    conn: &C,
    job: &ExportJobTarget<'_>,
    owner: &str,
    fence_token: i64,
    state: &str,
    last_error_code: Option<&str>,
    now_ms: i64,
) -> Result<bool> {
    debug_assert!(matches!(state, "idle" | "dirty" | "failed"));
    let released = conn
        .execute_raw(Statement::from_sql_and_values(
            conn.get_database_backend(),
            "UPDATE agent_export_job
         SET owner = NULL, lease_expires_at = NULL, state = ?,
             last_error_code = ?, updated_at = ?
         WHERE agent_kind = ? AND provider_session_id = ?
           AND scope_state = 'scoped' AND repo_id = ? AND worktree_id = ?
           AND workspace_id IS ? AND workspace_fence IS ?
           AND owner = ? AND fence_token = ?
           AND (agent_export_job.workspace_id IS NULL OR EXISTS (
               SELECT 1 FROM workspace_record
               WHERE workspace_id = agent_export_job.workspace_id
                 AND repo_id = agent_export_job.repo_id
                 AND lease_fence = agent_export_job.workspace_fence
                 AND state IN ('provisioning', 'active', 'releasing')
                 AND lease_owner IS NOT NULL
                 AND lease_expires_at > (unixepoch('now') * 1000)
           ))
           AND NOT EXISTS (
             SELECT 1 FROM agent_import_tombstone t
             WHERE t.agent_kind = agent_export_job.agent_kind
               AND t.provider_session_id = agent_export_job.provider_session_id
           )",
            [
                state.into(),
                last_error_code.into(),
                now_ms.into(),
                job.agent_kind.into(),
                job.provider_session_id.into(),
                job.scope.repo_id.clone().into(),
                job.scope.worktree_id.clone().into(),
                job.scope.workspace_id.clone().into(),
                job.scope.workspace_fence.into(),
                owner.into(),
                fence_token.into(),
            ],
        ))
        .await
        .context("release export lease")?;
    Ok(released.rows_affected() == 1)
}

pub async fn release(
    conn: &DatabaseConnection,
    job: &ExportJobTarget<'_>,
    owner: &str,
    fence_token: i64,
    state: &str,
    last_error_code: Option<&str>,
    now_ms: i64,
) -> Result<()> {
    let _ = release_inner(
        conn,
        job,
        owner,
        fence_token,
        state,
        last_error_code,
        now_ms,
    )
    .await?;
    Ok(())
}

/// One already-owned export lease settlement. Grouping the immutable lease
/// fence with its target state keeps normal settlement and recovery release
/// from drifting at their many call sites.
#[derive(Clone, Copy)]
pub(crate) struct ExportLeaseRelease<'a> {
    owner: &'a str,
    fence_token: i64,
    state: &'a str,
    last_error_code: Option<&'a str>,
    now_ms: i64,
}

impl<'a> ExportLeaseRelease<'a> {
    pub(crate) const fn new(
        owner: &'a str,
        fence_token: i64,
        state: &'a str,
        last_error_code: Option<&'a str>,
        now_ms: i64,
    ) -> Self {
        Self {
            owner,
            fence_token,
            state,
            last_error_code,
            now_ms,
        }
    }
}

/// Release a lease under the primary capture deadline. This is for ordinary
/// success-path settlement, so a deadline failure leaves the job inflight for
/// a later recovery path rather than publishing a late `idle` state.
pub(crate) async fn release_until(
    conn: &DatabaseConnection,
    job: &ExportJobTarget<'_>,
    release: ExportLeaseRelease<'_>,
    deadline: CaptureCommitDeadline,
) -> Result<()> {
    let txn =
        begin_export_job_mutation_transaction_until(conn, deadline, "begin export lease release")
            .await?;
    if let Err(error) = ensure_export_job_mutation_before_deadline(deadline) {
        txn.rollback().await.ok();
        return Err(error).context("authorize export lease release before mutation");
    }
    let released = match release_inner(
        &txn,
        job,
        release.owner,
        release.fence_token,
        release.state,
        release.last_error_code,
        release.now_ms,
    )
    .await
    {
        Ok(released) => released,
        Err(error) => {
            txn.rollback().await.ok();
            return Err(error);
        }
    };
    if !released {
        txn.rollback().await.ok();
        return Ok(());
    }
    commit_export_job_mutation_transaction_until(
        txn,
        job.scope,
        deadline,
        "commit export lease release",
    )
    .await
}

/// Retire an already-owned lease after a failed/abandoned capture. This path
/// never advances a generation or marks work `idle`; it only uses the narrow
/// paired recovery grace to publish an existing lease's retryable state.
pub(crate) async fn release_with_recovery_grace(
    conn: &DatabaseConnection,
    job: &ExportJobTarget<'_>,
    release: ExportLeaseRelease<'_>,
    primary_deadline: CaptureCommitDeadline,
) -> Result<()> {
    if release.state == "idle" {
        bail!(
            "OpenCode export recovery release may not mark a job idle; use the primary-deadline settlement path"
        );
    }
    let recovery_deadline = export_lease_release_recovery_deadline(primary_deadline)?;
    release_until(conn, job, release, recovery_deadline).await
}

/// Delete expired job rows (TTL scavenging — clean --gc / retention /
/// startup). Bounded by the TTL index.
pub async fn scavenge_expired(conn: &DatabaseConnection, now_ms: i64) -> Result<u64> {
    let result = conn
        .execute_raw(Statement::from_sql_and_values(
            conn.get_database_backend(),
            "DELETE FROM agent_export_job WHERE ttl_expires_at <= ?",
            [now_ms.into()],
        ))
        .await
        .context("scavenge expired export jobs")?;
    Ok(result.rows_affected())
}

#[cfg(test)]
mod tests {
    use std::time::{Duration, Instant};

    use sea_orm::Database;

    use super::*;
    use crate::internal::db::migration::run_builtin_migrations;

    async fn job_db() -> DatabaseConnection {
        let conn = Database::connect("sqlite::memory:").await.expect("mem db");
        conn.execute_raw(Statement::from_string(
            conn.get_database_backend(),
            "PRAGMA foreign_keys = OFF".to_string(),
        ))
        .await
        .expect("pragma");
        run_builtin_migrations(&conn).await.expect("migrations");
        conn
    }

    async fn file_job_db() -> (tempfile::TempDir, DatabaseConnection, std::path::PathBuf) {
        let directory = tempfile::tempdir().expect("create export-job database directory");
        let database_path = directory.path().join("export-job.db");
        let conn = crate::internal::db::create_database(
            database_path
                .to_str()
                .expect("export-job database path is utf-8"),
        )
        .await
        .expect("create export-job database");
        (directory, conn, database_path)
    }

    fn scope() -> CaptureScope {
        CaptureScope {
            repo_id: "export-job-test-repo".to_string(),
            worktree_id: String::new(),
            workspace_id: None,
            workspace_fence: None,
        }
    }

    fn leased_scope() -> CaptureScope {
        CaptureScope {
            repo_id: "export-job-test-repo".to_string(),
            worktree_id: String::new(),
            workspace_id: Some("export-job-workspace".to_string()),
            workspace_fence: Some(7),
        }
    }

    async fn seed_live_workspace_scope(conn: &DatabaseConnection) {
        conn.execute_unprepared(
            "INSERT INTO workspace_record (
                workspace_id, repo_id, kind, worktree_id, path, owner_kind,
                state, lease_owner, lease_fence, lease_expires_at, created_at, updated_at
             ) VALUES ('export-job-workspace', 'export-job-test-repo', 'task_copy', NULL,
                       '/tmp/export-job-workspace', 'agent', 'active', 'export-job-test-owner',
                       7, 9999999999999, 1, 1)",
        )
        .await
        .expect("seed live export-job workspace");
    }

    async fn export_job_count(conn: &DatabaseConnection) -> i64 {
        conn.query_one_raw(Statement::from_string(
            conn.get_database_backend(),
            "SELECT COUNT(*) AS count FROM agent_export_job".to_string(),
        ))
        .await
        .expect("count export jobs")
        .expect("export-job count row")
        .try_get_by("count")
        .expect("decode export-job count")
    }

    #[tokio::test]
    async fn finish_time_scope_fence_rolls_back_recorded_export_generation() {
        let conn = job_db().await;
        seed_live_workspace_scope(&conn).await;
        let scope = leased_scope();
        conn.execute_unprepared(
            "CREATE TRIGGER expire_export_scope_after_generation_bump
             AFTER UPDATE OF observed_generation ON agent_export_job
             BEGIN
                 UPDATE workspace_record
                    SET lease_expires_at = 0
                  WHERE workspace_id = 'export-job-workspace';
             END",
        )
        .await
        .expect("install generation-bump expiry trigger");

        let error = observe_idle(
            &conn,
            "opencode",
            "scope-fence-recorded",
            &scope,
            "runner",
            1,
        )
        .await
        .expect_err("post-bump expiry rejects recorded generation commit");
        assert!(format!("{error:#}").contains("workspace lease"));
        assert_eq!(
            export_job_count(&conn).await,
            0,
            "the expired lease rolls back the inserted job and generation bump"
        );
    }

    #[tokio::test]
    async fn finish_time_scope_fence_rolls_back_export_runner_lease() {
        let conn = job_db().await;
        seed_live_workspace_scope(&conn).await;
        let scope = leased_scope();
        conn.execute_unprepared(
            "CREATE TRIGGER expire_export_scope_after_runner_lease
             AFTER UPDATE OF owner ON agent_export_job
             BEGIN
                 UPDATE workspace_record
                    SET lease_expires_at = 0
                  WHERE workspace_id = 'export-job-workspace';
             END",
        )
        .await
        .expect("install runner-lease expiry trigger");

        let error = observe_idle(&conn, "opencode", "scope-fence-runner", &scope, "runner", 1)
            .await
            .expect_err("post-lease expiry rejects runner commit");
        assert!(format!("{error:#}").contains("workspace lease"));
        assert_eq!(
            export_job_count(&conn).await,
            0,
            "the expired lease rolls back the job, generation, and runner lease"
        );
    }

    /// The final SQLite deadline authorization must roll the entire
    /// observation back rather than making a newly-created job, its
    /// generation bump, or its runner owner durable. An expired SQLite half
    /// reaches the final transaction boundary without cancelling COMMIT.
    #[tokio::test]
    async fn observed_idle_sqlite_deadline_authorization_rolls_back_job_generation_and_owner() {
        let conn = job_db().await;
        seed_live_workspace_scope(&conn).await;
        let scope = leased_scope();
        let deadline =
            CaptureCommitDeadline::from_test_pair(Instant::now() + Duration::from_secs(5), 0);
        let error = observe_idle_until(
            &conn,
            "opencode",
            "post-final-fence-deadline",
            &scope,
            "runner",
            1,
            Some(deadline),
        )
        .await
        .expect_err("expired SQLite final authorization must roll back the export observation");
        assert!(
            format!("{error:#}").contains("managed execution deadline"),
            "unexpected final-authorization deadline error: {error:#}"
        );
        assert_eq!(
            export_job_count(&conn).await,
            0,
            "final SQLite deadline authorization must not publish a job, generation bump, or runner owner"
        );
    }

    /// Writer acquisition is the only mutable-observation step that may be
    /// cancellation-bounded. A real file-backed SQLite writer lock must not
    /// let a timed-out idle wake later and publish an export job.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn observed_idle_deadline_cancels_contended_sqlite_writer_without_export_job() {
        let (_directory, conn, database_path) = file_job_db().await;
        let holder = crate::internal::db::establish_connection_with_busy_timeout(
            database_path
                .to_str()
                .expect("export-job lock-holder database path is utf-8"),
            Duration::from_secs(1),
        )
        .await
        .expect("open independent export-job lock holder");
        let contender = crate::internal::db::establish_connection_with_busy_timeout(
            database_path
                .to_str()
                .expect("export-job deadline contender database path is utf-8"),
            Duration::from_secs(1),
        )
        .await
        .expect("open independent export-job deadline contender");
        let held = crate::internal::db::begin_write_transaction(&holder)
            .await
            .expect("acquire export-job SQLite writer lock");
        let scope = scope();
        let result = tokio::time::timeout(
            Duration::from_secs(2),
            observe_idle_until(
                &contender,
                "opencode",
                "deadline-locked-export-job",
                &scope,
                "deadline-runner",
                1,
                Some(CaptureCommitDeadline::from_test_pair(
                    Instant::now() + Duration::from_millis(30),
                    i64::MAX,
                )),
            ),
        )
        .await
        .expect("contended export-job writer acquisition must stop at its deadline");
        held.rollback()
            .await
            .expect("release export-job SQLite writer lock after deadline");

        let error = result.expect_err("contended export observation must stop at its deadline");
        assert!(
            format!("{error:#}").contains("managed execution deadline"),
            "writer-lock expiry must retain the export observation deadline error: {error:#}"
        );
        assert_eq!(
            tokio::time::timeout(Duration::from_secs(2), export_job_count(&conn))
                .await
                .expect("canceled writer acquisition must not leave SQLite locked"),
            0,
            "a canceled writer wait must not publish a delayed export job"
        );
    }

    /// The paired SQLite final authorization must roll a normal generation
    /// settlement back after its DML, rather than publishing a late `idle`
    /// lease release when the immutable wall-clock half has elapsed.
    #[tokio::test]
    async fn advance_and_release_sqlite_deadline_rolls_back_generation_and_lease() {
        let conn = job_db().await;
        let scope = scope();
        let (kind, sid, owner) = ("opencode", "settlement-final-deadline", "runner");
        let IdleOutcome::Runner {
            fence_token,
            target_generation,
            ..
        } = observe_idle(&conn, kind, sid, &scope, owner, 1_000)
            .await
            .expect("acquire export runner before final-deadline settlement")
        else {
            panic!("fresh export job must elect this test as runner");
        };
        let job = ExportJobTarget::new(kind, sid, &scope);

        let error = advance_and_release_until(
            &conn,
            &job,
            owner,
            fence_token,
            target_generation,
            2_000,
            CaptureCommitDeadline::from_test_pair(Instant::now() + Duration::from_secs(1), 0),
        )
        .await
        .expect_err("expired SQLite final authorization must roll back export settlement");
        assert!(
            format!("{error:#}").contains("managed execution deadline"),
            "unexpected final-authorization settlement error: {error:#}"
        );

        let row = conn
            .query_one_raw(Statement::from_sql_and_values(
                conn.get_database_backend(),
                "SELECT state, owner, observed_generation, processed_generation
                 FROM agent_export_job
                 WHERE agent_kind = ? AND provider_session_id = ?",
                [kind.into(), sid.into()],
            ))
            .await
            .expect("read export job after final-authorization rollback")
            .expect("export job remains after rolled-back settlement");
        assert_eq!(
            row.try_get_by::<String, _>("state")
                .expect("decode export job state"),
            "inflight",
            "an expired normal settlement must not publish idle"
        );
        assert_eq!(
            row.try_get_by::<Option<String>, _>("owner")
                .expect("decode export job owner"),
            Some(owner.to_string()),
            "an expired normal settlement must retain the runner lease for recovery"
        );
        assert_eq!(
            row.try_get_by::<i64, _>("observed_generation")
                .expect("decode observed generation"),
            1
        );
        assert_eq!(
            row.try_get_by::<i64, _>("processed_generation")
                .expect("decode processed generation"),
            0,
            "the final authorization rollback must undo the generation advance"
        );
    }

    /// A real independent SQLite exclusive lock may cancel only writer
    /// acquisition. Once the lock clears, the cancelled future must not wake
    /// and advance/release the export job behind the caller's back.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn advance_and_release_deadline_cancels_contended_writer_without_late_mutation() {
        let (_directory, conn, database_path) = file_job_db().await;
        conn.execute_unprepared("PRAGMA journal_mode = DELETE")
            .await
            .expect("force rollback-journal mode for export settlement deadline regression");
        let scope = scope();
        let (kind, sid, owner) = ("opencode", "settlement-writer-deadline", "runner");
        let IdleOutcome::Runner {
            fence_token,
            target_generation,
            ..
        } = observe_idle(&conn, kind, sid, &scope, owner, 1_000)
            .await
            .expect("acquire export runner before writer-lock settlement")
        else {
            panic!("fresh export job must elect this test as runner");
        };
        let job = ExportJobTarget::new(kind, sid, &scope);
        let holder = crate::internal::db::establish_connection_with_busy_timeout(
            database_path
                .to_str()
                .expect("export settlement lock-holder database path is utf-8"),
            Duration::from_secs(1),
        )
        .await
        .expect("open independent export settlement lock holder");
        let contender = crate::internal::db::establish_connection_with_busy_timeout(
            database_path
                .to_str()
                .expect("export settlement deadline contender database path is utf-8"),
            Duration::from_secs(1),
        )
        .await
        .expect("open independent export settlement deadline contender");
        holder
            .execute_unprepared("PRAGMA journal_mode = DELETE")
            .await
            .expect("force rollback-journal mode on export settlement lock holder");
        contender
            .execute_unprepared("PRAGMA journal_mode = DELETE")
            .await
            .expect("force rollback-journal mode on export settlement contender");
        let backend = conn.get_database_backend();
        holder
            .execute_raw(Statement::from_string(
                backend,
                "BEGIN EXCLUSIVE".to_string(),
            ))
            .await
            .expect("acquire exclusive export settlement SQLite lock");

        let started = Instant::now();
        let result = tokio::time::timeout(
            Duration::from_secs(2),
            advance_and_release_until(
                &contender,
                &job,
                owner,
                fence_token,
                target_generation,
                2_000,
                CaptureCommitDeadline::from_test_pair(
                    Instant::now() + Duration::from_millis(30),
                    i64::MAX,
                ),
            ),
        )
        .await
        .expect("contended export settlement writer acquisition must stop at its deadline");
        holder
            .execute_raw(Statement::from_string(backend, "ROLLBACK".to_string()))
            .await
            .expect("release exclusive export settlement SQLite lock");

        let error = result.expect_err("exclusive lock must exhaust export settlement deadline");
        assert!(
            format!("{error:#}").contains("managed execution deadline"),
            "unexpected export settlement writer-lock error: {error:#}"
        );
        assert!(
            started.elapsed() < Duration::from_secs(1),
            "export settlement writer acquisition must not wait for SQLite's busy timeout"
        );
        tokio::time::sleep(Duration::from_millis(100)).await;
        let row = conn
            .query_one_raw(Statement::from_sql_and_values(
                conn.get_database_backend(),
                "SELECT state, owner, processed_generation
                 FROM agent_export_job
                 WHERE agent_kind = ? AND provider_session_id = ?",
                [kind.into(), sid.into()],
            ))
            .await
            .expect("read export job after cancelled writer acquisition")
            .expect("export job remains after cancelled writer acquisition");
        assert_eq!(
            row.try_get_by::<String, _>("state")
                .expect("decode export job state"),
            "inflight",
            "a cancelled acquisition must not publish a delayed idle/dirty release"
        );
        assert_eq!(
            row.try_get_by::<Option<String>, _>("owner")
                .expect("decode export job owner"),
            Some(owner.to_string()),
            "a cancelled acquisition must retain the original runner lease"
        );
        assert_eq!(
            row.try_get_by::<i64, _>("processed_generation")
                .expect("decode processed generation"),
            0,
            "a cancelled acquisition must not advance the processed generation after lock release"
        );
    }

    /// A still-live monotonic primary paired with an expired SQLite boundary
    /// must stay expired during recovery. Fresh grace is only allowed after
    /// the primary monotonic deadline has elapsed, never to re-anchor final
    /// authorization for a live primary.
    #[tokio::test]
    async fn recovery_release_preserves_live_primary_sqlite_deadline() {
        let conn = job_db().await;
        let scope = scope();
        let (kind, sid, owner) = ("opencode", "recovery-final-deadline", "runner");
        let IdleOutcome::Runner { fence_token, .. } =
            observe_idle(&conn, kind, sid, &scope, owner, 1_000)
                .await
                .expect("acquire export runner before recovery final authorization")
        else {
            panic!("fresh export job must elect this test as runner");
        };
        let job = ExportJobTarget::new(kind, sid, &scope);

        let error = release_with_recovery_grace(
            &conn,
            &job,
            ExportLeaseRelease::new(owner, fence_token, "dirty", None, 2_000),
            CaptureCommitDeadline::from_test_pair(Instant::now() + Duration::from_secs(1), 0),
        )
        .await
        .expect_err("expired primary SQLite authorization must roll back recovery release");
        assert!(
            format!("{error:#}").contains("managed execution deadline"),
            "unexpected export recovery final-authorization error: {error:#}"
        );
        let row = conn
            .query_one_raw(Statement::from_sql_and_values(
                conn.get_database_backend(),
                "SELECT state, owner FROM agent_export_job
                 WHERE agent_kind = ? AND provider_session_id = ?",
                [kind.into(), sid.into()],
            ))
            .await
            .expect("read export job after recovery final-authorization rollback")
            .expect("export job remains after rolled-back recovery release");
        assert_eq!(
            row.try_get_by::<String, _>("state")
                .expect("decode export job state"),
            "inflight"
        );
        assert_eq!(
            row.try_get_by::<Option<String>, _>("owner")
                .expect("decode export job owner"),
            Some(owner.to_string()),
            "an expired final authorization must retain the owned lease"
        );
    }

    #[tokio::test]
    async fn recovery_release_refuses_idle_without_mutating_the_owned_lease() {
        let conn = job_db().await;
        let scope = scope();
        let (kind, sid, owner) = ("opencode", "recovery-idle-refusal", "runner");
        let IdleOutcome::Runner { fence_token, .. } =
            observe_idle(&conn, kind, sid, &scope, owner, 1_000)
                .await
                .expect("acquire export runner before idle-recovery rejection")
        else {
            panic!("fresh export job must elect this test as runner");
        };
        let job = ExportJobTarget::new(kind, sid, &scope);

        let error = release_with_recovery_grace(
            &conn,
            &job,
            ExportLeaseRelease::new(owner, fence_token, "idle", None, 2_000),
            CaptureCommitDeadline::from_test_pair(
                Instant::now() + Duration::from_secs(1),
                chrono::Utc::now().timestamp_millis().saturating_add(1_000),
            ),
        )
        .await
        .expect_err("recovery release must never publish idle");
        assert!(
            format!("{error:#}").contains("may not mark a job idle"),
            "unexpected recovery idle rejection: {error:#}"
        );

        let row = conn
            .query_one_raw(Statement::from_sql_and_values(
                conn.get_database_backend(),
                "SELECT state, owner, processed_generation FROM agent_export_job
                 WHERE agent_kind = ? AND provider_session_id = ?",
                [kind.into(), sid.into()],
            ))
            .await
            .expect("read export job after idle-recovery rejection")
            .expect("export job remains after idle-recovery rejection");
        assert_eq!(
            row.try_get_by::<String, _>("state")
                .expect("decode export job state"),
            "inflight"
        );
        assert_eq!(
            row.try_get_by::<Option<String>, _>("owner")
                .expect("decode export job owner"),
            Some(owner.to_string())
        );
        assert_eq!(
            row.try_get_by::<i64, _>("processed_generation")
                .expect("decode processed generation"),
            0
        );
    }

    /// opencode_export_inflight_generation_merges_third_idle: idles landing
    /// while a runner is inflight merge into `observed_generation`; the
    /// runner's advance reports MoreWork with the merged target.
    #[tokio::test]
    async fn inflight_generation_merges_later_idles() {
        let conn = job_db().await;
        let (kind, sid) = ("opencode", "s1");
        let scope = scope();
        let job = ExportJobTarget::new(kind, sid, &scope);

        let runner = observe_idle(&conn, kind, sid, &scope, "r1", 1_000)
            .await
            .unwrap();
        let IdleOutcome::Runner {
            fence_token,
            target_generation,
            ..
        } = runner
        else {
            panic!("first idle must become the runner");
        };
        assert_eq!(target_generation, 1);

        // Two more idles while inflight: recorded, not runners.
        assert_eq!(
            observe_idle(&conn, kind, sid, &scope, "r2", 2_000)
                .await
                .unwrap(),
            IdleOutcome::RecordedOnly
        );
        assert_eq!(
            observe_idle(&conn, kind, sid, &scope, "r3", 3_000)
                .await
                .unwrap(),
            IdleOutcome::RecordedOnly
        );

        // Runner finishes generation 1 → more work (target 3).
        let outcome = advance_processed(&conn, &job, "r1", fence_token, 1, 4_000)
            .await
            .unwrap();
        assert_eq!(
            outcome,
            AdvanceOutcome::MoreWork {
                target_generation: 3
            }
        );
        // Processes the merged batch → clean.
        let outcome = advance_processed(&conn, &job, "r1", fence_token, 3, 5_000)
            .await
            .unwrap();
        assert_eq!(outcome, AdvanceOutcome::Clean);
        release(&conn, &job, "r1", fence_token, "idle", None, 6_000)
            .await
            .unwrap();
    }

    #[tokio::test]
    async fn tombstone_blocks_export_job_creation_and_generation_advance() {
        let conn = job_db().await;
        let scope = scope();
        conn.execute_raw(Statement::from_string(
            conn.get_database_backend(),
            "INSERT INTO agent_import_tombstone (
                tombstone_id, agent_kind, provider_session_id, erased_session_id, erased_at
             ) VALUES ('t-export', 'opencode', 'erased', 'opencode__erased', 1)"
                .to_string(),
        ))
        .await
        .expect("seed tombstone");
        let error = observe_idle(&conn, "opencode", "erased", &scope, "runner", 1)
            .await
            .expect_err("erased export job must be blocked");
        assert!(error.to_string().contains("erased"));
        let row = conn
            .query_one_raw(Statement::from_string(
                conn.get_database_backend(),
                "SELECT COUNT(*) AS n FROM agent_export_job".to_string(),
            ))
            .await
            .unwrap()
            .unwrap();
        assert_eq!(row.try_get_by::<i64, _>("n").unwrap(), 0);
    }

    /// Codex M3 R1 P1-4: a delayed contender whose observed bump was already
    /// processed by another runner must NOT acquire and re-export a clean
    /// generation — acquisition requires pending work.
    #[tokio::test]
    async fn delayed_contender_cannot_reexport_clean_generation() {
        let conn = job_db().await;
        let (kind, sid) = ("opencode", "s1");
        let scope = scope();
        let job = ExportJobTarget::new(kind, sid, &scope);

        // A bumps (observed=1) but stalls before acquiring: simulate by
        // bumping WITHOUT holding the lease — B then bumps + runs + finishes.
        let IdleOutcome::Runner { fence_token, .. } =
            observe_idle(&conn, kind, sid, &scope, "b", 0)
                .await
                .unwrap()
        else {
            panic!("B becomes the runner");
        };
        // B processes everything observed so far and releases idle.
        assert_eq!(
            advance_processed(&conn, &job, "b", fence_token, 1, 1_000)
                .await
                .unwrap(),
            AdvanceOutcome::Clean
        );
        release(&conn, &job, "b", fence_token, "idle", None, 1_500)
            .await
            .unwrap();

        // A's delayed lease attempt (no new bump in between — emulate the
        // stalled path with a direct conditional acquisition): observe_idle
        // always bumps first, so instead assert the ACQUISITION predicate
        // directly: with processed == observed, the lease UPDATE matches no
        // row.
        let acquired = conn
            .execute_raw(Statement::from_sql_and_values(
                conn.get_database_backend(),
                "UPDATE agent_export_job
                 SET owner = 'a', lease_expires_at = 99999, state = 'inflight'
                 WHERE agent_kind = ? AND provider_session_id = ?
                   AND (owner IS NULL OR lease_expires_at IS NULL OR lease_expires_at <= 2000)
                   AND processed_generation < observed_generation",
                [kind.into(), sid.into()],
            ))
            .await
            .unwrap();
        assert_eq!(
            acquired.rows_affected(),
            0,
            "clean generation must not be re-acquirable"
        );

        // A fresh idle (new bump) re-enables acquisition normally.
        assert!(matches!(
            observe_idle(&conn, kind, sid, &scope, "a", 3_000)
                .await
                .unwrap(),
            IdleOutcome::Runner { .. }
        ));
    }

    /// A caller that does not need to append content still must preserve an
    /// idle observed during its run by releasing `dirty`, not `idle`.
    #[tokio::test]
    async fn advance_and_release_keeps_later_generation_dirty() {
        let conn = job_db().await;
        let (kind, sid) = ("opencode", "settle-dirty");
        let scope = scope();
        let job = ExportJobTarget::new(kind, sid, &scope);
        let IdleOutcome::Runner {
            fence_token,
            target_generation,
            ..
        } = observe_idle(&conn, kind, sid, &scope, "runner", 1_000)
            .await
            .unwrap()
        else {
            panic!("first idle must become the runner");
        };
        assert_eq!(
            observe_idle(&conn, kind, sid, &scope, "later", 2_000)
                .await
                .unwrap(),
            IdleOutcome::RecordedOnly
        );

        let outcome =
            advance_and_release(&conn, &job, "runner", fence_token, target_generation, 3_000)
                .await
                .unwrap();
        assert_eq!(
            outcome,
            AdvanceOutcome::MoreWork {
                target_generation: 2
            }
        );

        let row = conn
            .query_one_raw(Statement::from_sql_and_values(
                conn.get_database_backend(),
                "SELECT state, owner, observed_generation, processed_generation
                 FROM agent_export_job
                 WHERE agent_kind = ? AND provider_session_id = ?",
                [kind.into(), sid.into()],
            ))
            .await
            .unwrap()
            .expect("job row");
        assert_eq!(row.try_get_by::<String, _>("state").unwrap(), "dirty");
        assert_eq!(row.try_get_by::<Option<String>, _>("owner").unwrap(), None);
        assert_eq!(row.try_get_by::<i64, _>("observed_generation").unwrap(), 2);
        assert_eq!(row.try_get_by::<i64, _>("processed_generation").unwrap(), 1);
    }

    /// opencode_export_stale_owner_cannot_release_or_commit +
    /// opencode_export_inflight_ttl_takeover: an expired lease is taken over
    /// with a higher fence; the stale runner can neither advance nor release.
    #[tokio::test]
    async fn stale_owner_is_fenced_out_after_takeover() {
        let conn = job_db().await;
        let (kind, sid) = ("opencode", "s1");
        let scope = scope();
        let job = ExportJobTarget::new(kind, sid, &scope);

        let IdleOutcome::Runner {
            fence_token: stale_fence,
            ..
        } = observe_idle(&conn, kind, sid, &scope, "stale", 0)
            .await
            .unwrap()
        else {
            panic!("runner expected");
        };

        // Lease expires (EXPORT_LEASE_MS = 30s) → takeover at t=60s.
        let IdleOutcome::Runner {
            fence_token: fresh_fence,
            target_generation,
            ..
        } = observe_idle(&conn, kind, sid, &scope, "fresh", 60_000)
            .await
            .unwrap()
        else {
            panic!("takeover expected after lease expiry");
        };
        assert!(fresh_fence > stale_fence);
        assert_eq!(target_generation, 2);

        // Stale runner: advance and release are both fenced no-ops.
        assert_eq!(
            advance_processed(&conn, &job, "stale", stale_fence, 1, 61_000)
                .await
                .unwrap(),
            AdvanceOutcome::FencedOut
        );
        release(&conn, &job, "stale", stale_fence, "idle", None, 61_500)
            .await
            .unwrap(); // silent no-op
        let row = conn
            .query_one_raw(Statement::from_string(
                conn.get_database_backend(),
                "SELECT owner, state FROM agent_export_job".to_string(),
            ))
            .await
            .unwrap()
            .unwrap();
        let owner: Option<String> = row.try_get_by("owner").unwrap();
        let state: String = row.try_get_by("state").unwrap();
        assert_eq!(
            owner.as_deref(),
            Some("fresh"),
            "stale release must not strip the new owner"
        );
        assert_eq!(state, "inflight");

        // Fresh runner completes normally.
        assert_eq!(
            advance_processed(&conn, &job, "fresh", fresh_fence, 2, 62_000)
                .await
                .unwrap(),
            AdvanceOutcome::Clean
        );
    }

    /// opencode_export_max_loop_preserves_dirty + TTL scavenging: a runner
    /// that stops with observed > processed releases `dirty` (never falsely
    /// clean); expired rows are scavenged by TTL.
    #[tokio::test]
    async fn max_loop_release_stays_dirty_and_ttl_scavenges() {
        let conn = job_db().await;
        let (kind, sid) = ("opencode", "s1");
        let scope = scope();
        let job = ExportJobTarget::new(kind, sid, &scope);

        let IdleOutcome::Runner { fence_token, .. } =
            observe_idle(&conn, kind, sid, &scope, "r1", 0)
                .await
                .unwrap()
        else {
            panic!("runner expected");
        };
        // A new idle arrives; runner hits its loop bound and releases dirty.
        observe_idle(&conn, kind, sid, &scope, "other", 1_000)
            .await
            .unwrap();
        assert!(matches!(
            advance_processed(&conn, &job, "r1", fence_token, 1, 2_000)
                .await
                .unwrap(),
            AdvanceOutcome::MoreWork { .. }
        ));
        release(&conn, &job, "r1", fence_token, "dirty", None, 3_000)
            .await
            .unwrap();
        let row = conn
            .query_one_raw(Statement::from_string(
                conn.get_database_backend(),
                "SELECT state FROM agent_export_job".to_string(),
            ))
            .await
            .unwrap()
            .unwrap();
        let state: String = row.try_get_by("state").unwrap();
        assert_eq!(state, "dirty", "unfinished work must stay visibly dirty");

        // TTL scavenging removes the row once expired.
        assert_eq!(scavenge_expired(&conn, 3_000).await.unwrap(), 0);
        let far_future = 3_000 + 24 * 60 * 60 * 1_000 + 1;
        assert_eq!(scavenge_expired(&conn, far_future).await.unwrap(), 1);
    }

    /// W4: the historical global provider-session key is intentionally kept
    /// as a fail-closed collision boundary. A second workspace cannot bump or
    /// take over the first workspace's export generation.
    #[tokio::test]
    async fn provider_session_in_another_workspace_scope_is_rejected() {
        let conn = job_db().await;
        let first = scope();
        let second = CaptureScope {
            repo_id: first.repo_id.clone(),
            worktree_id: "linked-wt".to_string(),
            workspace_id: Some("workspace-b".to_string()),
            workspace_fence: Some(7),
        };
        assert!(matches!(
            observe_idle(&conn, "opencode", "same-provider", &first, "a", 1)
                .await
                .unwrap(),
            IdleOutcome::Runner { .. }
        ));
        let error = observe_idle(&conn, "opencode", "same-provider", &second, "b", 2)
            .await
            .expect_err("a second workspace must not reuse the provider session");
        assert!(
            error.to_string().contains("already claimed by another"),
            "cross-scope refusal must explain the ownership conflict: {error:#}"
        );
    }

    /// An export/import recovery row can survive after its catalog session is
    /// gone. It remains a provider-session ownership claim and must block a
    /// new capture from another scope just as a live session row would.
    #[tokio::test]
    async fn orphan_import_identity_in_another_scope_blocks_export_capture() {
        let conn = job_db().await;
        let scope = scope();
        conn.execute_raw(Statement::from_string(
            conn.get_database_backend(),
            "INSERT INTO agent_import_identity (
                identity_id, agent_kind, provider_session_id, source_kind, source_id,
                schema_version, next_ordinal, state, created_at, updated_at,
                repo_id, worktree_id, workspace_id, workspace_fence, scope_state
             ) VALUES ('foreign-import', 'opencode', 'orphan-provider', 'file', 'foreign',
                       1, 0, 'discovered', 1, 1,
                       'other-repo', 'linked-worktree', 'other-workspace', 9, 'scoped')"
                .to_string(),
        ))
        .await
        .expect("seed foreign scoped import identity");

        let error = observe_idle(&conn, "opencode", "orphan-provider", &scope, "owner", 1)
            .await
            .expect_err("orphan import identity must retain its provider claim");
        assert!(
            error.to_string().contains("already claimed by another"),
            "foreign orphan claim must be explained: {error:#}"
        );
    }
}
