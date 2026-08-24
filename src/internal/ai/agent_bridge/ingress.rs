//! Bridge session/event ingress, batch ack and provenance projection
//! (plan-20260818 LB-03).
//!
//! Maps `session.open`, `event.append`, `session.flush`, `session.close`,
//! `evidence.append` and `provenance.append` onto the durable store from
//! LB-02, with handshake-derived scope binding (GC-LB-06/07), server-side
//! redaction (GC-LB-08, ADR-LB-03) and ack semantics (GC-LB-09): an ack only
//! means the redacted projection is durable, never that the Harness raw
//! transcript has been taken over.

use sea_orm::{DatabaseConnection, DbErr};
use serde_json::{Value, json};

use super::{
    protocol::{BridgeError, BridgeRequest, BridgeResponse, JSONRPC_VERSION},
    storage::{self, AppendEvent, EventStatus},
};

/// Trusted bridge scope derived from the repository/context at handshake
/// (GC-LB-06/07). The request body never supplies these.
#[derive(Debug, Clone)]
pub struct BridgeContext {
    pub conn: DatabaseConnection,
    pub repository_id: String,
    pub worktree_id: Option<String>,
}

fn now_ms() -> i64 {
    use std::time::{SystemTime, UNIX_EPOCH};
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

fn ok_result(data: Value, id: Value) -> BridgeResponse {
    BridgeResponse {
        jsonrpc: JSONRPC_VERSION,
        result: Some(data),
        error: None,
        id,
    }
}

/// Look up a session's repository scope and verify it matches the trusted
/// context (fail-closed on mismatch — a session created by another repository
/// can never be written to from here).
async fn require_session_in_repo(ctx: &BridgeContext, session_id: &str) -> Result<(), BridgeError> {
    let session = storage::get_session(&ctx.conn, session_id)
        .await
        .map_err(db_error("look up bridge session"))?;
    match session {
        Some(row) if row.repository_id == ctx.repository_id => Ok(()),
        Some(_) => Err(BridgeError::scope_mismatch(format!(
            "bridge session '{session_id}' belongs to a different repository; refusing cross-repository write"
        ))),
        None => Err(BridgeError::scope_mismatch(format!(
            "bridge session '{session_id}' is not open in this connection; call session.open first"
        ))),
    }
}

fn db_error(context: &'static str) -> impl Fn(DbErr) -> BridgeError {
    move |err| BridgeError::internal(format!("{context}: {err}"))
}

/// Dispatch the session/event ingress methods. Returns `None` for a
/// notification (no reply). Unknown/read/mutation methods are handled by the
/// surrounding dispatcher (LB-04/LB-05); this function owns the ingress set.
pub async fn dispatch(
    ctx: &BridgeContext,
    request: &BridgeRequest,
) -> Result<Option<BridgeResponse>, BridgeError> {
    let id = request.id.clone().unwrap_or(Value::Null);
    match request.method.as_str() {
        "session.open" => Ok(Some(session_open(ctx, request, id).await?)),
        "event.append" => Ok(Some(event_append(ctx, request, id).await?)),
        "session.flush" => Ok(Some(session_flush(ctx, request, id).await?)),
        "session.close" => Ok(Some(session_close(ctx, request, id).await?)),
        "evidence.append" => Ok(Some(evidence_append(ctx, request, id, "evidence").await?)),
        "provenance.append" => Ok(Some(evidence_append(ctx, request, id, "provenance").await?)),
        _ => Ok(None),
    }
}

fn param_str(params: &Option<Value>, key: &str) -> Result<String, BridgeError> {
    params
        .as_ref()
        .and_then(|p| p.get(key))
        .and_then(Value::as_str)
        .map(str::to_string)
        .ok_or_else(|| BridgeError::invalid_params(format!("missing or non-string '{key}'")))
}

async fn session_open(
    ctx: &BridgeContext,
    request: &BridgeRequest,
    id: Value,
) -> Result<BridgeResponse, BridgeError> {
    let session_id = param_str(&request.params, "session_id")?;
    let parent_session_id = request
        .params
        .as_ref()
        .and_then(|p| p.get("parent_session_id"))
        .and_then(Value::as_str)
        .map(str::to_string);
    let agent_id = request
        .params
        .as_ref()
        .and_then(|p| p.get("agent_id"))
        .and_then(Value::as_str)
        .map(str::to_string);
    let workspace_id = request
        .params
        .as_ref()
        .and_then(|p| p.get("workspace_id"))
        .and_then(Value::as_str)
        .map(str::to_string);

    let (row, created) = storage::open_session(
        &ctx.conn,
        &session_id,
        &ctx.repository_id,
        ctx.worktree_id.as_deref(),
        workspace_id.as_deref(),
        parent_session_id.as_deref(),
        agent_id.as_deref(),
        now_ms(),
    )
    .await
    .map_err(db_error("open bridge session"))?;

    // Fail-closed: an existing session must belong to this repository.
    if row.repository_id != ctx.repository_id {
        return Err(BridgeError::scope_mismatch(format!(
            "bridge session '{session_id}' already belongs to repository '{}', not '{}'",
            row.repository_id, ctx.repository_id
        )));
    }

    // Fail-closed on a retry whose lineage differs from the stored scope
    // (GC-LB-06): a second `session.open` with a different workspace /
    // parent / agent must not be silently ignored. The stored scope is never
    // overwritten; a mismatch is a stable error.
    if !created {
        for (label, requested, stored) in [
            (
                "workspace_id",
                workspace_id.as_deref(),
                row.workspace_id.as_deref(),
            ),
            (
                "parent_session_id",
                parent_session_id.as_deref(),
                row.parent_session_id.as_deref(),
            ),
            ("agent_id", agent_id.as_deref(), row.agent_id.as_deref()),
        ] {
            if requested != stored {
                return Err(BridgeError::scope_mismatch(format!(
                    "bridge session '{session_id}' already has {label}={:?}, not {:?}; refusing scope drift",
                    stored, requested
                )));
            }
        }
    }

    Ok(ok_result(
        json!({
            "session_id": session_id,
            "repository_id": ctx.repository_id,
            "created": created,
            "last_acked_seq": row.last_acked_seq,
        }),
        id,
    ))
}

#[derive(serde::Deserialize)]
struct EventItem {
    #[serde(rename = "seq")]
    event_seq: i64,
    #[serde(rename = "type")]
    event_type: String,
    payload: String,
    #[serde(default)]
    operation_id: Option<String>,
}

async fn event_append(
    ctx: &BridgeContext,
    request: &BridgeRequest,
    id: Value,
) -> Result<BridgeResponse, BridgeError> {
    let session_id = param_str(&request.params, "session_id")?;
    require_session_in_repo(ctx, &session_id).await?;

    // Parse and validate the batch (bounded: <=64 events, <=256 KiB).
    let events = parse_event_batch(request)?;

    // Redact every payload before any persistence (fail-closed, no raw
    // fallback — GC-LB-08 / ADR-LB-03).
    let mut redacted: Vec<AppendEvent> = Vec::with_capacity(events.len());
    for ev in events {
        let redacted_payload = super::redaction::redact_payload(&ev.payload)?.to_string();
        if redacted_payload.len() > storage::MAX_EVENT_PAYLOAD_BYTES {
            return Err(super::redaction::redaction_failed(format!(
                "redacted event {seq} payload still exceeds {max} bytes; refusing (no raw fallback)",
                seq = ev.event_seq,
                max = storage::MAX_EVENT_PAYLOAD_BYTES,
            )));
        }
        redacted.push(AppendEvent {
            event_seq: ev.event_seq,
            event_type: ev.event_type,
            payload: redacted_payload,
            operation_id: ev.operation_id,
        });
    }

    let outcome = storage::append_events(&ctx.conn, &session_id, &redacted, now_ms())
        .await
        .map_err(db_error("append bridge events"))?;

    // Advance the ack cursor to the highest contiguous accepted sequence.
    let contiguous = storage::contiguous_acked_seq(&ctx.conn, &session_id)
        .await
        .map_err(db_error("compute contiguous ack"))?;
    if contiguous > outcome.last_acked_seq {
        storage::mark_acked(&ctx.conn, &session_id, contiguous, now_ms())
            .await
            .map_err(db_error("mark acked"))?;
    }

    let per_event: Vec<Value> = redacted
        .iter()
        .zip(outcome.per_event.iter())
        .map(|(ev, status)| {
            json!({
                "seq": ev.event_seq,
                "status": status_name(status),
            })
        })
        .collect();

    Ok(ok_result(
        json!({
            "session_id": session_id,
            "per_event": per_event,
            "last_acked_seq": contiguous,
        }),
        id,
    ))
}

fn status_name(status: &EventStatus) -> &'static str {
    match status {
        EventStatus::Accepted => "accepted",
        EventStatus::Duplicate => "duplicate",
        // A distinct, non-retryable status so a client can tell "content
        // diverged at an already-used sequence" from "malformed/oversized".
        EventStatus::DigestConflict => "conflict",
        EventStatus::Rejected => "rejected",
    }
}

fn parse_event_batch(request: &BridgeRequest) -> Result<Vec<EventItem>, BridgeError> {
    let params = request
        .params
        .as_ref()
        .ok_or_else(|| BridgeError::invalid_params("event.append requires params"))?;
    let events = params
        .get("events")
        .and_then(Value::as_array)
        .ok_or_else(|| BridgeError::invalid_params("event.append requires an 'events' array"))?;

    if events.len() > super::protocol::MAX_BATCH_EVENTS {
        return Err(BridgeError::invalid_params(format!(
            "event.append batch exceeds the v1 cap of {} events",
            super::protocol::MAX_BATCH_EVENTS
        )));
    }

    let mut out = Vec::with_capacity(events.len());
    for ev in events {
        let item: EventItem = serde_json::from_value(ev.clone())
            .map_err(|e| BridgeError::invalid_params(format!("invalid event object: {e}")))?;
        // Enforce the per-event payload cap BEFORE redaction.
        if item.payload.len() > super::protocol::MAX_EVENT_BYTES {
            return Err(super::redaction::redaction_failed(format!(
                "event {seq} payload exceeds the v1 {max}-byte cap; refusing",
                seq = item.event_seq,
                max = super::protocol::MAX_EVENT_BYTES,
            )));
        }
        out.push(item);
    }
    Ok(out)
}

async fn session_flush(
    ctx: &BridgeContext,
    request: &BridgeRequest,
    id: Value,
) -> Result<BridgeResponse, BridgeError> {
    let session_id = param_str(&request.params, "session_id")?;
    require_session_in_repo(ctx, &session_id).await?;
    storage::set_session_state(&ctx.conn, &session_id, "flushing", now_ms())
        .await
        .map_err(db_error("flush bridge session"))?;
    let last_acked = storage::contiguous_acked_seq(&ctx.conn, &session_id)
        .await
        .map_err(db_error("read ack"))?;
    Ok(ok_result(
        json!({ "session_id": session_id, "flushed": true, "last_acked_seq": last_acked }),
        id,
    ))
}

async fn session_close(
    ctx: &BridgeContext,
    request: &BridgeRequest,
    id: Value,
) -> Result<BridgeResponse, BridgeError> {
    let session_id = param_str(&request.params, "session_id")?;
    require_session_in_repo(ctx, &session_id).await?;
    storage::set_session_state(&ctx.conn, &session_id, "closed", now_ms())
        .await
        .map_err(db_error("close bridge session"))?;
    let last_acked = storage::contiguous_acked_seq(&ctx.conn, &session_id)
        .await
        .map_err(db_error("read ack"))?;
    Ok(ok_result(
        json!({ "session_id": session_id, "closed": true, "last_acked_seq": last_acked }),
        id,
    ))
}

/// `evidence.append` / `provenance.append` — record a stable association link
/// from the bridge session to an AI/VCS object id (ADR-LB-02: link, don't
/// alias the provider roster).
async fn evidence_append(
    ctx: &BridgeContext,
    request: &BridgeRequest,
    id: Value,
    source_type: &str,
) -> Result<BridgeResponse, BridgeError> {
    let session_id = param_str(&request.params, "session_id")?;
    require_session_in_repo(ctx, &session_id).await?;
    let source_id = param_str(&request.params, "source_id")?;
    let target_type = param_str(&request.params, "target_type")?;
    let target_id = param_str(&request.params, "target_id")?;

    let outcome = storage::insert_link(
        &ctx.conn,
        &session_id,
        source_type,
        &source_id,
        &target_type,
        &target_id,
        // `evidence.append` / `provenance.append` keep their established
        // one-source-one-target contract: a relink to a different object is a
        // conflict, whatever the target's type.
        storage::LinkSingularity::BySource,
        now_ms(),
    )
    .await
    .map_err(db_error("record bridge association link"))?;

    // Fail-closed: pointing the same source at a DIFFERENT target is a
    // digest conflict, never a silent no-op (GC-LB-09). A client that thinks
    // it re-pointed an evidence link must not get a false success.
    match outcome {
        storage::LinkOutcome::Inserted | storage::LinkOutcome::Existing => {}
        storage::LinkOutcome::Conflict { stored_target_id } => {
            return Err(BridgeError::digest_conflict(format!(
                "association {source_type}:{source_id} already points at target '{stored_target_id}', not '{target_id}'; refusing conflicting relink"
            )));
        }
    }

    Ok(ok_result(
        json!({
            "session_id": session_id,
            "source_type": source_type,
            "source_id": source_id,
            "target_id": target_id,
            "linked": true,
        }),
        id,
    ))
}
