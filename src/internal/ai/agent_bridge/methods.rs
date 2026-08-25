//! Bridge read methods and typed query scope (plan-20260818 LB-04).
//!
//! Implements the low-risk, high-level read methods (`context.get`,
//! `status.get`, `history.search`, `checkpoint.list`, `checkpoint.show`) over
//! the bridge durable projection, with a unified result envelope
//! (`schema_version`, `repository_id`, `workspace_id`, `operation_id`,
//! `status`, `data`, `warnings`). Scope is derived from the trusted context,
//! never from the request body (GC-LB-06). `diff.get` is wired to the real VCS
//! diff service through the typed [`super::vcs`] adapter: the request selects
//! a closed mode enum plus validated repository-relative paths, never a
//! free-form revision or pathspec.
//!
//! Unknown methods, unknown fields and arbitrary SQL/path/ref selectors are
//! rejected fail-closed; results are bounded by the v1 page/result caps.

use sea_orm::{ConnectionTrait, DatabaseBackend, DatabaseConnection, Statement};
use serde_json::{Value, json};

use super::{
    ingress::BridgeContext,
    protocol::{BridgeError, BridgeRequest, BridgeResponse, JSONRPC_VERSION, MAX_PAGE},
    vcs::{self, DiffMode},
};

/// The v1 result envelope schema version (fixed, additive).
pub const READ_SCHEMA_VERSION: i64 = 1;

fn ok_envelope(
    ctx: &BridgeContext,
    data: Value,
    operation_id: &str,
    warnings: Value,
) -> BridgeResponse {
    BridgeResponse {
        jsonrpc: JSONRPC_VERSION,
        result: Some(json!({
            "schema_version": READ_SCHEMA_VERSION,
            "repository_id": ctx.repository_id,
            "workspace_id": ctx.worktree_id,
            "operation_id": operation_id,
            "status": "ok",
            "data": data,
            "warnings": warnings,
        })),
        error: None,
        id: Value::Null,
    }
}

/// Dispatch a read method. Returns `Ok(None)` when the method is not a read
/// method (handled by the surrounding dispatcher).
pub async fn dispatch(
    ctx: &BridgeContext,
    request: &BridgeRequest,
) -> Result<Option<BridgeResponse>, BridgeError> {
    let id = request.id.clone().unwrap_or(Value::Null);
    match request.method.as_str() {
        "context.get" => {
            let mut resp = context_get(ctx, request).await?;
            resp.id = id;
            Ok(Some(resp))
        }
        "status.get" => {
            let mut resp = status_get(ctx, request).await?;
            resp.id = id;
            Ok(Some(resp))
        }
        "history.search" => {
            let mut resp = history_search(ctx, request).await?;
            resp.id = id;
            Ok(Some(resp))
        }
        "checkpoint.list" => {
            let mut resp = checkpoint_list(ctx, request).await?;
            resp.id = id;
            Ok(Some(resp))
        }
        "checkpoint.show" => {
            let mut resp = checkpoint_show(ctx, request).await?;
            resp.id = id;
            Ok(Some(resp))
        }
        "diff.get" => {
            let mut resp = diff_get(ctx, request).await?;
            resp.id = id;
            Ok(Some(resp))
        }
        _ => Ok(None),
    }
}

fn param_str(params: &Option<Value>, key: &str) -> Option<String> {
    params
        .as_ref()
        .and_then(|p| p.get(key))
        .and_then(Value::as_str)
        .map(str::to_string)
}

fn page_limit(params: &Option<Value>) -> (usize, Vec<Value>) {
    let requested = params
        .as_ref()
        .and_then(|p| p.get("limit"))
        .and_then(Value::as_u64);
    match requested {
        Some(n) => {
            let clamped = (n as usize).clamp(1, MAX_PAGE);
            if clamped != n as usize {
                let warn = if n == 0 {
                    json!({
                        "code": "limit_clamped",
                        "message": "requested limit 0 is below the v1 page floor of 1; returning 1".to_string(),
                    })
                } else {
                    json!({
                        "code": "limit_clamped",
                        "message": format!(
                            "requested limit {n} exceeded the v1 page cap of {MAX_PAGE}; returning {clamped}"
                        ),
                    })
                };
                (clamped, vec![warn])
            } else {
                (clamped, vec![])
            }
        }
        None => (MAX_PAGE.min(20), vec![]),
    }
}

/// `context.get` — recent active bridge sessions, recent bridge checkpoints
/// and the repository identity. Never returns raw transcript/reasoning/secret
/// (GC-LB-08).
async fn context_get(
    ctx: &BridgeContext,
    _request: &BridgeRequest,
) -> Result<BridgeResponse, BridgeError> {
    let sessions = list_sessions(&ctx.conn, &ctx.repository_id, 10)
        .await
        .map_err(db_error("context.get: list sessions"))?;
    let checkpoints = list_checkpoints(&ctx.conn, &ctx.repository_id, 10)
        .await
        .map_err(db_error("context.get: list checkpoints"))?;
    Ok(ok_envelope(
        ctx,
        json!({
            "sessions": sessions,
            "recent_checkpoints": checkpoints,
        }),
        "",
        json!([]),
    ))
}

/// `status.get` — repository identity and bridge session/event/checkpoint
/// totals (a structured workspace summary).
async fn status_get(
    ctx: &BridgeContext,
    _request: &BridgeRequest,
) -> Result<BridgeResponse, BridgeError> {
    let totals = totals(&ctx.conn, &ctx.repository_id)
        .await
        .map_err(db_error("status.get: read totals"))?;
    Ok(ok_envelope(
        ctx,
        json!({
            "bridge_sessions": totals.0,
            "bridge_events": totals.1,
            "bridge_checkpoints": totals.2,
        }),
        "",
        json!([]),
    ))
}

/// `history.search` — search bridge sessions/events by an optional term, with
/// bounded keyset pagination.
async fn history_search(
    ctx: &BridgeContext,
    request: &BridgeRequest,
) -> Result<BridgeResponse, BridgeError> {
    let term = param_str(&request.params, "query");
    let (limit, warnings) = page_limit(&request.params);
    let sessions = search_sessions(&ctx.conn, &ctx.repository_id, term.as_deref(), limit)
        .await
        .map_err(db_error("history.search: query sessions"))?;
    Ok(ok_envelope(
        ctx,
        json!({
            "query": term,
            "limit": limit,
            "sessions": sessions,
        }),
        "",
        json!(warnings),
    ))
}

/// `checkpoint.list` — list bridge checkpoints (optionally for one session),
/// newest first, bounded.
async fn checkpoint_list(
    ctx: &BridgeContext,
    request: &BridgeRequest,
) -> Result<BridgeResponse, BridgeError> {
    let session_id = param_str(&request.params, "session_id");
    let (limit, warnings) = page_limit(&request.params);
    let checkpoints =
        list_checkpoints_filtered(&ctx.conn, &ctx.repository_id, session_id.as_deref(), limit)
            .await
            .map_err(db_error("checkpoint.list: query checkpoints"))?;
    Ok(ok_envelope(
        ctx,
        json!({ "limit": limit, "checkpoints": checkpoints }),
        "",
        json!(warnings),
    ))
}

/// `checkpoint.show` — show one bridge checkpoint by id. Missing checkpoint is
/// an error, never a silent empty result.
async fn checkpoint_show(
    ctx: &BridgeContext,
    request: &BridgeRequest,
) -> Result<BridgeResponse, BridgeError> {
    let checkpoint_id = param_str(&request.params, "checkpoint_id")
        .ok_or_else(|| BridgeError::invalid_params("checkpoint.show requires checkpoint_id"))?;
    let row = get_checkpoint(&ctx.conn, &ctx.repository_id, &checkpoint_id)
        .await
        .map_err(db_error("checkpoint.show: read checkpoint"))?;
    let Some(row) = row else {
        return Err(BridgeError::scope_mismatch(format!(
            "bridge checkpoint '{checkpoint_id}' does not exist"
        )));
    };
    Ok(ok_envelope(
        ctx,
        json!({
            "checkpoint_id": row.0,
            "bridge_session_id": row.1,
            "agent_checkpoint_id": row.2,
            "target_oid": row.3,
        }),
        "",
        json!([]),
    ))
}

/// `diff.get` — the working-tree, staged, or checkpoint-scoped diff, bounded
/// by the v1 file page and patch budget.
///
/// The comparison sides come from a closed `mode` enum; `mode: "checkpoint"`
/// resolves its revision from the bridge's own `agent_bridge_checkpoint` row
/// (or its linked `agent_checkpoint.parent_commit`), so a request can never
/// name an arbitrary revision. Paths are validated repository-relative
/// selectors, never pathspec magic (GC-LB-03).
async fn diff_get(
    ctx: &BridgeContext,
    request: &BridgeRequest,
) -> Result<BridgeResponse, BridgeError> {
    let (mode, paths) = vcs::parse_diff_params(&request.params)?;
    let (limit, mut warnings) = page_limit(&request.params);
    let against = if mode == DiffMode::Checkpoint {
        let checkpoint_id = param_str(&request.params, "checkpoint_id").ok_or_else(|| {
            BridgeError::invalid_params("diff.get mode 'checkpoint' requires checkpoint_id")
        })?;
        Some(resolve_checkpoint_commit(ctx, &checkpoint_id).await?)
    } else {
        None
    };
    let (data, mut diff_warnings) = vcs::diff_data(mode, against, paths, limit).await?;
    warnings.append(&mut diff_warnings);
    Ok(ok_envelope(ctx, data, "", json!(warnings)))
}

/// Resolve the commit a bridge checkpoint pins, scoped to this repository.
///
/// Prefers the bridge checkpoint's own `target_oid`; falls back to the linked
/// `agent_checkpoint.parent_commit` (the snapshot `checkpoint rewind` restores
/// to). A checkpoint that pins no commit is a fail-closed error, never an
/// empty diff.
pub(super) async fn resolve_checkpoint_commit(
    ctx: &BridgeContext,
    checkpoint_id: &str,
) -> Result<git_internal::hash::ObjectHash, BridgeError> {
    let row = get_checkpoint(&ctx.conn, &ctx.repository_id, checkpoint_id)
        .await
        .map_err(db_error("resolve checkpoint commit"))?;
    let Some((_, _, agent_checkpoint_id, target_oid)) = row else {
        return Err(BridgeError::scope_mismatch(format!(
            "bridge checkpoint '{checkpoint_id}' does not exist in this repository"
        )));
    };
    if let Some(oid) = target_oid.filter(|v| !v.trim().is_empty()) {
        return vcs::parse_stored_commit_oid(&oid, "the bridge checkpoint");
    }
    let Some(agent_checkpoint_id) = agent_checkpoint_id else {
        return Err(BridgeError::invalid_params(format!(
            "bridge checkpoint '{checkpoint_id}' pins no commit (no target_oid and no linked \
             agent checkpoint); it cannot be used as a diff or restore target"
        )));
    };
    let parent = agent_checkpoint_parent_commit(&ctx.conn, &agent_checkpoint_id)
        .await
        .map_err(db_error("resolve linked agent checkpoint"))?;
    let Some(parent) = parent.filter(|v| !v.trim().is_empty()) else {
        return Err(BridgeError::invalid_params(format!(
            "bridge checkpoint '{checkpoint_id}' links agent checkpoint \
             '{agent_checkpoint_id}', which recorded no parent_commit (unborn HEAD at ingest); \
             it cannot be used as a diff or restore target"
        )));
    };
    vcs::parse_stored_commit_oid(&parent, "the linked agent checkpoint")
}

/// Read `agent_checkpoint.parent_commit` for one catalog row.
async fn agent_checkpoint_parent_commit(
    conn: &DatabaseConnection,
    agent_checkpoint_id: &str,
) -> Result<Option<String>, sea_orm::DbErr> {
    let rows = conn
        .query_all_raw(Statement::from_sql_and_values(
            DatabaseBackend::Sqlite,
            "SELECT parent_commit FROM agent_checkpoint WHERE checkpoint_id = ? LIMIT 1",
            [agent_checkpoint_id.into()],
        ))
        .await?;
    Ok(rows.into_iter().next().and_then(|r| {
        r.try_get::<Option<String>>("", "parent_commit")
            .ok()
            .flatten()
    }))
}

fn db_error(context: &'static str) -> impl Fn(sea_orm::DbErr) -> BridgeError {
    move |err| BridgeError::internal(format!("{context}: {err}"))
}

// ---------------------------------------------------------------------------
// Bridge-table readers (LB-04 scope: repository-scoped, bounded, no secrets)
// ---------------------------------------------------------------------------

async fn list_sessions(
    conn: &DatabaseConnection,
    repository_id: &str,
    limit: i64,
) -> Result<Vec<Value>, sea_orm::DbErr> {
    let rows = conn
        .query_all_raw(Statement::from_sql_and_values(
            DatabaseBackend::Sqlite,
            "SELECT bridge_session_id, session_state, last_event_seq, last_acked_seq, \
                    created_at \
             FROM agent_bridge_session WHERE repository_id = ? \
             ORDER BY created_at DESC LIMIT ?",
            [repository_id.into(), limit.into()],
        ))
        .await?;
    Ok(rows
        .into_iter()
        .map(|r| {
            json!({
                "session_id": r.try_get::<String>("", "bridge_session_id").unwrap_or_default(),
                "state": r.try_get::<String>("", "session_state").unwrap_or_default(),
                "last_event_seq": r.try_get::<i64>("", "last_event_seq").unwrap_or(0),
                "last_acked_seq": r.try_get::<i64>("", "last_acked_seq").unwrap_or(0),
            })
        })
        .collect())
}

async fn search_sessions(
    conn: &DatabaseConnection,
    repository_id: &str,
    term: Option<&str>,
    limit: usize,
) -> Result<Vec<Value>, sea_orm::DbErr> {
    let (sql, values): (String, Vec<sea_orm::Value>) = if let Some(term) = term {
        (
            "SELECT bridge_session_id, session_state, last_acked_seq, created_at \
             FROM agent_bridge_session WHERE repository_id = ? \
               AND (bridge_session_id LIKE ? OR agent_id LIKE ?) \
             ORDER BY created_at DESC LIMIT ?"
                .to_string(),
            vec![
                repository_id.into(),
                format!("%{term}%").into(),
                format!("%{term}%").into(),
                (limit as i64).into(),
            ],
        )
    } else {
        (
            "SELECT bridge_session_id, session_state, last_acked_seq, created_at \
             FROM agent_bridge_session WHERE repository_id = ? \
             ORDER BY created_at DESC LIMIT ?"
                .to_string(),
            vec![repository_id.into(), (limit as i64).into()],
        )
    };
    let rows = conn
        .query_all_raw(Statement::from_sql_and_values(
            DatabaseBackend::Sqlite,
            &sql,
            values,
        ))
        .await?;
    Ok(rows
        .into_iter()
        .map(|r| {
            json!({
                "session_id": r.try_get::<String>("", "bridge_session_id").unwrap_or_default(),
                "state": r.try_get::<String>("", "session_state").unwrap_or_default(),
                "last_acked_seq": r.try_get::<i64>("", "last_acked_seq").unwrap_or(0),
            })
        })
        .collect())
}

async fn totals(
    conn: &DatabaseConnection,
    repository_id: &str,
) -> Result<(i64, i64, i64), sea_orm::DbErr> {
    let sessions = count_scalar(conn, "agent_bridge_session", Some(repository_id)).await?;
    // The event and checkpoint tables carry no repository_id column — they are
    // keyed by bridge_session_id — so their counts MUST be scoped through the
    // owning session's repository (GC-LB-06). Counting them without the join
    // would leak global totals across every repository.
    let events = count_via_session(conn, "agent_bridge_event", "e", repository_id).await?;
    let checkpoints =
        count_via_session(conn, "agent_bridge_checkpoint", "c", repository_id).await?;
    Ok((sessions, events, checkpoints))
}

/// Count rows in a session-keyed table (`agent_bridge_event` / `agent_bridge_
/// checkpoint`) restricted to one repository via a JOIN to `agent_bridge_session`.
async fn count_via_session(
    conn: &DatabaseConnection,
    table: &str,
    alias: &str,
    repository_id: &str,
) -> Result<i64, sea_orm::DbErr> {
    let sql = format!(
        "SELECT COUNT(*) AS n FROM {table} {alias} \
         JOIN agent_bridge_session s ON s.bridge_session_id = {alias}.bridge_session_id \
         WHERE s.repository_id = ?"
    );
    let rows = conn
        .query_all_raw(Statement::from_sql_and_values(
            DatabaseBackend::Sqlite,
            &sql,
            [repository_id.into()],
        ))
        .await?;
    Ok(rows
        .into_iter()
        .next()
        .and_then(|r| r.try_get::<i64>("", "n").ok())
        .unwrap_or(0))
}

async fn count_scalar(
    conn: &DatabaseConnection,
    table: &str,
    repository_id: Option<&str>,
) -> Result<i64, sea_orm::DbErr> {
    let (sql, values): (String, Vec<sea_orm::Value>) = match repository_id {
        Some(rid) => (
            format!("SELECT COUNT(*) AS n FROM {table} WHERE repository_id = ?"),
            vec![rid.into()],
        ),
        None => (format!("SELECT COUNT(*) AS n FROM {table}"), vec![]),
    };
    let rows = conn
        .query_all_raw(Statement::from_sql_and_values(
            DatabaseBackend::Sqlite,
            &sql,
            values,
        ))
        .await?;
    Ok(rows
        .into_iter()
        .next()
        .and_then(|r| r.try_get::<i64>("", "n").ok())
        .unwrap_or(0))
}

async fn list_checkpoints(
    conn: &DatabaseConnection,
    repository_id: &str,
    limit: i64,
) -> Result<Vec<Value>, sea_orm::DbErr> {
    list_checkpoints_filtered(conn, repository_id, None, limit as usize).await
}

async fn list_checkpoints_filtered(
    conn: &DatabaseConnection,
    repository_id: &str,
    session_id: Option<&str>,
    limit: usize,
) -> Result<Vec<Value>, sea_orm::DbErr> {
    // Scope on repository through the owning session (GC-LB-06): a checkpoint
    // is only visible when its session belongs to the caller's repository.
    let (sql, values): (String, Vec<sea_orm::Value>) = match session_id {
        Some(sid) => (
            "SELECT c.checkpoint_id, c.bridge_session_id, c.agent_checkpoint_id, c.target_oid \
             FROM agent_bridge_checkpoint c \
             JOIN agent_bridge_session s ON s.bridge_session_id = c.bridge_session_id \
             WHERE s.repository_id = ? AND c.bridge_session_id = ? \
             ORDER BY c.created_at DESC LIMIT ?"
                .to_string(),
            vec![repository_id.into(), sid.into(), (limit as i64).into()],
        ),
        None => (
            "SELECT c.checkpoint_id, c.bridge_session_id, c.agent_checkpoint_id, c.target_oid \
             FROM agent_bridge_checkpoint c \
             JOIN agent_bridge_session s ON s.bridge_session_id = c.bridge_session_id \
             WHERE s.repository_id = ? \
             ORDER BY c.created_at DESC LIMIT ?"
                .to_string(),
            vec![repository_id.into(), (limit as i64).into()],
        ),
    };
    let rows = conn
        .query_all_raw(Statement::from_sql_and_values(
            DatabaseBackend::Sqlite,
            &sql,
            values,
        ))
        .await?;
    Ok(rows
        .into_iter()
        .map(|r| {
            json!({
                "checkpoint_id": r.try_get::<String>("", "checkpoint_id").unwrap_or_default(),
                "bridge_session_id": r.try_get::<String>("", "bridge_session_id").unwrap_or_default(),
                "agent_checkpoint_id": r.try_get::<Option<String>>("", "agent_checkpoint_id").ok().flatten(),
                "target_oid": r.try_get::<Option<String>>("", "target_oid").ok().flatten(),
            })
        })
        .collect())
}

async fn get_checkpoint(
    conn: &DatabaseConnection,
    repository_id: &str,
    checkpoint_id: &str,
) -> Result<Option<(String, String, Option<String>, Option<String>)>, sea_orm::DbErr> {
    let rows = conn
        .query_all_raw(Statement::from_sql_and_values(
            DatabaseBackend::Sqlite,
            "SELECT c.checkpoint_id, c.bridge_session_id, c.agent_checkpoint_id, c.target_oid \
             FROM agent_bridge_checkpoint c \
             JOIN agent_bridge_session s ON s.bridge_session_id = c.bridge_session_id \
             WHERE s.repository_id = ? AND c.checkpoint_id = ?",
            [repository_id.into(), checkpoint_id.into()],
        ))
        .await?;
    Ok(rows.into_iter().next().map(|r| {
        (
            r.try_get::<String>("", "checkpoint_id").unwrap_or_default(),
            r.try_get::<String>("", "bridge_session_id")
                .unwrap_or_default(),
            r.try_get::<Option<String>>("", "agent_checkpoint_id")
                .ok()
                .flatten(),
            r.try_get::<Option<String>>("", "target_oid").ok().flatten(),
        )
    }))
}
