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
    ingress::{BridgeContext, StoreBridgeContext},
    protocol::{BridgeError, BridgeRequest, BridgeResponse, JSONRPC_VERSION, MAX_PAGE},
    vcs::{self, DiffMode},
};
use crate::storage::{StorageBackendError, StorageBackendKind};

/// The v1 result envelope schema version (fixed, additive).
pub const READ_SCHEMA_VERSION: i64 = 1;

async fn vcs_response(ctx: &BridgeContext, id: Value, data: Value) -> BridgeResponse {
    let mut response = ok_envelope(ctx, data, "", json!([]));
    response.id = id;
    response
}

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
        "vcs.fence" => {
            let fence = super::vcs_write::current_write_fence().await?;
            Ok(Some(
                vcs_response(
                    ctx,
                    id,
                    json!({
                        "head": fence.head,
                        "indexGeneration": fence.index_generation,
                        "worktreeGeneration": fence.worktree_generation,
                    }),
                )
                .await,
            ))
        }
        "vcs.identity" => Ok(Some(
            vcs_response(ctx, id, super::vcs_read::identity().await?).await,
        )),
        "vcs.status" => {
            // The page's entries come from the parsed index snapshot and the
            // final worktree scan. The fence is published only when the
            // candidate and final worktree observations agree on path kinds,
            // identities, metadata and content. The write path rechecks it
            // before publication; a status read alone is not an atomic
            // filesystem snapshot.
            let observation = super::vcs_read::status_with_observation(&request.params).await?;
            let mut data = observation.data;
            super::vcs_write::rewrite_after_status_observation();
            let mut warnings: Vec<Value> = Vec::new();
            let fence = match (
                observation.index_generation.as_ref(),
                observation.worktree_generation.as_ref(),
            ) {
                (Some(index_generation), Some(worktree_generation)) => {
                    Some(super::vcs_write::fence_for_observation(
                        observation.head,
                        index_generation.clone(),
                        worktree_generation.clone(),
                    ))
                }
                _ => None,
            };
            match fence {
                Some(fence) => {
                    if let Some(object) = data.as_object_mut() {
                        object.insert(
                            "writeFence".to_string(),
                            json!({
                                "head": if fence.head.is_empty() {
                                    Value::Null
                                } else {
                                    Value::String(fence.head)
                                },
                                "indexGeneration": fence.index_generation,
                                "worktreeGeneration": fence.worktree_generation,
                            }),
                        );
                    }
                }
                None => {
                    warnings.push(json!({
                        "code": "write_fence_unavailable",
                        "message": "no write fence could be derived for this repository state, so \
                                    none is published; the write methods refuse it as well"
                    }));
                }
            }
            let mut response = ok_envelope(ctx, data, "", json!(warnings));
            response.id = id;
            Ok(Some(response))
        }
        "vcs.diff" => Ok(Some(
            vcs_response(ctx, id, super::vcs_read::diff(&request.params).await?).await,
        )),
        "vcs.log" => Ok(Some(
            vcs_response(ctx, id, super::vcs_read::log(&request.params).await?).await,
        )),
        "vcs.show" => Ok(Some(
            vcs_response(ctx, id, super::vcs_read::show(&request.params).await?).await,
        )),
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
    let (mode, paths) = vcs::parse_legacy_diff_params(&request.params)?;
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
    move |err| super::vcs::internal_hiding_host_paths(context, err)
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

// ---------------------------------------------------------------------------
// Store-backed read dispatch (`RB-25`, works on Redb and SQLite)
// ---------------------------------------------------------------------------

fn store_error(context: &'static str) -> impl Fn(StorageBackendError) -> BridgeError {
    move |err| super::vcs::internal_hiding_host_paths(context, err)
}

fn ok_envelope_with_store(
    ctx: &StoreBridgeContext,
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

fn vcs_response_with_store(ctx: &StoreBridgeContext, id: Value, data: Value) -> BridgeResponse {
    let mut response = ok_envelope_with_store(ctx, data, "", json!([]));
    response.id = id;
    response
}

/// Dispatch a read method against [`StoreBridgeContext`]. Returns `Ok(None)`
/// when the method is not a read method.
pub async fn dispatch_with_store(
    ctx: &StoreBridgeContext,
    request: &BridgeRequest,
) -> Result<Option<BridgeResponse>, BridgeError> {
    let id = request.id.clone().unwrap_or(Value::Null);
    match request.method.as_str() {
        "context.get" => {
            let mut resp = context_get_with_store(ctx, request).await?;
            resp.id = id;
            Ok(Some(resp))
        }
        "status.get" => {
            let mut resp = status_get_with_store(ctx, request).await?;
            resp.id = id;
            Ok(Some(resp))
        }
        "history.search" => {
            let mut resp = history_search_with_store(ctx, request).await?;
            resp.id = id;
            Ok(Some(resp))
        }
        "checkpoint.list" => {
            let mut resp = checkpoint_list_with_store(ctx, request).await?;
            resp.id = id;
            Ok(Some(resp))
        }
        "checkpoint.show" => {
            let mut resp = checkpoint_show_with_store(ctx, request).await?;
            resp.id = id;
            Ok(Some(resp))
        }
        "diff.get" => {
            let mut resp = diff_get_with_store(ctx, request).await?;
            resp.id = id;
            Ok(Some(resp))
        }
        "vcs.fence" => {
            let fence = super::vcs_write::current_write_fence().await?;
            Ok(Some(vcs_response_with_store(
                ctx,
                id,
                json!({
                    "head": fence.head,
                    "indexGeneration": fence.index_generation,
                    "worktreeGeneration": fence.worktree_generation,
                }),
            )))
        }
        "vcs.identity" => Ok(Some(vcs_response_with_store(
            ctx,
            id,
            super::vcs_read::identity().await?,
        ))),
        "vcs.status" => {
            let observation = super::vcs_read::status_with_observation(&request.params).await?;
            let mut data = observation.data;
            super::vcs_write::rewrite_after_status_observation();
            let mut warnings: Vec<Value> = Vec::new();
            let fence = match (
                observation.index_generation.as_ref(),
                observation.worktree_generation.as_ref(),
            ) {
                (Some(index_generation), Some(worktree_generation)) => {
                    Some(super::vcs_write::fence_for_observation(
                        observation.head,
                        index_generation.clone(),
                        worktree_generation.clone(),
                    ))
                }
                _ => None,
            };
            match fence {
                Some(fence) => {
                    if let Some(object) = data.as_object_mut() {
                        object.insert(
                            "writeFence".to_string(),
                            json!({
                                "head": if fence.head.is_empty() {
                                    Value::Null
                                } else {
                                    Value::String(fence.head)
                                },
                                "indexGeneration": fence.index_generation,
                                "worktreeGeneration": fence.worktree_generation,
                            }),
                        );
                    }
                }
                None => {
                    warnings.push(json!({
                        "code": "write_fence_unavailable",
                        "message": "no write fence could be derived for this repository state, so \
                                    none is published; the write methods refuse it as well"
                    }));
                }
            }
            let mut response = ok_envelope_with_store(ctx, data, "", json!(warnings));
            response.id = id;
            Ok(Some(response))
        }
        "vcs.diff" => Ok(Some(vcs_response_with_store(
            ctx,
            id,
            super::vcs_read::diff(&request.params).await?,
        ))),
        "vcs.log" => Ok(Some(vcs_response_with_store(
            ctx,
            id,
            super::vcs_read::log(&request.params).await?,
        ))),
        "vcs.show" => Ok(Some(vcs_response_with_store(
            ctx,
            id,
            super::vcs_read::show(&request.params).await?,
        ))),
        _ => Ok(None),
    }
}

struct BridgeSessionSummary {
    session_id: String,
    state: String,
    agent_id: Option<String>,
    last_event_seq: i64,
    last_acked_seq: i64,
    created_at: i64,
}

struct BridgeCheckpointSummary {
    checkpoint_id: String,
    bridge_session_id: String,
    agent_checkpoint_id: Option<String>,
    target_oid: Option<String>,
    created_at: i64,
}

struct BridgeCatalogSnapshot {
    sessions: Vec<BridgeSessionSummary>,
    checkpoints: Vec<BridgeCheckpointSummary>,
    event_count: i64,
}

async fn load_bridge_catalog_snapshot(
    ctx: &StoreBridgeContext,
) -> Result<BridgeCatalogSnapshot, BridgeError> {
    match crate::storage::runtime::detect_repo_backend(&ctx.storage)
        .map_err(store_error("detect repository storage backend"))?
    {
        StorageBackendKind::Redb => {
            #[cfg(feature = "storage-redb")]
            {
                load_redb_bridge_catalog_snapshot(ctx).await
            }
            #[cfg(not(feature = "storage-redb"))]
            {
                Err(BridgeError::internal(
                    "this Libra binary was compiled without `storage-redb`",
                ))
            }
        }
        StorageBackendKind::Sqlite => {
            #[cfg(feature = "storage-sqlite")]
            {
                let capture_db = ctx
                    .storage
                    .join(crate::storage::PRIVATE_SUBDIR)
                    .join("capture.db");
                let db_path = if capture_db.exists() {
                    capture_db
                } else {
                    ctx.storage.join(crate::utils::util::DATABASE)
                };
                let conn = crate::internal::db::open_database_without_migrations(&db_path)
                    .await
                    .map_err(|err| {
                        super::vcs::internal_hiding_host_paths("open capture database", err)
                    })?;
                load_sqlite_bridge_catalog_snapshot_with_conn(&conn, &ctx.repository_id)
                    .await
                    .map_err(db_error("query bridge catalog"))
            }
            #[cfg(not(feature = "storage-sqlite"))]
            {
                Err(BridgeError::internal(
                    "this Libra binary was compiled without `storage-sqlite`",
                ))
            }
        }
    }
}

#[cfg(feature = "storage-redb")]
async fn load_redb_bridge_catalog_snapshot(
    ctx: &StoreBridgeContext,
) -> Result<BridgeCatalogSnapshot, BridgeError> {
    use std::collections::{BTreeSet, HashSet};

    use redb::ReadableTable;

    use crate::{
        internal::db::schema::DatabaseRole,
        storage::{
            AgentBridgeCheckpointRecord, AgentBridgeEventRecord, AgentBridgeSessionRecord,
            capture_store::redb_capture_store::{
                AGENT_BRIDGE_CHECKPOINT_TABLE, AGENT_BRIDGE_EVENT_TABLE,
                AGENT_BRIDGE_SESSION_TABLE, CAPTURE_CODEC_VERSION,
            },
            codec::decode_versioned,
            meta::begin_checked_read,
            opener::open_redb_read_only,
        },
    };

    match crate::storage::runtime::resolve_repo_capture_redb_path(&ctx.storage) {
        Ok(capture_path) => {
            let repo_id = ctx.repository_id.clone();
            tokio::task::spawn_blocking(
                move || -> crate::storage::StorageResult<BridgeCatalogSnapshot> {
                    let db = open_redb_read_only(&capture_path, DatabaseRole::Capture)?;
                    let rtx = begin_checked_read(&db, DatabaseRole::Capture)?;

                    let mut sessions = Vec::new();
                    let mut repo_session_ids = HashSet::new();
                    match rtx.open_table(AGENT_BRIDGE_SESSION_TABLE) {
                        Ok(tbl) => {
                            for entry in
                                tbl.iter()
                                    .map_err(|err| StorageBackendError::BackendInternal {
                                        detail: format!("iterate agent_bridge_session: {err}"),
                                    })?
                            {
                                let (_, raw) =
                                    entry.map_err(|err| StorageBackendError::BackendInternal {
                                        detail: format!("read agent_bridge_session entry: {err}"),
                                    })?;
                                let rec: AgentBridgeSessionRecord =
                                    decode_versioned(raw.value(), CAPTURE_CODEC_VERSION)?;
                                if rec.repository_id == repo_id {
                                    repo_session_ids.insert(rec.bridge_session_id.clone());
                                    sessions.push(BridgeSessionSummary {
                                        session_id: rec.bridge_session_id,
                                        state: rec.session_state.as_str().to_string(),
                                        agent_id: rec.agent_id,
                                        last_event_seq: rec.last_event_seq,
                                        last_acked_seq: rec.last_acked_seq,
                                        created_at: rec.created_at,
                                    });
                                }
                            }
                        }
                        Err(redb::TableError::TableDoesNotExist(_)) => {}
                        Err(err) => {
                            return Err(StorageBackendError::BackendInternal {
                                detail: format!("open agent_bridge_session: {err}"),
                            });
                        }
                    }

                    let mut checkpoints = Vec::new();
                    match rtx.open_table(AGENT_BRIDGE_CHECKPOINT_TABLE) {
                        Ok(tbl) => {
                            for entry in
                                tbl.iter()
                                    .map_err(|err| StorageBackendError::BackendInternal {
                                        detail: format!("iterate agent_bridge_checkpoint: {err}"),
                                    })?
                            {
                                let (_, raw) =
                                    entry.map_err(|err| StorageBackendError::BackendInternal {
                                        detail: format!(
                                            "read agent_bridge_checkpoint entry: {err}"
                                        ),
                                    })?;
                                let rec: AgentBridgeCheckpointRecord =
                                    decode_versioned(raw.value(), CAPTURE_CODEC_VERSION)?;
                                if repo_session_ids.contains(&rec.bridge_session_id) {
                                    checkpoints.push(BridgeCheckpointSummary {
                                        checkpoint_id: rec.checkpoint_id,
                                        bridge_session_id: rec.bridge_session_id,
                                        agent_checkpoint_id: rec.agent_checkpoint_id,
                                        target_oid: rec.target_oid,
                                        created_at: rec.created_at,
                                    });
                                }
                            }
                        }
                        Err(redb::TableError::TableDoesNotExist(_)) => {}
                        Err(err) => {
                            return Err(StorageBackendError::BackendInternal {
                                detail: format!("open agent_bridge_checkpoint: {err}"),
                            });
                        }
                    }

                    let mut event_count = 0i64;
                    match rtx.open_table(AGENT_BRIDGE_EVENT_TABLE) {
                        Ok(tbl) => {
                            for entry in
                                tbl.iter()
                                    .map_err(|err| StorageBackendError::BackendInternal {
                                        detail: format!("iterate agent_bridge_event: {err}"),
                                    })?
                            {
                                let (_, raw) =
                                    entry.map_err(|err| StorageBackendError::BackendInternal {
                                        detail: format!("read agent_bridge_event entry: {err}"),
                                    })?;
                                let rec: AgentBridgeEventRecord =
                                    decode_versioned(raw.value(), CAPTURE_CODEC_VERSION)?;
                                if repo_session_ids.contains(&rec.bridge_session_id) {
                                    event_count = event_count.saturating_add(1);
                                }
                            }
                        }
                        Err(redb::TableError::TableDoesNotExist(_)) => {}
                        Err(err) => {
                            return Err(StorageBackendError::BackendInternal {
                                detail: format!("open agent_bridge_event: {err}"),
                            });
                        }
                    }

                    Ok(BridgeCatalogSnapshot {
                        sessions,
                        checkpoints,
                        event_count,
                    })
                },
            )
            .await
            .map_err(|err| {
                super::vcs::internal_hiding_host_paths("read redb bridge catalog snapshot", err)
            })?
            .map_err(store_error("read redb bridge catalog snapshot"))
        }
        Err(StorageBackendError::NotFound { .. }) => {
            // Standalone store fallback when no `storage-format.json` exists yet.
            let events = ctx
                .capture
                .list_bridge_events_by_time()
                .await
                .map_err(store_error("list bridge events"))?;
            let session_ids: BTreeSet<String> =
                events.iter().map(|e| e.bridge_session_id.clone()).collect();
            let mut sessions = Vec::new();
            let mut repo_session_ids = HashSet::new();
            for sid in session_ids {
                if let Some(rec) = ctx
                    .capture
                    .get_bridge_session(&sid)
                    .await
                    .map_err(store_error("get bridge session"))?
                    && rec.repository_id == ctx.repository_id
                {
                    repo_session_ids.insert(rec.bridge_session_id.clone());
                    sessions.push(BridgeSessionSummary {
                        session_id: rec.bridge_session_id,
                        state: rec.session_state.as_str().to_string(),
                        agent_id: rec.agent_id,
                        last_event_seq: rec.last_event_seq,
                        last_acked_seq: rec.last_acked_seq,
                        created_at: rec.created_at,
                    });
                }
            }
            let event_count = events
                .iter()
                .filter(|e| repo_session_ids.contains(&e.bridge_session_id))
                .count() as i64;
            let mut checkpoints = Vec::new();
            for sid in &repo_session_ids {
                for cp in ctx
                    .capture
                    .list_bridge_checkpoints(sid)
                    .await
                    .map_err(store_error("list bridge checkpoints"))?
                {
                    checkpoints.push(BridgeCheckpointSummary {
                        checkpoint_id: cp.checkpoint_id,
                        bridge_session_id: cp.bridge_session_id,
                        agent_checkpoint_id: cp.agent_checkpoint_id,
                        target_oid: cp.target_oid,
                        created_at: cp.created_at,
                    });
                }
            }
            Ok(BridgeCatalogSnapshot {
                sessions,
                checkpoints,
                event_count,
            })
        }
        Err(err) => Err(store_error("resolve capture redb path")(err)),
    }
}

#[cfg(feature = "storage-sqlite")]
async fn load_sqlite_bridge_catalog_snapshot_with_conn(
    conn: &DatabaseConnection,
    repository_id: &str,
) -> Result<BridgeCatalogSnapshot, sea_orm::DbErr> {
    let session_rows = conn
        .query_all_raw(Statement::from_sql_and_values(
            DatabaseBackend::Sqlite,
            "SELECT bridge_session_id, session_state, agent_id, last_event_seq, last_acked_seq, \
                    created_at \
             FROM agent_bridge_session WHERE repository_id = ?",
            [repository_id.into()],
        ))
        .await?;
    let sessions = session_rows
        .into_iter()
        .map(|r| BridgeSessionSummary {
            session_id: r
                .try_get::<String>("", "bridge_session_id")
                .unwrap_or_default(),
            state: r.try_get::<String>("", "session_state").unwrap_or_default(),
            agent_id: r.try_get::<Option<String>>("", "agent_id").ok().flatten(),
            last_event_seq: r.try_get::<i64>("", "last_event_seq").unwrap_or(0),
            last_acked_seq: r.try_get::<i64>("", "last_acked_seq").unwrap_or(0),
            created_at: r.try_get::<i64>("", "created_at").unwrap_or(0),
        })
        .collect();

    let cp_rows = conn
        .query_all_raw(Statement::from_sql_and_values(
            DatabaseBackend::Sqlite,
            "SELECT c.checkpoint_id, c.bridge_session_id, c.agent_checkpoint_id, c.target_oid, \
                    c.created_at \
             FROM agent_bridge_checkpoint c \
             JOIN agent_bridge_session s ON s.bridge_session_id = c.bridge_session_id \
             WHERE s.repository_id = ?",
            [repository_id.into()],
        ))
        .await?;
    let checkpoints = cp_rows
        .into_iter()
        .map(|r| BridgeCheckpointSummary {
            checkpoint_id: r.try_get::<String>("", "checkpoint_id").unwrap_or_default(),
            bridge_session_id: r
                .try_get::<String>("", "bridge_session_id")
                .unwrap_or_default(),
            agent_checkpoint_id: r
                .try_get::<Option<String>>("", "agent_checkpoint_id")
                .ok()
                .flatten(),
            target_oid: r.try_get::<Option<String>>("", "target_oid").ok().flatten(),
            created_at: r.try_get::<i64>("", "created_at").unwrap_or(0),
        })
        .collect();

    let event_count = count_via_session(conn, "agent_bridge_event", "e", repository_id).await?;
    Ok(BridgeCatalogSnapshot {
        sessions,
        checkpoints,
        event_count,
    })
}

async fn context_get_with_store(
    ctx: &StoreBridgeContext,
    _request: &BridgeRequest,
) -> Result<BridgeResponse, BridgeError> {
    let mut snapshot = load_bridge_catalog_snapshot(ctx).await?;
    snapshot.sessions.sort_by(|a, b| {
        b.created_at
            .cmp(&a.created_at)
            .then_with(|| b.session_id.cmp(&a.session_id))
    });
    snapshot.checkpoints.sort_by(|a, b| {
        b.created_at
            .cmp(&a.created_at)
            .then_with(|| b.checkpoint_id.cmp(&a.checkpoint_id))
    });
    let sessions: Vec<Value> = snapshot
        .sessions
        .into_iter()
        .take(10)
        .map(|s| {
            json!({
                "session_id": s.session_id,
                "state": s.state,
                "last_event_seq": s.last_event_seq,
                "last_acked_seq": s.last_acked_seq,
            })
        })
        .collect();
    let checkpoints: Vec<Value> = snapshot
        .checkpoints
        .into_iter()
        .take(10)
        .map(|c| {
            json!({
                "checkpoint_id": c.checkpoint_id,
                "bridge_session_id": c.bridge_session_id,
                "agent_checkpoint_id": c.agent_checkpoint_id,
                "target_oid": c.target_oid,
            })
        })
        .collect();
    Ok(ok_envelope_with_store(
        ctx,
        json!({
            "sessions": sessions,
            "recent_checkpoints": checkpoints,
        }),
        "",
        json!([]),
    ))
}

async fn status_get_with_store(
    ctx: &StoreBridgeContext,
    _request: &BridgeRequest,
) -> Result<BridgeResponse, BridgeError> {
    let snapshot = load_bridge_catalog_snapshot(ctx).await?;
    Ok(ok_envelope_with_store(
        ctx,
        json!({
            "bridge_sessions": snapshot.sessions.len() as i64,
            "bridge_events": snapshot.event_count,
            "bridge_checkpoints": snapshot.checkpoints.len() as i64,
        }),
        "",
        json!([]),
    ))
}

async fn history_search_with_store(
    ctx: &StoreBridgeContext,
    request: &BridgeRequest,
) -> Result<BridgeResponse, BridgeError> {
    let term = param_str(&request.params, "query");
    let (limit, warnings) = page_limit(&request.params);
    let mut snapshot = load_bridge_catalog_snapshot(ctx).await?;
    snapshot.sessions.sort_by(|a, b| {
        b.created_at
            .cmp(&a.created_at)
            .then_with(|| b.session_id.cmp(&a.session_id))
    });
    let needle = term.as_deref().map(str::to_ascii_lowercase);
    let sessions: Vec<Value> = snapshot
        .sessions
        .into_iter()
        .filter(|s| match needle.as_deref() {
            Some(q) => {
                s.session_id.to_ascii_lowercase().contains(q)
                    || s.agent_id
                        .as_deref()
                        .is_some_and(|aid| aid.to_ascii_lowercase().contains(q))
            }
            None => true,
        })
        .take(limit)
        .map(|s| {
            json!({
                "session_id": s.session_id,
                "state": s.state,
                "last_acked_seq": s.last_acked_seq,
            })
        })
        .collect();
    Ok(ok_envelope_with_store(
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

async fn checkpoint_list_with_store(
    ctx: &StoreBridgeContext,
    request: &BridgeRequest,
) -> Result<BridgeResponse, BridgeError> {
    let session_id = param_str(&request.params, "session_id");
    let (limit, warnings) = page_limit(&request.params);
    let mut snapshot = load_bridge_catalog_snapshot(ctx).await?;
    snapshot.checkpoints.sort_by(|a, b| {
        b.created_at
            .cmp(&a.created_at)
            .then_with(|| b.checkpoint_id.cmp(&a.checkpoint_id))
    });
    let checkpoints: Vec<Value> = snapshot
        .checkpoints
        .into_iter()
        .filter(|c| match session_id.as_deref() {
            Some(sid) => c.bridge_session_id == sid,
            None => true,
        })
        .take(limit)
        .map(|c| {
            json!({
                "checkpoint_id": c.checkpoint_id,
                "bridge_session_id": c.bridge_session_id,
                "agent_checkpoint_id": c.agent_checkpoint_id,
                "target_oid": c.target_oid,
            })
        })
        .collect();
    Ok(ok_envelope_with_store(
        ctx,
        json!({ "limit": limit, "checkpoints": checkpoints }),
        "",
        json!(warnings),
    ))
}

async fn get_checkpoint_with_store(
    ctx: &StoreBridgeContext,
    checkpoint_id: &str,
) -> Result<Option<crate::storage::AgentBridgeCheckpointRecord>, BridgeError> {
    if checkpoint_id.is_empty() || checkpoint_id.contains('\0') {
        return Ok(None);
    }
    let Some(cp) = ctx
        .capture
        .get_bridge_checkpoint(checkpoint_id)
        .await
        .map_err(store_error("checkpoint.show: read checkpoint"))?
    else {
        return Ok(None);
    };
    let Some(session) = ctx
        .capture
        .get_bridge_session(&cp.bridge_session_id)
        .await
        .map_err(store_error("checkpoint.show: verify session scope"))?
    else {
        return Ok(None);
    };
    if session.repository_id != ctx.repository_id {
        return Ok(None);
    }
    Ok(Some(cp))
}

async fn checkpoint_show_with_store(
    ctx: &StoreBridgeContext,
    request: &BridgeRequest,
) -> Result<BridgeResponse, BridgeError> {
    let checkpoint_id = param_str(&request.params, "checkpoint_id")
        .ok_or_else(|| BridgeError::invalid_params("checkpoint.show requires checkpoint_id"))?;
    let Some(row) = get_checkpoint_with_store(ctx, &checkpoint_id).await? else {
        return Err(BridgeError::scope_mismatch(format!(
            "bridge checkpoint '{checkpoint_id}' does not exist"
        )));
    };
    Ok(ok_envelope_with_store(
        ctx,
        json!({
            "checkpoint_id": row.checkpoint_id,
            "bridge_session_id": row.bridge_session_id,
            "agent_checkpoint_id": row.agent_checkpoint_id,
            "target_oid": row.target_oid,
        }),
        "",
        json!([]),
    ))
}

async fn diff_get_with_store(
    ctx: &StoreBridgeContext,
    request: &BridgeRequest,
) -> Result<BridgeResponse, BridgeError> {
    let (mode, paths) = vcs::parse_legacy_diff_params(&request.params)?;
    let (limit, mut warnings) = page_limit(&request.params);
    let against = if mode == DiffMode::Checkpoint {
        let checkpoint_id = param_str(&request.params, "checkpoint_id").ok_or_else(|| {
            BridgeError::invalid_params("diff.get mode 'checkpoint' requires checkpoint_id")
        })?;
        Some(resolve_checkpoint_commit_with_store(ctx, &checkpoint_id).await?)
    } else {
        None
    };
    let (data, mut diff_warnings) = vcs::diff_data(mode, against, paths, limit).await?;
    warnings.append(&mut diff_warnings);
    Ok(ok_envelope_with_store(ctx, data, "", json!(warnings)))
}

/// Resolve the commit a bridge checkpoint pins via [`StoreBridgeContext`].
pub(super) async fn resolve_checkpoint_commit_with_store(
    ctx: &StoreBridgeContext,
    checkpoint_id: &str,
) -> Result<git_internal::hash::ObjectHash, BridgeError> {
    let Some(row) = get_checkpoint_with_store(ctx, checkpoint_id).await? else {
        return Err(BridgeError::scope_mismatch(format!(
            "bridge checkpoint '{checkpoint_id}' does not exist in this repository"
        )));
    };
    if let Some(oid) = row.target_oid.filter(|v| !v.trim().is_empty()) {
        return vcs::parse_stored_commit_oid(&oid, "the bridge checkpoint");
    }
    let Some(agent_checkpoint_id) = row.agent_checkpoint_id else {
        return Err(BridgeError::invalid_params(format!(
            "bridge checkpoint '{checkpoint_id}' pins no commit (no target_oid and no linked \
             agent checkpoint); it cannot be used as a diff or restore target"
        )));
    };
    let parent = ctx
        .capture
        .get_checkpoint(&agent_checkpoint_id)
        .await
        .map_err(store_error("resolve linked agent checkpoint"))?
        .and_then(|cp| cp.parent_commit);
    let Some(parent) = parent.filter(|v| !v.trim().is_empty()) else {
        return Err(BridgeError::invalid_params(format!(
            "bridge checkpoint '{checkpoint_id}' links agent checkpoint \
             '{agent_checkpoint_id}', which recorded no parent_commit (unborn HEAD at ingest); \
             it cannot be used as a diff or restore target"
        )));
    };
    vcs::parse_stored_commit_oid(&parent, "the linked agent checkpoint")
}
