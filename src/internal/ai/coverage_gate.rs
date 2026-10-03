//! Per-turn coverage claim gate (plan-20260713 DR-05c-0, ADR-DR-09/10/16).
//!
//! `agent_coverage_claim` is the WRITE-FRONT idempotence gate: a checkpoint
//! writer reserves the logical turns it is about to cover *before* building
//! objects, and commits the claims (revision insert + claim advance + catalog
//! row) inside the SAME SQLite transaction as the traces ref CAS — so a
//! repeated TurnEnd, a crash retry, or a concurrent writer can never produce
//! a second visible checkpoint for an already-covered turn.
//!
//! Arbitration (ADR-DR-09) is embodied in [`reserve_live_turn_claims`]:
//! equivalent committed content → no-op; committed `incomplete` upgraded by
//! new `complete` content → revision advance; committed `complete` vs a
//! different `complete` digest → `conflicted` (doctor's job, never silent
//! overwrite); an unexpired foreign reservation → skip (someone else is
//! writing this turn); an expired one → fenced takeover. Every mutation is a
//! conditional write checked via `rows_affected == 1` — losers re-read, they
//! never assume.
//!
//! Failure policy (ADR-DR-10): any DB error here fails the checkpoint write
//! *closed* — the caller must not append to `refs/libra/traces` without a
//! reservation, and a commit-time fence mismatch rolls the whole final
//! transaction back (ref update included). A transient SQLite writer-lock
//! race is reported as an in-flight skip so the caller emits the same
//! replayable diagnostic as an explicit foreign reservation.

use std::time::{Duration, Instant};

use anyhow::{Context, Result, bail};
use async_trait::async_trait;
use sea_orm::{ConnectionTrait, DatabaseConnection, DatabaseTransaction, Statement};

#[cfg(test)]
pub(crate) use crate::internal::ai::capture::catalog::CaptureImportSessionLifecycleState as ImportSessionLifecycleState;
use crate::internal::ai::{
    capture::{
        catalog::{CaptureCatalogStore, CaptureImportSessionCommit},
        checkpoint::CheckpointCatalogMutation,
    },
    capture_scope::{
        CaptureCommitDeadline, CaptureFinalCommitAuthorizationError, CaptureScope,
        authorize_final_capture_commit,
    },
    history::{CheckpointScope, TracesCommitCtx, TracesTxnExtra},
    observed_agents::{
        COVERAGE_SCHEMA_VERSION, Completeness, NormalizedTurn, canonical_turn_bytes,
        redact_turns_with_report,
    },
};

/// Live reservation lease length. Generous relative to a single hook write
/// (sub-second) so takeover only fires on genuinely dead writers.
const LIVE_LEASE_MS: i64 = 60_000;

/// Lease sentinel for a `reserved_live` claim whose turns belong to a durable
/// pending terminal artifact (ADR-ACF-09a "Write trigger"). The artifact, not
/// a process, owns these claims: its replay commits them under the original
/// owner/fence, and only that commit or an explicit session erase releases
/// them. No live, export, or import writer may take them over on lease expiry.
pub(crate) const ARTIFACT_RETAINED_LEASE_MS: i64 = i64::MAX;

/// A failed managed capture may only spend this short, paired window releasing
/// an existing owned reservation. This is recovery evidence cleanup, never an
/// allowance to begin a new coverage reservation after the foreground budget.
const COVERAGE_ABANDONMENT_RECOVERY_GRACE: Duration = Duration::from_millis(250);

// Batch-delay injection is useful for proving that a historical import checks
// its total deadline between SQLite transactions. It must not be a hook
// environment knob: external integration binaries compile this module without
// `cfg(test)`, so only in-process unit tests can opt into the delay.
#[cfg(test)]
tokio::task_local! {
    static TEST_IMPORT_RESERVATION_BATCH_DELAY: Option<std::time::Duration>;
    static TEST_LIVE_RESERVATION_TURN_DELAY: Option<std::time::Duration>;
}

#[cfg(test)]
pub(crate) async fn with_import_reservation_batch_delay<F>(
    delay: std::time::Duration,
    future: F,
) -> F::Output
where
    F: std::future::Future,
{
    TEST_IMPORT_RESERVATION_BATCH_DELAY
        .scope(Some(delay), future)
        .await
}

#[cfg(test)]
fn import_reservation_batch_test_delay() -> Option<std::time::Duration> {
    TEST_IMPORT_RESERVATION_BATCH_DELAY
        .try_with(|delay| *delay)
        .ok()
        .flatten()
}

/// Test-only delay after each live reservation decision. It exercises the
/// rollback path when a single all-or-nothing live transaction outlives its
/// capture deadline; production has no delay/configuration seam here.
#[cfg(test)]
pub(crate) async fn with_live_reservation_turn_delay<F>(
    delay: std::time::Duration,
    future: F,
) -> F::Output
where
    F: std::future::Future,
{
    TEST_LIVE_RESERVATION_TURN_DELAY
        .scope(Some(delay), future)
        .await
}

#[cfg(test)]
fn live_reservation_turn_test_delay() -> Option<std::time::Duration> {
    TEST_LIVE_RESERVATION_TURN_DELAY
        .try_with(|delay| *delay)
        .ok()
        .flatten()
}

/// One reserved turn: the writer holds `(owner, fence_token)` and must present
/// both at commit time.
#[derive(Debug, Clone)]
pub struct ReservedTurnClaim {
    pub logical_turn_key: String,
    pub coverage_digest: String,
    pub completeness: Completeness,
    pub fence_token: i64,
    pub next_revision: i64,
}

/// Verify retained live reservations before an authenticated recovery artifact
/// takes responsibility for them. Lease expiry alone does not revoke a token;
/// takeover or abandonment advances its fence and must reject this writer.
pub(crate) async fn verify_reserved_live_claims_with_conn<C: ConnectionTrait>(
    conn: &C,
    scope: &CaptureScope,
    session_id: &str,
    owner: &str,
    claims: &[ReservedTurnClaim],
) -> Result<()> {
    scope.assert_workspace_fence_live(conn).await?;
    for claim in claims {
        let owned = conn
            .query_one_raw(Statement::from_sql_and_values(
                conn.get_database_backend(),
                "SELECT 1 FROM agent_coverage_claim
             WHERE session_id = ? AND logical_turn_key = ? AND coverage_schema_version = ?
               AND state = 'reserved_live' AND owner = ? AND fence_token = ?
               AND revision = ? LIMIT 1",
                [
                    session_id.into(),
                    claim.logical_turn_key.clone().into(),
                    COVERAGE_SCHEMA_VERSION.into(),
                    owner.into(),
                    claim.fence_token.into(),
                    (claim.next_revision - 1).into(),
                ],
            ))
            .await
            .context("cannot verify retained capture coverage claims; run `libra agent doctor`")?;
        if owned.is_none() {
            bail!(
                "capture recovery coverage ownership changed; retry through the current provider session"
            );
        }
    }
    Ok(())
}

/// Hand verified live reservations to the durable artifact that is being
/// persisted in this same writer transaction. The same owner/fence/revision
/// guards as [`verify_reserved_live_claims_with_conn`] apply; the lease
/// becomes [`ARTIFACT_RETAINED_LEASE_MS`] so an ordinary lease expiry can no
/// longer revoke the artifact's replay fence.
pub(crate) async fn retain_reserved_live_claims_for_artifact_with_conn<C: ConnectionTrait>(
    conn: &C,
    session_id: &str,
    owner: &str,
    claims: &[ReservedTurnClaim],
) -> Result<()> {
    for claim in claims {
        let retained = conn
            .execute_raw(Statement::from_sql_and_values(
                conn.get_database_backend(),
                "UPDATE agent_coverage_claim SET lease_expires_at = ?
                 WHERE session_id = ? AND logical_turn_key = ? AND coverage_schema_version = ?
                   AND state = 'reserved_live' AND owner = ? AND fence_token = ?
                   AND revision = ?",
                [
                    ARTIFACT_RETAINED_LEASE_MS.into(),
                    session_id.into(),
                    claim.logical_turn_key.clone().into(),
                    COVERAGE_SCHEMA_VERSION.into(),
                    owner.into(),
                    claim.fence_token.into(),
                    (claim.next_revision - 1).into(),
                ],
            ))
            .await
            .context("cannot retain capture coverage claims; run `libra agent doctor`")?;
        if retained.rows_affected() != 1 {
            bail!(
                "capture recovery coverage ownership changed; retry through the current provider session"
            );
        }
    }
    Ok(())
}

/// Outcome of a live reservation pass over one snapshot's normalized turns.
#[derive(Debug, Default)]
pub struct LiveReservationOutcome {
    pub reserved: Vec<ReservedTurnClaim>,
    /// Turns already covered by equivalent-or-better committed content.
    pub skipped_covered: usize,
    /// Turns currently reserved by another live writer (unexpired lease).
    pub skipped_inflight: usize,
    /// Subset of `skipped_inflight` held by a durable pending terminal
    /// artifact rather than a running writer. They stay "in flight" for
    /// export/import retry decisions, but no lease expiry will release them.
    pub skipped_retained: usize,
    /// Turns whose committed `complete` content differs from this snapshot's
    /// `complete` content — flagged `conflicted` for doctor, never rewritten.
    pub conflicted: usize,
}

impl LiveReservationOutcome {
    /// Nothing to write: every turn is covered / in flight / conflicted.
    pub fn is_noop(&self) -> bool {
        self.reserved.is_empty()
    }

    /// Codex M3 R2 P1-2: this pass reserved nothing to append, yet at least
    /// one turn is held by another LIVE writer (unexpired lease). The export
    /// job must then be released `dirty` (retryable) rather than advanced
    /// clean — if that writer crashes, its claim lease expires and only a
    /// dirty job lets a later idle recapture the transcript. A genuine
    /// all-covered no-op (nothing in flight) is NOT this case.
    pub fn is_inflight_only_skip(&self) -> bool {
        self.reserved.is_empty() && self.skipped_inflight > 0
    }
}

struct ExistingClaim {
    coverage_digest: String,
    completeness: String,
    revision: i64,
    state: String,
    lease_expires_at: Option<i64>,
    fence_token: Option<i64>,
    checkpoint_id: Option<String>,
}

async fn read_claim(
    conn: &impl ConnectionTrait,
    session_id: &str,
    logical_turn_key: &str,
) -> Result<Option<ExistingClaim>> {
    let row = conn
        .query_one_raw(Statement::from_sql_and_values(
            conn.get_database_backend(),
            "SELECT coverage_digest, completeness, revision, state,
                    lease_expires_at, fence_token, checkpoint_id
             FROM agent_coverage_claim
             WHERE session_id = ? AND logical_turn_key = ?
               AND coverage_schema_version = ?",
            [
                session_id.into(),
                logical_turn_key.into(),
                COVERAGE_SCHEMA_VERSION.into(),
            ],
        ))
        .await
        .context("query agent_coverage_claim")?;
    let Some(row) = row else {
        return Ok(None);
    };
    Ok(Some(ExistingClaim {
        coverage_digest: row.try_get_by("coverage_digest")?,
        completeness: row.try_get_by("completeness")?,
        revision: row.try_get_by("revision")?,
        state: row.try_get_by("state")?,
        lease_expires_at: row.try_get_by("lease_expires_at")?,
        fence_token: row.try_get_by("fence_token")?,
        checkpoint_id: row.try_get_by("checkpoint_id")?,
    }))
}

fn lease_deadline(now_ms: i64) -> Result<i64> {
    now_ms
        .checked_add(LIVE_LEASE_MS)
        .context("coverage reservation lease timestamp overflow")
}

fn next_revision(revision: i64) -> Result<i64> {
    revision
        .checked_add(1)
        .context("coverage claim revision overflow")
}

/// Insert a brand-new `reserved_live` claim. Returns the reservation, or
/// `None` when a concurrent writer won the INSERT race (unique violation) —
/// the caller re-reads and re-decides.
async fn try_insert_fresh_claim(
    conn: &impl ConnectionTrait,
    session_id: &str,
    turn: &NormalizedTurn,
    digest: &str,
    owner: &str,
    now_ms: i64,
    source_channel: &'static str,
) -> Result<Option<ReservedTurnClaim>> {
    let lease_expires_at = lease_deadline(now_ms)?;
    let reservation_state = reservation_state(source_channel)?;
    let result = conn
        .execute_raw(Statement::from_sql_and_values(
            conn.get_database_backend(),
            "INSERT INTO agent_coverage_claim (
                session_id, logical_turn_key, coverage_schema_version,
                coverage_digest, completeness, revision, state,
                owner, lease_expires_at, fence_token, source_channel,
                created_at, updated_at
             ) VALUES (?, ?, ?, ?, ?, 0, ?, ?, ?, 1, ?, ?, ?)
             ON CONFLICT(session_id, logical_turn_key, coverage_schema_version)
             DO NOTHING",
            [
                session_id.into(),
                turn.logical_turn_key.clone().into(),
                COVERAGE_SCHEMA_VERSION.into(),
                digest.into(),
                turn.completeness.as_db_str().into(),
                reservation_state.into(),
                owner.into(),
                lease_expires_at.into(),
                source_channel.into(),
                now_ms.into(),
                now_ms.into(),
            ],
        ))
        .await
        .context("insert agent_coverage_claim reservation")?;
    if result.rows_affected() == 1 {
        Ok(Some(ReservedTurnClaim {
            logical_turn_key: turn.logical_turn_key.clone(),
            coverage_digest: digest.to_string(),
            completeness: turn.completeness,
            fence_token: 1,
            next_revision: 1,
        }))
    } else {
        Ok(None)
    }
}

/// Conditionally re-own an existing claim row (upgrade / takeover /
/// re-reserve). All prior identifying fields are in the WHERE so a concurrent
/// mutation makes this a 0-row no-op the caller re-reads after.
#[allow(clippy::too_many_arguments)]
async fn try_reown_claim(
    conn: &impl ConnectionTrait,
    session_id: &str,
    logical_turn_key: &str,
    expected_state: &str,
    expected_fence: Option<i64>,
    new_digest: &str,
    new_completeness: Completeness,
    owner: &str,
    now_ms: i64,
    source_channel: &'static str,
) -> Result<Option<(i64, i64)>> {
    let new_fence = expected_fence
        .unwrap_or(0)
        .checked_add(1)
        .context("coverage claim fence token overflow")?;
    let lease_expires_at = lease_deadline(now_ms)?;
    let expected_fence_value: sea_orm::Value = expected_fence.into();
    let reservation_state = reservation_state(source_channel)?;
    let result = conn
        .execute_raw(Statement::from_sql_and_values(
            conn.get_database_backend(),
            "UPDATE agent_coverage_claim
             SET state = ?, owner = ?, lease_expires_at = ?,
                 fence_token = ?, coverage_digest = ?, completeness = ?,
                 source_channel = ?, updated_at = ?
             WHERE session_id = ? AND logical_turn_key = ?
               AND coverage_schema_version = ?
               AND state = ? AND fence_token IS ?",
            [
                reservation_state.into(),
                owner.into(),
                lease_expires_at.into(),
                new_fence.into(),
                new_digest.into(),
                new_completeness.as_db_str().into(),
                source_channel.into(),
                now_ms.into(),
                session_id.into(),
                logical_turn_key.into(),
                COVERAGE_SCHEMA_VERSION.into(),
                expected_state.into(),
                expected_fence_value,
            ],
        ))
        .await
        .context("re-own agent_coverage_claim")?;
    if result.rows_affected() == 1 {
        Ok(Some((new_fence, lease_expires_at)))
    } else {
        Ok(None)
    }
}

fn reservation_state(source_channel: &str) -> Result<&'static str> {
    match source_channel {
        "import" => Ok("reserved_import"),
        "live" | "export" => Ok("reserved_live"),
        other => bail!("unsupported coverage source channel '{other}'"),
    }
}

/// Mark a committed-complete-vs-different-complete collision `conflicted`
/// (ADR-DR-09: never silently overwrite committed complete content).
async fn try_mark_conflicted(
    conn: &impl ConnectionTrait,
    session_id: &str,
    logical_turn_key: &str,
    incumbent: &ExistingClaim,
    incoming_turn: &NormalizedTurn,
    incoming_source_channel: &'static str,
    now_ms: i64,
) -> Result<ConflictMarkOutcome> {
    let mut sanitized_turn = incoming_turn.clone();
    let redaction_report = redact_turns_with_report(std::slice::from_mut(&mut sanitized_turn));
    let incoming_digest = sanitized_turn.digest_hex();
    if incoming_digest == incumbent.coverage_digest {
        return Ok(ConflictMarkOutcome::EquivalentAfterRedaction);
    }
    let incoming_canonical_json = String::from_utf8(canonical_turn_bytes(&sanitized_turn.records))
        .context("coverage conflict canonical evidence is not UTF-8")?;
    let incoming_redaction_report_json = serde_json::to_string(&redaction_report)
        .context("serialize coverage conflict redaction report")?;
    let result = conn
        .execute_raw(Statement::from_sql_and_values(
            conn.get_database_backend(),
            "UPDATE agent_coverage_claim
         SET state = 'conflicted', updated_at = ?
         WHERE session_id = ? AND logical_turn_key = ?
           AND coverage_schema_version = ?
           AND state = 'catalog_committed' AND coverage_digest = ?",
            [
                now_ms.into(),
                session_id.into(),
                logical_turn_key.into(),
                COVERAGE_SCHEMA_VERSION.into(),
                incumbent.coverage_digest.clone().into(),
            ],
        ))
        .await
        .context("mark agent_coverage_claim conflicted")?;
    if result.rows_affected() != 1 {
        return Ok(ConflictMarkOutcome::LostRace);
    }
    conn.execute_raw(Statement::from_sql_and_values(
        conn.get_database_backend(),
        "INSERT INTO agent_coverage_conflict (
            session_id, logical_turn_key, coverage_schema_version,
            incumbent_revision, incumbent_digest, incumbent_checkpoint_id,
            incoming_digest, incoming_source_channel, incoming_observed_at,
            incoming_canonical_json, incoming_redaction_report_json
         ) VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)
         ON CONFLICT(session_id, logical_turn_key, coverage_schema_version) DO NOTHING",
        [
            session_id.into(),
            logical_turn_key.into(),
            COVERAGE_SCHEMA_VERSION.into(),
            incumbent.revision.into(),
            incumbent.coverage_digest.clone().into(),
            incumbent.checkpoint_id.clone().into(),
            incoming_digest.into(),
            incoming_source_channel.into(),
            now_ms.into(),
            incoming_canonical_json.into(),
            incoming_redaction_report_json.into(),
        ],
    ))
    .await
    .context("persist sanitized coverage conflict challenger")?;
    Ok(ConflictMarkOutcome::Marked)
}

enum ConflictMarkOutcome {
    Marked,
    EquivalentAfterRedaction,
    LostRace,
}

/// Reserve the turns of one live snapshot (ADR-DR-09 arbitration). Bounded:
/// each turn takes at most two decision rounds (initial read + one re-read
/// after losing a conditional write race).
pub async fn reserve_live_turn_claims(
    conn: &DatabaseConnection,
    session_id: &str,
    turns: &[NormalizedTurn],
    owner: &str,
    now_ms: i64,
) -> Result<LiveReservationOutcome> {
    let scope = CaptureScope::main_for_connection(conn).await?;
    reserve_live_turn_claims_with_capture_scope(conn, &scope, session_id, turns, owner, now_ms)
        .await
}

/// Scope-aware live reservation. Production callers must carry the scope
/// resolved at ingress so a released/fenced workspace cannot claim coverage
/// after it has lost ownership.
pub async fn reserve_live_turn_claims_with_capture_scope(
    conn: &DatabaseConnection,
    scope: &CaptureScope,
    session_id: &str,
    turns: &[NormalizedTurn],
    owner: &str,
    now_ms: i64,
) -> Result<LiveReservationOutcome> {
    reserve_turn_claims_for_channel_with_capture_scope(
        conn, scope, session_id, turns, owner, now_ms, "live",
    )
    .await
}

/// Deadline-aware live reservation for managed hooks. Unlike historical
/// import, live capture must retain its one-transaction, all-or-nothing
/// claim plan; the deadline is therefore propagated through that single
/// transaction rather than splitting it into batches.
pub(crate) async fn reserve_live_turn_claims_until(
    conn: &DatabaseConnection,
    scope: &CaptureScope,
    session_id: &str,
    turns: &[NormalizedTurn],
    owner: &str,
    now_ms: i64,
    deadline: CaptureCommitDeadline,
) -> Result<LiveReservationOutcome> {
    reserve_turn_claims_for_channel_with_capture_scope_until(
        conn,
        scope,
        session_id,
        turns,
        owner,
        now_ms,
        CaptureReservationExecution::live(deadline),
    )
    .await
}

/// The fixed capture channel and paired deadline that authorize one
/// deadline-aware reservation. Keeping these facts together prevents an
/// export path from accidentally applying the hook channel or a fresh budget.
#[derive(Clone, Copy)]
pub(crate) struct CaptureReservationExecution {
    source_channel: &'static str,
    deadline: CaptureCommitDeadline,
}

impl CaptureReservationExecution {
    pub(crate) const fn live(deadline: CaptureCommitDeadline) -> Self {
        Self {
            source_channel: "live",
            deadline,
        }
    }

    pub(crate) const fn export(deadline: CaptureCommitDeadline) -> Self {
        Self {
            source_channel: "export",
            deadline,
        }
    }
}

/// Deadline-aware scoped reservation with explicit provenance. This is the
/// managed-export counterpart to [`reserve_live_turn_claims_until`]: it
/// preserves the `export` channel while passing the established capture pair
/// through writer acquisition, pure reads, final authorization, and the
/// non-cancellable commit boundary.
pub(crate) async fn reserve_turn_claims_for_channel_with_capture_scope_until(
    conn: &DatabaseConnection,
    scope: &CaptureScope,
    session_id: &str,
    turns: &[NormalizedTurn],
    owner: &str,
    now_ms: i64,
    execution: CaptureReservationExecution,
) -> Result<LiveReservationOutcome> {
    ensure_reservation_deadline(Some(execution.deadline), execution.source_channel)?;
    let outcome = reserve_turn_claims_for_channel_inner(
        conn,
        scope,
        session_id,
        turns,
        owner,
        now_ms,
        execution.source_channel,
        Some(execution.deadline),
    )
    .await;
    match outcome {
        Err(error) if is_sqlite_busy(&error) => Ok(LiveReservationOutcome {
            skipped_inflight: turns.len(),
            ..LiveReservationOutcome::default()
        }),
        outcome => outcome,
    }
}

/// [`reserve_live_turn_claims`] with an explicit provenance channel
/// (`live` for hook events, `export` for the OpenCode bridge, `import` for
/// M4). The channel NEVER participates in arbitration (ADR-DR-09) — it is
/// recorded provenance only.
pub async fn reserve_turn_claims_for_channel(
    conn: &DatabaseConnection,
    session_id: &str,
    turns: &[NormalizedTurn],
    owner: &str,
    now_ms: i64,
    source_channel: &'static str,
) -> Result<LiveReservationOutcome> {
    let scope = CaptureScope::main_for_connection(conn).await?;
    reserve_turn_claims_for_channel_with_capture_scope(
        conn,
        &scope,
        session_id,
        turns,
        owner,
        now_ms,
        source_channel,
    )
    .await
}

/// Scope-aware reservation with explicit provenance. The lease is checked in
/// the same SQLite write transaction that inserts/re-owns the claim, rather
/// than only at the caller's preflight.
pub async fn reserve_turn_claims_for_channel_with_capture_scope(
    conn: &DatabaseConnection,
    scope: &CaptureScope,
    session_id: &str,
    turns: &[NormalizedTurn],
    owner: &str,
    now_ms: i64,
    source_channel: &'static str,
) -> Result<LiveReservationOutcome> {
    let outcome = reserve_turn_claims_for_channel_inner(
        conn,
        scope,
        session_id,
        turns,
        owner,
        now_ms,
        source_channel,
        None,
    )
    .await;
    match outcome {
        Err(error) if is_sqlite_busy(&error) => Ok(LiveReservationOutcome {
            skipped_inflight: turns.len(),
            ..LiveReservationOutcome::default()
        }),
        outcome => outcome,
    }
}

fn is_sqlite_busy(error: &anyhow::Error) -> bool {
    let message = format!("{error:#}").to_ascii_lowercase();
    message.contains("database is locked")
        || message.contains("database schema is locked")
        || message.contains("database table is locked")
        || message.contains("database is busy")
        || message.contains("sqlite_busy")
}

fn ensure_reservation_deadline(
    deadline: Option<CaptureCommitDeadline>,
    source_channel: &'static str,
) -> Result<()> {
    if deadline.is_some_and(|deadline| Instant::now() >= deadline.monotonic()) {
        bail!("{source_channel} coverage reservation exceeded its execution deadline");
    }
    Ok(())
}

/// Bound only writer acquisition for a deadline-aware reservation. Claim DML
/// and its COMMIT remain outside this timeout because a cancelled dispatched
/// mutation cannot prove that SQLite did not durably accept it.
async fn begin_reservation_write_transaction_until(
    conn: &DatabaseConnection,
    deadline: Option<CaptureCommitDeadline>,
    source_channel: &'static str,
) -> Result<DatabaseTransaction> {
    ensure_reservation_deadline(deadline, source_channel)?;
    let transaction = match deadline {
        Some(deadline) => tokio::time::timeout_at(
            tokio::time::Instant::from_std(deadline.monotonic()),
            crate::internal::db::begin_write_transaction(conn),
        )
        .await
        .map_err(|_| {
            anyhow::anyhow!("{source_channel} coverage reservation exceeded its execution deadline")
        })?,
        None => crate::internal::db::begin_write_transaction(conn).await,
    };
    transaction.context("begin coverage reservation transaction")
}

/// Bound pre-DML reads while retaining the explicit rollback path owned by
/// the reservation transaction. This helper must never wrap claim mutation,
/// final authorization, or COMMIT.
async fn await_reservation_precommit_read_until<T>(
    deadline: Option<CaptureCommitDeadline>,
    source_channel: &'static str,
    read: impl std::future::Future<Output = Result<T>>,
) -> Result<T> {
    ensure_reservation_deadline(deadline, source_channel)?;
    match deadline {
        Some(deadline) => {
            tokio::time::timeout_at(tokio::time::Instant::from_std(deadline.monotonic()), read)
                .await
                .map_err(|_| {
                    anyhow::anyhow!(
                        "{source_channel} coverage reservation exceeded its execution deadline"
                    )
                })?
        }
        None => read.await,
    }
}

/// Derive the narrow paired deadline allowed for releasing a pre-existing
/// owned coverage reservation after capture work stops. If the foreground
/// deadline is still live, it remains the ceiling in both clock domains; a
/// fresh recovery deadline must never re-anchor its SQLite authorization.
fn coverage_abandonment_recovery_deadline(
    deadline: CaptureCommitDeadline,
) -> Result<CaptureCommitDeadline> {
    if deadline.monotonic() <= Instant::now() {
        return CaptureCommitDeadline::from_budget(COVERAGE_ABANDONMENT_RECOVERY_GRACE)
            .context("establish coverage reservation-abandonment recovery deadline");
    }

    let recovery_deadline = CaptureCommitDeadline::from_budget(COVERAGE_ABANDONMENT_RECOVERY_GRACE)
        .context("establish coverage reservation-abandonment recovery deadline")?;
    Ok(CaptureCommitDeadline::from_established_pair(
        deadline.monotonic().min(recovery_deadline.monotonic()),
        deadline
            .sqlite_not_after_millis()
            .min(recovery_deadline.sqlite_not_after_millis()),
    ))
}

/// Release every still-owned, uncommitted reservation after a writer aborts
/// before object construction. Fence tokens advance so a cancelled attempt
/// cannot later reuse its stale plan.
pub async fn abandon_reserved_turn_claims(
    conn: &DatabaseConnection,
    session_id: &str,
    owner: &str,
    source_channel: &'static str,
    now_ms: i64,
) -> Result<()> {
    let scope = CaptureScope::main_for_connection(conn).await?;
    abandon_reserved_turn_claims_with_capture_scope(
        conn,
        &scope,
        session_id,
        owner,
        source_channel,
        now_ms,
    )
    .await
}

/// Scope-aware abort cleanup. Releasing a reservation is itself a mutable
/// ownership transition, so it must not run after the workspace lease was
/// released or fenced.
pub async fn abandon_reserved_turn_claims_with_capture_scope(
    conn: &DatabaseConnection,
    scope: &CaptureScope,
    session_id: &str,
    owner: &str,
    source_channel: &'static str,
    now_ms: i64,
) -> Result<()> {
    abandon_reserved_turn_claims_with_capture_scope_inner(
        conn,
        scope,
        session_id,
        owner,
        source_channel,
        now_ms,
        None,
    )
    .await
}

/// Deadline-aware managed-capture cleanup. The paired grace may release only
/// the caller's already-reserved rows; it bounds writer acquisition, pure
/// pre-DML reads, and the final SQLite authorization. Claim DML and COMMIT
/// remain non-cancellable so an expired acknowledgement cannot misreport a
/// durable abandonment as absent.
pub(crate) async fn abandon_reserved_turn_claims_with_capture_scope_until(
    conn: &DatabaseConnection,
    scope: &CaptureScope,
    session_id: &str,
    owner: &str,
    source_channel: &'static str,
    now_ms: i64,
    deadline: CaptureCommitDeadline,
) -> Result<()> {
    let recovery_deadline = coverage_abandonment_recovery_deadline(deadline)?;
    abandon_reserved_turn_claims_with_capture_scope_inner(
        conn,
        scope,
        session_id,
        owner,
        source_channel,
        now_ms,
        Some(recovery_deadline),
    )
    .await
}

/// Release every reservation `owner` still holds for `session_id` in the
/// caller's transaction; the single abandonment statement shared by managed
/// capture cleanup and the historical importer. Fence tokens advance so the
/// released owner cannot later commit its stale plan. The caller owns scope
/// verification, deadline checks, error context and the final commit.
pub(crate) async fn abandon_reserved_turn_claims_with_conn(
    conn: &impl ConnectionTrait,
    session_id: &str,
    owner: &str,
    source_channel: &'static str,
    now_ms: i64,
) -> Result<()> {
    let state = reservation_state(source_channel)?;
    conn.execute_raw(Statement::from_sql_and_values(
        conn.get_database_backend(),
        "UPDATE agent_coverage_claim
         SET state = 'abandoned', owner = NULL, lease_expires_at = NULL,
             attempt_checkpoint_id = NULL,
             fence_token = COALESCE(fence_token, 0) + 1, updated_at = ?
         WHERE session_id = ? AND state = ? AND owner = ?",
        [now_ms.into(), session_id.into(), state.into(), owner.into()],
    ))
    .await?;
    Ok(())
}

/// Extend every import reservation `owner` still holds for `session_id` in
/// the caller's attempt-binding transaction. A claim that a live writer
/// already preempted carries another owner and fence, so it is untouched.
pub(crate) async fn renew_import_turn_claim_leases_with_conn(
    conn: &impl ConnectionTrait,
    session_id: &str,
    owner: &str,
    lease_expires_at: i64,
    now_ms: i64,
) -> Result<()> {
    conn.execute_raw(Statement::from_sql_and_values(
        conn.get_database_backend(),
        "UPDATE agent_coverage_claim SET lease_expires_at = ?, updated_at = ?
         WHERE session_id = ? AND state = 'reserved_import' AND owner = ?",
        [
            lease_expires_at.into(),
            now_ms.into(),
            session_id.into(),
            owner.into(),
        ],
    ))
    .await?;
    Ok(())
}

/// Record the checkpoint an import attempt is about to construct on its
/// fenced reservation, in the caller's attempt-binding transaction. Returns
/// `false` when the claim was abandoned or taken over since it was reserved.
pub(crate) async fn bind_import_turn_claim_attempt_with_conn(
    conn: &impl ConnectionTrait,
    session_id: &str,
    claim: &ReservedTurnClaim,
    owner: &str,
    checkpoint_id: &str,
    now_ms: i64,
) -> Result<bool> {
    let bound = conn
        .execute_raw(Statement::from_sql_and_values(
            conn.get_database_backend(),
            "UPDATE agent_coverage_claim SET attempt_checkpoint_id = ?, updated_at = ?
             WHERE session_id = ? AND logical_turn_key = ?
               AND coverage_schema_version = ? AND state = 'reserved_import'
               AND owner = ? AND fence_token = ?",
            [
                checkpoint_id.into(),
                now_ms.into(),
                session_id.into(),
                claim.logical_turn_key.clone().into(),
                COVERAGE_SCHEMA_VERSION.into(),
                owner.into(),
                claim.fence_token.into(),
            ],
        ))
        .await?;
    Ok(bound.rows_affected() == 1)
}

#[allow(clippy::too_many_arguments)]
async fn abandon_reserved_turn_claims_with_capture_scope_inner(
    conn: &DatabaseConnection,
    scope: &CaptureScope,
    session_id: &str,
    owner: &str,
    source_channel: &'static str,
    now_ms: i64,
    deadline: Option<CaptureCommitDeadline>,
) -> Result<()> {
    // Reject an unknown channel before taking the SQLite writer lock.
    reservation_state(source_channel)?;
    let txn = begin_reservation_write_transaction_until(conn, deadline, source_channel)
        .await
        .context("begin coverage reservation abandonment transaction")?;
    if let Err(error) = await_reservation_precommit_read_until(
        deadline,
        source_channel,
        scope.assert_workspace_fence_live(&txn),
    )
    .await
    {
        txn.rollback().await.ok();
        return Err(error)
            .context("verify capture workspace lease before abandoning coverage reservations");
    }
    if let Err(error) = ensure_reservation_deadline(deadline, source_channel) {
        txn.rollback().await.ok();
        return Err(error);
    }
    abandon_reserved_turn_claims_with_conn(&txn, session_id, owner, source_channel, now_ms)
        .await
        .context("abandon coverage reservations for aborted checkpoint writer")?;
    if let Err(error) = ensure_reservation_deadline(deadline, source_channel) {
        txn.rollback().await.ok();
        return Err(error);
    }
    match authorize_final_capture_commit(Some(scope), &txn, deadline).await {
        Ok(()) => {}
        Err(CaptureFinalCommitAuthorizationError::DeadlineElapsed) => {
            txn.rollback().await.ok();
            bail!("{source_channel} coverage reservation exceeded its execution deadline");
        }
        Err(error) => {
            txn.rollback().await.ok();
            return Err(anyhow::Error::new(error)).context(
                "verify capture workspace lease before committing coverage reservation abandonment",
            );
        }
    }
    // Final authorization is the last SQL operation. Do not apply deadline
    // cancellation to COMMIT: SQLite may have dispatched it before a dropped
    // acknowledgement future, so the durable cleanup result would be unknown.
    txn.commit()
        .await
        .context("commit coverage reservation abandonment transaction")?;
    Ok(())
}

pub(crate) async fn reserve_import_turn_claims_until(
    conn: &DatabaseConnection,
    scope: &CaptureScope,
    session_id: &str,
    turns: &[NormalizedTurn],
    owner: &str,
    now_ms: i64,
    deadline: CaptureCommitDeadline,
) -> Result<LiveReservationOutcome> {
    // Bound SQLite writer-lock residency and re-establish the tombstone
    // barrier between batches. Earlier batches remain explicitly owned and
    // are abandoned by the importer if a later batch reaches the deadline.
    const IMPORT_RESERVATION_BATCH_TURNS: usize = 64;
    let mut aggregate = LiveReservationOutcome::default();
    for batch in turns.chunks(IMPORT_RESERVATION_BATCH_TURNS) {
        ensure_reservation_deadline(Some(deadline), "import")?;
        let outcome = reserve_turn_claims_for_channel_inner(
            conn,
            scope,
            session_id,
            batch,
            owner,
            now_ms,
            "import",
            Some(deadline),
        )
        .await?;
        #[cfg(test)]
        if let Some(delay) = import_reservation_batch_test_delay() {
            tokio::time::sleep(delay).await;
        }
        aggregate.reserved.extend(outcome.reserved);
        aggregate.skipped_covered = aggregate
            .skipped_covered
            .saturating_add(outcome.skipped_covered);
        aggregate.skipped_inflight = aggregate
            .skipped_inflight
            .saturating_add(outcome.skipped_inflight);
        aggregate.skipped_retained = aggregate
            .skipped_retained
            .saturating_add(outcome.skipped_retained);
        aggregate.conflicted = aggregate.conflicted.saturating_add(outcome.conflicted);
    }
    Ok(aggregate)
}

#[allow(clippy::too_many_arguments)]
async fn reserve_turn_claims_for_channel_inner(
    conn: &DatabaseConnection,
    scope: &CaptureScope,
    session_id: &str,
    turns: &[NormalizedTurn],
    owner: &str,
    now_ms: i64,
    source_channel: &'static str,
    deadline: Option<CaptureCommitDeadline>,
) -> Result<LiveReservationOutcome> {
    // ADR-DR-19: reserve the complete snapshot under one SQLite writer
    // transaction and establish the tombstone barrier before any claim
    // mutation. This closes both fresh-INSERT and re-own races with erasure.
    // Reads the tombstone barrier before it writes the claim, so the write
    // lock is taken up front (`db::begin_write_transaction`).
    let txn = begin_reservation_write_transaction_until(conn, deadline, source_channel).await?;
    if let Err(error) = ensure_reservation_deadline(deadline, source_channel) {
        txn.rollback().await.ok();
        return Err(error);
    }
    if let Err(error) = await_reservation_precommit_read_until(
        deadline,
        source_channel,
        scope.assert_workspace_fence_live(&txn),
    )
    .await
    {
        txn.rollback().await.ok();
        return Err(error)
            .context("verify capture workspace lease before reserving coverage claims");
    }
    if let Err(error) = ensure_reservation_deadline(deadline, source_channel) {
        txn.rollback().await.ok();
        return Err(error);
    }
    let writable = match await_reservation_precommit_read_until(deadline, source_channel, async {
        txn.query_one_raw(Statement::from_sql_and_values(
            txn.get_database_backend(),
            "SELECT 1 AS writable
                 FROM agent_session s
                 WHERE s.session_id = ?
                   AND NOT EXISTS (
                     SELECT 1 FROM agent_import_tombstone t
                     WHERE t.agent_kind = s.agent_kind
                       AND t.provider_session_id = s.provider_session_id
                   )",
            [session_id.into()],
        ))
        .await
        .context("verify coverage reservation tombstone barrier")
    })
    .await
    {
        Ok(writable) => writable,
        Err(error) => {
            txn.rollback().await.ok();
            return Err(error);
        }
    };
    if let Err(error) = ensure_reservation_deadline(deadline, source_channel) {
        txn.rollback().await.ok();
        return Err(error);
    }
    if writable.is_none() {
        txn.rollback().await.ok();
        bail!("agent session was erased or is unavailable for coverage reservation");
    }
    let mut outcome = LiveReservationOutcome::default();
    for turn in turns {
        if let Err(error) = ensure_reservation_deadline(deadline, source_channel) {
            txn.rollback().await.ok();
            return Err(error);
        }
        let digest = turn.digest_hex();
        let mut rounds = 0;
        loop {
            if let Err(error) = ensure_reservation_deadline(deadline, source_channel) {
                txn.rollback().await.ok();
                return Err(error);
            }
            rounds += 1;
            let existing = match await_reservation_precommit_read_until(
                deadline,
                source_channel,
                read_claim(&txn, session_id, &turn.logical_turn_key),
            )
            .await
            {
                Ok(existing) => existing,
                Err(error) => {
                    txn.rollback().await.ok();
                    return Err(error);
                }
            };
            if let Err(error) = ensure_reservation_deadline(deadline, source_channel) {
                txn.rollback().await.ok();
                return Err(error);
            }
            let decision = decide_and_attempt(
                &txn,
                session_id,
                turn,
                &digest,
                owner,
                now_ms,
                existing,
                source_channel,
            )
            .await?;
            if let Err(error) = ensure_reservation_deadline(deadline, source_channel) {
                txn.rollback().await.ok();
                return Err(error);
            }
            match decision {
                AttemptOutcome::Reserved(claim) => {
                    outcome.reserved.push(claim);
                    break;
                }
                AttemptOutcome::SkipCovered => {
                    outcome.skipped_covered += 1;
                    break;
                }
                AttemptOutcome::SkipInflight => {
                    outcome.skipped_inflight += 1;
                    break;
                }
                AttemptOutcome::SkipRetained => {
                    outcome.skipped_inflight += 1;
                    outcome.skipped_retained += 1;
                    break;
                }
                AttemptOutcome::Conflicted => {
                    outcome.conflicted += 1;
                    break;
                }
                AttemptOutcome::LostRace if rounds < 3 => continue,
                AttemptOutcome::LostRace => {
                    // Two consecutive lost races: someone very active owns
                    // this turn right now; treat as in-flight, next event
                    // will retry.
                    outcome.skipped_inflight += 1;
                    break;
                }
            }
        }
        #[cfg(test)]
        if source_channel == "live"
            && let Some(delay) = live_reservation_turn_test_delay()
        {
            tokio::time::sleep(delay).await;
        }
    }
    if let Err(error) = ensure_reservation_deadline(deadline, source_channel) {
        txn.rollback().await.ok();
        return Err(error);
    }
    match authorize_final_capture_commit(Some(scope), &txn, deadline).await {
        Ok(()) => {}
        Err(CaptureFinalCommitAuthorizationError::DeadlineElapsed) => {
            txn.rollback().await.ok();
            bail!("{source_channel} coverage reservation exceeded its execution deadline");
        }
        Err(error) => {
            txn.rollback().await.ok();
            return Err(anyhow::Error::new(error))
                .context("verify capture workspace lease before committing coverage reservation");
        }
    }
    // The authorization above is the last SQL statement. Do not recheck or
    // cancel around this commit: SQLite may already have dispatched COMMIT,
    // so the acknowledgement must be awaited to avoid reporting a timeout
    // after durable claims were published.
    txn.commit()
        .await
        .context("commit coverage reservation transaction")?;
    Ok(outcome)
}

enum AttemptOutcome {
    Reserved(ReservedTurnClaim),
    SkipCovered,
    SkipInflight,
    SkipRetained,
    Conflicted,
    LostRace,
}

#[allow(clippy::too_many_arguments)]
async fn decide_and_attempt(
    conn: &impl ConnectionTrait,
    session_id: &str,
    turn: &NormalizedTurn,
    digest: &str,
    owner: &str,
    now_ms: i64,
    existing: Option<ExistingClaim>,
    source_channel: &'static str,
) -> Result<AttemptOutcome> {
    let Some(existing) = existing else {
        return Ok(
            match try_insert_fresh_claim(
                conn,
                session_id,
                turn,
                digest,
                owner,
                now_ms,
                source_channel,
            )
            .await?
            {
                Some(claim) => AttemptOutcome::Reserved(claim),
                None => AttemptOutcome::LostRace,
            },
        );
    };

    match existing.state.as_str() {
        "catalog_committed" => {
            let completeness_upgrade = existing.completeness == "incomplete"
                && turn.completeness == Completeness::Complete;
            if existing.coverage_digest == digest && !completeness_upgrade {
                // Equivalent content/completeness is a pure no-op. Terminal
                // evidence is deliberately outside the semantic digest, so a
                // same-digest incomplete -> complete transition must still
                // advance the revision and session lifecycle.
                return Ok(AttemptOutcome::SkipCovered);
            }
            match (existing.completeness.as_str(), turn.completeness) {
                ("incomplete", Completeness::Complete) => {
                    // Upgrade path: incomplete → complete advances the turn's
                    // current revision (ADR-DR-16).
                    match try_reown_claim(
                        conn,
                        session_id,
                        &turn.logical_turn_key,
                        "catalog_committed",
                        existing.fence_token,
                        digest,
                        turn.completeness,
                        owner,
                        now_ms,
                        source_channel,
                    )
                    .await?
                    {
                        Some((fence, _lease)) => Ok(AttemptOutcome::Reserved(ReservedTurnClaim {
                            logical_turn_key: turn.logical_turn_key.clone(),
                            coverage_digest: digest.to_string(),
                            completeness: turn.completeness,
                            fence_token: fence,
                            next_revision: next_revision(existing.revision)?,
                        })),
                        None => Ok(AttemptOutcome::LostRace),
                    }
                }
                ("complete", Completeness::Complete) => {
                    // complete → different complete: never auto-overwrite.
                    let conflict = try_mark_conflicted(
                        conn,
                        session_id,
                        &turn.logical_turn_key,
                        &existing,
                        turn,
                        source_channel,
                        now_ms,
                    )
                    .await?;
                    Ok(match conflict {
                        ConflictMarkOutcome::Marked => AttemptOutcome::Conflicted,
                        ConflictMarkOutcome::EquivalentAfterRedaction => {
                            AttemptOutcome::SkipCovered
                        }
                        ConflictMarkOutcome::LostRace => AttemptOutcome::LostRace,
                    })
                }
                // A (different) incomplete snapshot never downgrades or
                // replaces committed content.
                (_, Completeness::Incomplete) => Ok(AttemptOutcome::SkipCovered),
                // incomplete → incomplete with different digest: keep the
                // committed one; a later complete parse upgrades it.
                _ => Ok(AttemptOutcome::SkipCovered),
            }
        }
        "reserved_live" | "reserved_import" => {
            // A durable pending terminal artifact owns these turns until its
            // replay commits them or an explicit erase abandons them. Taking
            // them over would advance the fence the artifact must present,
            // leaving its authenticated snapshot permanently unreplayable.
            if existing.state == "reserved_live"
                && existing.lease_expires_at == Some(ARTIFACT_RETAINED_LEASE_MS)
            {
                return Ok(AttemptOutcome::SkipRetained);
            }
            let lease_live = existing.lease_expires_at.is_some_and(|t| t > now_ms);
            // A live hook/export writer may preempt an import reservation
            // before the import has committed the traces ref (ADR-DR-09).
            // The conditional re-own increments the fence so the stale import
            // holder cannot later update ref/claim/identity. Import never
            // preempts a live writer.
            let live_preempts_import =
                existing.state == "reserved_import" && matches!(source_channel, "live" | "export");
            if lease_live && !live_preempts_import {
                return Ok(AttemptOutcome::SkipInflight);
            }
            // Expired lease: fenced takeover (stale holder's later writes
            // fail their fence check and roll back).
            match try_reown_claim(
                conn,
                session_id,
                &turn.logical_turn_key,
                &existing.state,
                existing.fence_token,
                digest,
                turn.completeness,
                owner,
                now_ms,
                source_channel,
            )
            .await?
            {
                Some((fence, _lease)) => Ok(AttemptOutcome::Reserved(ReservedTurnClaim {
                    logical_turn_key: turn.logical_turn_key.clone(),
                    coverage_digest: digest.to_string(),
                    completeness: turn.completeness,
                    fence_token: fence,
                    next_revision: next_revision(existing.revision)?,
                })),
                None => Ok(AttemptOutcome::LostRace),
            }
        }
        "abandoned" => {
            match try_reown_claim(
                conn,
                session_id,
                &turn.logical_turn_key,
                "abandoned",
                existing.fence_token,
                digest,
                turn.completeness,
                owner,
                now_ms,
                source_channel,
            )
            .await?
            {
                Some((fence, _lease)) => Ok(AttemptOutcome::Reserved(ReservedTurnClaim {
                    logical_turn_key: turn.logical_turn_key.clone(),
                    coverage_digest: digest.to_string(),
                    completeness: turn.completeness,
                    fence_token: fence,
                    next_revision: next_revision(existing.revision)?,
                })),
                None => Ok(AttemptOutcome::LostRace),
            }
        }
        // Conflicted rows stay parked for doctor; never auto-resolved here.
        _ => Ok(AttemptOutcome::Conflicted),
    }
}

/// The transactional commit plan for one gated checkpoint write: applied by
/// `HistoryManager` INSIDE the ref-CAS transaction (ADR-DR-10 — ref update,
/// catalog row, coverage revisions and claim advances all commit or all roll
/// back together).
pub struct LiveClaimCommitPlan {
    /// Provenance channel recorded on revisions ('live' | 'export' | 'import').
    pub source_channel: &'static str,
    pub session_id: String,
    pub checkpoint_id: String,
    pub owner: String,
    pub parent_commit: Option<String>,
    pub created_at: i64,
    pub now_ms: i64,
    pub claims: Vec<ReservedTurnClaim>,
    /// Import-only session lifecycle/ownership update, committed with the
    /// first visible checkpoint instead of during lease acquisition.
    pub import_session: Option<CaptureImportSessionCommit>,
    /// Import-only identity cursor/fence update. `None` for live/export.
    pub import_identity: Option<ImportIdentityCommit>,
    /// Scope resolved at capture ingress. A final ref-CAS transaction must
    /// re-check the workspace lease because object construction can outlive
    /// the earlier reservation transaction.
    pub capture_scope: Option<CaptureScope>,
}

/// Import-job state advanced in the same transaction as ref/catalog/claims.
#[derive(Debug, Clone)]
pub struct ImportIdentityCommit {
    pub identity_id: String,
    pub observed_digest: String,
    pub owner: String,
    pub fence_token: i64,
    pub next_ordinal: i64,
    pub final_turn: bool,
}

#[async_trait]
impl TracesTxnExtra for LiveClaimCommitPlan {
    async fn apply(&self, txn: &DatabaseTransaction, ctx: &TracesCommitCtx) -> Result<()> {
        if let Some(scope) = &self.capture_scope {
            scope.assert_workspace_fence_live(txn).await.context(
                "verify capture workspace lease before final coverage checkpoint transaction",
            )?;
        }
        // ADR-DR-19: tombstone is the final transactional write barrier for
        // every gated writer. It is checked after object construction but in
        // the SAME transaction as the ref CAS; an erase that wins first makes
        // this transaction fail before the ref/catalog can advance.
        let writable = txn
            .query_one_raw(Statement::from_sql_and_values(
                txn.get_database_backend(),
                "SELECT 1 AS writable
                 FROM agent_session s
                 WHERE s.session_id = ?
                   AND NOT EXISTS (
                     SELECT 1 FROM agent_import_tombstone t
                     WHERE t.agent_kind = s.agent_kind
                       AND t.provider_session_id = s.provider_session_id
                   )",
                [self.session_id.clone().into()],
            ))
            .await
            .context("verify agent-session tombstone write barrier")?;
        if writable.is_none() {
            bail!(
                "agent session was erased or tombstoned while the checkpoint was in flight; \
                 rolling back the checkpoint transaction"
            );
        }

        if let Some(session) = &self.import_session {
            CaptureCatalogStore::apply_import_session_lifecycle(txn, session)
                .await
                .context("commit imported session lifecycle")?;
        }

        if let Some(identity) = &self.import_identity {
            let state = if identity.final_turn {
                "committed"
            } else {
                "writing"
            };
            let committed_digest: sea_orm::Value = if identity.final_turn {
                Some(identity.observed_digest.clone()).into()
            } else {
                None::<String>.into()
            };
            let retained_owner: sea_orm::Value = if identity.final_turn {
                None::<String>.into()
            } else {
                Some(identity.owner.clone()).into()
            };
            let retained_lease: sea_orm::Value = if identity.final_turn {
                None::<i64>.into()
            } else {
                Some(
                    self.now_ms
                        .checked_add(LIVE_LEASE_MS)
                        .context("import identity lease timestamp overflow")?,
                )
                .into()
            };
            let advanced = txn
                .execute_raw(Statement::from_sql_and_values(
                    txn.get_database_backend(),
                    "UPDATE agent_import_identity
                     SET state = ?, observed_digest = ?,
                         committed_digest = COALESCE(?, committed_digest),
                         next_ordinal = ?, attempt_checkpoint_id = ?,
                         owner = ?, lease_expires_at = ?, updated_at = ?
                     WHERE identity_id = ? AND owner = ? AND fence_token = ?
                       AND state IN ('leased','writing')",
                    [
                        state.into(),
                        identity.observed_digest.clone().into(),
                        committed_digest,
                        identity.next_ordinal.into(),
                        self.checkpoint_id.clone().into(),
                        retained_owner,
                        retained_lease,
                        self.now_ms.into(),
                        identity.identity_id.clone().into(),
                        identity.owner.clone().into(),
                        identity.fence_token.into(),
                    ],
                ))
                .await
                .context("advance agent_import_identity in ref transaction")?;
            if advanced.rows_affected() != 1 {
                bail!(
                    "import identity lease was fenced out while committing turn; \
                     rolling back checkpoint transaction"
                );
            }
        }

        // Catalog row first (claim advance references checkpoint_id). The
        // checkpoint seam retains the `ON CONFLICT DO NOTHING` crash-retry
        // backstop, but runs it in this exact ref/coverage transaction.
        CheckpointCatalogMutation::new(
            self.checkpoint_id.clone(),
            self.session_id.clone(),
            CheckpointScope::Committed,
            self.parent_commit.clone(),
            self.created_at,
        )
        .context("prepare agent_checkpoint catalog companion")?
        .apply(txn, ctx)
        .await
        .context("commit typed checkpoint catalog companion in ref transaction")?;

        for claim in &self.claims {
            // Append-only revision history (ADR-DR-16).
            txn.execute_raw(Statement::from_sql_and_values(
                txn.get_database_backend(),
                "INSERT INTO agent_coverage_revision (
                    session_id, logical_turn_key, coverage_schema_version,
                    revision, checkpoint_id, coverage_digest, completeness,
                    source_channel, created_at
                 ) VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?)",
                [
                    self.session_id.clone().into(),
                    claim.logical_turn_key.clone().into(),
                    COVERAGE_SCHEMA_VERSION.into(),
                    claim.next_revision.into(),
                    self.checkpoint_id.clone().into(),
                    claim.coverage_digest.clone().into(),
                    claim.completeness.as_db_str().into(),
                    self.source_channel.into(),
                    self.now_ms.into(),
                ],
            ))
            .await
            .context("insert agent_coverage_revision in ref transaction")?;

            // Advance the claim — owner + fence + state guarded. Zero rows
            // means our reservation was fenced out; the WHOLE transaction
            // (ref update included) must roll back (ADR-DR-10).
            let advanced = txn
                .execute_raw(Statement::from_sql_and_values(
                    txn.get_database_backend(),
                    "UPDATE agent_coverage_claim
                     SET state = 'catalog_committed', revision = ?,
                         coverage_digest = ?, completeness = ?,
                         checkpoint_id = ?, traces_commit = ?,
                         owner = NULL, lease_expires_at = NULL, updated_at = ?
                     WHERE session_id = ? AND logical_turn_key = ?
                       AND coverage_schema_version = ?
                       AND state = ?
                       AND owner = ? AND fence_token = ?",
                    [
                        claim.next_revision.into(),
                        claim.coverage_digest.clone().into(),
                        claim.completeness.as_db_str().into(),
                        self.checkpoint_id.clone().into(),
                        ctx.commit_hash.clone().into(),
                        self.now_ms.into(),
                        self.session_id.clone().into(),
                        claim.logical_turn_key.clone().into(),
                        COVERAGE_SCHEMA_VERSION.into(),
                        reservation_state(self.source_channel)?.into(),
                        self.owner.clone().into(),
                        claim.fence_token.into(),
                    ],
                ))
                .await
                .context("advance agent_coverage_claim in ref transaction")?;
            if advanced.rows_affected() != 1 {
                bail!(
                    "coverage claim for turn '{}' was fenced out during commit \
                     (stale reservation); rolling back checkpoint transaction",
                    claim.logical_turn_key
                );
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use sea_orm::Database;

    use super::*;
    use crate::internal::{
        ai::observed_agents::SemanticRecord, config::ConfigKv,
        db::migration::run_builtin_migrations,
    };

    /// Codex M3 R2 P1-2: the export path must release DIRTY (retryable) only
    /// when it reserved nothing yet turns are held in flight by another writer,
    /// and must NOT confuse that with a genuine all-covered no-op.
    #[test]
    fn inflight_only_skip_distinguishes_foreign_hold_from_covered_noop() {
        // Nothing reserved, one turn held by another live writer → retry dirty.
        let foreign_hold = LiveReservationOutcome {
            skipped_inflight: 1,
            ..Default::default()
        };
        assert!(foreign_hold.is_inflight_only_skip());

        // Genuine all-covered no-op (nothing in flight) → advance honestly.
        let all_covered = LiveReservationOutcome {
            skipped_covered: 3,
            ..Default::default()
        };
        assert!(!all_covered.is_inflight_only_skip());

        // A fully empty outcome is not an in-flight skip either.
        assert!(!LiveReservationOutcome::default().is_inflight_only_skip());
    }

    #[tokio::test]
    async fn in_process_batch_delay_checks_import_deadline_before_a_second_reservation_batch() {
        let (conn, scope) = scoped_gate_db().await;
        let session = "claude_code__scope_expiry";
        let turns = (0..65)
            .map(|ordinal| {
                turn(
                    &format!("batch-deadline-{ordinal}"),
                    "historical import coverage",
                    Completeness::Complete,
                )
            })
            .collect::<Vec<_>>();
        let deadline = CaptureCommitDeadline::from_test_pair(
            Instant::now() + std::time::Duration::from_millis(500),
            chrono::Utc::now().timestamp_millis().saturating_add(500),
        );
        let error = with_import_reservation_batch_delay(
            std::time::Duration::from_millis(650),
            reserve_import_turn_claims_until(
                &conn,
                &scope,
                session,
                &turns,
                "batch-deadline-owner",
                1,
                deadline,
            ),
        )
        .await
        .expect_err("the second batch must observe the elapsed import deadline");
        assert!(
            format!("{error:#}").contains("execution deadline"),
            "unexpected batch-deadline error: {error:#}"
        );
        assert_eq!(
            scalar(
                &conn,
                "SELECT COUNT(*) AS n FROM agent_coverage_claim \
                 WHERE session_id = 'claude_code__scope_expiry' \
                   AND logical_turn_key LIKE 'batch-deadline-%'",
            )
            .await,
            64,
            "the deadline check must prevent a partial second reservation batch"
        );
        assert_eq!(
            scalar(
                &conn,
                "SELECT COUNT(*) AS n FROM agent_coverage_claim \
                 WHERE session_id = 'claude_code__scope_expiry' \
                   AND logical_turn_key = 'batch-deadline-64'",
            )
            .await,
            0,
            "the final turn must remain unreserved after the inter-batch deadline"
        );
    }

    #[tokio::test]
    async fn import_reservation_deadline_bounds_file_writer_acquisition_without_claim_mutation() {
        let repository = tempfile::tempdir().expect("create coverage deadline repository");
        let database_path = repository.path().join("coverage-deadline.sqlite");
        let conn = crate::internal::db::create_database(
            database_path
                .to_str()
                .expect("coverage deadline database path is utf-8"),
        )
        .await
        .expect("create coverage deadline database");
        let backend = conn.get_database_backend();
        conn.execute_unprepared("PRAGMA journal_mode = DELETE")
            .await
            .expect("force rollback-journal mode for coverage deadline regression");
        let scope = CaptureScope {
            repo_id: "coverage-deadline-repo".to_string(),
            worktree_id: String::new(),
            workspace_id: Some("coverage-deadline-workspace".to_string()),
            workspace_fence: Some(7),
        };
        conn.execute_raw(Statement::from_sql_and_values(
            backend,
            "INSERT INTO workspace_record (
                workspace_id, repo_id, kind, worktree_id, path, owner_kind,
                state, lease_owner, lease_fence, lease_expires_at, created_at, updated_at
             ) VALUES (?, ?, 'task_copy', NULL, '/tmp/coverage-deadline-workspace', 'agent',
                       'active', 'coverage-deadline-owner', ?, 9999999999999, 1, 1)",
            [
                scope.workspace_id.clone().into(),
                scope.repo_id.clone().into(),
                scope.workspace_fence.into(),
            ],
        ))
        .await
        .expect("seed coverage deadline workspace lease");
        conn.execute_unprepared(
            "INSERT INTO agent_session (
                session_id, agent_kind, provider_session_id, state, working_dir,
                metadata_json, redaction_report, started_at, last_event_at, schema_version,
                repo_id, worktree_id, workspace_id, workspace_fence, scope_state
             ) VALUES ('coverage-deadline-session', 'claude_code', 'coverage-deadline-provider',
                       'active', '/tmp', '{}', '{}', 0, 0, 1,
                       'coverage-deadline-repo', '', 'coverage-deadline-workspace', 7, 'scoped')",
        )
        .await
        .expect("seed coverage deadline session");

        let locker = crate::internal::db::establish_connection_with_busy_timeout(
            database_path
                .to_str()
                .expect("coverage deadline database path is utf-8"),
            std::time::Duration::from_millis(50),
        )
        .await
        .expect("open coverage deadline lock holder");
        locker
            .execute_unprepared("PRAGMA journal_mode = DELETE")
            .await
            .expect("force rollback-journal mode on coverage deadline lock holder");
        locker
            .execute_raw(Statement::from_string(
                backend,
                "BEGIN EXCLUSIVE".to_string(),
            ))
            .await
            .expect("acquire exclusive coverage deadline lock");

        let turns = [turn(
            "coverage-deadline-turn",
            "historical coverage under a database lock",
            Completeness::Complete,
        )];
        let started = Instant::now();
        let reservation = tokio::time::timeout(
            std::time::Duration::from_secs(2),
            reserve_import_turn_claims_until(
                &conn,
                &scope,
                "coverage-deadline-session",
                &turns,
                "coverage-deadline-owner",
                1,
                CaptureCommitDeadline::from_budget(std::time::Duration::from_millis(30))
                    .expect("establish coverage reservation deadline"),
            ),
        )
        .await
        .expect("coverage writer acquisition must honor its deadline");
        locker
            .execute_raw(Statement::from_string(backend, "ROLLBACK".to_string()))
            .await
            .expect("release exclusive coverage deadline lock");

        let error = reservation.expect_err("locked import reservation must time out");
        assert!(
            format!("{error:#}")
                .contains("import coverage reservation exceeded its execution deadline"),
            "unexpected locked import reservation error: {error:#}"
        );
        assert!(
            started.elapsed() < std::time::Duration::from_secs(1),
            "coverage writer acquisition must not wait for SQLite's busy timeout"
        );
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
        assert_eq!(
            scalar(
                &conn,
                "SELECT COUNT(*) AS n FROM agent_coverage_claim
                 WHERE session_id = 'coverage-deadline-session'
                   AND logical_turn_key = 'coverage-deadline-turn'",
            )
            .await,
            0,
            "a cancelled coverage writer acquisition must not publish a delayed claim"
        );
    }

    /// A failed managed hook may use only its short recovery grace to release
    /// an existing owned claim. An independent SQLite writer lock must not
    /// leave that cleanup queued and let it abandon the claim after the grace.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn abandonment_recovery_grace_bounds_file_writer_acquisition_without_late_write() {
        let repository = tempfile::tempdir().expect("create coverage cleanup deadline repository");
        let database_path = repository.path().join("coverage-cleanup-deadline.sqlite");
        let conn = crate::internal::db::create_database(
            database_path
                .to_str()
                .expect("coverage cleanup deadline database path is utf-8"),
        )
        .await
        .expect("create coverage cleanup deadline database");
        let backend = conn.get_database_backend();
        conn.execute_unprepared("PRAGMA journal_mode = DELETE")
            .await
            .expect("force rollback-journal mode for coverage cleanup deadline regression");
        let scope = CaptureScope {
            repo_id: "coverage-cleanup-deadline-repo".to_string(),
            worktree_id: String::new(),
            workspace_id: Some("coverage-cleanup-deadline-workspace".to_string()),
            workspace_fence: Some(7),
        };
        conn.execute_raw(Statement::from_sql_and_values(
            backend,
            "INSERT INTO workspace_record (
                workspace_id, repo_id, kind, worktree_id, path, owner_kind,
                state, lease_owner, lease_fence, lease_expires_at, created_at, updated_at
             ) VALUES (?, ?, 'task_copy', NULL, '/tmp/coverage-cleanup-deadline-workspace', 'agent',
                       'active', 'coverage-cleanup-deadline-owner', ?, 9999999999999, 1, 1)",
            [
                scope.workspace_id.clone().into(),
                scope.repo_id.clone().into(),
                scope.workspace_fence.into(),
            ],
        ))
        .await
        .expect("seed coverage cleanup deadline workspace lease");
        conn.execute_unprepared(
            "INSERT INTO agent_session (
                session_id, agent_kind, provider_session_id, state, working_dir,
                metadata_json, redaction_report, started_at, last_event_at, schema_version,
                repo_id, worktree_id, workspace_id, workspace_fence, scope_state
             ) VALUES ('coverage-cleanup-deadline-session', 'claude_code',
                       'coverage-cleanup-deadline-provider', 'active', '/tmp', '{}', '{}', 0, 0, 1,
                       'coverage-cleanup-deadline-repo', '', 'coverage-cleanup-deadline-workspace', 7, 'scoped')",
        )
        .await
        .expect("seed coverage cleanup deadline session");

        let owner = "coverage-cleanup-deadline-owner";
        let turn = turn(
            "coverage-cleanup-deadline-turn",
            "coverage reservation retained under a recovery writer lock",
            Completeness::Complete,
        );
        let reserved = reserve_live_turn_claims_with_capture_scope(
            &conn,
            &scope,
            "coverage-cleanup-deadline-session",
            std::slice::from_ref(&turn),
            owner,
            1,
        )
        .await
        .expect("seed owned coverage reservation before recovery lock");
        assert_eq!(reserved.reserved.len(), 1);

        let locker = crate::internal::db::establish_connection_with_busy_timeout(
            database_path
                .to_str()
                .expect("coverage cleanup deadline database path is utf-8"),
            Duration::from_secs(1),
        )
        .await
        .expect("open coverage cleanup deadline lock holder");
        locker
            .execute_unprepared("PRAGMA journal_mode = DELETE")
            .await
            .expect("force rollback-journal mode on coverage cleanup deadline lock holder");
        locker
            .execute_raw(Statement::from_string(
                backend,
                "BEGIN EXCLUSIVE".to_string(),
            ))
            .await
            .expect("acquire exclusive coverage cleanup deadline lock");

        let started = Instant::now();
        let cleanup = tokio::time::timeout(
            Duration::from_secs(2),
            abandon_reserved_turn_claims_with_capture_scope_until(
                &conn,
                &scope,
                "coverage-cleanup-deadline-session",
                owner,
                "live",
                2,
                CaptureCommitDeadline::from_test_pair(
                    Instant::now() - Duration::from_millis(1),
                    chrono::Utc::now().timestamp_millis().saturating_sub(1),
                ),
            ),
        )
        .await
        .expect("coverage recovery cleanup must honor its short writer-acquisition grace");
        locker
            .execute_raw(Statement::from_string(backend, "ROLLBACK".to_string()))
            .await
            .expect("release exclusive coverage cleanup deadline lock");

        let error =
            cleanup.expect_err("exclusive lock must exhaust coverage cleanup recovery grace");
        assert!(
            format!("{error:#}")
                .contains("live coverage reservation exceeded its execution deadline"),
            "unexpected coverage cleanup recovery-lock error: {error:#}"
        );
        assert!(
            started.elapsed() < Duration::from_secs(1),
            "coverage cleanup must not wait for SQLite's default busy timeout"
        );
        tokio::time::sleep(Duration::from_millis(100)).await;
        assert_eq!(
            scalar(
                &conn,
                "SELECT COUNT(*) AS n FROM agent_coverage_claim
                 WHERE session_id = 'coverage-cleanup-deadline-session'
                   AND logical_turn_key = 'coverage-cleanup-deadline-turn'
                   AND state = 'reserved_live'
                   AND owner = 'coverage-cleanup-deadline-owner'",
            )
            .await,
            1,
            "a cancelled recovery acquisition must not abandon the claim after the lock clears"
        );
    }

    #[tokio::test]
    async fn abandonment_recovery_preserves_live_primary_sqlite_deadline_at_final_authorization() {
        let (conn, scope) = scoped_gate_db().await;
        let session = "claude_code__scope_expiry";
        let owner = "coverage-cleanup-final-sqlite-owner";
        let turn = turn(
            "coverage-cleanup-final-sqlite-deadline",
            "coverage cleanup final authorization",
            Completeness::Complete,
        );
        let reserved = reserve_live_turn_claims_with_capture_scope(
            &conn,
            &scope,
            session,
            std::slice::from_ref(&turn),
            owner,
            1,
        )
        .await
        .expect("seed owned coverage reservation before final authorization");
        assert_eq!(reserved.reserved.len(), 1);

        // The monotonic foreground budget remains live, but its original
        // SQLite authorization is already expired. Cleanup must retain that
        // stricter wall-clock ceiling rather than replace it with fresh grace.
        let error = abandon_reserved_turn_claims_with_capture_scope_until(
            &conn,
            &scope,
            session,
            owner,
            "live",
            2,
            CaptureCommitDeadline::from_test_pair(Instant::now() + Duration::from_secs(1), 0),
        )
        .await
        .expect_err("expired primary SQLite authorization must roll back abandonment");
        assert!(
            format!("{error:#}")
                .contains("live coverage reservation exceeded its execution deadline"),
            "unexpected coverage cleanup final authorization error: {error:#}"
        );
        assert_eq!(
            scalar(
                &conn,
                "SELECT COUNT(*) AS n FROM agent_coverage_claim
                 WHERE session_id = 'claude_code__scope_expiry'
                   AND logical_turn_key = 'coverage-cleanup-final-sqlite-deadline'
                   AND state = 'reserved_live'
                   AND owner = 'coverage-cleanup-final-sqlite-owner'",
            )
            .await,
            1,
            "expired final authorization must leave the owned reservation intact"
        );
    }

    #[tokio::test]
    async fn live_reservation_deadline_rolls_back_the_entire_claim_plan() {
        let (conn, scope) = scoped_gate_db().await;
        let session = "claude_code__scope_expiry";
        let turns = [
            turn(
                "live-deadline-first",
                "first live coverage turn",
                Completeness::Complete,
            ),
            turn(
                "live-deadline-second",
                "second live coverage turn",
                Completeness::Complete,
            ),
        ];
        let deadline = CaptureCommitDeadline::from_test_pair(
            Instant::now() + std::time::Duration::from_millis(500),
            chrono::Utc::now().timestamp_millis().saturating_add(500),
        );
        let error = with_live_reservation_turn_delay(
            std::time::Duration::from_millis(650),
            reserve_live_turn_claims_until(
                &conn,
                &scope,
                session,
                &turns,
                "live-deadline-owner",
                1,
                deadline,
            ),
        )
        .await
        .expect_err("an elapsed live deadline must abort its all-or-nothing reservation");
        assert!(
            format!("{error:#}")
                .contains("live coverage reservation exceeded its execution deadline"),
            "unexpected live-deadline error: {error:#}"
        );
        assert_eq!(
            scalar(
                &conn,
                "SELECT COUNT(*) AS n FROM agent_coverage_claim
                 WHERE session_id = 'claude_code__scope_expiry'
                   AND logical_turn_key LIKE 'live-deadline-%'",
            )
            .await,
            0,
            "a deadline during a live reservation must roll back every claimed turn"
        );
    }

    #[tokio::test]
    async fn live_reservation_final_sqlite_deadline_rolls_back_claims() {
        let (conn, scope) = scoped_gate_db().await;
        let turns = [turn(
            "live-final-sqlite-deadline",
            "coverage final authorization",
            Completeness::Complete,
        )];
        // The monotonic budget is still live, but the immutable wall-clock
        // authorization is already expired. The claim DML therefore reaches
        // the final SQLite predicate and must roll back rather than publish.
        let deadline = CaptureCommitDeadline::from_test_pair(
            Instant::now() + std::time::Duration::from_secs(1),
            0,
        );
        let error = reserve_live_turn_claims_until(
            &conn,
            &scope,
            "claude_code__scope_expiry",
            &turns,
            "live-final-sqlite-owner",
            1,
            deadline,
        )
        .await
        .expect_err("expired final SQLite authorization must reject the claim commit");
        assert!(
            format!("{error:#}")
                .contains("live coverage reservation exceeded its execution deadline"),
            "unexpected final authorization error: {error:#}"
        );
        assert_eq!(
            scalar(
                &conn,
                "SELECT COUNT(*) AS n FROM agent_coverage_claim
                 WHERE session_id = 'claude_code__scope_expiry'
                   AND logical_turn_key = 'live-final-sqlite-deadline'",
            )
            .await,
            0,
            "the final authorization rejection must roll back the new claim"
        );
    }

    #[tokio::test]
    async fn export_reservation_execution_keeps_export_provenance_and_final_deadline() {
        let (conn, scope) = scoped_gate_db().await;
        let session = "claude_code__scope_expiry";
        let successful_turn = turn(
            "export-provenance-turn",
            "coverage reserved by the OpenCode export path",
            Completeness::Complete,
        );
        let outcome = reserve_turn_claims_for_channel_with_capture_scope_until(
            &conn,
            &scope,
            session,
            std::slice::from_ref(&successful_turn),
            "export-provenance-owner",
            1,
            CaptureReservationExecution::export(CaptureCommitDeadline::from_test_pair(
                Instant::now() + std::time::Duration::from_secs(1),
                chrono::Utc::now().timestamp_millis().saturating_add(1_000),
            )),
        )
        .await
        .expect("export reservation should succeed within its paired deadline");
        assert_eq!(outcome.reserved.len(), 1);
        let provenance = conn
            .query_one_raw(Statement::from_sql_and_values(
                conn.get_database_backend(),
                "SELECT state, source_channel FROM agent_coverage_claim
                 WHERE session_id = ? AND logical_turn_key = ?",
                [
                    session.into(),
                    successful_turn.logical_turn_key.clone().into(),
                ],
            ))
            .await
            .expect("read export coverage provenance")
            .expect("export reservation exists");
        assert_eq!(
            provenance
                .try_get_by::<String, _>("state")
                .expect("decode export reservation state"),
            "reserved_live"
        );
        assert_eq!(
            provenance
                .try_get_by::<String, _>("source_channel")
                .expect("decode export reservation channel"),
            "export"
        );

        let expired_turn = turn(
            "export-final-sqlite-deadline",
            "coverage must not commit after export final authorization expires",
            Completeness::Complete,
        );
        let error = reserve_turn_claims_for_channel_with_capture_scope_until(
            &conn,
            &scope,
            session,
            std::slice::from_ref(&expired_turn),
            "export-final-sqlite-owner",
            2,
            CaptureReservationExecution::export(CaptureCommitDeadline::from_test_pair(
                Instant::now() + std::time::Duration::from_secs(1),
                0,
            )),
        )
        .await
        .expect_err("expired export SQLite authorization must roll back the claim");
        assert!(
            format!("{error:#}")
                .contains("export coverage reservation exceeded its execution deadline"),
            "unexpected export final authorization error: {error:#}"
        );
        assert_eq!(
            scalar(
                &conn,
                "SELECT COUNT(*) AS n FROM agent_coverage_claim
                 WHERE session_id = 'claude_code__scope_expiry'
                   AND logical_turn_key = 'export-final-sqlite-deadline'",
            )
            .await,
            0,
            "an expired export final authorization must not publish a delayed claim"
        );
    }

    #[test]
    fn import_session_lifecycle_requires_state_timestamp_pair() {
        assert_eq!(
            ImportSessionLifecycleState::from_import_state("active", None)
                .expect("active import state with no terminal timestamp"),
            ImportSessionLifecycleState::Active
        );
        assert_eq!(
            ImportSessionLifecycleState::from_import_state("stopped", Some(1))
                .expect("stopped import state with terminal timestamp"),
            ImportSessionLifecycleState::Stopped
        );
        assert!(
            ImportSessionLifecycleState::from_import_state("active", Some(1)).is_err(),
            "reactivated imports must clear stale stopped_at"
        );
        assert!(
            ImportSessionLifecycleState::from_import_state("stopped", None).is_err(),
            "terminal imports must retain a stopped_at timestamp"
        );
    }

    async fn gate_db() -> DatabaseConnection {
        let conn = Database::connect("sqlite::memory:").await.expect("mem db");
        // The migration set assumes the bootstrap schema (ai_thread etc.)
        // exists; this unit fixture only needs the capture/coverage tables,
        // so relax FK enforcement instead of replaying the full bootstrap.
        conn.execute_raw(Statement::from_string(
            conn.get_database_backend(),
            "PRAGMA foreign_keys = OFF".to_string(),
        ))
        .await
        .expect("pragma");
        run_builtin_migrations(&conn).await.expect("migrations");
        ConfigKv::set_with_conn(&conn, "libra.repoid", "coverage-gate-test", false)
            .await
            .expect("seed repository identity");
        // FK target for claims.
        conn.execute_raw(Statement::from_string(
            conn.get_database_backend(),
            "INSERT INTO agent_session (
                session_id, agent_kind, provider_session_id, state, working_dir,
                metadata_json, redaction_report, started_at, last_event_at, schema_version,
                repo_id, worktree_id, workspace_id, workspace_fence, scope_state
             ) VALUES ('claude_code__s1', 'claude_code', 's1', 'active', '/tmp',
                       '{}', '{}', 0, 0, 1,
                       'coverage-gate-test', '', NULL, NULL, 'scoped')"
                .to_string(),
        ))
        .await
        .expect("seed session");
        conn
    }

    fn turn(key: &str, text: &str, completeness: Completeness) -> NormalizedTurn {
        NormalizedTurn {
            logical_turn_key: key.to_string(),
            ordinal: 0,
            completeness,
            started_at: None,
            ended_at: None,
            records: vec![SemanticRecord::User {
                text: text.to_string(),
            }],
        }
    }

    async fn claim_row(conn: &DatabaseConnection, key: &str) -> (String, i64, Option<i64>) {
        let row = conn
            .query_one_raw(Statement::from_sql_and_values(
                conn.get_database_backend(),
                "SELECT state, revision, fence_token FROM agent_coverage_claim \
                 WHERE logical_turn_key = ?",
                [key.into()],
            ))
            .await
            .expect("query")
            .expect("row");
        (
            row.try_get_by("state").unwrap(),
            row.try_get_by("revision").unwrap(),
            row.try_get_by("fence_token").ok().flatten(),
        )
    }

    async fn scalar(conn: &DatabaseConnection, sql: &str) -> i64 {
        conn.query_one_raw(Statement::from_string(
            conn.get_database_backend(),
            sql.to_string(),
        ))
        .await
        .expect("scalar query")
        .expect("scalar row")
        .try_get_by("n")
        .expect("scalar value")
    }

    /// Simulate the in-transaction commit of a reservation (what
    /// `LiveClaimCommitPlan::apply` does), without building objects.
    async fn commit_reserved(
        conn: &DatabaseConnection,
        session_id: &str,
        owner: &str,
        claim: &ReservedTurnClaim,
        checkpoint_id: &str,
    ) -> Result<()> {
        let txn = sea_orm::TransactionTrait::begin(conn).await?;
        let plan = LiveClaimCommitPlan {
            source_channel: "live",
            session_id: session_id.to_string(),
            checkpoint_id: checkpoint_id.to_string(),
            owner: owner.to_string(),
            parent_commit: None,
            created_at: 0,
            now_ms: 1,
            claims: vec![claim.clone()],
            import_session: None,
            import_identity: None,
            capture_scope: None,
        };
        let ctx = TracesCommitCtx {
            commit_hash: format!("commit-{checkpoint_id}"),
            tree_oid: "t".to_string(),
            metadata_blob_oid: "m".to_string(),
        };
        plan.apply(&txn, &ctx).await?;
        txn.commit().await?;
        Ok(())
    }

    async fn scoped_gate_db() -> (DatabaseConnection, CaptureScope) {
        let conn = gate_db().await;
        let scope = CaptureScope {
            repo_id: "coverage-gate-test".to_string(),
            worktree_id: String::new(),
            workspace_id: Some("coverage-gate-workspace".to_string()),
            workspace_fence: Some(7),
        };
        conn.execute_raw(Statement::from_sql_and_values(
            conn.get_database_backend(),
            "INSERT INTO workspace_record (
                workspace_id, repo_id, kind, worktree_id, path, owner_kind,
                state, lease_owner, lease_fence, lease_expires_at, created_at, updated_at
             ) VALUES (?, ?, 'task_copy', NULL, '/tmp/coverage-gate-workspace', 'agent',
                       'active', 'coverage-gate-owner', ?, 9999999999999, 1, 1)",
            [
                scope.workspace_id.clone().into(),
                scope.repo_id.clone().into(),
                scope.workspace_fence.into(),
            ],
        ))
        .await
        .expect("seed live coverage workspace");
        conn.execute_raw(Statement::from_sql_and_values(
            conn.get_database_backend(),
            "INSERT INTO agent_session (
                session_id, agent_kind, provider_session_id, state, working_dir,
                metadata_json, redaction_report, started_at, last_event_at, schema_version,
                repo_id, worktree_id, workspace_id, workspace_fence, scope_state
             ) VALUES ('claude_code__scope_expiry', 'claude_code', 'scope-expiry-session',
                       'active', '/tmp', '{}', '{}', 0, 0, 1, ?, ?, ?, ?, 'scoped')",
            [
                scope.repo_id.clone().into(),
                scope.worktree_id.clone().into(),
                scope.workspace_id.clone().into(),
                scope.workspace_fence.into(),
            ],
        ))
        .await
        .expect("seed scoped coverage session");
        (conn, scope)
    }

    /// A workspace can expire after coverage is reserved but before object
    /// construction reaches ref-CAS. Neither the final companion writes nor
    /// abort cleanup may mutate that now-stale claim.
    #[tokio::test]
    async fn expired_workspace_scope_blocks_coverage_final_and_abandon_mutations() {
        let (conn, scope) = scoped_gate_db().await;
        let session = "claude_code__scope_expiry";
        let turn = turn(
            "scope-expiry",
            "captured while lease was live",
            Completeness::Complete,
        );
        let reserved = reserve_live_turn_claims_with_capture_scope(
            &conn,
            &scope,
            session,
            std::slice::from_ref(&turn),
            "scope-owner",
            1,
        )
        .await
        .expect("reserve while workspace lease is live");
        let claim = reserved
            .reserved
            .first()
            .expect("one reserved turn")
            .clone();

        conn.execute_raw(Statement::from_string(
            conn.get_database_backend(),
            "UPDATE workspace_record SET lease_expires_at = 0 WHERE workspace_id = 'coverage-gate-workspace'"
                .to_string(),
        ))
        .await
        .expect("expire workspace lease after reservation");

        let plan = LiveClaimCommitPlan {
            source_channel: "live",
            session_id: session.to_string(),
            checkpoint_id: "scope-expiry-checkpoint".to_string(),
            owner: "scope-owner".to_string(),
            parent_commit: None,
            created_at: 1,
            now_ms: 2,
            claims: vec![claim],
            import_session: None,
            import_identity: None,
            capture_scope: Some(scope.clone()),
        };
        let txn = crate::internal::db::begin_write_transaction(&conn)
            .await
            .expect("begin final scope check transaction");
        let error = plan
            .apply(
                &txn,
                &TracesCommitCtx {
                    commit_hash: "scope-expiry-commit".to_string(),
                    tree_oid: "scope-expiry-tree".to_string(),
                    metadata_blob_oid: "scope-expiry-metadata".to_string(),
                },
            )
            .await
            .expect_err("expired scope must reject the final coverage transaction");
        assert!(format!("{error:#}").contains("workspace lease"));
        txn.rollback()
            .await
            .expect("roll back rejected final transaction");

        let error = abandon_reserved_turn_claims_with_capture_scope(
            &conn,
            &scope,
            session,
            "scope-owner",
            "live",
            2,
        )
        .await
        .expect_err("expired scope must not abandon a coverage reservation");
        assert!(format!("{error:#}").contains("workspace lease"));
        assert_eq!(
            scalar(
                &conn,
                "SELECT COUNT(*) AS n FROM agent_checkpoint WHERE checkpoint_id = 'scope-expiry-checkpoint'",
            )
            .await,
            0,
            "final checkpoint catalog row must not be written"
        );
        assert_eq!(
            scalar(
                &conn,
                "SELECT COUNT(*) AS n FROM agent_coverage_revision WHERE checkpoint_id = 'scope-expiry-checkpoint'",
            )
            .await,
            0,
            "coverage revision must not be appended"
        );
        assert_eq!(
            scalar(
                &conn,
                "SELECT COUNT(*) AS n FROM agent_coverage_claim
                 WHERE logical_turn_key = 'scope-expiry' AND state = 'reserved_live'
                   AND owner = 'scope-owner'",
            )
            .await,
            1,
            "stale cleanup must leave the original reservation untouched"
        );
    }

    /// The reservation and abandonment transactions can both mutate claims
    /// after their initial lease check. Expire the workspace from a trigger
    /// after each mutation so the final commit fence, rather than timing,
    /// proves that neither change survives.
    #[tokio::test]
    async fn post_mutation_scope_expiry_rolls_back_coverage_reserve_and_abandon() {
        let (conn, scope) = scoped_gate_db().await;
        let session = "claude_code__scope_expiry";
        let turn = turn(
            "post-mutation-expiry",
            "coverage must stay transactional",
            Completeness::Complete,
        );

        conn.execute_raw(Statement::from_string(
            conn.get_database_backend(),
            "CREATE TRIGGER expire_scope_after_coverage_reserve
             AFTER INSERT ON agent_coverage_claim
             WHEN NEW.session_id = 'claude_code__scope_expiry'
             BEGIN
                 UPDATE workspace_record SET lease_expires_at = 0
                 WHERE workspace_id = 'coverage-gate-workspace';
             END"
            .to_string(),
        ))
        .await
        .expect("install post-reservation expiry trigger");
        let error = reserve_live_turn_claims_with_capture_scope(
            &conn,
            &scope,
            session,
            std::slice::from_ref(&turn),
            "post-mutation-owner",
            1,
        )
        .await
        .expect_err("expiry after claim insert must roll back reservation");
        assert!(format!("{error:#}").contains("workspace lease"));
        assert_eq!(
            scalar(
                &conn,
                "SELECT COUNT(*) AS n FROM agent_coverage_claim
                 WHERE logical_turn_key = 'post-mutation-expiry'",
            )
            .await,
            0,
            "post-reservation expiry must not leave a coverage claim"
        );
        scope
            .assert_workspace_fence_live(&conn)
            .await
            .expect("post-reservation expiry trigger must roll back with the claim");
        conn.execute_raw(Statement::from_string(
            conn.get_database_backend(),
            "DROP TRIGGER expire_scope_after_coverage_reserve".to_string(),
        ))
        .await
        .expect("remove post-reservation expiry trigger");

        let reserved = reserve_live_turn_claims_with_capture_scope(
            &conn,
            &scope,
            session,
            std::slice::from_ref(&turn),
            "post-mutation-owner",
            1,
        )
        .await
        .expect("reserve claim while workspace lease is live");
        assert_eq!(reserved.reserved.len(), 1);
        conn.execute_raw(Statement::from_string(
            conn.get_database_backend(),
            "CREATE TRIGGER expire_scope_after_coverage_abandon
             AFTER UPDATE OF state ON agent_coverage_claim
             WHEN NEW.session_id = 'claude_code__scope_expiry'
               AND NEW.logical_turn_key = 'post-mutation-expiry'
               AND NEW.state = 'abandoned'
             BEGIN
                 UPDATE workspace_record SET lease_expires_at = 0
                 WHERE workspace_id = 'coverage-gate-workspace';
             END"
            .to_string(),
        ))
        .await
        .expect("install post-abandonment expiry trigger");
        let error = abandon_reserved_turn_claims_with_capture_scope(
            &conn,
            &scope,
            session,
            "post-mutation-owner",
            "live",
            2,
        )
        .await
        .expect_err("expiry after claim abandonment must roll back cleanup");
        assert!(format!("{error:#}").contains("workspace lease"));
        assert_eq!(
            scalar(
                &conn,
                "SELECT COUNT(*) AS n FROM agent_coverage_claim
                 WHERE logical_turn_key = 'post-mutation-expiry'
                   AND state = 'reserved_live' AND owner = 'post-mutation-owner'",
            )
            .await,
            1,
            "post-abandonment expiry must retain the owned reservation"
        );
        scope
            .assert_workspace_fence_live(&conn)
            .await
            .expect("post-abandonment expiry trigger must roll back with the claim");
    }

    /// ACF-08: the historical importer renews, binds and abandons its claims
    /// through these shared statements inside its own identity/attempt
    /// transaction. Each must touch only the caller's owner/fence and roll
    /// back with that transaction.
    #[tokio::test]
    async fn import_claim_transitions_share_the_callers_transaction() {
        /// `(turn, state, owner, lease_expires_at, attempt, fence)`.
        type ClaimRow = (
            String,
            String,
            Option<String>,
            Option<i64>,
            Option<String>,
            i64,
        );
        async fn import_claims(conn: &impl ConnectionTrait) -> Vec<ClaimRow> {
            conn.query_all_raw(Statement::from_string(
                conn.get_database_backend(),
                "SELECT logical_turn_key, state, owner, lease_expires_at,
                        attempt_checkpoint_id, fence_token
                 FROM agent_coverage_claim
                 WHERE session_id = 'claude_code__scope_expiry'
                 ORDER BY logical_turn_key"
                    .to_string(),
            ))
            .await
            .expect("query import claims")
            .into_iter()
            .map(|row| {
                (
                    row.try_get_by("logical_turn_key").expect("turn key"),
                    row.try_get_by("state").expect("state"),
                    row.try_get_by("owner").expect("owner"),
                    row.try_get_by("lease_expires_at").expect("lease"),
                    row.try_get_by("attempt_checkpoint_id").expect("attempt"),
                    row.try_get_by("fence_token").expect("fence"),
                )
            })
            .collect()
        }

        let (conn, scope) = scoped_gate_db().await;
        let session = "claude_code__scope_expiry";
        let owner = "import:1:transition-owner";
        let turns = [
            turn("import-a", "first imported turn", Completeness::Complete),
            turn("import-b", "second imported turn", Completeness::Complete),
        ];
        let reserved = reserve_import_turn_claims_until(
            &conn,
            &scope,
            session,
            &turns,
            owner,
            1,
            CaptureCommitDeadline::from_test_pair(
                Instant::now() + Duration::from_secs(5),
                chrono::Utc::now().timestamp_millis().saturating_add(5_000),
            ),
        )
        .await
        .expect("reserve import claims")
        .reserved;
        assert_eq!(reserved.len(), 2);
        let before = import_claims(&conn).await;
        assert!(
            before
                .iter()
                .all(|claim| claim.1 == "reserved_import" && claim.2.as_deref() == Some(owner))
        );

        // Renewal and binding are visible only through the caller's commit.
        let txn = crate::internal::db::begin_write_transaction(&conn)
            .await
            .expect("begin rolled-back attempt transaction");
        renew_import_turn_claim_leases_with_conn(&txn, session, owner, 9_000, 2)
            .await
            .expect("renew in caller transaction");
        assert!(
            bind_import_turn_claim_attempt_with_conn(&txn, session, &reserved[0], owner, "ckpt", 2)
                .await
                .expect("bind in caller transaction")
        );
        txn.rollback().await.expect("roll back attempt transaction");
        assert_eq!(import_claims(&conn).await, before);

        let txn = crate::internal::db::begin_write_transaction(&conn)
            .await
            .expect("begin attempt transaction");
        renew_import_turn_claim_leases_with_conn(&txn, session, "import:1:other", 7_000, 2)
            .await
            .expect("renew for a foreign owner");
        renew_import_turn_claim_leases_with_conn(&txn, session, owner, 9_000, 2)
            .await
            .expect("renew owned claims");
        let mut stale = reserved[0].clone();
        stale.fence_token += 1;
        for (claim, claim_owner) in [(&stale, owner), (&reserved[0], "import:1:other")] {
            assert!(
                !bind_import_turn_claim_attempt_with_conn(
                    &txn,
                    session,
                    claim,
                    claim_owner,
                    "ckpt",
                    2
                )
                .await
                .expect("refused bind"),
                "a foreign owner or stale fence must not bind the attempt"
            );
        }
        assert!(
            bind_import_turn_claim_attempt_with_conn(&txn, session, &reserved[0], owner, "ckpt", 2)
                .await
                .expect("bind owned claim")
        );
        txn.commit().await.expect("commit attempt transaction");
        let bound = import_claims(&conn).await;
        assert!(bound.iter().all(|claim| claim.3 == Some(9_000)));
        assert_eq!(bound[0].4.as_deref(), Some("ckpt"));
        assert_eq!(bound[1].4, None);

        // Abandonment releases exactly this owner's import reservations.
        let txn = crate::internal::db::begin_write_transaction(&conn)
            .await
            .expect("begin abandonment transaction");
        abandon_reserved_turn_claims_with_conn(&txn, session, "import:1:other", "import", 3)
            .await
            .expect("abandon for a foreign owner");
        abandon_reserved_turn_claims_with_conn(&txn, session, owner, "live", 3)
            .await
            .expect("abandon the live state for this owner");
        assert_eq!(import_claims(&txn).await, bound, "no claim may change yet");
        abandon_reserved_turn_claims_with_conn(&txn, session, owner, "import", 3)
            .await
            .expect("abandon owned import claims");
        txn.commit().await.expect("commit abandonment");
        let abandoned = import_claims(&conn).await;
        for (after, before) in abandoned.iter().zip(&bound) {
            assert_eq!(
                (after.1.as_str(), &after.2, after.3, &after.4, after.5),
                ("abandoned", &None, None, &None, before.5 + 1),
                "abandonment must release the lease and advance the fence"
            );
        }
        let txn = crate::internal::db::begin_write_transaction(&conn)
            .await
            .expect("begin late bind transaction");
        assert!(
            !bind_import_turn_claim_attempt_with_conn(
                &txn,
                session,
                &reserved[1],
                owner,
                "late",
                4
            )
            .await
            .expect("late bind"),
            "an abandoned reservation must reject its former owner"
        );
        assert!(
            abandon_reserved_turn_claims_with_conn(&txn, session, owner, "unknown", 4)
                .await
                .is_err(),
            "an unknown source channel must fail closed"
        );
        txn.rollback().await.expect("roll back late bind");
    }

    /// crash_after_claim_before_objects_recovers: a writer that reserved a
    /// claim and died before building objects must not block the turn — after
    /// the lease expires the next writer takes over and commits normally.
    #[tokio::test]
    async fn crash_after_claim_before_objects_recovers() {
        let conn = gate_db().await;
        let session = "claude_code__s1";
        let t = turn("u1", "hi", Completeness::Complete);

        // Crashed writer: reserved, never committed.
        let dead = reserve_live_turn_claims(&conn, session, std::slice::from_ref(&t), "dead", 0)
            .await
            .expect("reserve");
        assert_eq!(dead.reserved.len(), 1);

        // Before lease expiry the turn is in-flight (no takeover).
        let blocked =
            reserve_live_turn_claims(&conn, session, std::slice::from_ref(&t), "next", 1_000)
                .await
                .expect("reserve while leased");
        assert_eq!(blocked.skipped_inflight, 1);
        assert!(blocked.reserved.is_empty());

        // After expiry: takeover + normal commit → the turn recovers.
        let recovered =
            reserve_live_turn_claims(&conn, session, std::slice::from_ref(&t), "next", 100_000)
                .await
                .expect("takeover");
        assert_eq!(recovered.reserved.len(), 1);
        commit_reserved(
            &conn,
            session,
            "next",
            &recovered.reserved[0],
            "cp-recovered",
        )
        .await
        .expect("commit after takeover");
        let (state, revision, _) = claim_row(&conn, "u1").await;
        assert_eq!(state, "catalog_committed");
        assert_eq!(revision, 1);
    }

    /// ADR-ACF-09a: claims handed to a durable pending terminal artifact are
    /// never taken over once the ordinary 60 s lease would have expired —
    /// not by a live redelivery/resumed writer and not by an import — yet the
    /// artifact's own owner/fence still commits them on replay. The control
    /// turn proves the same clock does take over an ordinary expired claim.
    #[tokio::test]
    async fn artifact_retained_claims_survive_lease_expiry_until_replay_commit() {
        let conn = gate_db().await;
        let session = "claude_code__s1";
        let retained_turn = turn("retained", "artifact turn", Completeness::Complete);
        let control_turn = turn("control", "ordinary turn", Completeness::Complete);
        let artifact = reserve_live_turn_claims(
            &conn,
            session,
            &[retained_turn.clone(), control_turn.clone()],
            "artifact-owner",
            0,
        )
        .await
        .expect("reserve artifact turns");
        assert_eq!(artifact.reserved.len(), 2);
        let retained_claim = artifact
            .reserved
            .iter()
            .find(|claim| claim.logical_turn_key == "retained")
            .expect("retained claim reserved")
            .clone();
        retain_reserved_live_claims_for_artifact_with_conn(
            &conn,
            session,
            "artifact-owner",
            std::slice::from_ref(&retained_claim),
        )
        .await
        .expect("hand the verified claim to the artifact");
        assert_eq!(
            scalar(
                &conn,
                "SELECT COUNT(*) AS n FROM agent_coverage_claim
                 WHERE logical_turn_key = 'retained' AND state = 'reserved_live'
                   AND owner = 'artifact-owner' AND fence_token = 1
                   AND lease_expires_at = 9223372036854775807",
            )
            .await,
            1,
            "retention must mark only the verified claim"
        );
        let foreign = retain_reserved_live_claims_for_artifact_with_conn(
            &conn,
            session,
            "foreign-owner",
            std::slice::from_ref(&retained_claim),
        )
        .await
        .expect_err("a foreign owner cannot retain another writer's claim");
        assert!(format!("{foreign:#}").contains("coverage ownership changed"));

        // Far past the ordinary lease: the redelivery/resumed live writer and
        // an import both observe an in-flight retained turn, never a takeover.
        let live = reserve_live_turn_claims(
            &conn,
            session,
            &[retained_turn.clone(), control_turn.clone()],
            "redelivery-owner",
            10 * LIVE_LEASE_MS,
        )
        .await
        .expect("live reservation after the ordinary lease");
        assert_eq!(live.skipped_inflight, 1);
        assert_eq!(live.skipped_retained, 1);
        assert_eq!(live.reserved.len(), 1);
        assert_eq!(live.reserved[0].logical_turn_key, "control");
        let import = reserve_turn_claims_for_channel(
            &conn,
            session,
            std::slice::from_ref(&retained_turn),
            "import-owner",
            20 * LIVE_LEASE_MS,
            "import",
        )
        .await
        .expect("import reservation after the ordinary lease");
        assert!(import.reserved.is_empty());
        assert_eq!(import.skipped_inflight, 1);
        assert_eq!(import.skipped_retained, 1);
        assert!(import.is_inflight_only_skip());
        assert_eq!(
            scalar(
                &conn,
                "SELECT COUNT(*) AS n FROM agent_coverage_claim
                 WHERE logical_turn_key = 'retained' AND state = 'reserved_live'
                   AND owner = 'artifact-owner' AND fence_token = 1 AND revision = 0",
            )
            .await,
            1,
            "no later writer may advance the artifact's fence"
        );

        commit_reserved(
            &conn,
            session,
            "artifact-owner",
            &retained_claim,
            "cp-artifact-replay",
        )
        .await
        .expect("artifact replay commits under its original owner/fence");
        let (state, revision, fence) = claim_row(&conn, "retained").await;
        assert_eq!(state, "catalog_committed");
        assert_eq!(revision, 1);
        assert_eq!(fence, Some(1));
        assert_eq!(
            scalar(
                &conn,
                "SELECT COUNT(*) AS n FROM agent_coverage_claim
                 WHERE logical_turn_key = 'retained' AND lease_expires_at IS NULL",
            )
            .await,
            1,
            "the replay commit releases the retention sentinel"
        );
    }

    #[tokio::test]
    async fn tombstone_blocks_reservation_and_fences_reserved_commit() {
        let conn = gate_db().await;
        let session = "claude_code__s1";
        let t = turn("erase-turn", "hi", Completeness::Complete);
        let reserved =
            reserve_live_turn_claims(&conn, session, std::slice::from_ref(&t), "stale", 0)
                .await
                .expect("initial reservation");
        conn.execute_raw(Statement::from_string(
            conn.get_database_backend(),
            "INSERT INTO agent_import_tombstone (
                tombstone_id, agent_kind, provider_session_id, erased_session_id, erased_at
             ) VALUES ('t-coverage', 'claude_code', 's1', 'claude_code__s1', 1)"
                .to_string(),
        ))
        .await
        .expect("seed tombstone");

        let error = commit_reserved(&conn, session, "stale", &reserved.reserved[0], "cp-erased")
            .await
            .expect_err("final transaction must observe tombstone");
        assert!(error.to_string().contains("tombstoned"));
        let error = reserve_live_turn_claims(&conn, session, &[t], "new", 2)
            .await
            .expect_err("new reservation must observe tombstone");
        assert!(error.to_string().contains("erased"));
        let row = conn
            .query_one_raw(Statement::from_string(
                conn.get_database_backend(),
                "SELECT COUNT(*) AS n FROM agent_checkpoint WHERE checkpoint_id = 'cp-erased'"
                    .to_string(),
            ))
            .await
            .unwrap()
            .unwrap();
        assert_eq!(row.try_get_by::<i64, _>("n").unwrap(), 0);
    }

    /// live_takeover_increments_fence_and_blocks_(import|stale)_ref_cas:
    /// an expired reservation is taken over with a HIGHER fence; the stale
    /// holder's commit then fails its fence check and must roll back.
    #[tokio::test]
    async fn live_takeover_increments_fence_and_blocks_stale_commit() {
        let conn = gate_db().await;
        let session = "claude_code__s1";
        let t = turn("u1", "hi", Completeness::Complete);

        // Stale writer reserves at now=0 (lease expires at 60_000).
        let stale =
            reserve_live_turn_claims(&conn, session, std::slice::from_ref(&t), "stale-owner", 0)
                .await
                .expect("reserve");
        assert_eq!(stale.reserved.len(), 1);
        let stale_claim = stale.reserved[0].clone();
        assert_eq!(stale_claim.fence_token, 1);

        // Lease expired: a new writer takes over with fence 2.
        let fresh = reserve_live_turn_claims(
            &conn,
            session,
            std::slice::from_ref(&t),
            "fresh-owner",
            100_000,
        )
        .await
        .expect("takeover");
        assert_eq!(fresh.reserved.len(), 1);
        assert_eq!(fresh.reserved[0].fence_token, 2);

        // The stale holder's commit must fail closed (fence mismatch).
        let err = commit_reserved(&conn, session, "stale-owner", &stale_claim, "cp-stale")
            .await
            .expect_err("stale fence must be rejected");
        assert!(err.to_string().contains("fenced out"), "got: {err:#}");
        let (state, revision, fence) = claim_row(&conn, "u1").await;
        assert_eq!(state, "reserved_live");
        assert_eq!(revision, 0, "stale commit must not advance the claim");
        assert_eq!(fence, Some(2));
        // coverage_revision_atomic_current_pointer: the failed transaction
        // must leave NOTHING behind — no catalog row, no revision row; the
        // claim pointer, revision history and catalog stay consistent
        // together.
        let count = |sql: &'static str| {
            let conn = conn.clone();
            async move {
                let row = conn
                    .query_one_raw(Statement::from_string(
                        conn.get_database_backend(),
                        sql.to_string(),
                    ))
                    .await
                    .expect("count query")
                    .expect("count row");
                let n: i64 = row.try_get_by("n").expect("count");
                n
            }
        };
        assert_eq!(
            count("SELECT COUNT(*) AS n FROM agent_checkpoint WHERE checkpoint_id = 'cp-stale'")
                .await,
            0,
            "rolled-back transaction must not leave a catalog row"
        );
        assert_eq!(
            count("SELECT COUNT(*) AS n FROM agent_coverage_revision").await,
            0,
            "rolled-back transaction must not leave a revision row"
        );

        // The fresh holder commits fine.
        commit_reserved(
            &conn,
            session,
            "fresh-owner",
            &fresh.reserved[0],
            "cp-fresh",
        )
        .await
        .expect("fresh commit");
        let (state, revision, _) = claim_row(&conn, "u1").await;
        assert_eq!(state, "catalog_committed");
        assert_eq!(revision, 1);
    }

    /// ordinal_source_reorder_conflicts: a committed complete turn whose
    /// content changes (e.g. source reorder under an ordinal key) parks the
    /// claim as `conflicted` — never silently re-covered.
    #[tokio::test]
    async fn committed_complete_content_change_conflicts() {
        let conn = gate_db().await;
        let session = "claude_code__s1";
        let original = turn("ordinal:0", "first", Completeness::Complete);
        let reserved = reserve_live_turn_claims(&conn, session, &[original], "w1", 0)
            .await
            .expect("reserve");
        commit_reserved(&conn, session, "w1", &reserved.reserved[0], "cp1")
            .await
            .expect("commit");

        // Reordered/rewritten source: same logical key, different complete
        // content.
        let secret_key = "AKIAIOSFODNN7EXAMPLE";
        let secret_value = format!("sk-ant-{}", "a".repeat(40));
        let mut secret_input = std::collections::BTreeMap::new();
        secret_input.insert(
            secret_key.to_string(),
            crate::internal::ai::observed_agents::CanonValue::Str(secret_value.clone()),
        );
        let reordered = NormalizedTurn {
            logical_turn_key: "ordinal:0".to_string(),
            ordinal: 0,
            completeness: Completeness::Complete,
            started_at: None,
            ended_at: None,
            records: vec![SemanticRecord::ToolCall {
                call_id: Some("call-2".to_string()),
                input: crate::internal::ai::observed_agents::CanonValue::Object(secret_input),
                name: "inspect".to_string(),
            }],
        };
        let outcome = reserve_live_turn_claims(
            &conn,
            session,
            std::slice::from_ref(&reordered),
            "w2",
            1_000,
        )
        .await
        .expect("reserve conflict");
        assert!(outcome.reserved.is_empty());
        assert_eq!(outcome.conflicted, 1);
        let (state, revision, _) = claim_row(&conn, "ordinal:0").await;
        assert_eq!(state, "conflicted");
        assert_eq!(revision, 1, "committed revision is preserved for doctor");
        let conflict = conn
            .query_one_raw(Statement::from_string(
                conn.get_database_backend(),
                "SELECT incumbent_revision, incumbent_digest, incoming_digest,
                        incoming_source_channel, incoming_canonical_json,
                        incoming_redaction_report_json
                 FROM agent_coverage_conflict"
                    .to_string(),
            ))
            .await
            .expect("query conflict evidence")
            .expect("conflict evidence row");
        assert_eq!(
            conflict.try_get_by::<i64, _>("incumbent_revision").unwrap(),
            1
        );
        let mut expected = reordered.clone();
        let expected_report = redact_turns_with_report(std::slice::from_mut(&mut expected));
        let incoming_digest = conflict.try_get_by::<String, _>("incoming_digest").unwrap();
        assert_eq!(incoming_digest, expected.digest_hex());
        assert_eq!(
            conflict
                .try_get_by::<String, _>("incoming_source_channel")
                .unwrap(),
            "live"
        );
        let canonical = conflict
            .try_get_by::<String, _>("incoming_canonical_json")
            .unwrap();
        assert!(!canonical.contains(secret_key));
        assert!(!canonical.contains(&secret_value));
        assert!(canonical.contains("redacted-key-sha256-"));
        assert!(canonical.contains("<REDACTED:anthropic-api-key>"));
        assert_eq!(
            canonical.as_bytes(),
            canonical_turn_bytes(&expected.records),
            "stored candidate must be the exact sanitized payload hashed by the digest"
        );
        let stored_report: serde_json::Value = serde_json::from_str(
            &conflict
                .try_get_by::<String, _>("incoming_redaction_report_json")
                .unwrap(),
        )
        .unwrap();
        assert_eq!(
            stored_report,
            serde_json::to_value(&expected_report).unwrap()
        );
        assert!(expected_report.bytes_redacted > 0);
        assert!(
            expected_report
                .matches
                .iter()
                .any(|entry| entry.rule_id == "aws-access-key-id")
        );
        assert!(
            expected_report
                .matches
                .iter()
                .any(|entry| entry.rule_id == "anthropic-api-key")
        );
    }

    /// shared_live_snapshot_upgrade_keeps_other_turns_visible (claim level):
    /// upgrading ONE turn of a multi-turn snapshot leaves the other turns'
    /// committed claims and revisions untouched.
    #[tokio::test]
    async fn upgrading_one_turn_leaves_other_claims_untouched() {
        let conn = gate_db().await;
        let session = "claude_code__s1";
        let t1 = turn("u1", "one", Completeness::Complete);
        let t2 = turn("u2", "two", Completeness::Incomplete);

        let first = reserve_live_turn_claims(&conn, session, &[t1.clone(), t2], "w1", 0)
            .await
            .expect("reserve both");
        assert_eq!(first.reserved.len(), 2);
        for claim in &first.reserved {
            commit_reserved(&conn, session, "w1", claim, "cp1")
                .await
                .expect("commit");
        }

        // Second snapshot: t1 unchanged (skip), t2 now complete (upgrade).
        let t2_complete = turn("u2", "two done", Completeness::Complete);
        let second = reserve_live_turn_claims(&conn, session, &[t1, t2_complete], "w2", 1_000)
            .await
            .expect("reserve upgrade");
        assert_eq!(second.skipped_covered, 1, "t1 already covered");
        assert_eq!(second.reserved.len(), 1, "only t2 upgrades");
        commit_reserved(&conn, session, "w2", &second.reserved[0], "cp2")
            .await
            .expect("commit upgrade");

        let (s1, r1, _) = claim_row(&conn, "u1").await;
        assert_eq!((s1.as_str(), r1), ("catalog_committed", 1), "t1 untouched");
        let (s2, r2, _) = claim_row(&conn, "u2").await;
        assert_eq!((s2.as_str(), r2), ("catalog_committed", 2), "t2 advanced");
        // Both checkpoints remain in the catalog — no checkpoint-level
        // supersede (ADR-DR-16).
        let rows = conn
            .query_all_raw(Statement::from_string(
                conn.get_database_backend(),
                "SELECT checkpoint_id FROM agent_checkpoint".to_string(),
            ))
            .await
            .expect("checkpoints");
        assert_eq!(rows.len(), 2);
    }
}
