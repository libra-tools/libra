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
//! transaction back (ref update included).

use anyhow::{Context, Result, bail};
use async_trait::async_trait;
use sea_orm::{ConnectionTrait, DatabaseConnection, DatabaseTransaction, Statement};

use crate::internal::ai::{
    history::{TracesCommitCtx, TracesTxnExtra},
    observed_agents::{COVERAGE_SCHEMA_VERSION, Completeness, NormalizedTurn},
};

/// Live reservation lease length. Generous relative to a single hook write
/// (sub-second) so takeover only fires on genuinely dead writers.
const LIVE_LEASE_MS: i64 = 60_000;

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

/// Outcome of a live reservation pass over one snapshot's normalized turns.
#[derive(Debug, Default)]
pub struct LiveReservationOutcome {
    pub reserved: Vec<ReservedTurnClaim>,
    /// Turns already covered by equivalent-or-better committed content.
    pub skipped_covered: usize,
    /// Turns currently reserved by another live writer (unexpired lease).
    pub skipped_inflight: usize,
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
}

async fn read_claim(
    conn: &impl ConnectionTrait,
    session_id: &str,
    logical_turn_key: &str,
) -> Result<Option<ExistingClaim>> {
    let row = conn
        .query_one(Statement::from_sql_and_values(
            conn.get_database_backend(),
            "SELECT coverage_digest, completeness, revision, state,
                    lease_expires_at, fence_token
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
    let result = conn
        .execute(Statement::from_sql_and_values(
            conn.get_database_backend(),
            "INSERT INTO agent_coverage_claim (
                session_id, logical_turn_key, coverage_schema_version,
                coverage_digest, completeness, revision, state,
                owner, lease_expires_at, fence_token, source_channel,
                created_at, updated_at
             ) VALUES (?, ?, ?, ?, ?, 0, 'reserved_live', ?, ?, 1, ?, ?, ?)
             ON CONFLICT(session_id, logical_turn_key, coverage_schema_version)
             DO NOTHING",
            [
                session_id.into(),
                turn.logical_turn_key.clone().into(),
                COVERAGE_SCHEMA_VERSION.into(),
                digest.into(),
                turn.completeness.as_db_str().into(),
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
    let result = conn
        .execute(Statement::from_sql_and_values(
            conn.get_database_backend(),
            "UPDATE agent_coverage_claim
             SET state = 'reserved_live', owner = ?, lease_expires_at = ?,
                 fence_token = ?, coverage_digest = ?, completeness = ?,
                 source_channel = ?, updated_at = ?
             WHERE session_id = ? AND logical_turn_key = ?
               AND coverage_schema_version = ?
               AND state = ? AND fence_token IS ?",
            [
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

/// Mark a committed-complete-vs-different-complete collision `conflicted`
/// (ADR-DR-09: never silently overwrite committed complete content).
async fn try_mark_conflicted(
    conn: &impl ConnectionTrait,
    session_id: &str,
    logical_turn_key: &str,
    expected_digest: &str,
    now_ms: i64,
) -> Result<bool> {
    let result = conn
        .execute(Statement::from_sql_and_values(
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
                expected_digest.into(),
            ],
        ))
        .await
        .context("mark agent_coverage_claim conflicted")?;
    Ok(result.rows_affected() == 1)
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
    reserve_turn_claims_for_channel(conn, session_id, turns, owner, now_ms, "live").await
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
    let mut outcome = LiveReservationOutcome::default();
    for turn in turns {
        let digest = turn.digest_hex();
        let mut rounds = 0;
        loop {
            rounds += 1;
            let existing = read_claim(conn, session_id, &turn.logical_turn_key).await?;
            let decision = decide_and_attempt(
                conn,
                session_id,
                turn,
                &digest,
                owner,
                now_ms,
                existing,
                source_channel,
            )
            .await?;
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
    }
    Ok(outcome)
}

enum AttemptOutcome {
    Reserved(ReservedTurnClaim),
    SkipCovered,
    SkipInflight,
    Conflicted,
    LostRace,
}

#[allow(clippy::too_many_arguments)]
async fn decide_and_attempt(
    conn: &DatabaseConnection,
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
            if existing.coverage_digest == digest {
                // Byte-identical content (any completeness): pure no-op.
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
                    let marked = try_mark_conflicted(
                        conn,
                        session_id,
                        &turn.logical_turn_key,
                        &existing.coverage_digest,
                        now_ms,
                    )
                    .await?;
                    Ok(if marked {
                        AttemptOutcome::Conflicted
                    } else {
                        AttemptOutcome::LostRace
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
            let lease_live = existing.lease_expires_at.is_some_and(|t| t > now_ms);
            if lease_live {
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
}

#[async_trait]
impl TracesTxnExtra for LiveClaimCommitPlan {
    async fn apply(&self, txn: &DatabaseTransaction, ctx: &TracesCommitCtx) -> Result<()> {
        // Catalog row first (claim advance references checkpoint_id). The
        // `ON CONFLICT DO NOTHING` backstop keeps a crash-retry idempotent —
        // but within one transaction the row is always fresh.
        txn.execute(Statement::from_sql_and_values(
            txn.get_database_backend(),
            "INSERT INTO agent_checkpoint (
                checkpoint_id, session_id, scope, parent_commit, tree_oid,
                metadata_blob_oid, traces_commit, created_at
             ) VALUES (?, ?, 'committed', ?, ?, ?, ?, ?)
             ON CONFLICT(checkpoint_id) DO NOTHING",
            [
                self.checkpoint_id.clone().into(),
                self.session_id.clone().into(),
                self.parent_commit.clone().into(),
                ctx.tree_oid.clone().into(),
                ctx.metadata_blob_oid.clone().into(),
                ctx.commit_hash.clone().into(),
                self.created_at.into(),
            ],
        ))
        .await
        .context("insert agent_checkpoint row in ref transaction")?;

        for claim in &self.claims {
            // Append-only revision history (ADR-DR-16).
            txn.execute(Statement::from_sql_and_values(
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
                .execute(Statement::from_sql_and_values(
                    txn.get_database_backend(),
                    "UPDATE agent_coverage_claim
                     SET state = 'catalog_committed', revision = ?,
                         coverage_digest = ?, completeness = ?,
                         checkpoint_id = ?, traces_commit = ?,
                         owner = NULL, lease_expires_at = NULL, updated_at = ?
                     WHERE session_id = ? AND logical_turn_key = ?
                       AND coverage_schema_version = ?
                       AND state = 'reserved_live'
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
        ai::observed_agents::SemanticRecord, db::migration::run_builtin_migrations,
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

    async fn gate_db() -> DatabaseConnection {
        let conn = Database::connect("sqlite::memory:").await.expect("mem db");
        // The migration set assumes the bootstrap schema (ai_thread etc.)
        // exists; this unit fixture only needs the capture/coverage tables,
        // so relax FK enforcement instead of replaying the full bootstrap.
        conn.execute(Statement::from_string(
            conn.get_database_backend(),
            "PRAGMA foreign_keys = OFF".to_string(),
        ))
        .await
        .expect("pragma");
        run_builtin_migrations(&conn).await.expect("migrations");
        // FK target for claims.
        conn.execute(Statement::from_string(
            conn.get_database_backend(),
            "INSERT INTO agent_session (
                session_id, agent_kind, provider_session_id, state, working_dir,
                metadata_json, redaction_report, started_at, last_event_at, schema_version
             ) VALUES ('claude_code__s1', 'claude_code', 's1', 'active', '/tmp',
                       '{}', '{}', 0, 0, 1)"
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
            records: vec![SemanticRecord::User {
                text: text.to_string(),
            }],
        }
    }

    async fn claim_row(conn: &DatabaseConnection, key: &str) -> (String, i64, Option<i64>) {
        let row = conn
            .query_one(Statement::from_sql_and_values(
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
        txn.execute(Statement::from_sql_and_values(
            txn.get_database_backend(),
            "INSERT OR IGNORE INTO agent_checkpoint (
                checkpoint_id, session_id, scope, parent_commit, tree_oid,
                metadata_blob_oid, traces_commit, created_at
             ) VALUES (?, ?, 'committed', NULL, 't', 'm', ?, 0)",
            [
                checkpoint_id.into(),
                session_id.into(),
                format!("commit-{checkpoint_id}").into(),
            ],
        ))
        .await?;
        let plan = LiveClaimCommitPlan {
            source_channel: "live",
            session_id: session_id.to_string(),
            checkpoint_id: checkpoint_id.to_string(),
            owner: owner.to_string(),
            parent_commit: None,
            created_at: 0,
            now_ms: 1,
            claims: vec![claim.clone()],
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
                    .query_one(Statement::from_string(
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
        let reordered = turn("ordinal:0", "second", Completeness::Complete);
        let outcome = reserve_live_turn_claims(&conn, session, &[reordered], "w2", 1_000)
            .await
            .expect("reserve conflict");
        assert!(outcome.reserved.is_empty());
        assert_eq!(outcome.conflicted, 1);
        let (state, revision, _) = claim_row(&conn, "ordinal:0").await;
        assert_eq!(state, "conflicted");
        assert_eq!(revision, 1, "committed revision is preserved for doctor");
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
            .query_all(Statement::from_string(
                conn.get_database_backend(),
                "SELECT checkpoint_id FROM agent_checkpoint".to_string(),
            ))
            .await
            .expect("checkpoints");
        assert_eq!(rows.len(), 2);
    }
}
