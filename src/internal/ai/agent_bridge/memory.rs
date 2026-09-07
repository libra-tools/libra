//! Audited Memory recall adapter for the DeepSeek Harness bridge.

use serde::Deserialize;
use serde_json::{Value, json};

use super::{
    ingress::{BridgeContext, require_session_in_repo},
    protocol::{BridgeError, BridgeRequest, BridgeResponse, JSONRPC_VERSION, code},
};
use crate::internal::ai::memory::{
    ActorKind, ActorRefV1, AuditedMemoryDeliveryErrorKind, AuditedMemoryDeliveryV1,
    AuthenticatedMemoryContext, DshEpisodeInput, DshEpisodeRecordErrorKind,
};

const MEMORY_RECALL_SCHEMA_VERSION: u32 = 1;
const MAX_RAW_QUERY_BYTES: usize = 16 * 1024;

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct MemoryRecallParams {
    session_id: String,
    query_text: String,
    turn: Option<u64>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct MemoryEpisodeRecordParams {
    session_id: String,
    turn: u64,
    goal: String,
    response_text: String,
}

/// Dispatch `memory.recall`. Other methods are left for the surrounding
/// bridge dispatcher.
pub async fn dispatch(
    ctx: &BridgeContext,
    request: &BridgeRequest,
) -> Result<Option<BridgeResponse>, BridgeError> {
    match request.method.as_str() {
        "memory.recall" => Ok(Some(recall(ctx, request).await?)),
        "memory.episode.record" => Ok(Some(record_episode(ctx, request).await?)),
        _ => Ok(None),
    }
}

async fn recall(
    ctx: &BridgeContext,
    request: &BridgeRequest,
) -> Result<BridgeResponse, BridgeError> {
    let params: MemoryRecallParams = serde_json::from_value(
        request
            .params
            .clone()
            .ok_or_else(|| BridgeError::invalid_params("memory.recall requires params"))?,
    )
    .map_err(|_| {
        BridgeError::invalid_params(
            "memory.recall requires string session_id and query_text, with optional positive turn",
        )
    })?;
    if params.session_id.trim().is_empty() {
        return Err(BridgeError::invalid_params(
            "memory.recall session_id must not be empty",
        ));
    }
    if params.query_text.len() > MAX_RAW_QUERY_BYTES {
        return Err(BridgeError::invalid_params(format!(
            "memory.recall query_text exceeds the {MAX_RAW_QUERY_BYTES}-byte wire limit"
        )));
    }

    require_session_in_repo(ctx, &params.session_id).await?;
    ctx.require_active_session(&params.session_id).await?;
    if let Some(turn) = params.turn {
        if turn == 0 {
            return Err(BridgeError::invalid_params("turn must be positive"));
        }
        ctx.episode_recorder()
            .map_err(record_error)?
            .begin_turn(&params.session_id, turn)
            .await
            .map_err(|error| record_error(error.kind()))?;
    }

    let context = AuthenticatedMemoryContext::new(
        &ctx.repository_id,
        ActorRefV1 {
            kind: ActorKind::Agent,
            principal_id: format!("deepseek-harness:{}", params.session_id),
        },
    )
    .map_err(|_| memory_error(AuditedMemoryDeliveryErrorKind::Policy))?;
    let delivery = ctx
        .memory_delivery()
        .map_err(memory_error)?
        .recall(&context, &params.query_text)
        .await
        .map_err(|error| memory_error(error.kind()))?;

    Ok(BridgeResponse {
        jsonrpc: JSONRPC_VERSION,
        result: Some(json!({
            "schema_version": MEMORY_RECALL_SCHEMA_VERSION,
            "data": {
                "delivery": delivery.as_ref().map(delivery_json),
            },
        })),
        error: None,
        id: request.id.clone().unwrap_or(Value::Null),
    })
}

async fn record_episode(
    ctx: &BridgeContext,
    request: &BridgeRequest,
) -> Result<BridgeResponse, BridgeError> {
    let params: MemoryEpisodeRecordParams = serde_json::from_value(
        request
            .params
            .clone()
            .ok_or_else(|| BridgeError::invalid_params("memory.episode.record requires params"))?,
    )
    .map_err(|_| {
        BridgeError::invalid_params(
            "memory.episode.record params must contain session_id, turn, goal, and response_text",
        )
    })?;
    if params.session_id.trim().is_empty()
        || params.turn == 0
        || params.goal.trim().is_empty()
        || params.response_text.trim().is_empty()
    {
        return Err(BridgeError::invalid_params(
            "memory.episode.record requires non-empty source evidence and a positive turn",
        ));
    }
    require_session_in_repo(ctx, &params.session_id).await?;
    ctx.require_active_session(&params.session_id).await?;

    let record = ctx
        .episode_recorder()
        .map_err(record_error)?
        .record(DshEpisodeInput {
            session_id: params.session_id,
            turn: params.turn,
            goal: params.goal,
            response_text: params.response_text,
        })
        .await
        .map_err(|error| record_error(error.kind()))?;

    Ok(BridgeResponse {
        jsonrpc: JSONRPC_VERSION,
        result: Some(json!({
            "schema_version": 1,
            "data": {
                "task_id": record.task_id,
                "note_id": record.note_id,
                "revision_oid": record.revision_oid,
            },
        })),
        error: None,
        id: request.id.clone().unwrap_or(Value::Null),
    })
}

fn delivery_json(delivery: &AuditedMemoryDeliveryV1) -> Value {
    json!({
        "prompt_section": delivery.prompt_section(),
        "receipt_id": delivery.receipt_id(),
        "view_hash": delivery.view_hash(),
        "bundle_hash": delivery.bundle_hash(),
        "selected_count": delivery.selected_count(),
        "token_budget": delivery.token_budget(),
    })
}

fn memory_error(kind: AuditedMemoryDeliveryErrorKind) -> BridgeError {
    let error = BridgeError::new(
        code::INTERNAL_ERROR,
        kind.stable_code(),
        "audited Memory recall failed; inspect repository Memory diagnostics before retrying",
    );
    if kind.retryable() {
        error.retryable()
    } else {
        error
    }
}

fn record_error(kind: DshEpisodeRecordErrorKind) -> BridgeError {
    match kind {
        DshEpisodeRecordErrorKind::Unavailable | DshEpisodeRecordErrorKind::InvalidInput => {
            BridgeError::new(
                code::INTERNAL_ERROR,
                "LBR-MEMORY-002",
                "DSH Memory Episode generation is unavailable or received invalid source evidence",
            )
        }
        DshEpisodeRecordErrorKind::Storage | DshEpisodeRecordErrorKind::Generation => {
            BridgeError::new(
                code::INTERNAL_ERROR,
                "LBR-MEMORY-005",
                "DSH Memory Episode generation failed; inspect repository Memory diagnostics before retrying",
            )
            .retryable()
        }
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use sea_orm::{ConnectionTrait, Statement};
    use sha2::{Digest, Sha256};

    use super::*;
    use crate::internal::ai::{
        agent_bridge::{ingress, protocol::parse_request_line},
        context_budget::ContextBudget,
        memory::{
            AuditedMemoryDelivery, MemorySensitivity,
            memory_test_commit_injectable_episode as commit_injectable_episode,
            memory_test_fixture as fixture, memory_test_history as history,
            memory_test_seed_code_head as seed_code_head,
        },
    };

    fn request(line: &str) -> BridgeRequest {
        parse_request_line(line).expect("valid bridge request")
    }

    async fn open(ctx: &BridgeContext, session_id: &str) {
        ingress::dispatch(
            ctx,
            &request(&format!(
                r#"{{"jsonrpc":"2.0","method":"session.open","params":{{"session_id":"{session_id}"}},"id":1}}"#
            )),
        )
        .await
        .expect("open bridge session")
        .expect("session.open response");
    }

    #[tokio::test]
    async fn recall_requires_an_active_session() {
        let fixture = fixture().await;
        let ctx = BridgeContext::new(
            fixture.database.as_ref().clone(),
            fixture.digest.repository_id(),
            None,
        );
        let error = dispatch(
            &ctx,
            &request(
                r#"{"jsonrpc":"2.0","method":"memory.recall","params":{"session_id":"missing","query_text":"memory"},"id":1}"#,
            ),
        )
        .await
        .expect_err("unopened session must fail closed");
        assert_eq!(error.stable_code, "LBR-AGENT-032");

        open(&ctx, "closed-session").await;
        ingress::dispatch(
            &ctx,
            &request(
                r#"{"jsonrpc":"2.0","method":"session.close","params":{"session_id":"closed-session"},"id":2}"#,
            ),
        )
        .await
        .expect("close bridge session")
        .expect("session.close response");
        let error = dispatch(
            &ctx,
            &request(
                r#"{"jsonrpc":"2.0","method":"memory.recall","params":{"session_id":"closed-session","query_text":"memory"},"id":3}"#,
            ),
        )
        .await
        .expect_err("closed session must leave the process active set");
        assert_eq!(error.stable_code, "LBR-AGENT-032");
    }

    #[tokio::test]
    async fn recall_returns_the_exact_receipted_bridge_contract() {
        let fixture = fixture().await;
        let code_commit = seed_code_head(&fixture).await;
        commit_injectable_episode(
            &fixture,
            code_commit,
            "task-dsh-bridge-recall",
            1,
            MemorySensitivity::Internal,
            "bridgeuniquerecalltoken",
            "The bridge returns this audited Memory without rewriting it.",
        )
        .await;
        let delivery = AuditedMemoryDelivery::from_dependencies(
            Arc::new(history(&fixture)),
            Arc::clone(&fixture.digest),
            ContextBudget::default(),
        );
        let ctx = BridgeContext::new(
            fixture.database.as_ref().clone(),
            fixture.digest.repository_id(),
            None,
        )
        .with_memory_delivery(Ok(Arc::new(delivery)));
        open(&ctx, "dsh-session").await;

        let response = dispatch(
            &ctx,
            &request(
                r#"{"jsonrpc":"2.0","method":"memory.recall","params":{"session_id":"dsh-session","query_text":"bridgeuniquerecalltoken"},"id":7}"#,
            ),
        )
        .await
        .expect("memory recall")
        .expect("memory recall response");
        let result = response.result.expect("result");
        let delivery = &result["data"]["delivery"];
        assert_eq!(result["schema_version"], 1);
        assert_eq!(delivery["selected_count"], 1);
        assert_eq!(delivery["token_budget"], 1_600);
        let prompt_section = delivery["prompt_section"].as_str().expect("prompt section");
        assert!(prompt_section.contains("bridgeuniquerecalltoken"));
        assert_eq!(
            delivery["bundle_hash"].as_str().expect("bundle hash"),
            format!(
                "sha256:{}",
                hex::encode(Sha256::digest(prompt_section.as_bytes()))
            )
        );

        let receipt_id = delivery["receipt_id"].as_str().expect("receipt id");
        let persisted = fixture
            .database
            .query_one_raw(Statement::from_sql_and_values(
                fixture.database.get_database_backend(),
                "SELECT principal_hmac FROM context_selection_receipt WHERE receipt_id = ?",
                [receipt_id.into()],
            ))
            .await
            .expect("query persisted receipt")
            .expect("delivery receipt exists before response construction");
        let persisted_principal = persisted
            .try_get::<String>("", "principal_hmac")
            .expect("persisted principal HMAC");
        let expected_principal = fixture
            .digest
            .principal_digest(b"deepseek-harness:dsh-session")
            .expect("derive expected DSH principal")
            .encoded();
        assert_eq!(persisted_principal, expected_principal);
    }

    #[tokio::test]
    async fn recall_rejects_unknown_fields_and_returns_null_for_no_query_terms() {
        let fixture = fixture().await;
        let delivery = AuditedMemoryDelivery::from_dependencies(
            Arc::new(history(&fixture)),
            Arc::clone(&fixture.digest),
            ContextBudget::default(),
        );
        let ctx = BridgeContext::new(
            fixture.database.as_ref().clone(),
            fixture.digest.repository_id(),
            None,
        )
        .with_memory_delivery(Ok(Arc::new(delivery)));
        open(&ctx, "dsh-session").await;

        let error = dispatch(
            &ctx,
            &request(
                r#"{"jsonrpc":"2.0","method":"memory.recall","params":{"session_id":"dsh-session","query_text":"memory","budget":9999},"id":2}"#,
            ),
        )
        .await
        .expect_err("unknown client policy fields must fail closed");
        assert_eq!(error.stable_code, "LBR-AGENT-027");

        let oversized = "q".repeat(MAX_RAW_QUERY_BYTES + 1);
        let error = dispatch(
            &ctx,
            &request(&format!(
                r#"{{"jsonrpc":"2.0","method":"memory.recall","params":{{"session_id":"dsh-session","query_text":"{oversized}"}},"id":4}}"#,
            )),
        )
        .await
        .expect_err("oversized raw query must fail closed");
        assert_eq!(error.stable_code, "LBR-AGENT-027");
        assert!(!error.message.contains(&oversized));

        let response = dispatch(
            &ctx,
            &request(
                r#"{"jsonrpc":"2.0","method":"memory.recall","params":{"session_id":"dsh-session","query_text":"---"},"id":3}"#,
            ),
        )
        .await
        .expect("no-query recall")
        .expect("no-query response");
        assert!(response.result.expect("result")["data"]["delivery"].is_null());
    }
}
