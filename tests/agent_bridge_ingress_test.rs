//! `tests/agent_bridge_ingress_test.rs` — session/event ingress, batch ack and
//! provenance projection (plan-20260818 LB-03).
//!
//! Exercises `ingress::dispatch` against a real migrated in-memory database:
//! `session.open` idempotency + scope binding, `event.append` batch ack with
//! duplicate/digest-conflict/oversize handling, and `session.flush`/`close`
//! durability semantics.

use libra::internal::{
    ai::agent_bridge::{
        ingress::{BridgeContext, dispatch},
        protocol::parse_request_line,
        storage::get_session,
    },
    db::migration::run_builtin_migrations,
};
use sea_orm::Database;
use serde_json::Value;

async fn ctx() -> BridgeContext {
    let conn = Database::connect("sqlite::memory:").await.expect("connect");
    run_builtin_migrations(&conn)
        .await
        .expect("apply migrations");
    BridgeContext {
        conn,
        repository_id: "repo-1".into(),
        worktree_id: None,
    }
}

fn request(line: &str) -> libra::internal::ai::agent_bridge::protocol::BridgeRequest {
    parse_request_line(line).expect("valid request")
}

fn result_of(resp: &libra::internal::ai::agent_bridge::protocol::BridgeResponse) -> Value {
    resp.result.clone().expect("result")
}

#[tokio::test]
async fn session_open_is_idempotent_and_scope_bound() {
    let c = ctx().await;
    let r1 = dispatch(
        &c,
        &request(
            r#"{"jsonrpc":"2.0","method":"session.open","params":{"session_id":"s1"},"id":1}"#,
        ),
    )
    .await
    .expect("open")
    .expect("response");
    let v1 = result_of(&r1);
    assert_eq!(v1["created"], true);
    assert_eq!(v1["repository_id"], "repo-1");

    // Second open: not created, same session.
    let r2 = dispatch(
        &c,
        &request(
            r#"{"jsonrpc":"2.0","method":"session.open","params":{"session_id":"s1"},"id":2}"#,
        ),
    )
    .await
    .expect("open")
    .expect("response");
    let v2 = result_of(&r2);
    assert_eq!(v2["created"], false);
    assert_eq!(v2["session_id"], "s1");
}

#[tokio::test]
async fn event_batch_is_idempotent_and_reports_last_ack() {
    let c = ctx().await;
    dispatch(
        &c,
        &request(
            r#"{"jsonrpc":"2.0","method":"session.open","params":{"session_id":"s1"},"id":1}"#,
        ),
    )
    .await
    .expect("open");

    let append = |seqs: &[i64], id: u64| {
        let events: Vec<String> = seqs
            .iter()
            .map(|s| format!(r#"{{"seq":{s},"type":"tool/result","payload":"{{\"tool\":\"libra_status\",\"n\":{s}}}"}}"#))
            .collect();
        format!(
            r#"{{"jsonrpc":"2.0","method":"event.append","params":{{"session_id":"s1","events":[{}]}},"id":{id}}}"#,
            events.join(",")
        )
    };

    // First batch seqs 1,2 -> both accepted, last_acked = 2.
    let r = dispatch(&c, &request(&append(&[1, 2], 2)))
        .await
        .expect("append")
        .expect("resp");
    let v = result_of(&r);
    assert_eq!(v["last_acked_seq"], 2);
    assert_eq!(v["per_event"][0]["status"], "accepted");
    assert_eq!(v["per_event"][1]["status"], "accepted");

    // Replay seq 1 with the SAME payload -> duplicate, last_acked stays 2.
    let r = dispatch(&c, &request(&append(&[1], 3)))
        .await
        .expect("replay")
        .expect("resp");
    let v = result_of(&r);
    assert_eq!(v["per_event"][0]["status"], "duplicate");
    assert_eq!(v["last_acked_seq"], 2);

    // Replay seq 2 with a DIFFERENT payload -> rejected (digest conflict).
    let tampered = r#"{"jsonrpc":"2.0","method":"event.append","params":{"session_id":"s1","events":[{"seq":2,"type":"tool/result","payload":"{\"tool\":\"TAMPERED\"}"}]},"id":4}"#;
    let r = dispatch(&c, &request(tampered))
        .await
        .expect("conflict")
        .expect("resp");
    let v = result_of(&r);
    assert_eq!(v["per_event"][0]["status"], "conflict");
    assert_eq!(v["last_acked_seq"], 2);
}

#[tokio::test]
async fn event_append_requires_open_session_in_repo() {
    let c = ctx().await;
    // Append without opening the session -> scope error (fail-closed).
    let err = dispatch(
        &c,
        &request(r#"{"jsonrpc":"2.0","method":"event.append","params":{"session_id":"nope","events":[]},"id":1}"#),
    )
    .await
    .expect_err("append must fail closed");
    assert_eq!(err.code, 1006, "scope mismatch code");
}

#[tokio::test]
async fn event_batch_respects_v1_caps() {
    let c = ctx().await;
    dispatch(
        &c,
        &request(
            r#"{"jsonrpc":"2.0","method":"session.open","params":{"session_id":"s1"},"id":1}"#,
        ),
    )
    .await
    .expect("open");

    // Oversized batch (65 events > cap 64) -> invalid params, fail-closed.
    let events: Vec<String> = (0..65)
        .map(|i| format!(r#"{{"seq":{i},"type":"t","payload":"{{}}"}}"#))
        .collect();
    let big = format!(
        r#"{{"jsonrpc":"2.0","method":"event.append","params":{{"session_id":"s1","events":[{}]}},"id":9}}"#,
        events.join(",")
    );
    let r = dispatch(&c, &request(&big)).await;
    let err = r.expect_err("oversized batch must fail closed");
    assert_eq!(err.code, -32602);
}

#[tokio::test]
async fn session_flush_and_close_are_durable() {
    let c = ctx().await;
    dispatch(
        &c,
        &request(
            r#"{"jsonrpc":"2.0","method":"session.open","params":{"session_id":"s1"},"id":1}"#,
        ),
    )
    .await
    .expect("open");

    // flush enters 'flushing'; append durability is already committed.
    let r = dispatch(
        &c,
        &request(
            r#"{"jsonrpc":"2.0","method":"session.flush","params":{"session_id":"s1"},"id":2}"#,
        ),
    )
    .await
    .expect("flush")
    .expect("resp");
    let v = result_of(&r);
    assert_eq!(v["flushed"], true);

    // close -> 'closed'.
    let r = dispatch(
        &c,
        &request(
            r#"{"jsonrpc":"2.0","method":"session.close","params":{"session_id":"s1"},"id":3}"#,
        ),
    )
    .await
    .expect("close")
    .expect("resp");
    let v = result_of(&r);
    assert_eq!(v["closed"], true);

    // The state persisted durably (queryable via the store).
    let row = get_session(&c.conn, "s1")
        .await
        .expect("get")
        .expect("exists");
    assert_eq!(row.session_state, "closed");
}

#[tokio::test]
async fn evidence_and_provenance_links_are_recorded() {
    let c = ctx().await;
    dispatch(
        &c,
        &request(
            r#"{"jsonrpc":"2.0","method":"session.open","params":{"session_id":"s1"},"id":1}"#,
        ),
    )
    .await
    .expect("open");

    let r = dispatch(
        &c,
        &request(r#"{"jsonrpc":"2.0","method":"evidence.append","params":{"session_id":"s1","source_id":"ev-1","target_type":"ai_evidence","target_id":"checkpoint-abc"},"id":2}"#),
    )
    .await
    .expect("evidence")
    .expect("resp");
    let v = result_of(&r);
    assert_eq!(v["linked"], true);
    assert_eq!(v["source_type"], "evidence");

    let r = dispatch(
        &c,
        &request(r#"{"jsonrpc":"2.0","method":"provenance.append","params":{"session_id":"s1","source_id":"op-1","target_type":"operation","target_id":"operation-x"},"id":3}"#),
    )
    .await
    .expect("provenance")
    .expect("resp");
    let v = result_of(&r);
    assert_eq!(v["linked"], true);
    assert_eq!(v["source_type"], "provenance");
}

/// A conflicting evidence/provenance relink (same source, different target)
/// fails closed with a digest conflict instead of silently no-op'ing
/// (R1-P1-2 regression guard).
#[tokio::test]
async fn evidence_conflicting_relink_fails_closed() {
    let c = ctx().await;
    dispatch(
        &c,
        &request(
            r#"{"jsonrpc":"2.0","method":"session.open","params":{"session_id":"s1"},"id":1}"#,
        ),
    )
    .await
    .expect("open");

    // Link source ev-1 -> checkpoint-abc.
    let r = dispatch(
        &c,
        &request(r#"{"jsonrpc":"2.0","method":"evidence.append","params":{"session_id":"s1","source_id":"ev-1","target_type":"ai_evidence","target_id":"checkpoint-abc"},"id":2}"#),
    )
    .await
    .expect("evidence")
    .expect("resp");
    assert_eq!(result_of(&r)["linked"], true);

    // Re-pointing the SAME source at a DIFFERENT target must fail closed.
    let err = dispatch(
        &c,
        &request(r#"{"jsonrpc":"2.0","method":"evidence.append","params":{"session_id":"s1","source_id":"ev-1","target_type":"ai_evidence","target_id":"checkpoint-OTHER"},"id":3}"#),
    )
    .await
    .expect_err("conflicting relink must fail closed");
    assert_eq!(err.stable_code, "LBR-AGENT-033");
}

/// A retry of `session.open` that drifts workspace/parent/agent lineage fails
/// closed with a scope mismatch (R1-P2-7 regression guard).
#[tokio::test]
async fn session_open_scope_drift_fails_closed() {
    let c = ctx().await;
    dispatch(
        &c,
        &request(r#"{"jsonrpc":"2.0","method":"session.open","params":{"session_id":"s1","workspace_id":"ws-1"},"id":1}"#),
    )
    .await
    .expect("open");

    // Same session id but a different workspace_id on retry -> scope drift.
    let err = dispatch(
        &c,
        &request(r#"{"jsonrpc":"2.0","method":"session.open","params":{"session_id":"s1","workspace_id":"ws-2"},"id":2}"#),
    )
    .await
    .expect_err("scope drift must fail closed");
    assert_eq!(err.stable_code, "LBR-AGENT-032");
}

/// `last_event_seq` advances to the highest accepted sequence after an append
/// (R1-P2-6 regression guard).
#[tokio::test]
async fn last_event_seq_advances_after_append() {
    let c = ctx().await;
    dispatch(
        &c,
        &request(
            r#"{"jsonrpc":"2.0","method":"session.open","params":{"session_id":"s1"},"id":1}"#,
        ),
    )
    .await
    .expect("open");

    let r = dispatch(
        &c,
        &request(r#"{"jsonrpc":"2.0","method":"event.append","params":{"session_id":"s1","events":[{"seq":1,"type":"tool/result","payload":"{\"tool\":\"a\"}"},{"seq":2,"type":"tool/result","payload":"{\"tool\":\"b\"}"}]},"id":2}"#),
    )
    .await
    .expect("append")
    .expect("resp");
    assert_eq!(result_of(&r)["last_acked_seq"], 2);

    let row = get_session(&c.conn, "s1")
        .await
        .expect("get")
        .expect("exists");
    assert_eq!(
        row.last_event_seq, 2,
        "last_event_seq tracks the highest present sequence"
    );
    assert_eq!(row.last_acked_seq, 2);
}
