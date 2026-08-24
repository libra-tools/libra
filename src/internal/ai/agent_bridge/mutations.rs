//! Bridge mutation dispatcher with approval / actor / operation-id /
//! provenance binding (plan-20260818 LB-05).
//!
//! Every mutation:
//!  1. requires an `operation_id` (idempotency + digest-conflict key);
//!  2. binds the trusted session scope (repository/actor lineage);
//!  3. enforces the approval gate for dangerous actions (default deny);
//!  4. dedupes by `operation_id` with a params digest (replay = no-op,
//!     digest drift = fail-closed conflict);
//!  5. records auditable provenance association links.
//!
//! `checkpoint.create` is wired to the durable bridge checkpoint store. The
//! VCS side-effect methods (`commit.create`, `review.run`,
//! `checkpoint.restore`) run the same admission/authorization pipeline and
//! then reach the real services through the typed [`super::vcs`] adapter:
//!
//!  - `commit.create` commits the current index (no `-a`, no pathspec, no
//!    amend, no author override) after an optional `expected_head` fence;
//!  - `checkpoint.restore` restores the working tree to the commit a bridge
//!    checkpoint pins, and refuses on HEAD drift or a dirty index/worktree so
//!    nothing is destroyed or partially applied;
//!  - `review.run` admits and starts a read-only review run and returns its
//!    identifiers; replaying the same `operation_id` reports that run's state
//!    instead of starting a second one.

use serde_json::{Value, json};
use sha2::{Digest, Sha256};

use super::{
    authorization::{Approval, bind_actor, enforce},
    ingress::BridgeContext,
    methods::resolve_checkpoint_commit,
    protocol::{BridgeError, BridgeRequest, BridgeResponse, SOURCE_DEEPSEEK_HARNESS},
    provenance, storage, vcs,
};

/// Returns `Ok(Some(response))` when `method` is a mutation handled here, or
/// `Ok(None)` for non-mutations (so the read/ingress dispatchers can try).
pub async fn dispatch(
    ctx: &BridgeContext,
    request: &BridgeRequest,
) -> Result<Option<BridgeResponse>, BridgeError> {
    let method = request.method.as_str();
    if !is_mutation(method) {
        return Ok(None);
    }
    let id = request.id.clone().unwrap_or(Value::Null);
    Ok(Some(dispatch_mutation(ctx, method, request, id).await?))
}

fn is_mutation(method: &str) -> bool {
    matches!(
        method,
        "checkpoint.create" | "checkpoint.restore" | "commit.create" | "review.run"
    )
}

fn ok_result(result: Value, id: Value) -> BridgeResponse {
    BridgeResponse {
        jsonrpc: crate::internal::ai::agent_bridge::protocol::JSONRPC_VERSION,
        result: Some(result),
        error: None,
        id,
    }
}

fn params_digest(params: &Option<Value>) -> String {
    let text = params
        .as_ref()
        .map(Value::to_string)
        .unwrap_or_else(|| "{}".to_string());
    let mut h = Sha256::new();
    h.update(text.as_bytes());
    format!("{:x}", h.finalize())
}

async fn dispatch_mutation(
    ctx: &BridgeContext,
    method: &str,
    request: &BridgeRequest,
    id: Value,
) -> Result<BridgeResponse, BridgeError> {
    // 1. operation_id is mandatory for every mutation.
    let operation_id = provenance::require_param_str(&request.params, "operation_id")?;

    // 2. Trusted session scope (repository/actor lineage).
    let (session, session_id) = provenance::trusted_session_scope(ctx, request).await?;

    // 3. Record the operation attempt up front (idempotency + digest
    //    conflict). Doing this before the gate/actor checks makes EVERY
    //    attempt traceable (LB-05: approval/denial/audit results must be
    //    traceable) and lets a denial/spoof land a terminal status instead of
    //    leaving no durable trace.
    let digest = params_digest(&request.params);
    let (fresh, existing_digest) = storage::upsert_operation(
        &ctx.conn,
        &operation_id,
        &session_id,
        method,
        &digest,
        &ctx.repository_id,
        session.workspace_id.as_deref(),
        now_ms(),
    )
    .await
    .map_err(|e| BridgeError::internal(format!("record bridge operation: {e}")))?;
    if !fresh
        && let Some(existing) = existing_digest
        && existing != digest
    {
        return Err(BridgeError::digest_conflict(format!(
            "operation '{operation_id}' was already recorded with a different payload digest; refusing replay with diverging params"
        )));
    }

    // 4. Approval gate (dangerous actions default deny). A denial is recorded
    //    durably so audit can trace it (never swallowed as success).
    let approval = match Approval::from_params(&request.params) {
        Some(Ok(a)) => Some(a),
        Some(Err(e)) => return Err(e),
        None => None,
    };
    if let Err(e) = enforce(method, approval.as_ref()) {
        storage::finish_operation(&ctx.conn, &operation_id, "failed", None, now_ms())
            .await
            .map_err(|err| BridgeError::internal(format!("finish bridge operation: {err}")))?;
        return Err(e);
    }

    // 5. Actor binding: request self-report must match the trusted scope.
    let self_actor = request
        .params
        .as_ref()
        .and_then(|p| p.get("actor"))
        .and_then(Value::as_object)
        .map(|o| {
            (
                o.get("kind").and_then(Value::as_str).unwrap_or(""),
                o.get("id").and_then(Value::as_str).unwrap_or(""),
            )
        });
    if let Some((rk, ri)) = self_actor
        && let Err(e) = bind_actor(
            method,
            Some((rk, ri)),
            Some("deepseek-harness"),
            session.agent_id.as_deref(),
        )
    {
        storage::finish_operation(&ctx.conn, &operation_id, "failed", None, now_ms())
            .await
            .map_err(|err| BridgeError::internal(format!("finish bridge operation: {err}")))?;
        return Err(e);
    }

    // 6. Replay short-circuit for the NON-idempotent services. `commit.create`,
    //    `review.run` and `checkpoint.restore` each have a real side effect;
    //    replaying a recorded `operation_id` must report the ORIGINAL result,
    //    never execute a second time (plan 故障恢复矩阵: "commit/checkpoint 后
    //    response 丢失 → 以 operation id 查询并返回原结果"). `checkpoint.create`
    //    is intrinsically idempotent and re-runs instead, so its replay keeps
    //    reporting `created: false`.
    if !fresh && replays_recorded_result(method) {
        let recorded = storage::get_operation_result(&ctx.conn, &operation_id)
            .await
            .map_err(|e| BridgeError::internal(format!("read bridge operation: {e}")))?;
        match recorded {
            Some((status, Some(result_digest))) if status == "applied" => {
                return Ok(ok_result(
                    replayed_result(method, &result_digest, &session_id)?,
                    id,
                ));
            }
            Some((status, _)) if status == "pending" => {
                return Err(BridgeError::denied(format!(
                    "operation '{operation_id}' ({method}) is still in flight; wait for it to \
                     finish before replaying"
                ))
                .retryable());
            }
            // `failed` / `quarantined` (or an applied row with no recorded
            // result): a corrected retry is allowed to run again.
            _ => {}
        }
    }

    // 7. Route to the underlying writer. Every path must reach a terminal
    //    operation status so a failed/poisoned `operation_id` is never left
    //    `pending` and never permanently blocks a corrected retry (LB-05).
    let outcome = match method {
        "checkpoint.create" => {
            checkpoint_create(ctx, &session, &session_id, &operation_id, request).await
        }
        "commit.create" => commit_create(ctx, &session, &session_id, &operation_id, request).await,
        "checkpoint.restore" => {
            checkpoint_restore(ctx, &session, &session_id, &operation_id, request).await
        }
        "review.run" => review_run(ctx, &session, &session_id, &operation_id, request).await,
        // INVARIANT: `is_mutation` (above) mirrors exactly the match arms in
        // `dispatch_mutation`, so every allowlisted mutation method that
        // reaches this dispatcher is one of the handled arms. This arm is
        // unreachable by construction.
        _ => unreachable!("is_mutation guards the match"),
    };
    let result = match outcome {
        Ok(value) => value,
        Err(e) => {
            storage::finish_operation(&ctx.conn, &operation_id, "failed", None, now_ms())
                .await
                .map_err(|err| BridgeError::internal(format!("finish bridge operation: {err}")))?;
            return Err(e);
        }
    };

    // Populate the result digest so `agent_bridge_operation.result_digest` is
    // meaningful AND replayable: it is the stable identity of what the
    // mutation produced (bridge checkpoint id, commit oid, restored commit
    // oid, review run id), which is exactly what `replayed_result` reads back.
    let result_digest = result_identity(method, &result);
    let result_digest = result_digest.as_deref();
    storage::finish_operation(&ctx.conn, &operation_id, "applied", result_digest, now_ms())
        .await
        .map_err(|e| BridgeError::internal(format!("finish bridge operation: {e}")))?;
    Ok(ok_result(result, id))
}

/// Whether a replayed `operation_id` must report the recorded result instead
/// of executing again. True for every mutation with a non-idempotent service
/// side effect.
fn replays_recorded_result(method: &str) -> bool {
    matches!(
        method,
        "commit.create" | "review.run" | "checkpoint.restore"
    )
}

/// The stable identity of a mutation result, stored as the operation's
/// `result_digest` and read back on replay.
fn result_identity(method: &str, result: &Value) -> Option<String> {
    let key = match method {
        "checkpoint.create" => "checkpoint_id",
        "commit.create" => "commit",
        "checkpoint.restore" => "target_commit",
        "review.run" => "run_id",
        _ => return None,
    };
    result.get(key).and_then(Value::as_str).map(str::to_string)
}

/// Rebuild the response for a replayed operation from its recorded identity.
fn replayed_result(
    method: &str,
    result_digest: &str,
    session_id: &str,
) -> Result<Value, BridgeError> {
    match method {
        "commit.create" => Ok(json!({
            "commit": result_digest,
            "session_id": session_id,
            "replayed": true,
        })),
        "checkpoint.restore" => Ok(json!({
            "target_commit": result_digest,
            "session_id": session_id,
            "head_moved": false,
            "replayed": true,
        })),
        "review.run" => {
            let mut state = vcs::review_state(result_digest)?;
            if let Some(object) = state.as_object_mut() {
                object.insert("replayed".to_string(), json!(true));
                object.insert("session_id".to_string(), json!(session_id));
            }
            Ok(state)
        }
        // INVARIANT: only `replays_recorded_result` methods reach here.
        _ => Err(BridgeError::internal(format!(
            "method '{method}' has no replay projection"
        ))),
    }
}

/// `commit.create` — commit the current index and record the association graph.
///
/// The commit itself carries no bridge metadata (LB-05 AC4): session, actor,
/// workspace, parent lineage and evidence ids are recorded as
/// `agent_bridge_link` rows keyed by the commit oid, so the association is
/// queryable without parsing a commit message.
async fn commit_create(
    ctx: &BridgeContext,
    session: &storage::BridgeSessionRow,
    session_id: &str,
    operation_id: &str,
    request: &BridgeRequest,
) -> Result<Value, BridgeError> {
    let message = vcs::parse_commit_message(&request.params)?;
    let signoff = param_bool(&request.params, "signoff");
    let allow_empty = param_bool(&request.params, "allow_empty");

    // HEAD fence: when the caller states the head it prepared against, a drift
    // means another writer committed in between, so we refuse before writing.
    if let Some(expected) = param_str(&request.params, "expected_head") {
        let fence = vcs::read_fence().await;
        vcs::check_head_fence(&fence, &expected)?;
    }

    let mut result = vcs::commit_create(&message, signoff, allow_empty).await?;
    let commit = result
        .get("commit")
        .and_then(Value::as_str)
        .ok_or_else(|| BridgeError::internal("commit.create produced no commit id".to_string()))?
        .to_string();

    let evidence_ids = evidence_ids(request);
    record_association_links(
        ctx,
        session,
        session_id,
        operation_id,
        "commit",
        &commit,
        &evidence_ids,
    )
    .await?;
    if let Some(object) = result.as_object_mut() {
        object.insert("session_id".to_string(), json!(session_id));
        object.insert(
            "provenance".to_string(),
            provenance::build_provenance(
                ctx,
                session_id,
                operation_id,
                Some(SOURCE_DEEPSEEK_HARNESS),
                session.agent_id.as_deref(),
                session.parent_session_id.as_deref(),
                session.workspace_id.as_deref(),
                None,
                &evidence_ids,
            ),
        );
    }
    Ok(result)
}

/// `checkpoint.restore` — restore the working tree to a bridge checkpoint's
/// commit.
///
/// Dangerous by classification, so an explicit approval has already been
/// enforced. On top of that the fence is verified here: HEAD must still be
/// where the caller expects, and the index/worktree must be clean, so no
/// uncommitted work is destroyed and nothing is partially applied (LB-05 AC5).
async fn checkpoint_restore(
    ctx: &BridgeContext,
    session: &storage::BridgeSessionRow,
    session_id: &str,
    operation_id: &str,
    request: &BridgeRequest,
) -> Result<Value, BridgeError> {
    // Params first: everything the request must carry is checked before the
    // bridge reads any store or touches the repository.
    let checkpoint_id = provenance::require_param_str(&request.params, "checkpoint_id")?;
    let expected_head = param_str(&request.params, "expected_head").ok_or_else(|| {
        BridgeError::invalid_params(
            "checkpoint.restore requires 'expected_head' (the HEAD commit the caller prepared \
             against) so a concurrent move is detected before the working tree is overwritten",
        )
    })?;
    let target = resolve_checkpoint_commit(ctx, &checkpoint_id).await?;

    // Fence, in fail-closed order: HEAD first (cheap, exact), then the
    // index/worktree cleanliness the restore would overwrite.
    let fence = vcs::read_fence().await;
    vcs::check_head_fence(&fence, &expected_head)?;
    vcs::check_clean_worktree_fence().await?;

    let mut result = vcs::checkpoint_restore(&target).await?;
    let evidence_ids = evidence_ids(request);
    record_association_links(
        ctx,
        session,
        session_id,
        operation_id,
        "restore",
        &format!("{checkpoint_id}@{target}"),
        &evidence_ids,
    )
    .await?;
    if let Some(object) = result.as_object_mut() {
        object.insert("checkpoint_id".to_string(), json!(checkpoint_id));
        object.insert("session_id".to_string(), json!(session_id));
    }
    Ok(result)
}

/// `review.run` — admit and start a read-only review run.
///
/// Validation (launchable reviewers, checkpoint scope, HEAD, run admission)
/// happens before any side effect; the reviewers themselves outlive the v1
/// request deadline, so the run is supervised in the background and the caller
/// polls it by replaying the same `operation_id`.
async fn review_run(
    ctx: &BridgeContext,
    session: &storage::BridgeSessionRow,
    session_id: &str,
    operation_id: &str,
    request: &BridgeRequest,
) -> Result<Value, BridgeError> {
    let review = vcs::parse_review_params(&request.params)?;
    let (run_id, mut state) = vcs::review_start(&review).await?;
    let evidence_ids = evidence_ids(request);
    record_association_links(
        ctx,
        session,
        session_id,
        operation_id,
        "review",
        &run_id,
        &evidence_ids,
    )
    .await?;
    if let Some(object) = state.as_object_mut() {
        object.insert("session_id".to_string(), json!(session_id));
    }
    Ok(state)
}

/// Record the durable association graph for one mutation result.
///
/// `source_id` is the result's stable identity (commit oid, review run id, …).
/// Each association is an `agent_bridge_link` row, so replay is idempotent and
/// a relink to a different target fails closed.
async fn record_association_links(
    ctx: &BridgeContext,
    session: &storage::BridgeSessionRow,
    session_id: &str,
    operation_id: &str,
    source_type: &str,
    source_id: &str,
    evidence_ids: &[String],
) -> Result<(), BridgeError> {
    use provenance::{Association, Relation};

    let mut links: Vec<Association<'_>> = vec![Association {
        source_type,
        source_id,
        relation: Relation::Operation,
        target_id: operation_id,
    }];
    if let Some(ws) = session.workspace_id.as_deref() {
        links.push(Association {
            source_type,
            source_id,
            relation: Relation::Workspace,
            target_id: ws,
        });
    }
    if let Some(parent) = session.parent_session_id.as_deref() {
        links.push(Association {
            source_type,
            source_id,
            relation: Relation::ParentSession,
            target_id: parent,
        });
    }
    for evidence in evidence_ids {
        links.push(Association {
            source_type,
            source_id,
            relation: Relation::Evidence,
            target_id: evidence,
        });
    }
    provenance::record_links(ctx, session_id, &links, now_ms()).await
}

/// The optional `evidence_ids` array of a mutation request.
fn evidence_ids(request: &BridgeRequest) -> Vec<String> {
    request
        .params
        .as_ref()
        .and_then(|p| p.get("evidence_ids"))
        .and_then(Value::as_array)
        .map(|arr| {
            arr.iter()
                .filter_map(Value::as_str)
                .map(str::to_string)
                .collect()
        })
        .unwrap_or_default()
}

fn param_bool(params: &Option<Value>, key: &str) -> bool {
    params
        .as_ref()
        .and_then(|p| p.get(key))
        .and_then(Value::as_bool)
        .unwrap_or(false)
}

fn param_str(params: &Option<Value>, key: &str) -> Option<String> {
    params
        .as_ref()
        .and_then(|p| p.get(key))
        .and_then(Value::as_str)
        .map(str::to_string)
}

async fn checkpoint_create(
    ctx: &BridgeContext,
    session: &storage::BridgeSessionRow,
    session_id: &str,
    operation_id: &str,
    request: &BridgeRequest,
) -> Result<Value, BridgeError> {
    let checkpoint_id = provenance::require_param_str(&request.params, "checkpoint_id")?;
    let agent_checkpoint_id = request
        .params
        .as_ref()
        .and_then(|p| p.get("agent_checkpoint_id"))
        .and_then(Value::as_str);
    let target_oid = request
        .params
        .as_ref()
        .and_then(|p| p.get("target_oid"))
        .and_then(Value::as_str);
    let evidence_ids: Vec<String> = request
        .params
        .as_ref()
        .and_then(|p| p.get("evidence_ids"))
        .and_then(Value::as_array)
        .map(|arr| {
            arr.iter()
                .filter_map(Value::as_str)
                .map(str::to_string)
                .collect()
        })
        .unwrap_or_default();

    let created = storage::insert_checkpoint(
        &ctx.conn,
        &checkpoint_id,
        session_id,
        agent_checkpoint_id,
        target_oid,
        now_ms(),
    )
    .await
    .map_err(|e| BridgeError::internal(format!("record bridge checkpoint: {e}")))?;

    // Persist the durable provenance association graph (LB-05 AC4: checkpoint
    // forms queryable associations with session, parent session/agent,
    // workspace, actor and evidence ids — never encoded into a commit
    // message). Each association is an `agent_bridge_link` row keyed by a
    // stable source, so replay is idempotent and the graph is queryable.
    record_association_links(
        ctx,
        session,
        session_id,
        operation_id,
        "checkpoint",
        &checkpoint_id,
        &evidence_ids,
    )
    .await?;

    Ok(json!({
        "checkpoint_id": checkpoint_id,
        "session_id": session_id,
        "created": created,
        "provenance": provenance::build_provenance(
            ctx,
            session_id,
            operation_id,
            Some(SOURCE_DEEPSEEK_HARNESS),
            session.agent_id.as_deref(),
            session.parent_session_id.as_deref(),
            session.workspace_id.as_deref(),
            None,
            &evidence_ids,
        ),
    }))
}

fn now_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}
