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
//!    no uncommitted work is overwritten. The restore rewrites paths one by
//!    one, so a failure part way through can leave the worktree partly
//!    restored; the restored commit is recorded before the optional
//!    bookkeeping, and an outcome that cannot be established is reported as
//!    indeterminate rather than restored a second time;
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
    provenance, publication, storage, vcs,
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
        "checkpoint.create"
            | "checkpoint.restore"
            | "commit.create"
            | "review.run"
            | "vcs.stage"
            | "vcs.unstage"
            | "vcs.commit"
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
    // 4. The approval block is parsed BEFORE the operation row is created. A
    //    malformed approval is a malformed request, not a failed operation: it
    //    must not leave a pending row that a corrected retry can never
    //    resolve (the corrected request has a different digest).
    let approval = match Approval::from_params(&request.params) {
        Some(Ok(a)) => Some(a),
        Some(Err(e)) => return Err(e),
        None => None,
    };

    let digest = params_digest(&request.params);
    let (fresh, existing) = storage::upsert_operation(
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
    .map_err(|error| vcs::internal_hiding_host_paths("record bridge operation", error))?;
    if !fresh && let Some(existing) = existing {
        // An operation id is bound to the method, the trusted scope and the
        // payload it was first used with. A different method under the same
        // id must never replay the first method's result.
        if existing.method != method {
            return Err(BridgeError::digest_conflict(format!(
                "operation '{operation_id}' was already recorded for method '{}'; refusing it for method '{method}'",
                existing.method
            )));
        }
        if existing.bridge_session_id != session_id || existing.repository_id != ctx.repository_id {
            return Err(BridgeError::scope_mismatch(format!(
                "operation '{operation_id}' was already recorded in another scope; refusing replay"
            )));
        }
        if existing.params_digest != digest {
            return Err(BridgeError::digest_conflict(format!(
                "operation '{operation_id}' was already recorded with a different payload digest; refusing replay with diverging params"
            )));
        }
    }

    // 5. Approval gate (dangerous actions default deny). A denial is recorded
    //    durably so audit can trace it (never swallowed as success).
    if let Err(e) = enforce(method, approval.as_ref()) {
        let recorded = storage::fail_operation_keep_result(&ctx.conn, &operation_id, now_ms())
            .await
            .map_err(|err| vcs::internal_hiding_host_paths("finish bridge operation", err))?;
        return Err(if recorded {
            e
        } else {
            with_durable_state_hint(e, &operation_id, false)
        });
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
        let recorded = storage::fail_operation_keep_result(&ctx.conn, &operation_id, now_ms())
            .await
            .map_err(|err| vcs::internal_hiding_host_paths("finish bridge operation", err))?;
        return Err(if recorded {
            e
        } else {
            with_durable_state_hint(e, &operation_id, false)
        });
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
            .map_err(|error| vcs::internal_hiding_host_paths("read bridge operation", error))?;
        // A terminal outcome is the operation's final answer. Every later
        // replay reports it and never runs the operation again.
        if let Some(result) = recorded.as_ref()
            && result.terminal_outcome
            && let Some(result_digest) = result.result_digest.as_ref()
        {
            return Ok(ok_result(
                replayed_result(method, result_digest, &session_id, true)?,
                id,
            ));
        }
        // A refusal that happened before any executor started is not an
        // outcome: the same operation id runs again.
        let refused_before_start = recorded.as_ref().is_some_and(|result| {
            is_refusal_row(
                &result.method,
                &result.status,
                result.result_digest.as_deref(),
            )
        });
        let started_before_start = recorded
            .as_ref()
            .and_then(|result| result.result_digest.as_deref())
            .is_some_and(is_started_marker);
        if matches!(method, "commit.create" | "vcs.commit")
            && let Some(result) = recorded.as_ref()
            && let Some(result_digest) = result.result_digest.as_ref()
            && commit_intent(result_digest).is_some()
        {
            match published_commit_for_intent(ctx, &operation_id, method).await? {
                CommitReconciliation::Terminal(terminal) => {
                    let terminal_json = serde_json::to_string(&terminal).map_err(|error| {
                        vcs::internal_hiding_host_paths("record terminal commit outcome", error)
                    })?;
                    return Ok(ok_result(
                        replayed_result(method, &terminal_json, &session_id, true)?,
                        id,
                    ));
                }
                CommitReconciliation::AlreadyApplied(digest) => {
                    let result = replay_applied_commit_result(
                        ctx,
                        &session,
                        &session_id,
                        &operation_id,
                        request,
                        method,
                        &digest,
                    )
                    .await?;
                    return Ok(ok_result(result, id));
                }
                CommitReconciliation::Published { oid, paths } => {
                    let result = record_published_commit_result(
                        ctx,
                        &session,
                        request,
                        &PublishedCommit {
                            operation_id: &operation_id,
                            session_id: &session_id,
                            method,
                            oid: &oid,
                            paths: &paths,
                        },
                    )
                    .await?;
                    return Ok(ok_result(result, id));
                }
            }
        } else if let Some(result) = recorded.as_ref()
            && let Some(result_digest) = result.result_digest.as_ref()
            && !refused_before_start
            && !started_before_start
        {
            if matches!(method, "vcs.stage" | "vcs.unstage") {
                // A stage or unstage records the index effect it is about to
                // install. Reconcile it against the repository's publication
                // record so a crash is reported as the truth — applied,
                // indeterminate, or not applied — instead of "still in
                // flight", and never replayed as a success it did not achieve.
                if vcs_write_intent(result_digest) {
                    let outcome = reconcile_vcs_write(ctx, &operation_id).await?;
                    let outcome_json = serde_json::to_string(&outcome).map_err(|error| {
                        vcs::internal_hiding_host_paths("record recovered vcs write", error)
                    })?;
                    return Ok(ok_result(
                        replayed_result(
                            method,
                            &outcome_json,
                            &session_id,
                            vcs_write_terminal(&outcome_json),
                        )?,
                        id,
                    ));
                }
            }
            if method == "commit.create" {
                if !recorded_commit_exists(result_digest) {
                    return Err(BridgeError::internal(
                        "recorded commit is missing or invalid; refusing replay",
                    ));
                }
                record_association_links(
                    ctx,
                    &session,
                    &session_id,
                    &operation_id,
                    "commit",
                    result_digest,
                    &evidence_ids(request),
                )
                .await?;
            }
            if method == "vcs.commit" {
                let oid = vcs_commit_oid(result_digest).ok_or_else(|| {
                    BridgeError::internal("recorded commit is missing or invalid; refusing replay")
                })?;
                if !recorded_commit_exists(&oid) {
                    return Err(BridgeError::internal(
                        "recorded commit is missing or invalid; refusing replay",
                    ));
                }
                if param_bool(&request.params, "linkSession") {
                    record_association_links(
                        ctx,
                        &session,
                        &session_id,
                        &operation_id,
                        "commit",
                        &oid,
                        &evidence_ids(request),
                    )
                    .await?;
                }
            }
            return Ok(ok_result(
                replayed_result(method, result_digest, &session_id, false)?,
                id,
            ));
        }
        match recorded {
            Some(storage::OperationResult {
                result_digest: Some(_),
                ..
            }) => {
                if started_before_start {
                    // The operation started but never recorded a result. The
                    // claim proves whether its executor is still running; a
                    // live executor makes this request retryable, and a
                    // stopped one gives the operation a durable terminal
                    // answer instead of a repeated side effect.
                    let reconciled =
                        reconcile_started_operation(ctx, &operation_id, method).await?;
                    let (payload, terminal) = reconciled.payload();
                    return Ok(ok_result(
                        replayed_result(method, payload, &session_id, terminal)?,
                        id,
                    ));
                }
            }
            Some(result) if result.status == "pending" => {
                if matches!(method, "vcs.stage" | "vcs.unstage") {
                    // The operation stopped before it recorded its intent, so
                    // it did not replace the index. Reconciling it under the
                    // repository lock proves no publisher is still running.
                    let outcome = reconcile_vcs_write(ctx, &operation_id).await?;
                    let outcome_json = serde_json::to_string(&outcome).map_err(|error| {
                        vcs::internal_hiding_host_paths("record recovered vcs write", error)
                    })?;
                    return Ok(ok_result(
                        replayed_result(
                            method,
                            &outcome_json,
                            &session_id,
                            vcs_write_terminal(&outcome_json),
                        )?,
                        id,
                    ));
                }
                if matches!(method, "vcs.commit" | "commit.create") {
                    // A commit that stopped before it recorded its intent
                    // published nothing that can be identified. Reconciling it
                    // under the repository lock proves that no publisher is
                    // still running, so the operation can reach a terminal
                    // state instead of staying in flight forever.
                    match reconcile_pending_commit(ctx, &operation_id, method).await? {
                        CommitReconciliation::Terminal(terminal) => {
                            let terminal_json =
                                serde_json::to_string(&terminal).map_err(|error| {
                                    vcs::internal_hiding_host_paths(
                                        "record recovered commit",
                                        error,
                                    )
                                })?;
                            return Ok(ok_result(
                                replayed_result(method, &terminal_json, &session_id, true)?,
                                id,
                            ));
                        }
                        CommitReconciliation::AlreadyApplied(digest) => {
                            let result = replay_applied_commit_result(
                                ctx,
                                &session,
                                &session_id,
                                &operation_id,
                                request,
                                method,
                                &digest,
                            )
                            .await?;
                            return Ok(ok_result(result, id));
                        }
                        CommitReconciliation::Published { oid, paths } => {
                            let result = record_published_commit_result(
                                ctx,
                                &session,
                                request,
                                &PublishedCommit {
                                    operation_id: &operation_id,
                                    session_id: &session_id,
                                    method,
                                    oid: &oid,
                                    paths: &paths,
                                },
                            )
                            .await?;
                            return Ok(ok_result(result, id));
                        }
                    }
                }
                // The operation stopped before it recorded a result. Take the
                // repository claim: it proves no executor is still running, so
                // the operation can reach a durable terminal answer. A busy
                // claim means the executor is live, and the caller retries.
                // The pause seam runs before the claim, so a test can change
                // the durable state while this replay is between the two.
                pause_before_reconciliation().await;
                let _claim = try_acquire_bridge_write_claim()?;
                // The row is re-read under the claim: the executor may have
                // recorded its result while this replay waited for the claim.
                let fresh = storage::get_operation_result(&ctx.conn, &operation_id)
                    .await
                    .map_err(|error| {
                        vcs::internal_hiding_host_paths("read bridge operation", error)
                    })?;
                if let Some(result) = fresh.as_ref()
                    && result.terminal_outcome
                    && let Some(digest) = result.result_digest.as_ref()
                {
                    let terminal = serde_json::from_str::<Value>(digest).map_err(|error| {
                        vcs::internal_hiding_host_paths("read terminal outcome", error)
                    })?;
                    return Ok(ok_result(
                        replayed_result(method, &terminal_json_of(&terminal)?, &session_id, true)?,
                        id,
                    ));
                }
                if let Some(result) = fresh.as_ref()
                    && result.status == "applied"
                    && let Some(digest) = result.result_digest.as_ref()
                {
                    return Ok(ok_result(
                        replayed_result(method, digest, &session_id, false)?,
                        id,
                    ));
                }
                // The operation started while this replay waited for the claim.
                // It is reconciled, never overwritten: its recorded identity —
                // a review run id, for instance — is the answer.
                if let Some(result) = fresh.as_ref()
                    && result
                        .result_digest
                        .as_deref()
                        .is_some_and(is_started_marker)
                {
                    let reconciled =
                        reconcile_started_under_claim(ctx, &operation_id, method).await?;
                    let (payload, terminal) = reconciled.payload();
                    return Ok(ok_result(
                        replayed_result(method, payload, &session_id, terminal)?,
                        id,
                    ));
                }
                let terminal = vcs_write_terminal_outcome(
                    &operation_id,
                    "indeterminate",
                    "this operation stopped before it recorded what it would do; whether it \
                     completed cannot be established, so re-read the current state and issue a \
                     new operation",
                );
                let durable =
                    record_terminal_outcome_or_durable(ctx, &operation_id, &terminal).await?;
                return Ok(ok_result(
                    replayed_result(method, &terminal_json_of(&durable)?, &session_id, true)?,
                    id,
                ));
            }
            // `refused`: the request was refused before any executor started,
            // so the same operation id may run again.
            Some(result) if result.status == "refused" => {}
            // `failed` / `quarantined` (or an applied row with no recorded
            // result): a corrected retry is allowed to run again — but only
            // for an idempotent method. A method with a real side effect must
            // not repeat it under the same operation id, so its failed row
            // becomes a durable terminal answer.
            _ => {
                if replays_recorded_result(method) && !refused_before_start {
                    // The claim proves no executor is still running, and the
                    // row is re-read under it: an executor that recorded its
                    // result or started meanwhile is honoured, never answered
                    // for.
                    let _claim = try_acquire_bridge_write_claim()?;
                    let fresh = storage::get_operation_result(&ctx.conn, &operation_id)
                        .await
                        .map_err(|error| {
                            vcs::internal_hiding_host_paths("read bridge operation", error)
                        })?;
                    if let Some(result) = fresh.as_ref()
                        && (result.terminal_outcome
                            || result
                                .result_digest
                                .as_deref()
                                .is_some_and(is_started_marker))
                    {
                        let reconciled =
                            reconcile_started_under_claim(ctx, &operation_id, method).await?;
                        let (payload, terminal) = reconciled.payload();
                        return Ok(ok_result(
                            replayed_result(method, payload, &session_id, terminal)?,
                            id,
                        ));
                    }
                    if let Some(result) = fresh.as_ref()
                        && result.status == "applied"
                        && let Some(digest) = result.result_digest.as_ref()
                    {
                        return Ok(ok_result(
                            replayed_result(method, digest, &session_id, false)?,
                            id,
                        ));
                    }
                    let terminal = vcs_write_terminal_outcome(
                        &operation_id,
                        "indeterminate",
                        "this operation failed before it recorded a verifiable result; whether \
                         it completed cannot be established, so re-read the current state and \
                         issue a new operation",
                    );
                    let durable =
                        record_terminal_outcome_or_durable(ctx, &operation_id, &terminal).await?;
                    return Ok(ok_result(
                        replayed_result(method, &terminal_json_of(&durable)?, &session_id, true)?,
                        id,
                    ));
                }
            }
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
        "vcs.stage" | "vcs.unstage" | "vcs.commit" => {
            vcs_write(ctx, &session, &session_id, &operation_id, method, request).await
        }
        // INVARIANT: `is_mutation` (above) mirrors exactly the match arms in
        // `dispatch_mutation`, so every allowlisted mutation method that
        // reaches this dispatcher is one of the handled arms. This arm is
        // unreachable by construction.
        _ => unreachable!("is_mutation guards the match"),
    };
    let result = match outcome {
        Ok(value) => value,
        Err(e) => {
            // A refusal that happened BEFORE the executor started — the
            // repository claim was busy, or the review budget was full — has no
            // side effect. It keeps the operation retryable under the same
            // operation id, so the caller's documented retry really retries.
            if e.retryable && e.stable_code == "LBR-AGENT-035" {
                let marker = refusal_marker("the repository was busy when this request arrived");
                let refused = storage::finish_operation_if_not_started(
                    &ctx.conn,
                    &operation_id,
                    "failed",
                    Some(&marker),
                    now_ms(),
                )
                .await
                .map_err(|err| vcs::internal_hiding_host_paths("finish bridge operation", err))?;
                if !refused {
                    // The operation is no longer a fresh start: the durable row
                    // decides. A refusal marker means the operation never
                    // started and the refusal stays its answer; a durable
                    // result is reported as it is; anything else means the
                    // operation is under way and the caller must replay it
                    // instead of retrying a side effect.
                    if let Some(durable) =
                        replay_durable_operation(ctx, &operation_id, method, &session_id).await?
                    {
                        return Ok(ok_result(durable, id));
                    }
                    let recorded = storage::get_operation_result(&ctx.conn, &operation_id)
                        .await
                        .map_err(|error| {
                            vcs::internal_hiding_host_paths("read bridge operation", error)
                        })?;
                    let refused_before_start = recorded.as_ref().is_some_and(|result| {
                        is_refusal_row(
                            &result.method,
                            &result.status,
                            result.result_digest.as_deref(),
                        )
                    });
                    if refused_before_start {
                        return Err(e);
                    }
                    return Err(BridgeError::internal(format!(
                        "the operation '{operation_id}' already has a durable state; replay it to \
                         learn its outcome"
                    ))
                    .retryable());
                }
                return Err(e);
            }
            let failed = storage::fail_operation_keep_result(&ctx.conn, &operation_id, now_ms())
                .await
                .map_err(|err| vcs::internal_hiding_host_paths("finish bridge operation", err))?;
            if !failed {
                // Another request already recorded this operation's final
                // answer, so the local error would hide it. The error keeps
                // its own context (a failed association link, for example) and
                // names the durable outcome the caller must replay.
                let retryable = e.stable_code == "LBR-AGENT-038";
                return Err(with_durable_state_hint(e, &operation_id, retryable));
            }
            return Err(e);
        }
    };

    // Populate the result digest so `agent_bridge_operation.result_digest` is
    // meaningful AND replayable: it is the stable identity of what the
    // mutation produced (bridge checkpoint id, commit oid, restored commit
    // oid, review run id), which is exactly what `replayed_result` reads back.
    let result_digest = result_identity(method, &result);
    let result_digest = result_digest.as_deref();
    let recorded = storage::finish_operation_unless_terminal(
        &ctx.conn,
        &operation_id,
        "applied",
        result_digest,
        now_ms(),
    )
    .await
    .map_err(|error| vcs::internal_hiding_host_paths("finish bridge operation", error))?;
    if !recorded {
        // The operation already has its final answer, so the durable outcome is
        // what the caller is told — never this caller's own result.
        let durable = durable_outcome(ctx, &operation_id).await?;
        return Ok(ok_result(durable, id));
    }
    Ok(ok_result(result, id))
}

/// Whether a replayed `operation_id` must report the recorded result instead
/// of executing again. True for every mutation with a non-idempotent service
/// side effect.
fn replays_recorded_result(method: &str) -> bool {
    matches!(
        method,
        "commit.create"
            | "review.run"
            | "checkpoint.restore"
            | "vcs.stage"
            | "vcs.unstage"
            | "vcs.commit"
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
        "vcs.stage" | "vcs.unstage" | "vcs.commit" => {
            return serde_json::to_string(result).ok();
        }
        _ => return None,
    };
    result.get(key).and_then(Value::as_str).map(str::to_string)
}

fn vcs_commit_oid(digest: &str) -> Option<String> {
    serde_json::from_str::<Value>(digest)
        .ok()?
        .get("commitOid")
        .and_then(Value::as_str)
        .map(str::to_string)
}

fn recorded_commit_exists(digest: &str) -> bool {
    use git_internal::internal::object::commit::Commit;

    use crate::utils::object_ext::CommitExt;

    let Ok(oid) = crate::internal::object_format::parse_repo_oid(digest) else {
        return false;
    };
    Commit::load(&oid).is_ok()
}

fn commit_intent(digest: &str) -> Option<Value> {
    let value: Value = serde_json::from_str(digest).ok()?;
    if value.get("commitIntent").and_then(Value::as_bool) != Some(true) {
        return None;
    }
    Some(value)
}

fn try_acquire_bridge_write_claim() -> Result<super::vcs_write::BridgeWriteClaim, BridgeError> {
    super::vcs_write::BridgeWriteClaim::try_acquire()?
        .ok_or_else(|| {
            BridgeError::denied(
                "another bridge write or recovery is active in this repository; retry after it finishes",
            )
            .retryable()
        })
}

/// Refuse an executor if another request already completed or terminalized its
/// operation before this executor acquired the repository claim.
async fn ensure_operation_can_start(
    ctx: &BridgeContext,
    operation_id: &str,
    method: &str,
) -> Result<(), BridgeError> {
    // The row is re-read under the repository claim, so a request that read a
    // stale state earlier cannot execute an operation that another request has
    // since started.
    let recorded = storage::get_operation_result(&ctx.conn, operation_id)
        .await
        .map_err(|error| vcs::internal_hiding_host_paths("read bridge operation", error))?;
    if let Some(result) = recorded.as_ref()
        && (result.terminal_outcome || result.status == "applied")
    {
        return Err(BridgeError::stale_fence(format!(
            "stale_state: operation '{operation_id}' ({method}) already has a final result; issue a new operation"
        )));
    }
    // An operation that already started is reconciled, never repeated. A
    // commit intent counts: that commit may already have published.
    if let Some(result) = recorded.as_ref()
        && result.result_digest.as_deref().is_some_and(|digest| {
            is_started_marker(digest) || vcs_write_intent(digest) || commit_intent(digest).is_some()
        })
    {
        return Err(BridgeError::stale_fence(format!(
            "stale_state: operation '{operation_id}' ({method}) already started; replay it to learn \
             its outcome instead of running it again"
        )));
    }
    Ok(())
}

/// The outcome of a commit reconciliation.
enum CommitReconciliation {
    /// The operation published this commit and the operation row still has an
    /// intent; the caller should finish recording its replayable result.
    Published { oid: String, paths: Value },
    /// The operation row became applied while this replay waited for the
    /// cross-process claim. Preserve the stored payload's original contract.
    AlreadyApplied(String),
    /// The operation's outcome is terminal and must never run again.
    Terminal(Value),
}

async fn terminalize_commit_recovery(
    ctx: &BridgeContext,
    operation_id: &str,
    message: &str,
) -> Result<CommitReconciliation, BridgeError> {
    let terminal = vcs_write_terminal_outcome(operation_id, "indeterminate", message);
    let durable = record_terminal_outcome_or_durable(ctx, operation_id, &terminal).await?;
    Ok(CommitReconciliation::Terminal(durable))
}

async fn published_commit_for_intent(
    ctx: &BridgeContext,
    operation_id: &str,
    method: &str,
) -> Result<CommitReconciliation, BridgeError> {
    let _claim = try_acquire_bridge_write_claim()?;
    reconcile_commit_under_claim(ctx, operation_id, method).await
}

/// Reconcile a commit operation while the caller already holds the repository
/// bridge claim.
///
/// The claim proves that no other bridge executor or reconciler is running, so
/// the operation row and the publication record cannot change under this
/// decision.
async fn reconcile_commit_under_claim(
    ctx: &BridgeContext,
    operation_id: &str,
    method: &str,
) -> Result<CommitReconciliation, BridgeError> {
    let _gate = super::vcs_write::acquire_publish_gate().await;
    let fresh = storage::get_operation_result(&ctx.conn, operation_id)
        .await
        .map_err(|error| vcs::internal_hiding_host_paths("read bridge operation", error))?;
    if let Some(result) = fresh.as_ref()
        && result.terminal_outcome
        && let Some(digest) = result.result_digest.as_ref()
    {
        let terminal = serde_json::from_str::<Value>(digest).map_err(|error| {
            vcs::internal_hiding_host_paths("read terminal commit outcome", error)
        })?;
        return Ok(CommitReconciliation::Terminal(terminal));
    }
    if let Some(result) = fresh.as_ref()
        && result.status == "applied"
        && let Some(digest) = result.result_digest.as_ref()
    {
        return Ok(CommitReconciliation::AlreadyApplied(digest.clone()));
    }
    let intent = fresh
        .as_ref()
        .and_then(|result| result.result_digest.as_deref())
        .and_then(commit_intent);
    if let Some(oid) = publication::published_commit(operation_id)
        .await
        .map_err(|error| {
            vcs::internal_hiding_host_paths("commit recovery could not read its publication", error)
        })?
    {
        if !recorded_commit_exists(&oid) {
            return terminalize_commit_recovery(
                ctx,
                operation_id,
                "the recorded commit publication cannot be verified; inspect repository history and issue a new operation",
            )
            .await;
        }
        return Ok(CommitReconciliation::Published {
            oid,
            paths: intent
                .as_ref()
                .and_then(|value| value.get("paths").cloned())
                .unwrap_or_else(|| json!([])),
        });
    }
    let Some(intent) = intent else {
        return terminalize_commit_recovery(
            ctx,
            operation_id,
            "this commit intent disappeared before recovery; inspect repository history and issue a new operation",
        )
        .await;
    };
    let Some(expected) = intent.get("expectedHead").and_then(Value::as_str) else {
        return terminalize_commit_recovery(
            ctx,
            operation_id,
            "this commit intent has no valid HEAD fence; inspect repository history and issue a new operation",
        )
        .await;
    };

    // The repository claim proves no bridge executor is still running; the
    // exclusive maintenance lock now excludes supported CLI writers while we
    // validate and, if safe, resume the prepared publication.
    let _publishers = super::vcs_write::acquire_repository_write_lock()?;
    if let Some(oid) = publication::published_commit(operation_id)
        .await
        .map_err(|error| {
            vcs::internal_hiding_host_paths(
                "commit recovery could not re-read its publication",
                error,
            )
        })?
    {
        if !recorded_commit_exists(&oid) {
            return terminalize_commit_recovery(
                ctx,
                operation_id,
                "the recorded commit publication cannot be verified; inspect repository history and issue a new operation",
            )
            .await;
        }
        return Ok(CommitReconciliation::Published {
            oid,
            paths: intent.get("paths").cloned().unwrap_or_else(|| json!([])),
        });
    }
    let prepared = publication::prepared_commit(operation_id)
        .await
        .map_err(|error| vcs::internal_hiding_host_paths("re-read prepared commit", error))?;
    let Some(prepared) = prepared else {
        // No prepared publication. A commit prepares outside the publication
        // lock and publishes inside it, so this cannot prove that nothing was
        // published: the outcome is uncertain, not "not applied".
        let terminal = vcs_write_terminal_outcome(
            operation_id,
            "indeterminate",
            "whether this operation published a commit cannot be established, so re-read the \
             repository history and issue a new operation",
        );
        let durable = record_terminal_outcome_or_durable(ctx, operation_id, &terminal).await?;
        return Ok(CommitReconciliation::Terminal(durable));
    };
    if !recorded_commit_exists(&prepared) {
        return terminalize_commit_recovery(
            ctx,
            operation_id,
            "the prepared commit object cannot be verified; inspect repository history and issue a new operation",
        )
        .await;
    }
    let Some(message) = intent.get("message").and_then(Value::as_str) else {
        return terminalize_commit_recovery(
            ctx,
            operation_id,
            "this commit intent has no valid message; inspect repository history and issue a new operation",
        )
        .await;
    };
    let head = vcs::read_fence().await?;
    let actual_head = head
        .head
        .as_ref()
        .map(|oid| oid.to_string())
        .unwrap_or_default();
    if actual_head != expected {
        return terminalize_commit_recovery(
            ctx,
            operation_id,
            "repository HEAD changed after this commit was prepared; the operation is terminal and must not be retried",
        )
        .await;
    }
    let expected_index = intent
        .get("expectedIndex")
        .and_then(Value::as_str)
        .map(str::to_string);
    let expected_worktree = intent
        .get("expectedWorktree")
        .and_then(Value::as_str)
        .map(str::to_string);
    if method == "vcs.commit" && (expected_index.is_none() || expected_worktree.is_none()) {
        return terminalize_commit_recovery(
            ctx,
            operation_id,
            "this commit intent has an incomplete admitted fence; inspect repository history and issue a new operation",
        )
        .await;
    }
    if expected_worktree.is_some() && expected_index.is_none() {
        return terminalize_commit_recovery(
            ctx,
            operation_id,
            "this commit intent has an incomplete index/worktree fence; inspect repository history and issue a new operation",
        )
        .await;
    }
    if let Some(expected_worktree) = expected_worktree.as_deref() {
        let actual = super::vcs_write::current_write_fence().await?;
        if super::vcs_write::require_matching_fence(
            &actual,
            expected,
            expected_index.as_deref().unwrap_or_default(),
            expected_worktree,
        )
        .is_err()
        {
            return terminalize_commit_recovery(
                ctx,
                operation_id,
                "repository state changed after this commit was prepared; the operation is terminal and must not be retried",
            )
            .await;
        }
    } else if let Some(expected_index) = expected_index.as_deref() {
        let (actual_head, actual_index) = super::vcs_write::current_commit_fence().await?;
        if actual_head != expected || actual_index != expected_index {
            return terminalize_commit_recovery(
                ctx,
                operation_id,
                "repository HEAD or index changed after this commit was prepared; the operation is terminal and must not be retried",
            )
            .await;
        }
    }
    if crate::command::commit::ensure_bridge_prepared_recovery_has_no_sequences()
        .await
        .is_err()
    {
        return terminalize_commit_recovery(
            ctx,
            operation_id,
            "prepared commit recovery is blocked by an active merge, cherry-pick, revert, or squash; resolve it and issue a new operation",
        )
        .await;
    }
    let commit_result = publication::with_operation_fence(
        operation_id,
        expected,
        expected_index.as_deref(),
        expected_worktree.as_deref(),
        async { crate::command::commit::resume_prepared_bridge_commit(&prepared, message).await },
    )
    .await;
    match commit_result {
        Ok(()) => {}
        Err(crate::command::commit::CommitError::BridgeFenceChanged) => {
            return terminalize_commit_recovery(
                ctx,
                operation_id,
                "repository state changed during prepared commit recovery; inspect repository history and issue a new operation",
            )
            .await;
        }
        Err(_error) => {
            return terminalize_commit_recovery(
                ctx,
                operation_id,
                "prepared commit recovery stopped without verifiable publication; inspect repository history and issue a new operation",
            )
            .await;
        }
    }
    let published = publication::published_commit(operation_id)
        .await
        .map_err(|error| {
            vcs::internal_hiding_host_paths("commit recovery could not verify publication", error)
        })?;
    if published.as_deref() != Some(prepared.as_str()) {
        return terminalize_commit_recovery(
            ctx,
            operation_id,
            "prepared commit recovery ended without a verifiable publication record; inspect repository history and issue a new operation",
        )
        .await;
    }
    Ok(CommitReconciliation::Published {
        oid: prepared,
        paths: intent.get("paths").cloned().unwrap_or_else(|| json!([])),
    })
}

/// Whether a recorded digest is a stage/unstage intent rather than a result.
fn vcs_write_intent(digest: &str) -> bool {
    serde_json::from_str::<Value>(digest)
        .ok()
        .and_then(|value| value.get("vcsWriteIntent").and_then(Value::as_bool))
        .unwrap_or(false)
}

/// Whether a recorded digest is a terminal stage/unstage outcome.
fn vcs_write_terminal(digest: &str) -> bool {
    serde_json::from_str::<Value>(digest)
        .ok()
        .and_then(|value| {
            value
                .get("vcsWriteOutcome")
                .and_then(Value::as_str)
                .map(str::to_string)
        })
        .is_some()
}

/// The terminal outcome of a stage or unstage that cannot be replayed.
///
/// The recorded outcome is durable, so a later replay of the same operation id
/// reports it instead of running the operation again.
fn vcs_write_terminal_outcome(operation_id: &str, outcome: &str, message: &str) -> Value {
    vcs_write_terminal_outcome_with(operation_id, outcome, message, json!({}))
}

/// A terminal outcome with the extra facts the operation recorded.
fn vcs_write_terminal_outcome_with(
    operation_id: &str,
    outcome: &str,
    message: &str,
    extra: Value,
) -> Value {
    let mut value = json!({
        "vcsWriteOutcome": outcome,
        "operationId": operation_id,
        "outcome": outcome,
        "commitOid": Value::Null,
        "paths": Value::Array(Vec::new()),
        "failures": [],
        "message": message,
    });
    if let (Some(object), Some(extra)) = (value.as_object_mut(), extra.as_object()) {
        for (key, entry) in extra {
            object.insert(key.clone(), entry.clone());
        }
    }
    value
}

/// Reconcile a stage or unstage whose recorded outcome is missing.
///
/// The evidence is operation-specific: the repository records what this
/// operation was about to install and whether that installation completed.
/// Holding the repository's exclusive publication lock proves that no
/// publisher is running, so the record cannot still change under this
/// decision; a lock that is already held means another operation is
/// publishing, and this answer is refused instead of guessed.
async fn reconcile_vcs_write(
    ctx: &BridgeContext,
    operation_id: &str,
) -> Result<Value, BridgeError> {
    let _claim = try_acquire_bridge_write_claim()?;
    let _gate = super::vcs_write::acquire_publish_gate().await;
    let _publishers = super::vcs_write::acquire_repository_write_lock()?;

    // Everything is decided from the operation row as it is NOW, under the
    // lock: the operation may have completed while this replay waited, and a
    // terminal outcome another replay already recorded must never be
    // overwritten with a different answer.
    let fresh = storage::get_operation_result(&ctx.conn, operation_id)
        .await
        .map_err(|error| vcs::internal_hiding_host_paths("read bridge operation", error))?;
    if let Some(result) = fresh.as_ref()
        && result.terminal_outcome
        && let Some(digest) = result.result_digest.as_ref()
    {
        return serde_json::from_str::<Value>(digest)
            .map_err(|error| vcs::internal_hiding_host_paths("read terminal outcome", error));
    }
    if let Some(result) = fresh.as_ref()
        && result.status == "applied"
        && let Some(digest) = result.result_digest.as_ref()
    {
        return serde_json::from_str::<Value>(digest)
            .map_err(|error| vcs::internal_hiding_host_paths("read recorded result", error));
    }
    // The intent state AND the paths come from the fresh row, not from what
    // the caller saw before it waited for the lock: the original execution
    // may have recorded its intent in the meantime.
    let fresh_intent = fresh
        .as_ref()
        .and_then(|result| result.result_digest.as_deref())
        .and_then(|digest| serde_json::from_str::<Value>(digest).ok());
    let has_intent = fresh_intent
        .as_ref()
        .and_then(|value| value.get("vcsWriteIntent").and_then(Value::as_bool))
        .unwrap_or(false);
    let paths = fresh_intent
        .as_ref()
        .and_then(|value| value.get("paths").cloned())
        .unwrap_or_else(|| json!([]));

    let recorded = publication::index_publication(operation_id)
        .await
        .map_err(|error| vcs::internal_hiding_host_paths("read index publication", error))?;
    let outcome = match recorded {
        Some((_, phase)) if phase == "published" => {
            let published = json!({
                "operationId": operation_id,
                "outcome": "applied",
                "commitOid": Value::Null,
                "paths": paths,
                "failures": [],
            });
            let published_json = serde_json::to_string(&published).map_err(|error| {
                vcs::internal_hiding_host_paths("record recovered vcs write", error)
            })?;
            let recorded = storage::finish_operation_unless_terminal(
                &ctx.conn,
                operation_id,
                "applied",
                Some(&published_json),
                now_ms(),
            )
            .await
            .map_err(|error| {
                vcs::internal_hiding_host_paths("record recovered vcs write", error)
            })?;
            if !recorded {
                let durable = storage::get_operation_result(&ctx.conn, operation_id)
                    .await
                    .map_err(|error| {
                        vcs::internal_hiding_host_paths("read recovered vcs write", error)
                    })?
                    .ok_or_else(|| BridgeError::internal("bridge operation row disappeared"))?;
                let digest = durable.result_digest.ok_or_else(|| {
                    BridgeError::internal("bridge operation has no durable result")
                })?;
                if durable.terminal_outcome || durable.status == "applied" {
                    return serde_json::from_str::<Value>(&digest).map_err(|error| {
                        vcs::internal_hiding_host_paths("read recovered vcs write", error)
                    });
                }
                return Err(BridgeError::internal(
                    "recovered vcs write result was rejected without a durable outcome",
                )
                .retryable());
            }
            published
        }
        Some(_) => {
            // The operation was prepared but never recorded a completed
            // publication. The replacement may or may not have happened before
            // the bridge stopped, so the outcome is INDETERMINATE. It is
            // recorded as terminal and never re-run.
            let terminal = vcs_write_terminal_outcome(
                operation_id,
                "indeterminate",
                "the index replacement for this operation may or may not have completed before \
                 the bridge stopped; re-read the index state and issue a new operation",
            );
            record_terminal_outcome_or_durable(ctx, operation_id, &terminal).await?
        }
        None => {
            // The operation recorded its intent under the repository lock and
            // never reached the replacement, so it did not replace the index.
            // The preparation may have stored content objects for the staged
            // paths.
            // Without an intent the executor may still be starting, so the
            // outcome cannot be established.
            if !has_intent {
                let terminal = vcs_write_terminal_outcome(
                    operation_id,
                    "indeterminate",
                    "this operation stopped before it recorded what it would change; it may \
                     still be starting, so re-read the index state and issue a new operation",
                );
                return record_terminal_outcome_or_durable(ctx, operation_id, &terminal).await;
            }
            let terminal = vcs_write_terminal_outcome(
                operation_id,
                "not-applied",
                "this operation stopped before it replaced the index; the index was not changed \
                 (it may have stored objects for the staged content)",
            );
            record_terminal_outcome_or_durable(ctx, operation_id, &terminal).await?
        }
    };
    Ok(outcome)
}

/// Reconcile a commit operation that never recorded its intent.
///
/// The commit executor records its intent before it prepares anything, so a
/// pending row without an intent means the executor stopped before it could
/// publish. Holding the repository bridge claim proves no bridge executor is
/// live; the maintenance lock also excludes supported CLI writers while the
/// terminal decision is recorded.
async fn reconcile_pending_commit(
    ctx: &BridgeContext,
    operation_id: &str,
    method: &str,
) -> Result<CommitReconciliation, BridgeError> {
    pause_before_reconciliation().await;
    let _claim = try_acquire_bridge_write_claim()?;

    let recorded = storage::get_operation_result(&ctx.conn, operation_id)
        .await
        .map_err(|error| vcs::internal_hiding_host_paths("read bridge operation", error))?;
    if let Some(result) = recorded.as_ref()
        && result.terminal_outcome
        && let Some(digest) = result.result_digest.as_ref()
    {
        let terminal = serde_json::from_str::<Value>(digest)
            .map_err(|error| vcs::internal_hiding_host_paths("read terminal outcome", error))?;
        return Ok(CommitReconciliation::Terminal(terminal));
    }
    if let Some(result) = recorded.as_ref()
        && result.status == "applied"
        && let Some(digest) = result.result_digest.as_ref()
    {
        return Ok(CommitReconciliation::AlreadyApplied(digest.clone()));
    }
    // The row may have gained a commit intent while this replay waited for the
    // claim. Such an operation must be reconciled against its publication
    // record, not terminalized: it may have published a commit already.
    if recorded
        .as_ref()
        .and_then(|result| result.result_digest.as_deref())
        .and_then(commit_intent)
        .is_some()
    {
        return reconcile_commit_under_claim(ctx, operation_id, method).await;
    }

    // The operation recorded no intent. Decide under the publication locks,
    // and re-read the row there: an executor may have recorded its intent
    // while this replay waited for the locks.
    let terminal = {
        let _gate = super::vcs_write::acquire_publish_gate().await;
        let _publishers = super::vcs_write::acquire_repository_write_lock()?;
        let fresh = storage::get_operation_result(&ctx.conn, operation_id)
            .await
            .map_err(|error| vcs::internal_hiding_host_paths("read bridge operation", error))?;
        if let Some(result) = fresh.as_ref()
            && result.terminal_outcome
            && let Some(digest) = result.result_digest.as_ref()
        {
            let terminal = serde_json::from_str::<Value>(digest)
                .map_err(|error| vcs::internal_hiding_host_paths("read terminal outcome", error))?;
            return Ok(CommitReconciliation::Terminal(terminal));
        }
        if let Some(result) = fresh.as_ref()
            && result.status == "applied"
            && let Some(digest) = result.result_digest.as_ref()
        {
            return Ok(CommitReconciliation::AlreadyApplied(digest.clone()));
        }
        if fresh
            .as_ref()
            .and_then(|result| result.result_digest.as_deref())
            .and_then(commit_intent)
            .is_some()
        {
            None
        } else {
            Some(vcs_write_terminal_outcome(
                operation_id,
                "indeterminate",
                "this operation stopped before it recorded what it would commit; whether it \
                 published a commit cannot be established, so re-read the repository history and \
                 issue a new operation",
            ))
        }
    };
    let Some(terminal) = terminal else {
        // The executor recorded its intent while the decision was being made:
        // reconcile it against its publication record instead of terminalizing
        // an operation that may have published a commit.
        return reconcile_commit_under_claim(ctx, operation_id, method).await;
    };
    let durable = record_terminal_outcome_or_durable(ctx, operation_id, &terminal).await?;
    Ok(CommitReconciliation::Terminal(durable))
}

/// Reconcile an operation that started but never recorded a result.
///
/// The returned payload is what the replay reports: the recorded result when
/// the operation completed after all, or a durable terminal answer when its
/// outcome cannot be established.
async fn reconcile_started_operation(
    ctx: &BridgeContext,
    operation_id: &str,
    method: &str,
) -> Result<ReconciledOperation, BridgeError> {
    // The pause seam runs before the claim, so a test can complete the
    // operation while this reconciliation waits for it.
    pause_before_reconciliation().await;
    let _claim = try_acquire_bridge_write_claim()?;
    reconcile_started_under_claim(ctx, operation_id, method).await
}

/// The durable answer of a reconciled operation.
///
/// A terminal outcome carries its own answer; an applied row carries the
/// result identity the operation produced (a commit oid, a run id). The two
/// are projected differently, so the reconciliation names which one it found
/// instead of leaving the caller to guess.
enum ReconciledOperation {
    /// The operation recorded an immutable terminal outcome.
    Terminal(String),
    /// The operation completed and recorded its result identity.
    Applied(String),
}

impl ReconciledOperation {
    /// The payload to replay, and whether it is a terminal outcome.
    fn payload(&self) -> (&str, bool) {
        match self {
            ReconciledOperation::Terminal(payload) => (payload, true),
            ReconciledOperation::Applied(payload) => (payload, false),
        }
    }
}

/// Reconcile a started operation while the caller already holds the claim.
async fn reconcile_started_under_claim(
    ctx: &BridgeContext,
    operation_id: &str,
    method: &str,
) -> Result<ReconciledOperation, BridgeError> {
    let fresh = storage::get_operation_result(&ctx.conn, operation_id)
        .await
        .map_err(|error| vcs::internal_hiding_host_paths("read bridge operation", error))?;
    if let Some(result) = fresh.as_ref()
        && result.terminal_outcome
        && let Some(digest) = result.result_digest.as_ref()
    {
        return Ok(ReconciledOperation::Terminal(digest.clone()));
    }
    if let Some(result) = fresh.as_ref()
        && result.status == "applied"
        && let Some(digest) = result.result_digest.as_ref()
    {
        return Ok(ReconciledOperation::Applied(digest.clone()));
    }
    // A started review holds the run id it allocated. The launch itself may
    // never have happened, so the recovery keeps that id in a durable
    // terminal outcome — its completion was never recorded here — and never
    // launches the run again.
    if method == "review.run"
        && let Some(result) = fresh.as_ref()
        && let Some(digest) = result.result_digest.as_ref()
        && let Some(run_id) = started_run_id(digest)
    {
        let terminal = vcs_write_terminal_outcome_with(
            operation_id,
            "indeterminate",
            "this review's launch is indeterminate: its run id was recorded before the launch, so \
             the run may never have started and it is never launched again; poll the run id for \
             whatever outcome it reached",
            json!({"runId": run_id}),
        );
        return Ok(ReconciledOperation::Terminal(terminal_json_of(
            &record_terminal_outcome_or_durable(ctx, operation_id, &terminal).await?,
        )?));
    }
    let terminal = vcs_write_terminal_outcome(
        operation_id,
        "indeterminate",
        "this operation started but never recorded a verifiable result; whether it completed \
         cannot be established, so re-read the current state and issue a new operation",
    );
    Ok(ReconciledOperation::Terminal(terminal_json_of(
        &record_terminal_outcome_or_durable(ctx, operation_id, &terminal).await?,
    )?))
}

/// Record that an operation started, before it changes anything.
///
/// The marker makes a concurrent refusal unable to claim that no executor ran:
/// the operation row is no longer empty, so a refusal cannot be recorded, and a
/// later failure keeps the marker, so a replay reports an indeterminate outcome
/// instead of repeating the side effect.
async fn record_operation_started(
    ctx: &BridgeContext,
    operation_id: &str,
    method: &str,
    detail: Value,
) -> Result<(), BridgeError> {
    let mut marker = json!({
        "vcsWriteStarted": true,
        "method": method,
    });
    if let (Some(object), Some(details)) = (marker.as_object_mut(), detail.as_object()) {
        for (key, value) in details {
            object.insert(key.clone(), value.clone());
        }
    }
    let marker = marker.to_string();
    #[cfg(all(feature = "test-bridge-hooks", debug_assertions))]
    if FAIL_NEXT_STARTED_RECORD.swap(false, std::sync::atomic::Ordering::SeqCst) {
        return Err(BridgeError::internal(
            "injected failure while recording the operation start",
        ));
    }
    // The write is conditional: a terminal outcome recorded meanwhile is the
    // operation's final answer and is never overwritten.
    let applied = storage::finish_operation_unless_terminal(
        &ctx.conn,
        operation_id,
        "pending",
        Some(&marker),
        now_ms(),
    )
    .await
    .map_err(|error| vcs::internal_hiding_host_paths("record operation start", error))?;
    if !applied {
        // The operation already has its final answer, so no side effect may
        // follow this marker.
        return Err(BridgeError::stale_fence(format!(
            "stale_state: operation '{operation_id}' already has a final result; issue a new operation"
        )));
    }
    Ok(())
}

/// Test seam: the next operation-started record fails once.
#[cfg(all(feature = "test-bridge-hooks", debug_assertions))]
static FAIL_NEXT_STARTED_RECORD: std::sync::atomic::AtomicBool =
    std::sync::atomic::AtomicBool::new(false);

/// Test seam: the next operation-started record fails once.
#[cfg(all(feature = "test-bridge-hooks", debug_assertions))]
pub fn fail_next_started_record_for_test() {
    FAIL_NEXT_STARTED_RECORD.store(true, std::sync::atomic::Ordering::SeqCst);
}

/// The marker of a refusal that happened before any executor started.
///
/// The operation row failed, but nothing ran: the repository claim was busy or
/// the review budget was full. The same operation id may therefore run again,
/// which the fixed status vocabulary cannot express on its own.
fn refusal_marker(reason: &str) -> String {
    json!({"bridgeRefusal": reason}).to_string()
}

/// The run id a started review recorded before it launched the run.
fn started_run_id(digest: &str) -> Option<String> {
    serde_json::from_str::<Value>(digest)
        .ok()
        .and_then(|value| {
            value
                .get("runId")
                .and_then(Value::as_str)
                .map(str::to_string)
        })
}

/// Whether a recorded result marks an operation that already started.
fn is_started_marker(digest: &str) -> bool {
    serde_json::from_str::<Value>(digest)
        .ok()
        .and_then(|value| {
            value
                .get("vcsWriteStarted")
                .and_then(Value::as_bool)
                .map(|started| started.to_string())
        })
        .is_some()
}

/// Name the durable outcome an error would hide.
///
/// A failure or denial that cannot be recorded means the operation already
/// reached its final answer: the error keeps its own code and context — a
/// denial stays fail-closed — but the caller is told that a durable outcome
/// exists and is recovered by replaying the operation id.
fn with_durable_state_hint(error: BridgeError, operation_id: &str, retryable: bool) -> BridgeError {
    let mut error = BridgeError {
        message: format!(
            "{}; the operation '{operation_id}' already has a durable outcome, so replaying the \
             operation id recovers it",
            error.message
        ),
        ..error
    };
    if retryable {
        error = error.retryable();
    }
    error
}

/// Whether a recorded result is a pre-execution refusal marker.
fn is_refusal_marker(digest: &str) -> bool {
    serde_json::from_str::<Value>(digest)
        .ok()
        .and_then(|value| {
            value
                .get("bridgeRefusal")
                .and_then(Value::as_str)
                .map(str::to_string)
        })
        .is_some()
}

/// Whether an operation row records a pre-execution refusal.
///
/// A refusal is a FAILED row that never started. `checkpoint.create` stores the
/// caller-provided checkpoint ID in the same column, so a marker-shaped
/// checkpoint ID must never be read as a refusal: that would claim an operation
/// never ran when it did.
fn is_refusal_row(method: &str, status: &str, result_digest: Option<&str>) -> bool {
    status == "failed"
        && method != "checkpoint.create"
        && result_digest.is_some_and(is_refusal_marker)
}

/// Serialize a terminal outcome for the replay projection.
fn terminal_json_of(terminal: &Value) -> Result<String, BridgeError> {
    serde_json::to_string(terminal)
        .map_err(|error| vcs::internal_hiding_host_paths("record terminal outcome", error))
}

/// Persist a terminal outcome so later replays report it instead of running
/// the operation again.
async fn record_terminal_outcome(
    ctx: &BridgeContext,
    operation_id: &str,
    terminal: &Value,
) -> Result<bool, BridgeError> {
    let terminal_json = serde_json::to_string(terminal)
        .map_err(|error| vcs::internal_hiding_host_paths("record terminal outcome", error))?;
    // The write is conditional: an outcome that is already terminal stays the
    // operation's answer, and the caller re-reads the durable outcome when the
    // update does not apply.
    storage::finish_operation_terminal(&ctx.conn, operation_id, &terminal_json, now_ms())
        .await
        .map_err(|error| vcs::internal_hiding_host_paths("record terminal outcome", error))
}

/// Record a terminal outcome and return what the operation durably holds.
///
/// A rejected write means the operation already has its final answer, so the
/// DURABLE outcome is returned instead of the one this caller proposed.
async fn record_terminal_outcome_or_durable(
    ctx: &BridgeContext,
    operation_id: &str,
    terminal: &Value,
) -> Result<Value, BridgeError> {
    if record_terminal_outcome(ctx, operation_id, terminal).await? {
        return Ok(terminal.clone());
    }
    durable_outcome(ctx, operation_id).await
}

/// The outcome an operation durably holds right now.
async fn durable_outcome(ctx: &BridgeContext, operation_id: &str) -> Result<Value, BridgeError> {
    let recorded = storage::get_operation_result(&ctx.conn, operation_id)
        .await
        .map_err(|error| vcs::internal_hiding_host_paths("read bridge operation", error))?;
    match recorded {
        Some(result) if result.terminal_outcome => {
            let digest = result.result_digest.ok_or_else(|| {
                BridgeError::internal("terminal operation has no recorded outcome")
            })?;
            serde_json::from_str::<Value>(&digest)
                .map_err(|error| vcs::internal_hiding_host_paths("read recorded outcome", error))
        }
        Some(_) => Err(BridgeError::internal(
            "conditional terminal write was rejected but the row has no terminal outcome",
        )),
        None => Err(BridgeError::internal(
            "conditional terminal write was rejected but the operation row is missing",
        )),
    }
}

/// Replay a durable terminal or applied result from its authoritative row.
async fn replay_durable_operation(
    ctx: &BridgeContext,
    operation_id: &str,
    method: &str,
    session_id: &str,
) -> Result<Option<Value>, BridgeError> {
    let recorded = storage::get_operation_result(&ctx.conn, operation_id)
        .await
        .map_err(|error| vcs::internal_hiding_host_paths("read bridge operation", error))?;
    let Some(recorded) = recorded else {
        return Ok(None);
    };
    let Some(digest) = recorded.result_digest.as_deref() else {
        return Ok(None);
    };
    if recorded.terminal_outcome {
        return replayed_result(method, digest, session_id, true).map(Some);
    }
    if recorded.status == "applied" && replays_recorded_result(method) {
        return replayed_result(method, digest, session_id, false).map(Some);
    }
    Ok(None)
}

fn replayed_result(
    method: &str,
    result_digest: &str,
    session_id: &str,
    terminal_outcome: bool,
) -> Result<Value, BridgeError> {
    match method {
        "commit.create" => {
            // A terminal outcome carries its own answer; a recorded commit
            // keeps the original identity.
            if terminal_outcome {
                let mut value: Value = serde_json::from_str(result_digest).map_err(|error| {
                    vcs::internal_hiding_host_paths("commit replay payload is not JSON", error)
                })?;
                if let Some(object) = value.as_object_mut() {
                    object.insert("replayed".to_string(), json!(true));
                    object.insert("session_id".to_string(), json!(session_id));
                }
                return Ok(value);
            }
            Ok(json!({
                "commit": result_digest,
                "session_id": session_id,
                "replayed": true,
            }))
        }
        "checkpoint.restore" => {
            // A terminal outcome carries its own answer; a recorded restore
            // keeps the commit it restored.
            if terminal_outcome {
                let mut value: Value = serde_json::from_str(result_digest).map_err(|error| {
                    vcs::internal_hiding_host_paths("restore replay payload is not JSON", error)
                })?;
                if let Some(object) = value.as_object_mut() {
                    object.insert("replayed".to_string(), json!(true));
                    object.insert("session_id".to_string(), json!(session_id));
                }
                return Ok(value);
            }
            Ok(json!({
                "target_commit": result_digest,
                "session_id": session_id,
                "head_moved": false,
                "replayed": true,
            }))
        }
        "vcs.stage" | "vcs.unstage" | "vcs.commit" => {
            let mut value: Value = serde_json::from_str(result_digest).map_err(|error| {
                vcs::internal_hiding_host_paths("vcs replay payload is not JSON", error)
            })?;
            if let Some(object) = value.as_object_mut() {
                // A terminal outcome keeps its own outcome; only a recorded
                // success becomes a "replayed" success.
                let terminal = terminal_outcome;
                object.insert("replayed".to_string(), json!(true));
                if !terminal {
                    object.insert("outcome".to_string(), json!("replayed"));
                }
            }
            Ok(value)
        }
        "review.run" => {
            // A terminal outcome carries its own answer. A recovered review
            // keeps its run id, and the replay projects that run's CURRENT
            // state next to the immutable outcome, so the caller can discover
            // what the run reached without launching a second one.
            if terminal_outcome {
                let mut value: Value = serde_json::from_str(result_digest).map_err(|error| {
                    vcs::internal_hiding_host_paths("review replay payload is not JSON", error)
                })?;
                let run_id = value
                    .get("runId")
                    .and_then(Value::as_str)
                    .map(str::to_string);
                if let Some(run_id) = run_id {
                    let state = vcs::review_state(&run_id)?;
                    if let Some(object) = value.as_object_mut() {
                        object.insert("run".to_string(), state);
                    }
                }
                if let Some(object) = value.as_object_mut() {
                    object.insert("replayed".to_string(), json!(true));
                    object.insert("session_id".to_string(), json!(session_id));
                }
                return Ok(value);
            }
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

/// A commit this operation published, with the replay identity it is
/// recorded under.
struct PublishedCommit<'a> {
    operation_id: &'a str,
    session_id: &'a str,
    method: &'a str,
    oid: &'a str,
    paths: &'a Value,
}

/// Record and return the replay of a commit this operation published.
///
/// The repository claim is held from the durable-row reread through the
/// conditional result write and response projection. This prevents a competing
/// recovery from terminalizing the operation between publication validation
/// and the success response.
async fn record_published_commit_result(
    ctx: &BridgeContext,
    session: &storage::BridgeSessionRow,
    request: &BridgeRequest,
    published: &PublishedCommit<'_>,
) -> Result<Value, BridgeError> {
    let PublishedCommit {
        operation_id,
        session_id,
        method,
        oid,
        paths,
    } = published;
    let _claim = try_acquire_bridge_write_claim()?;

    if let Some(result) = replay_durable_operation(ctx, operation_id, method, session_id).await? {
        return Ok(result);
    }
    let fresh = storage::get_operation_result(&ctx.conn, operation_id)
        .await
        .map_err(|error| vcs::internal_hiding_host_paths("read bridge operation", error))?;
    let has_commit_intent = fresh
        .as_ref()
        .and_then(|result| result.result_digest.as_deref())
        .and_then(commit_intent)
        .is_some();
    if !has_commit_intent {
        return Err(BridgeError::internal(
            "published commit has no matching durable intent; retry the operation replay",
        )
        .retryable());
    }
    let published_oid = publication::published_commit(operation_id)
        .await
        .map_err(|error| {
            vcs::internal_hiding_host_paths("read commit publication evidence", error)
        })?;
    if published_oid.as_deref() != Some(*oid) || !recorded_commit_exists(oid) {
        return Err(BridgeError::internal(
            "published commit evidence changed before its result was recorded; retry the operation replay",
        )
        .retryable());
    }

    let result_digest = if *method == "vcs.commit" {
        let published = json!({
            "operationId": operation_id,
            "outcome": "applied",
            "commitOid": oid,
            "paths": paths,
            "failures": [],
        });
        serde_json::to_string(&published).map_err(|error| {
            vcs::internal_hiding_host_paths("vcs.commit recovery could not be stored", error)
        })?
    } else {
        oid.to_string()
    };

    if *method == "commit.create" || param_bool(&request.params, "linkSession") {
        record_association_links(
            ctx,
            session,
            session_id,
            operation_id,
            "commit",
            oid,
            &evidence_ids(request),
        )
        .await?;
    }

    let recorded = storage::finish_operation_unless_terminal(
        &ctx.conn,
        operation_id,
        "applied",
        Some(&result_digest),
        now_ms(),
    )
    .await
    .map_err(|error| vcs::internal_hiding_host_paths("record recovered commit", error))?;
    if recorded {
        return replayed_result(method, &result_digest, session_id, false);
    }
    replay_durable_operation(ctx, operation_id, method, session_id)
        .await?
        .ok_or_else(|| {
            BridgeError::internal(
                "commit result write was rejected but no durable replay result exists",
            )
            .retryable()
        })
}

/// Replay an already-applied commit row that a concurrent request observed
/// only after waiting for the repository claim.
async fn replay_applied_commit_result(
    ctx: &BridgeContext,
    session: &storage::BridgeSessionRow,
    session_id: &str,
    operation_id: &str,
    request: &BridgeRequest,
    method: &str,
    result_digest: &str,
) -> Result<Value, BridgeError> {
    let oid = if method == "commit.create" {
        result_digest.to_string()
    } else {
        vcs_commit_oid(result_digest).ok_or_else(|| {
            BridgeError::internal("recorded commit is missing or invalid; refusing replay")
        })?
    };
    if !recorded_commit_exists(&oid) {
        return Err(BridgeError::internal(
            "recorded commit is missing or invalid; refusing replay",
        ));
    }
    if method == "commit.create" || param_bool(&request.params, "linkSession") {
        record_association_links(
            ctx,
            session,
            session_id,
            operation_id,
            "commit",
            &oid,
            &evidence_ids(request),
        )
        .await?;
    }
    replayed_result(method, result_digest, session_id, false)
}

/// `commit.create` — commit the current index and record the association graph.
//////
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
    // The claim is held for the whole execution, so a replay in another
    // process cannot answer for a live operation. The pause seam runs before
    // the claim, so a test can finalize an operation whose executor has not
    // started yet.
    pause_before_claim().await;
    let _claim = try_acquire_bridge_write_claim()?;
    ensure_operation_can_start(ctx, operation_id, "commit.create").await?;
    let _gate = super::vcs_write::acquire_publish_gate().await;
    // Capture a request fence before hooks run. Commit code reacquires the
    // exclusive repository lock after hooks and verifies this exact snapshot
    // before creating trees or moving refs.
    let (expected_head, expected_index) = super::vcs_write::current_commit_fence().await?;
    if let Some(expected) = param_str(&request.params, "expected_head")
        && expected != expected_head
    {
        return Err(BridgeError::stale_fence(
            "stale_state: HEAD changed before commit admission; refresh and retry",
        ));
    }
    // The intent records the HEAD this attempt observed. It does not write a
    // tree: recovery uses the publication row, and a tree written here would
    // change `commit.create` before the commit itself runs.
    let intent = json!({
        "commitIntent": true,
        "expectedHead": expected_head,
        "expectedIndex": expected_index.clone(),
        "message": message,
    })
    .to_string();
    let recorded = storage::finish_operation_unless_terminal(
        &ctx.conn,
        operation_id,
        "pending",
        Some(&intent),
        now_ms(),
    )
    .await
    .map_err(|error| vcs::internal_hiding_host_paths("record commit intent", error))?;
    if !recorded {
        // The operation already has its final answer, so no commit may follow.
        return Err(BridgeError::stale_fence(format!(
            "stale_state: operation '{operation_id}' already has a final result; issue a new operation"
        )));
    }
    if !publication::operation_id_is_bindable(operation_id) {
        return Err(BridgeError::invalid_params(
            "operation_id must be 1..=128 bytes of ASCII letters, digits, '_', '-', '.' or ':'",
        ));
    }
    publication::ensure_table()
        .await
        .map_err(|error| vcs::internal_hiding_host_paths("commit.create", error))?;

    let mut result = publication::with_operation_fence(
        operation_id,
        &expected_head,
        Some(&expected_index),
        None,
        async { vcs::commit_create_unguarded(&message, signoff, allow_empty).await },
    )
    .await?;
    let commit = result
        .get("commit")
        .and_then(Value::as_str)
        .ok_or_else(|| BridgeError::internal("commit.create produced no commit id".to_string()))?
        .to_string();

    let evidence_ids = evidence_ids(request);
    let recorded = storage::finish_operation_unless_terminal(
        &ctx.conn,
        operation_id,
        "applied",
        Some(&commit),
        now_ms(),
    )
    .await
    .map_err(|error| {
        vcs::internal_hiding_host_paths(
            &format!("commit.create published {commit} but could not record it"),
            error,
        )
    })?;
    if !recorded {
        return replay_durable_operation(ctx, operation_id, "commit.create", session_id)
            .await?
            .ok_or_else(|| {
                BridgeError::internal(
                    "commit.create result write was rejected without a durable outcome",
                )
                .retryable()
            });
    }
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
/// uncommitted work is destroyed (LB-05 AC5).
///
/// The restore itself rewrites paths one by one, so a failure part way
/// through can leave the worktree partly restored. The restored commit is
/// recorded under the repository claim before the optional association
/// bookkeeping, so the operation's identity survives such a failure, and a
/// recovery that cannot establish the outcome reports it as indeterminate
/// rather than restoring a second time.
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
    // The pause seam runs before the claim, so a test can change the durable
    // state while the executor is between the two.
    pause_before_claim().await;
    // The checkpoint is resolved before the claim: an unknown or
    // non-materializable checkpoint is a scope error, not a busy repository.
    let target = resolve_checkpoint_commit(ctx, &checkpoint_id).await?;
    let _claim = try_acquire_bridge_write_claim()?;
    ensure_operation_can_start(ctx, operation_id, "checkpoint.restore").await?;
    let _gate = super::vcs_write::acquire_publish_gate().await;
    let _publishers = super::vcs_write::acquire_repository_write_lock()?;

    // Fence, in fail-closed order: HEAD first (cheap, exact), then the
    // index/worktree cleanliness the restore would overwrite. The bridge gate
    // and exclusive repository lock cover those reads through the restore.
    let fence = vcs::read_fence().await?;
    vcs::check_head_fence(&fence, &expected_head)?;
    vcs::check_clean_worktree_fence().await?;
    let target_oid_for_intent = target.to_string();

    // The operation is marked as started before the worktree is touched, so a
    // failure part way through can never be replayed as a fresh restore.
    record_operation_started(
        ctx,
        operation_id,
        "checkpoint.restore",
        json!({"checkpointId": checkpoint_id, "target": target_oid_for_intent}),
    )
    .await?;
    let mut result = vcs::checkpoint_restore(&target).await?;
    // The restored commit is the operation's durable identity: it is recorded
    // under the claim before the optional association bookkeeping, so a link
    // failure cannot make a replay restore the worktree a second time.
    let target_oid = target.to_string();
    let recorded = storage::finish_operation_unless_terminal(
        &ctx.conn,
        operation_id,
        "applied",
        Some(&target_oid),
        now_ms(),
    )
    .await
    .map_err(|error| vcs::internal_hiding_host_paths("record restored commit", error))?;
    if !recorded {
        return replay_durable_operation(ctx, operation_id, "checkpoint.restore", session_id)
            .await?
            .ok_or_else(|| {
                BridgeError::internal("restore result write was rejected without a durable outcome")
                    .retryable()
            });
    }
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
    // The run is started under the repository claim, and its identity is
    // recorded BEFORE the association bookkeeping: a failure in the optional
    // links must not make a replay start a second review run.
    let _claim = try_acquire_bridge_write_claim()?;
    ensure_operation_can_start(ctx, operation_id, "review.run").await?;
    // The run is validated and admitted BEFORE the operation is marked as
    // started: an admission refusal proves that no review began, so the
    // operation stays retryable under the same operation id.
    let prepared = vcs::review_prepare(&review).await?;
    // The operation is marked as started, with the allocated run id, BEFORE
    // the run is launched: a failure in that interval still knows which run
    // id was reserved, and a replay can never start a second one. The launch
    // itself remains unproven until the run records its own state.
    record_operation_started(
        ctx,
        operation_id,
        "review.run",
        json!({"runId": prepared.run_id()}),
    )
    .await?;
    let (run_id, mut state) = vcs::review_launch(prepared).await?;
    let recorded = storage::finish_operation_unless_terminal(
        &ctx.conn,
        operation_id,
        "applied",
        Some(&run_id),
        now_ms(),
    )
    .await
    .map_err(|error| vcs::internal_hiding_host_paths("record review run id", error))?;
    if !recorded {
        return replay_durable_operation(ctx, operation_id, "review.run", session_id)
            .await?
            .ok_or_else(|| {
                BridgeError::internal("review run id write was rejected without a durable outcome")
                    .retryable()
            });
    }
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

async fn vcs_write(
    ctx: &BridgeContext,
    session: &storage::BridgeSessionRow,
    session_id: &str,
    operation_id: &str,
    method: &str,
    request: &BridgeRequest,
) -> Result<Value, BridgeError> {
    let path_values = request
        .params
        .as_ref()
        .and_then(|value| value.get("paths"))
        .cloned()
        .unwrap_or(json!([]));
    let paths = vcs::parse_diff_params(&Some(json!({
        "mode": "worktree",
        "paths": path_values,
    })))?
    .1;
    if paths.is_empty() {
        return Err(BridgeError::invalid_params(
            "vcs.stage, vcs.unstage, and vcs.commit require at least one repository-relative path",
        ));
    }
    let expected_head = param_str(&request.params, "expectedHead")
        .ok_or_else(|| BridgeError::invalid_params("vcs write methods require expectedHead"))?;
    let expected_index = param_str(&request.params, "expectedIndex")
        .ok_or_else(|| BridgeError::invalid_params("vcs write methods require expectedIndex"))?;
    let expected_worktree = param_str(&request.params, "expectedWorktree")
        .ok_or_else(|| BridgeError::invalid_params("vcs write methods require expectedWorktree"))?;
    let expected = super::vcs_write::WriteFence {
        head: expected_head,
        index_generation: expected_index,
        worktree_generation: expected_worktree,
    };
    let intent_head = expected.head.clone();
    let intent_index = expected.index_generation.clone();
    let intent_worktree = expected.worktree_generation.clone();
    // The claim is held for the whole execution, hooks included, so a replay
    // in another process cannot conclude anything while this operation is
    // still running. It is a separate lock from the maintenance lock, so a
    // hook may still start another Libra command.
    // The claim is held for the whole execution, hooks included, so a replay
    // in another process cannot conclude anything while this operation is
    // still running. It is a separate lock from the maintenance lock, so a
    // hook may still start another Libra command.
    pause_before_claim().await;
    let _claim = try_acquire_bridge_write_claim()?;
    return super::vcs_write::run_fenced(&expected, method != "vcs.commit", async move {
        ensure_operation_can_start(ctx, operation_id, method).await?;
        if method == "vcs.commit" {
            let message = vcs::parse_commit_message(&request.params)?;
            let intent = json!({
                "commitIntent": true,
                "expectedHead": intent_head,
                "expectedIndex": intent_index.clone(),
                "expectedWorktree": intent_worktree.clone(),
                "message": message,
                "paths": paths,
            })
            .to_string();
            let recorded = storage::finish_operation_unless_terminal(
                &ctx.conn,
                operation_id,
                "pending",
                Some(&intent),
                now_ms(),
            )
            .await
            .map_err(|error| {
                vcs::internal_hiding_host_paths("record vcs commit intent", error)
            })?;
            if !recorded {
                // The operation already has its final answer, so no commit may
                // follow.
                return Err(BridgeError::stale_fence(format!(
                    "stale_state: operation '{operation_id}' already has a final result; issue a new operation"
                )));
            }
            if !publication::operation_id_is_bindable(operation_id) {
                return Err(BridgeError::invalid_params(
                    "operation_id must be 1..=128 bytes of ASCII letters, digits, '_', '-', '.' or ':'",
                ));
            }
            publication::ensure_table()
                .await
                .map_err(|error| vcs::internal_hiding_host_paths("vcs.commit", error))?;
            let created = publication::with_operation_fence(
                operation_id,
                &intent_head,
                Some(&intent_index),
                Some(&intent_worktree),
                async { super::vcs_write::commit_index(&message, &paths).await },
            )
            .await?;
            let commit_oid = created["commitOid"]
                .as_str()
                .ok_or_else(|| BridgeError::internal("vcs.commit produced no commit id"))?
                .to_string();
            let published = json!({
                "operationId": operation_id,
                "outcome": "applied",
                "commitOid": commit_oid,
                "paths": created["paths"],
                "failures": [],
            });
            let published_json = serde_json::to_string(&published).map_err(|error| {
                vcs::internal_hiding_host_paths("vcs.commit could not record its result", error)
            })?;
            let recorded = storage::finish_operation_unless_terminal(
                &ctx.conn,
                operation_id,
                "applied",
                Some(&published_json),
                now_ms(),
            )
            .await
            .map_err(|error| {
                vcs::internal_hiding_host_paths(
                    &format!("vcs.commit published {commit_oid} but could not record it"),
                    error,
                )
            })?;
            if !recorded {
                return replay_durable_operation(ctx, operation_id, "vcs.commit", session_id)
                    .await?
                    .ok_or_else(|| {
                        BridgeError::internal(
                            "vcs.commit result write was rejected without a durable outcome",
                        )
                        .retryable()
                    });
            }
            if param_bool(&request.params, "linkSession") {
                record_association_links(
                    ctx,
                    session,
                    session_id,
                    operation_id,
                    "commit",
                    &commit_oid,
                    &evidence_ids(request),
                )
                .await?;
            }
            return Ok(published);
        }
        let prepared = if method == "vcs.stage" {
            super::vcs_write::stage_paths(&paths).await?
        } else {
            super::vcs_write::unstage_paths(&paths).await?
        };
        // Record the exact index effect BEFORE the live index is replaced, and
        // record that the replacement completed AFTER it. Only the second row
        // proves that this operation published the index, so a crash between
        // them is reported as indeterminate instead of guessed.
        publication::ensure_index_table().await.map_err(|error| {
            vcs::internal_hiding_host_paths("vcs write: publication table", error)
        })?;
        let intent = json!({
            "vcsWriteIntent": true,
            "method": method,
            "expectedHead": intent_head,
            "expectedIndex": intent_index,
            "expectedWorktree": intent_worktree,
            "paths": prepared.applied(),
            "indexAfter": prepared.digest(),
        })
        .to_string();
        let recorded = storage::finish_operation_unless_terminal(
            &ctx.conn,
            operation_id,
            "pending",
            Some(&intent),
            now_ms(),
        )
        .await
        .map_err(|error| {
            vcs::internal_hiding_host_paths("record vcs write intent", error)
        })?;
        if !recorded {
            // The operation already has its final answer, so no index
            // replacement may follow.
            return Err(BridgeError::stale_fence(format!(
                "stale_state: operation '{operation_id}' already has a final result; issue a new operation"
            )));
        }
        publication::prepare_index_publication(operation_id, prepared.digest())
            .await
            .map_err(|error| {
                vcs::internal_hiding_host_paths("record prepared index publication", error)
            })?;
        prepared.publish()?;
        publication::publish_index_publication(operation_id)
            .await
            .map_err(|error| {
                vcs::internal_hiding_host_paths("record published index publication", error)
            })?;
        Ok(json!({
            "operationId": operation_id,
            "outcome": "applied",
            "commitOid": Value::Null,
            "paths": prepared.applied(),
            "failures": [],
        }))
    })
    .await;
}

/// Record the durable association graph for one mutation result.
///
/// `source_id` is the result's stable identity (commit oid, review run id, …).
/// Each association is an `agent_bridge_link` row, so replay is idempotent and
/// a relink to a different target fails closed.
#[cfg(all(feature = "test-bridge-hooks", debug_assertions))]
static FAIL_NEXT_ASSOCIATION: std::sync::atomic::AtomicBool =
    std::sync::atomic::AtomicBool::new(false);

/// Test seam: the next association write fails once. Not used by the protocol.
#[cfg(all(feature = "test-bridge-hooks", debug_assertions))]
pub fn fail_next_association_for_test() {
    FAIL_NEXT_ASSOCIATION.store(true, std::sync::atomic::Ordering::SeqCst);
}

/// A pause between an operation row and the next protected step.
///
/// The paused task signals that it reached the pause and then waits for the
/// resume, so a test can change the durable state while the task is between
/// the two. Only the debug+feature build has this seam.
#[cfg(all(feature = "test-bridge-hooks", debug_assertions))]
pub struct PauseGate {
    reached: Option<tokio::sync::oneshot::Sender<()>>,
    resume: tokio::sync::oneshot::Receiver<()>,
}

#[cfg(all(feature = "test-bridge-hooks", debug_assertions))]
impl PauseGate {
    /// Announce that the executor reached the pause.
    fn reached(&mut self) {
        if let Some(reached) = self.reached.take() {
            let _ = reached.send(());
        }
    }

    /// Wait for the test to release the executor.
    async fn wait(&mut self) {
        let _ = (&mut self.resume).await;
    }
}

/// Build a pause, plus the test's "reached" and "resume" endpoints.
#[cfg(all(feature = "test-bridge-hooks", debug_assertions))]
pub fn pause_gate_channel() -> (
    PauseGate,
    tokio::sync::oneshot::Receiver<()>,
    tokio::sync::oneshot::Sender<()>,
) {
    let (reached_tx, reached_rx) = tokio::sync::oneshot::channel();
    let (resume_tx, resume_rx) = tokio::sync::oneshot::channel();
    (
        PauseGate {
            reached: Some(reached_tx),
            resume: resume_rx,
        },
        reached_rx,
        resume_tx,
    )
}

#[cfg(all(feature = "test-bridge-hooks", debug_assertions))]
static PAUSE_BEFORE_CLAIM: std::sync::Mutex<Option<PauseGate>> = std::sync::Mutex::new(None);

/// Test seam: pause the next VCS write before it claims its operation.
#[cfg(all(feature = "test-bridge-hooks", debug_assertions))]
pub fn set_pause_before_claim_for_test(pause: Option<PauseGate>) {
    *PAUSE_BEFORE_CLAIM
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner()) = pause;
}

#[cfg(all(feature = "test-bridge-hooks", debug_assertions))]
static PAUSE_BEFORE_RECONCILIATION: std::sync::Mutex<Option<PauseGate>> =
    std::sync::Mutex::new(None);

/// Test seam: pause the next replay before it reconciles its operation.
#[cfg(all(feature = "test-bridge-hooks", debug_assertions))]
pub fn set_pause_before_reconciliation_for_test(pause: Option<PauseGate>) {
    *PAUSE_BEFORE_RECONCILIATION
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner()) = pause;
}

#[cfg(all(feature = "test-bridge-hooks", debug_assertions))]
async fn pause_before_claim() {
    let mut pause = PAUSE_BEFORE_CLAIM
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .take();
    if let Some(pause) = pause.as_mut() {
        pause.reached();
        pause.wait().await;
    }
}

#[cfg(not(all(feature = "test-bridge-hooks", debug_assertions)))]
async fn pause_before_claim() {}

#[cfg(all(feature = "test-bridge-hooks", debug_assertions))]
async fn pause_before_reconciliation() {
    let mut pause = PAUSE_BEFORE_RECONCILIATION
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .take();
    if let Some(pause) = pause.as_mut() {
        pause.reached();
        pause.wait().await;
    }
}

#[cfg(not(all(feature = "test-bridge-hooks", debug_assertions)))]
async fn pause_before_reconciliation() {}

async fn record_association_links(
    ctx: &BridgeContext,
    session: &storage::BridgeSessionRow,
    session_id: &str,
    operation_id: &str,
    source_type: &str,
    source_id: &str,
    evidence_ids: &[String],
) -> Result<(), BridgeError> {
    #[cfg(all(feature = "test-bridge-hooks", debug_assertions))]
    if FAIL_NEXT_ASSOCIATION.swap(false, std::sync::atomic::Ordering::SeqCst) {
        return Err(BridgeError::internal(
            "injected association failure before links are written",
        ));
    }
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

    // The checkpoint and its association rows are repository records, so they
    // are written under the same bridge write claim every other mutation uses.
    // A busy claim refuses the request with a retryable error instead of
    // writing beside a claimed executor. The claim waits briefly here: the
    // write is idempotent and quick, so two callers in one process must not
    // refuse each other.
    let _claim = super::vcs_write::BridgeWriteClaim::acquire_within(
        std::time::Duration::from_secs(5),
    )
    .await?
    .ok_or_else(|| {
        BridgeError::denied(
            "another bridge write or recovery is active in this repository; retry after it finishes",
        )
        .retryable()
    })?;
    let created = storage::insert_checkpoint(
        &ctx.conn,
        &checkpoint_id,
        session_id,
        agent_checkpoint_id,
        target_oid,
        now_ms(),
    )
    .await
    .map_err(|error| vcs::internal_hiding_host_paths("record bridge checkpoint", error))?;

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
