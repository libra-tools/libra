//! Bridge durable storage (plan-20260818 LB-02 / ADR-LB-02 / ADR-LB-03).
//!
//! Row types and service operations over the `agent_bridge_*` tables created
//! by migration `2026081801`. This is the server-side durable projection
//! (GC-LB-09): a bridge ack only means these rows are committed, never that
//! the Harness raw transcript has been taken over by Libra.
//!
//! All idempotency and fail-closed semantics live here so LB-03 (ingress) can
//! build on a correct store: `(bridge_session_id, event_seq)` and
//! `operation_id` retries are no-ops when the digest matches and hard
//! conflicts when it does not; scope is always supplied by the caller from
//! the trusted handshake/context, never self-reported.

use sea_orm::{ConnectionTrait, DatabaseBackend, DbErr, Statement, Value};
use sha2::{Digest, Sha256};

use super::protocol::SOURCE_DEEPSEEK_HARNESS;

/// 256 KiB — the v1 per-event payload cap (mirrors `MAX_EVENT_BYTES`).
pub const MAX_EVENT_PAYLOAD_BYTES: usize = 256 * 1024;

/// SHA-256 hex digest of a payload (lowercase, 64 chars).
pub fn sha256_hex(data: &[u8]) -> String {
    let mut hasher = Sha256::new();
    hasher.update(data);
    hex(hasher.finalize().as_slice())
}

fn hex(bytes: &[u8]) -> String {
    let mut s = String::with_capacity(bytes.len() * 2);
    for b in bytes {
        use std::fmt::Write;
        let _ = write!(s, "{b:02x}");
    }
    s
}

// ---------------------------------------------------------------------------
// Row types
// ---------------------------------------------------------------------------

#[derive(Debug, Clone)]
pub struct BridgeSessionRow {
    pub bridge_session_id: String,
    pub repository_id: String,
    pub worktree_id: Option<String>,
    pub workspace_id: Option<String>,
    pub parent_session_id: Option<String>,
    pub agent_id: Option<String>,
    pub session_state: String,
    pub last_event_seq: i64,
    pub last_acked_seq: i64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum EventStatus {
    /// Newly persisted.
    Accepted,
    /// A replay whose digest matches the existing row — no-op success.
    Duplicate,
    /// A replay at a used sequence whose digest differs — fail-closed.
    DigestConflict,
    /// Rejected for a validation reason (oversize, bad schema).
    Rejected,
}

#[derive(Debug, Clone)]
pub struct AppendOutcome {
    pub per_event: Vec<EventStatus>,
    pub last_acked_seq: i64,
}

// ---------------------------------------------------------------------------
// Session lifecycle
// ---------------------------------------------------------------------------

/// Create (or return the existing) bridge session. Idempotent by
/// `bridge_session_id`; a conflicting scope never overwrites an existing row
/// (GC-LB-06). Returns the row plus a bool indicating it was newly created.
// The explicit scope/context parameters (repository/worktree/workspace/
// parent/agent/now) are intentional: the bridge never trusts a self-reported
// scope, so each is passed in from the verified handshake/context rather than
// bundled into a caller-controlled struct.
#[allow(clippy::too_many_arguments)]
pub async fn open_session<C: ConnectionTrait>(
    conn: &C,
    bridge_session_id: &str,
    repository_id: &str,
    worktree_id: Option<&str>,
    workspace_id: Option<&str>,
    parent_session_id: Option<&str>,
    agent_id: Option<&str>,
    now_ms: i64,
) -> Result<(BridgeSessionRow, bool), DbErr> {
    let existing = get_session(conn, bridge_session_id).await?;
    if let Some(row) = existing {
        // Idempotent open: return the existing row. A scope mismatch is the
        // caller's job to detect before calling (LB-03 checks it fail-closed).
        return Ok((row, false));
    }

    let stmt = Statement::from_sql_and_values(
        DatabaseBackend::Sqlite,
        "INSERT INTO agent_bridge_session \
         (bridge_session_id, source, repository_id, worktree_id, workspace_id, \
          parent_session_id, agent_id, session_state, schema_version, \
          last_event_seq, last_acked_seq, created_at, updated_at) \
         VALUES (?, ?, ?, ?, ?, ?, ?, 'open', 1, 0, 0, ?, ?) \
         ON CONFLICT(bridge_session_id) DO NOTHING",
        [
            bridge_session_id.into(),
            SOURCE_DEEPSEEK_HARNESS.into(),
            repository_id.into(),
            worktree_id.map(Value::from).unwrap_or(Value::String(None)),
            workspace_id.map(Value::from).unwrap_or(Value::String(None)),
            parent_session_id
                .map(Value::from)
                .unwrap_or(Value::String(None)),
            agent_id.map(Value::from).unwrap_or(Value::String(None)),
            now_ms.into(),
            now_ms.into(),
        ],
    );
    // ON CONFLICT(bridge_session_id) DO NOTHING makes the open idempotent even
    // under a concurrent duplicate open: a loser's INSERT matches zero rows
    // instead of surfacing a raw unique-constraint DbErr, so a client retry
    // racing the original request gets the existing session back (matching
    // the documented idempotent contract). `rows_affected()` distinguishes the
    // winner (1 row inserted -> created) from a loser (0 rows -> not created);
    // this matters because the caller gates the fail-closed scope check on
    // `created` — a loser that reported `created: true` would silently attach
    // to the winner's scope instead of surfacing a scope mismatch.
    let affected = conn.execute_raw(stmt).await?;
    let created = affected.rows_affected() > 0;

    let row = get_session(conn, bridge_session_id).await?.ok_or_else(|| {
        DbErr::Custom(
            "bridge session insert was not durable: read-after-write returned no row".into(),
        )
    })?;
    Ok((row, created))
}

pub async fn get_session<C: ConnectionTrait>(
    conn: &C,
    bridge_session_id: &str,
) -> Result<Option<BridgeSessionRow>, DbErr> {
    let stmt = Statement::from_sql_and_values(
        DatabaseBackend::Sqlite,
        "SELECT bridge_session_id, repository_id, worktree_id, workspace_id, \
                parent_session_id, agent_id, session_state, last_event_seq, last_acked_seq \
         FROM agent_bridge_session WHERE bridge_session_id = ?",
        [bridge_session_id.into()],
    );
    let rows = conn.query_all_raw(stmt).await?;
    Ok(rows.into_iter().next().map(|row| BridgeSessionRow {
        bridge_session_id: row
            .try_get::<String>("", "bridge_session_id")
            .unwrap_or_default(),
        repository_id: row
            .try_get::<String>("", "repository_id")
            .unwrap_or_default(),
        worktree_id: row
            .try_get::<Option<String>>("", "worktree_id")
            .ok()
            .flatten(),
        workspace_id: row
            .try_get::<Option<String>>("", "workspace_id")
            .ok()
            .flatten(),
        parent_session_id: row
            .try_get::<Option<String>>("", "parent_session_id")
            .ok()
            .flatten(),
        agent_id: row.try_get::<Option<String>>("", "agent_id").ok().flatten(),
        session_state: row
            .try_get::<String>("", "session_state")
            .unwrap_or_default(),
        last_event_seq: row.try_get::<i64>("", "last_event_seq").unwrap_or(0),
        last_acked_seq: row.try_get::<i64>("", "last_acked_seq").unwrap_or(0),
    }))
}

/// Transition a session to a terminal/visible state (`flushing`/`closed`).
/// `flushing` is entered before a bounded drain; `closed` is final.
pub async fn set_session_state<C: ConnectionTrait>(
    conn: &C,
    bridge_session_id: &str,
    state: &str,
    now_ms: i64,
) -> Result<(), DbErr> {
    let stmt = Statement::from_sql_and_values(
        DatabaseBackend::Sqlite,
        "UPDATE agent_bridge_session SET session_state = ?, updated_at = ? \
         WHERE bridge_session_id = ?",
        [state.into(), now_ms.into(), bridge_session_id.into()],
    );
    conn.execute_raw(stmt).await?;
    Ok(())
}

/// Advance the highest acked sequence. Only monotonic (never rewinds).
pub async fn mark_acked<C: ConnectionTrait>(
    conn: &C,
    bridge_session_id: &str,
    seq: i64,
    now_ms: i64,
) -> Result<(), DbErr> {
    let stmt = Statement::from_sql_and_values(
        DatabaseBackend::Sqlite,
        "UPDATE agent_bridge_session \
         SET last_acked_seq = MAX(last_acked_seq, ?), updated_at = ? \
         WHERE bridge_session_id = ?",
        [seq.into(), now_ms.into(), bridge_session_id.into()],
    );
    conn.execute_raw(stmt).await?;
    Ok(())
}

// ---------------------------------------------------------------------------
// Event append (the idempotent/conflict core, consumed by LB-03)
// ---------------------------------------------------------------------------

/// Append a batch of events idempotently. For each event:
/// - an unused `(bridge_session_id, event_seq)` is inserted -> `Accepted`;
/// - an existing sequence with the SAME digest is a no-op -> `Duplicate`;
/// - an existing sequence with a DIFFERENT digest -> `DigestConflict`
///   (fail-closed; the batch is not partially claimed as success).
///
/// Returns per-event status plus the session's highest acked sequence so far.
pub async fn append_events<C: ConnectionTrait>(
    conn: &C,
    bridge_session_id: &str,
    events: &[AppendEvent],
    now_ms: i64,
) -> Result<AppendOutcome, DbErr> {
    let mut statuses = Vec::with_capacity(events.len());
    for ev in events {
        // Validate bounded payload before any write (GC-LB-11 / ADR-LB-03).
        if ev.payload.len() > MAX_EVENT_PAYLOAD_BYTES {
            statuses.push(EventStatus::Rejected);
            continue;
        }
        let digest = sha256_hex(ev.payload.as_bytes());

        // Try insert; on unique-conflict the DO NOTHING leaves the row intact
        // and reports 0 affected rows.
        let insert = Statement::from_sql_and_values(
            DatabaseBackend::Sqlite,
            "INSERT INTO agent_bridge_event \
             (bridge_session_id, event_seq, event_type, payload_sha256, payload, operation_id, created_at) \
             VALUES (?, ?, ?, ?, ?, ?, ?) \
             ON CONFLICT(bridge_session_id, event_seq) DO NOTHING",
            [
                bridge_session_id.into(),
                ev.event_seq.into(),
                ev.event_type.clone().into(),
                digest.clone().into(),
                ev.payload.clone().into(),
                ev.operation_id
                    .clone()
                    .map(Value::from)
                    .unwrap_or(Value::String(None)),
                now_ms.into(),
            ],
        );
        let affected = conn.execute_raw(insert).await?;

        if affected.rows_affected() > 0 {
            statuses.push(EventStatus::Accepted);
            continue;
        }

        // Existing row: compare digest to distinguish replay from conflict.
        let existing = get_event_digest(conn, bridge_session_id, ev.event_seq).await?;
        match existing {
            Some(existing_digest) if existing_digest == digest => {
                statuses.push(EventStatus::Duplicate);
            }
            Some(_) => statuses.push(EventStatus::DigestConflict),
            None => {
                // Row disappeared between the conflict and the read (should
                // not happen inside one connection's transaction).
                statuses.push(EventStatus::Rejected);
            }
        }
    }

    // Track the highest event sequence present in the batch (accepted or an
    // idempotent replay) so `last_event_seq` reflects reality; it is
    // independent of the contiguous ack cursor (`last_acked_seq`).
    let max_present = events
        .iter()
        .zip(statuses.iter())
        .filter(|(_, s)| matches!(s, EventStatus::Accepted | EventStatus::Duplicate))
        .map(|(e, _)| e.event_seq)
        .max();
    if let Some(seq) = max_present {
        let stmt = Statement::from_sql_and_values(
            DatabaseBackend::Sqlite,
            "UPDATE agent_bridge_session \
             SET last_event_seq = MAX(last_event_seq, ?), updated_at = ? \
             WHERE bridge_session_id = ?",
            [seq.into(), now_ms.into(), bridge_session_id.into()],
        );
        conn.execute_raw(stmt).await?;
    }

    let last_acked = get_session(conn, bridge_session_id)
        .await?
        .map(|s| s.last_acked_seq)
        .unwrap_or(0);

    Ok(AppendOutcome {
        per_event: statuses,
        last_acked_seq: last_acked,
    })
}

#[derive(Debug, Clone)]
pub struct AppendEvent {
    pub event_seq: i64,
    pub event_type: String,
    pub payload: String,
    pub operation_id: Option<String>,
}

async fn get_event_digest<C: ConnectionTrait>(
    conn: &C,
    bridge_session_id: &str,
    event_seq: i64,
) -> Result<Option<String>, DbErr> {
    let stmt = Statement::from_sql_and_values(
        DatabaseBackend::Sqlite,
        "SELECT payload_sha256 FROM agent_bridge_event \
         WHERE bridge_session_id = ? AND event_seq = ?",
        [bridge_session_id.into(), event_seq.into()],
    );
    let rows = conn.query_all_raw(stmt).await?;
    Ok(rows
        .into_iter()
        .next()
        .and_then(|r| r.try_get::<String>("", "payload_sha256").ok()))
}

/// Query the highest accepted sequence for a session (the durable cursor a
/// peer resumes from).
pub async fn last_accepted_seq<C: ConnectionTrait>(
    conn: &C,
    bridge_session_id: &str,
) -> Result<i64, DbErr> {
    let stmt = Statement::from_sql_and_values(
        DatabaseBackend::Sqlite,
        "SELECT COALESCE(MAX(event_seq), 0) AS last_seq FROM agent_bridge_event \
         WHERE bridge_session_id = ?",
        [bridge_session_id.into()],
    );
    let rows = conn.query_all_raw(stmt).await?;
    Ok(rows
        .into_iter()
        .next()
        .and_then(|r| r.try_get::<i64>("", "last_seq").ok())
        .unwrap_or(0))
}

/// Compute the highest contiguous accepted sequence (the durable ack cursor):
/// the largest `n` such that every sequence `1..=n` has an event row. A gap in
/// the middle stops the cursor so a peer never believes a later event is acked
/// when an earlier one is missing.
pub async fn contiguous_acked_seq<C: ConnectionTrait>(
    conn: &C,
    bridge_session_id: &str,
) -> Result<i64, DbErr> {
    let stmt = Statement::from_sql_and_values(
        DatabaseBackend::Sqlite,
        "SELECT event_seq FROM agent_bridge_event \
         WHERE bridge_session_id = ? ORDER BY event_seq ASC",
        [bridge_session_id.into()],
    );
    let rows = conn.query_all_raw(stmt).await?;
    let mut expect = 1i64;
    for row in rows {
        let seq: i64 = row.try_get("", "event_seq").unwrap_or(0);
        if seq == expect {
            expect += 1;
        } else if seq > expect {
            // Gap: stop at the last contiguous value.
            break;
        }
        // seq < expect is a duplicate — already counted; continue.
    }
    Ok(expect - 1)
}

// ---------------------------------------------------------------------------
// Operation registry (idempotent by operation_id + digest conflict)
// ---------------------------------------------------------------------------

/// Record a bridge operation. Returns `Ok((true, None))` on insert, or
/// `Ok((false, Some(existing_digest)))` when the same `operation_id` was
/// already recorded (the caller compares digests to decide replay vs
/// conflict).
// Explicit scope parameters as in `open_session` (GC-LB-06).
#[allow(clippy::too_many_arguments)]
pub async fn upsert_operation<C: ConnectionTrait>(
    conn: &C,
    operation_id: &str,
    bridge_session_id: &str,
    method: &str,
    params_digest: &str,
    repository_id: &str,
    workspace_id: Option<&str>,
    now_ms: i64,
) -> Result<(bool, Option<String>), DbErr> {
    let insert = Statement::from_sql_and_values(
        DatabaseBackend::Sqlite,
        "INSERT INTO agent_bridge_operation \
         (operation_id, bridge_session_id, method, params_digest, repository_id, workspace_id, \
          status, created_at) \
         VALUES (?, ?, ?, ?, ?, ?, 'pending', ?) \
         ON CONFLICT(operation_id) DO NOTHING",
        [
            operation_id.into(),
            bridge_session_id.into(),
            method.into(),
            params_digest.into(),
            repository_id.into(),
            workspace_id.map(Value::from).unwrap_or(Value::String(None)),
            now_ms.into(),
        ],
    );
    let affected = conn.execute_raw(insert).await?;
    if affected.rows_affected() > 0 {
        return Ok((true, None));
    }
    let stmt = Statement::from_sql_and_values(
        DatabaseBackend::Sqlite,
        "SELECT params_digest FROM agent_bridge_operation WHERE operation_id = ?",
        [operation_id.into()],
    );
    let rows = conn.query_all_raw(stmt).await?;
    let existing = rows
        .into_iter()
        .next()
        .and_then(|r| r.try_get::<String>("", "params_digest").ok());
    Ok((false, existing))
}

/// Mark an operation applied (with its result digest) or failed.
pub async fn finish_operation<C: ConnectionTrait>(
    conn: &C,
    operation_id: &str,
    status: &str,
    result_digest: Option<&str>,
    now_ms: i64,
) -> Result<(), DbErr> {
    let stmt = Statement::from_sql_and_values(
        DatabaseBackend::Sqlite,
        "UPDATE agent_bridge_operation SET status = ?, result_digest = ?, completed_at = ? \
         WHERE operation_id = ?",
        [
            status.into(),
            result_digest
                .map(Value::from)
                .unwrap_or(Value::String(None)),
            now_ms.into(),
            operation_id.into(),
        ],
    );
    conn.execute_raw(stmt).await?;
    Ok(())
}

/// Read an operation's current status (for audit/observability and tests).
pub async fn get_operation_status<C: ConnectionTrait>(
    conn: &C,
    operation_id: &str,
) -> Result<Option<String>, DbErr> {
    let stmt = Statement::from_sql_and_values(
        DatabaseBackend::Sqlite,
        "SELECT status FROM agent_bridge_operation WHERE operation_id = ?",
        [operation_id.into()],
    );
    let rows = conn.query_all_raw(stmt).await?;
    Ok(rows
        .into_iter()
        .next()
        .and_then(|r| r.try_get::<String>("", "status").ok()))
}

/// Read an operation's `(status, result_digest)` pair.
///
/// A replayed non-idempotent mutation (`review.run`) uses this to return the
/// ORIGINAL result instead of executing a second time — the failure-recovery
/// contract for "mutation applied but the response was lost" (plan-20260818
/// 故障恢复矩阵: "以 operation id 查询并返回原结果").
pub async fn get_operation_result<C: ConnectionTrait>(
    conn: &C,
    operation_id: &str,
) -> Result<Option<(String, Option<String>)>, DbErr> {
    let stmt = Statement::from_sql_and_values(
        DatabaseBackend::Sqlite,
        "SELECT status, result_digest FROM agent_bridge_operation WHERE operation_id = ?",
        [operation_id.into()],
    );
    let rows = conn.query_all_raw(stmt).await?;
    Ok(rows.into_iter().next().map(|r| {
        (
            r.try_get::<String>("", "status").unwrap_or_default(),
            r.try_get::<Option<String>>("", "result_digest")
                .ok()
                .flatten(),
        )
    }))
}

/// Outcome of an association-link insert, mirroring the event/operation
/// idempotency + conflict discipline (GC-LB-09): a retry that points the same
/// source at the same target is a success no-op; pointing it at a DIFFERENT
/// target is a conflict and must fail closed, never silently overwrite.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LinkOutcome {
    /// The link was newly created.
    Inserted,
    /// The link already pointed at exactly this target — idempotent no-op.
    Existing,
    /// The source already points at a different target; the requested retarget
    /// was refused (no last-writer-wins).
    Conflict { stored_target_id: String },
}

/// How many targets one association source may point at.
///
/// The `agent_bridge_link` table stores edges keyed by
/// `(source_type, source_id, target_type, target_id)`, so the *cardinality* of
/// a relation is a service-level decision. Getting it wrong in either direction
/// is a real defect: too strict and legitimate provenance (a checkpoint's
/// several evidence ids) is refused as a conflict; too loose and a silent
/// retarget replaces the recorded truth.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LinkSingularity {
    /// At most one target for this source, whatever its type. Used by
    /// `evidence.append` / `provenance.append`, whose contract is that one
    /// evidence source names exactly one object.
    BySource,
    /// At most one target per relation kind: a mutation result has one
    /// operation, one workspace, one parent session — but the relation kinds
    /// coexist.
    ByRelation,
    /// Many targets are legitimate (a result's evidence ids).
    Multi,
}

/// Insert an association link idempotently, honouring the relation's
/// cardinality.
///
/// Re-inserting exactly the same edge is always an idempotent no-op. Pointing a
/// *singular* relation at a different target is reported as
/// `LinkOutcome::Conflict` and never silently overwrites (GC-LB-09).
///
/// # Arguments
///
/// * `singularity` - How many targets this relation may have.
///
/// # Returns
///
/// `Inserted` for a new edge, `Existing` for an exact replay, or `Conflict`
/// carrying the target already recorded for a singular relation.
// Explicit scope parameters as in `open_session` (GC-LB-06).
#[allow(clippy::too_many_arguments)]
pub async fn insert_link<C: ConnectionTrait>(
    conn: &C,
    bridge_session_id: &str,
    source_type: &str,
    source_id: &str,
    target_type: &str,
    target_id: &str,
    singularity: LinkSingularity,
    now_ms: i64,
) -> Result<LinkOutcome, DbErr> {
    if singularity != LinkSingularity::Multi {
        // Singular relation: an already-recorded target decides the outcome
        // before anything is written.
        let (sql, values): (&str, Vec<Value>) = match singularity {
            LinkSingularity::BySource => (
                "SELECT target_id FROM agent_bridge_link WHERE source_type = ? AND source_id = ?",
                vec![source_type.into(), source_id.into()],
            ),
            _ => (
                "SELECT target_id FROM agent_bridge_link \
                 WHERE source_type = ? AND source_id = ? AND target_type = ?",
                vec![source_type.into(), source_id.into(), target_type.into()],
            ),
        };
        let rows = conn
            .query_all_raw(Statement::from_sql_and_values(
                DatabaseBackend::Sqlite,
                sql,
                values,
            ))
            .await?;
        if let Some(stored) = rows
            .into_iter()
            .next()
            .and_then(|r| r.try_get::<String>("", "target_id").ok())
        {
            return Ok(if stored == target_id {
                LinkOutcome::Existing
            } else {
                LinkOutcome::Conflict {
                    stored_target_id: stored,
                }
            });
        }
    }
    let insert = Statement::from_sql_and_values(
        DatabaseBackend::Sqlite,
        "INSERT INTO agent_bridge_link \
         (bridge_session_id, source_type, source_id, target_type, target_id, created_at) \
         VALUES (?, ?, ?, ?, ?, ?) \
         ON CONFLICT(source_type, source_id, target_type, target_id) DO NOTHING",
        [
            bridge_session_id.into(),
            source_type.into(),
            source_id.into(),
            target_type.into(),
            target_id.into(),
            now_ms.into(),
        ],
    );
    let affected = conn.execute_raw(insert).await?;
    Ok(if affected.rows_affected() > 0 {
        LinkOutcome::Inserted
    } else {
        LinkOutcome::Existing
    })
}

/// Register a bridge checkpoint, optionally linking it to an existing
/// `agent_checkpoint` catalog row. Returns `true` when newly created.
pub async fn insert_checkpoint<C: ConnectionTrait>(
    conn: &C,
    checkpoint_id: &str,
    bridge_session_id: &str,
    agent_checkpoint_id: Option<&str>,
    target_oid: Option<&str>,
    now_ms: i64,
) -> Result<bool, DbErr> {
    let insert = Statement::from_sql_and_values(
        DatabaseBackend::Sqlite,
        "INSERT INTO agent_bridge_checkpoint \
         (checkpoint_id, bridge_session_id, agent_checkpoint_id, target_oid, created_at) \
         VALUES (?, ?, ?, ?, ?) \
         ON CONFLICT(checkpoint_id) DO NOTHING",
        [
            checkpoint_id.into(),
            bridge_session_id.into(),
            agent_checkpoint_id
                .map(Value::from)
                .unwrap_or(Value::String(None)),
            target_oid.map(Value::from).unwrap_or(Value::String(None)),
            now_ms.into(),
        ],
    );
    let affected = conn.execute_raw(insert).await?;
    Ok(affected.rows_affected() > 0)
}
