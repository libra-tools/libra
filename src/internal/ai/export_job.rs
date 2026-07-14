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

use anyhow::{Context, Result, anyhow};
use sea_orm::{ConnectionTrait, DatabaseConnection, Statement};

/// Lease length for one export run: must cover the export subprocess
/// deadline (≤3s, GC-DR-04) plus parse/redact/claim with margin.
const EXPORT_LEASE_MS: i64 = 30_000;
/// Job-row TTL: quiet jobs are scavenged after a day.
const EXPORT_JOB_TTL_MS: i64 = 24 * 60 * 60 * 1_000;

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

fn now_or(now_ms: i64, delta: i64) -> i64 {
    now_ms.saturating_add(delta)
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
    owner: &str,
    now_ms: i64,
) -> Result<IdleOutcome> {
    // Ensure the row exists (first idle creates it).
    conn.execute(Statement::from_sql_and_values(
        conn.get_database_backend(),
        "INSERT INTO agent_export_job (
            job_id, agent_kind, provider_session_id, observed_generation,
            processed_generation, state, created_at, updated_at, ttl_expires_at
         ) VALUES (?, ?, ?, 0, 0, 'idle', ?, ?, ?)
         ON CONFLICT(agent_kind, provider_session_id) DO NOTHING",
        [
            uuid::Uuid::new_v4().to_string().into(),
            agent_kind.into(),
            provider_session_id.into(),
            now_ms.into(),
            now_ms.into(),
            now_or(now_ms, EXPORT_JOB_TTL_MS).into(),
        ],
    ))
    .await
    .context("insert agent_export_job row")?;

    // Unconditional observed bump — every idle counts exactly once.
    conn.execute(Statement::from_sql_and_values(
        conn.get_database_backend(),
        "UPDATE agent_export_job
         SET observed_generation = observed_generation + 1, updated_at = ?,
             ttl_expires_at = ?
         WHERE agent_kind = ? AND provider_session_id = ?",
        [
            now_ms.into(),
            now_or(now_ms, EXPORT_JOB_TTL_MS).into(),
            agent_kind.into(),
            provider_session_id.into(),
        ],
    ))
    .await
    .context("bump observed_generation")?;

    // Lease attempt: only when no live lease exists (expired or absent) AND
    // pending work remains (processed < observed) — a delayed contender whose
    // bump was already processed by another runner must NOT re-export a
    // clean generation (Codex M3 R1 P1-4).
    let new_fence_seed = now_ms; // any monotonic-ish base; real fence below
    let _ = new_fence_seed;
    let acquired = conn
        .execute(Statement::from_sql_and_values(
            conn.get_database_backend(),
            "UPDATE agent_export_job
             SET owner = ?, lease_expires_at = ?,
                 fence_token = COALESCE(fence_token, 0) + 1,
                 state = 'inflight', updated_at = ?
             WHERE agent_kind = ? AND provider_session_id = ?
               AND (owner IS NULL OR lease_expires_at IS NULL OR lease_expires_at <= ?)
               AND processed_generation < observed_generation",
            [
                owner.into(),
                now_or(now_ms, EXPORT_LEASE_MS).into(),
                now_ms.into(),
                agent_kind.into(),
                provider_session_id.into(),
                now_ms.into(),
            ],
        ))
        .await
        .context("acquire export lease")?;
    if acquired.rows_affected() != 1 {
        return Ok(IdleOutcome::RecordedOnly);
    }

    let row = conn
        .query_one(Statement::from_sql_and_values(
            conn.get_database_backend(),
            "SELECT job_id, fence_token, observed_generation FROM agent_export_job
             WHERE agent_kind = ? AND provider_session_id = ? AND owner = ?",
            [agent_kind.into(), provider_session_id.into(), owner.into()],
        ))
        .await
        .context("read acquired export job")?
        .ok_or_else(|| anyhow!("export job vanished after lease acquisition"))?;
    Ok(IdleOutcome::Runner {
        job_id: row.try_get_by("job_id")?,
        fence_token: row
            .try_get_by::<Option<i64>, _>("fence_token")?
            .unwrap_or(0),
        target_generation: row.try_get_by("observed_generation")?,
    })
}

/// Advance `processed_generation` to `target` under owner+fence. Returns
/// whether more work arrived meanwhile (`observed > processed`): the runner
/// loops (within its deadline) or releases dirty. Zero rows = fenced out —
/// the stale runner must stop without touching anything else.
pub async fn advance_processed(
    conn: &DatabaseConnection,
    agent_kind: &str,
    provider_session_id: &str,
    owner: &str,
    fence_token: i64,
    target: i64,
    now_ms: i64,
) -> Result<AdvanceOutcome> {
    let advanced = conn
        .execute(Statement::from_sql_and_values(
            conn.get_database_backend(),
            "UPDATE agent_export_job
             SET processed_generation = ?, updated_at = ?
             WHERE agent_kind = ? AND provider_session_id = ?
               AND owner = ? AND fence_token = ?
               AND processed_generation < ?",
            [
                target.into(),
                now_ms.into(),
                agent_kind.into(),
                provider_session_id.into(),
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
        .query_one(Statement::from_sql_and_values(
            conn.get_database_backend(),
            "SELECT observed_generation, processed_generation FROM agent_export_job
             WHERE agent_kind = ? AND provider_session_id = ?",
            [agent_kind.into(), provider_session_id.into()],
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
    agent_kind: &str,
    provider_session_id: &str,
    owner: &str,
    fence_token: i64,
    target: i64,
    now_ms: i64,
) -> Result<AdvanceOutcome> {
    let outcome = advance_processed(
        conn,
        agent_kind,
        provider_session_id,
        owner,
        fence_token,
        target,
        now_ms,
    )
    .await?;
    let state = match outcome {
        AdvanceOutcome::Clean => Some("idle"),
        AdvanceOutcome::MoreWork { .. } => Some("dirty"),
        AdvanceOutcome::FencedOut => None,
    };
    if let Some(state) = state {
        release(
            conn,
            agent_kind,
            provider_session_id,
            owner,
            fence_token,
            state,
            None,
            now_ms,
        )
        .await?;
    }
    Ok(outcome)
}

/// Release the lease under owner+fence, marking the terminal state honestly:
/// `dirty` when work remains, `failed` with a stable code, else `idle`. A
/// fenced-out release is a silent no-op (the new owner's state wins).
#[allow(clippy::too_many_arguments)]
pub async fn release(
    conn: &DatabaseConnection,
    agent_kind: &str,
    provider_session_id: &str,
    owner: &str,
    fence_token: i64,
    state: &str,
    last_error_code: Option<&str>,
    now_ms: i64,
) -> Result<()> {
    debug_assert!(matches!(state, "idle" | "dirty" | "failed"));
    conn.execute(Statement::from_sql_and_values(
        conn.get_database_backend(),
        "UPDATE agent_export_job
         SET owner = NULL, lease_expires_at = NULL, state = ?,
             last_error_code = ?, updated_at = ?
         WHERE agent_kind = ? AND provider_session_id = ?
           AND owner = ? AND fence_token = ?",
        [
            state.into(),
            last_error_code.into(),
            now_ms.into(),
            agent_kind.into(),
            provider_session_id.into(),
            owner.into(),
            fence_token.into(),
        ],
    ))
    .await
    .context("release export lease")?;
    Ok(())
}

/// Delete expired job rows (TTL scavenging — clean --gc / retention /
/// startup). Bounded by the TTL index.
pub async fn scavenge_expired(conn: &DatabaseConnection, now_ms: i64) -> Result<u64> {
    let result = conn
        .execute(Statement::from_sql_and_values(
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
    use sea_orm::Database;

    use super::*;
    use crate::internal::db::migration::run_builtin_migrations;

    async fn job_db() -> DatabaseConnection {
        let conn = Database::connect("sqlite::memory:").await.expect("mem db");
        conn.execute(Statement::from_string(
            conn.get_database_backend(),
            "PRAGMA foreign_keys = OFF".to_string(),
        ))
        .await
        .expect("pragma");
        run_builtin_migrations(&conn).await.expect("migrations");
        conn
    }

    /// opencode_export_inflight_generation_merges_third_idle: idles landing
    /// while a runner is inflight merge into `observed_generation`; the
    /// runner's advance reports MoreWork with the merged target.
    #[tokio::test]
    async fn inflight_generation_merges_later_idles() {
        let conn = job_db().await;
        let (kind, sid) = ("opencode", "s1");

        let runner = observe_idle(&conn, kind, sid, "r1", 1_000).await.unwrap();
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
            observe_idle(&conn, kind, sid, "r2", 2_000).await.unwrap(),
            IdleOutcome::RecordedOnly
        );
        assert_eq!(
            observe_idle(&conn, kind, sid, "r3", 3_000).await.unwrap(),
            IdleOutcome::RecordedOnly
        );

        // Runner finishes generation 1 → more work (target 3).
        let outcome = advance_processed(&conn, kind, sid, "r1", fence_token, 1, 4_000)
            .await
            .unwrap();
        assert_eq!(
            outcome,
            AdvanceOutcome::MoreWork {
                target_generation: 3
            }
        );
        // Processes the merged batch → clean.
        let outcome = advance_processed(&conn, kind, sid, "r1", fence_token, 3, 5_000)
            .await
            .unwrap();
        assert_eq!(outcome, AdvanceOutcome::Clean);
        release(&conn, kind, sid, "r1", fence_token, "idle", None, 6_000)
            .await
            .unwrap();
    }

    /// Codex M3 R1 P1-4: a delayed contender whose observed bump was already
    /// processed by another runner must NOT acquire and re-export a clean
    /// generation — acquisition requires pending work.
    #[tokio::test]
    async fn delayed_contender_cannot_reexport_clean_generation() {
        let conn = job_db().await;
        let (kind, sid) = ("opencode", "s1");

        // A bumps (observed=1) but stalls before acquiring: simulate by
        // bumping WITHOUT holding the lease — B then bumps + runs + finishes.
        let IdleOutcome::Runner { fence_token, .. } =
            observe_idle(&conn, kind, sid, "b", 0).await.unwrap()
        else {
            panic!("B becomes the runner");
        };
        // B processes everything observed so far and releases idle.
        assert_eq!(
            advance_processed(&conn, kind, sid, "b", fence_token, 1, 1_000)
                .await
                .unwrap(),
            AdvanceOutcome::Clean
        );
        release(&conn, kind, sid, "b", fence_token, "idle", None, 1_500)
            .await
            .unwrap();

        // A's delayed lease attempt (no new bump in between — emulate the
        // stalled path with a direct conditional acquisition): observe_idle
        // always bumps first, so instead assert the ACQUISITION predicate
        // directly: with processed == observed, the lease UPDATE matches no
        // row.
        let acquired = conn
            .execute(Statement::from_sql_and_values(
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
            observe_idle(&conn, kind, sid, "a", 3_000).await.unwrap(),
            IdleOutcome::Runner { .. }
        ));
    }

    /// A caller that does not need to append content still must preserve an
    /// idle observed during its run by releasing `dirty`, not `idle`.
    #[tokio::test]
    async fn advance_and_release_keeps_later_generation_dirty() {
        let conn = job_db().await;
        let (kind, sid) = ("opencode", "settle-dirty");
        let IdleOutcome::Runner {
            fence_token,
            target_generation,
            ..
        } = observe_idle(&conn, kind, sid, "runner", 1_000)
            .await
            .unwrap()
        else {
            panic!("first idle must become the runner");
        };
        assert_eq!(
            observe_idle(&conn, kind, sid, "later", 2_000)
                .await
                .unwrap(),
            IdleOutcome::RecordedOnly
        );

        let outcome = advance_and_release(
            &conn,
            kind,
            sid,
            "runner",
            fence_token,
            target_generation,
            3_000,
        )
        .await
        .unwrap();
        assert_eq!(
            outcome,
            AdvanceOutcome::MoreWork {
                target_generation: 2
            }
        );

        let row = conn
            .query_one(Statement::from_sql_and_values(
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

        let IdleOutcome::Runner {
            fence_token: stale_fence,
            ..
        } = observe_idle(&conn, kind, sid, "stale", 0).await.unwrap()
        else {
            panic!("runner expected");
        };

        // Lease expires (EXPORT_LEASE_MS = 30s) → takeover at t=60s.
        let IdleOutcome::Runner {
            fence_token: fresh_fence,
            target_generation,
            ..
        } = observe_idle(&conn, kind, sid, "fresh", 60_000)
            .await
            .unwrap()
        else {
            panic!("takeover expected after lease expiry");
        };
        assert!(fresh_fence > stale_fence);
        assert_eq!(target_generation, 2);

        // Stale runner: advance and release are both fenced no-ops.
        assert_eq!(
            advance_processed(&conn, kind, sid, "stale", stale_fence, 1, 61_000)
                .await
                .unwrap(),
            AdvanceOutcome::FencedOut
        );
        release(&conn, kind, sid, "stale", stale_fence, "idle", None, 61_500)
            .await
            .unwrap(); // silent no-op
        let row = conn
            .query_one(Statement::from_string(
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
            advance_processed(&conn, kind, sid, "fresh", fresh_fence, 2, 62_000)
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

        let IdleOutcome::Runner { fence_token, .. } =
            observe_idle(&conn, kind, sid, "r1", 0).await.unwrap()
        else {
            panic!("runner expected");
        };
        // A new idle arrives; runner hits its loop bound and releases dirty.
        observe_idle(&conn, kind, sid, "other", 1_000)
            .await
            .unwrap();
        assert!(matches!(
            advance_processed(&conn, kind, sid, "r1", fence_token, 1, 2_000)
                .await
                .unwrap(),
            AdvanceOutcome::MoreWork { .. }
        ));
        release(&conn, kind, sid, "r1", fence_token, "dirty", None, 3_000)
            .await
            .unwrap();
        let row = conn
            .query_one(Statement::from_string(
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
}
