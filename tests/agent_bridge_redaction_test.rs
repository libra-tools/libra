//! `tests/agent_bridge_redaction_test.rs` — server-side redaction
//! fail-closed (plan-20260818 LB-03, ADR-LB-03, GC-LB-08).
//!
//! Proves raw prompt/reasoning/secrets never land in the durable projection:
//! a secret-bearing event is persisted redacted (secret absent, marker
//! present), and a payload that cannot be safely projected is refused rather
//! than stored raw or acked.

use libra::internal::{
    ai::agent_bridge::{
        ingress::{BridgeContext, dispatch},
        protocol::{BridgeRequest, parse_request_line},
    },
    db::migration::run_builtin_migrations,
};
use sea_orm::{ConnectionTrait, Database, DbBackend, Statement};
use serde_json::Value;

fn request(line: &str) -> BridgeRequest {
    parse_request_line(line).expect("valid request")
}

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

async fn read_event_payload(
    conn: &sea_orm::DatabaseConnection,
    session: &str,
    seq: i64,
) -> Option<String> {
    let rows = conn
        .query_all_raw(Statement::from_sql_and_values(
            DbBackend::Sqlite,
            "SELECT payload FROM agent_bridge_event WHERE bridge_session_id = ? AND event_seq = ?",
            [session.into(), seq.into()],
        ))
        .await
        .expect("query");
    rows.into_iter()
        .next()
        .and_then(|r| r.try_get::<String>("", "payload").ok())
}

#[tokio::test]
async fn event_secret_is_redacted_before_persistence() {
    let c = ctx().await;
    dispatch(
        &c,
        &request(
            r#"{"jsonrpc":"2.0","method":"session.open","params":{"session_id":"s1"},"id":1}"#,
        ),
    )
    .await
    .expect("open");

    // Append an event carrying a secret token in its payload. The token is a
    // GitHub-PAT-shaped value (36 alphanumerics after `ghp_`) that the default
    // redactor's `github-token` rule catches.
    let secret = format!("{}_{}", "ghp", "abcdefghijklmnopqrstuvwxyz0123456789");
    let payload_text =
        serde_json::json!({ "tool": "api_call", "token": secret, "note": "hi" }).to_string();
    // The event `payload` field is a JSON string holding the projection text.
    let payload_field = serde_json::to_string(&payload_text).expect("encode payload string");
    let frame = format!(
        r#"{{"jsonrpc":"2.0","method":"event.append","params":{{"session_id":"s1","events":[{{"seq":1,"type":"tool/result","payload":{payload_field}}}]}},"id":2}}"#
    );
    dispatch(&c, &request(&frame))
        .await
        .expect("append")
        .expect("resp");

    let stored = read_event_payload(&c.conn, "s1", 1)
        .await
        .expect("event persisted");
    // The redactor replaces the secret value with a non-secret marker, so the
    // raw token value never survives into the durable projection.
    let stored_v: serde_json::Value = serde_json::from_str(&stored).expect("stored is JSON");
    assert_ne!(
        stored_v["token"].as_str(),
        Some(secret.as_str()),
        "raw secret token must not survive redaction: {stored}"
    );
    // The redaction marker is present (secret scrubbed, not dropped).
    assert!(
        stored_v["token"]
            .as_str()
            .is_some_and(|v| v.contains("REDACTED") || v == "***"),
        "secret should be replaced with a marker: {stored}"
    );
}

#[tokio::test]
async fn non_json_payload_is_refused_not_acked() {
    let c = ctx().await;
    dispatch(
        &c,
        &request(
            r#"{"jsonrpc":"2.0","method":"session.open","params":{"session_id":"s1"},"id":1}"#,
        ),
    )
    .await
    .expect("open");

    // A payload that is not valid JSON cannot be a typed projection and is
    // refused fail-closed (no raw fallback, no ack).
    let frame = r#"{"jsonrpc":"2.0","method":"event.append","params":{"session_id":"s1","events":[{"seq":1,"type":"tool/result","payload":"this is not json"}]},"id":2}"#;
    let err = dispatch(&c, &request(frame))
        .await
        .expect_err("non-JSON payload must fail closed");
    assert_eq!(err.code, 1008, "redaction-failed code (no raw fallback)");
    assert_eq!(err.stable_code, "LBR-AGENT-034");

    // Nothing was persisted.
    assert!(
        read_event_payload(&c.conn, "s1", 1).await.is_none(),
        "refused payload must not be persisted"
    );
}

#[tokio::test]
async fn valid_structured_payload_is_persisted() {
    let c = ctx().await;
    dispatch(
        &c,
        &request(
            r#"{"jsonrpc":"2.0","method":"session.open","params":{"session_id":"s1"},"id":1}"#,
        ),
    )
    .await
    .expect("open");

    let frame = r#"{"jsonrpc":"2.0","method":"event.append","params":{"session_id":"s1","events":[{"seq":1,"type":"tool/result","payload":"{\"kind\":\"tool/result\",\"tool\":\"libra_status\",\"ok\":true}"}]},"id":2}"#;
    dispatch(&c, &request(frame))
        .await
        .expect("append")
        .expect("resp");

    let stored = read_event_payload(&c.conn, "s1", 1)
        .await
        .expect("event persisted");
    let v: Value = serde_json::from_str(&stored).expect("stored payload is JSON");
    assert_eq!(v["tool"], "libra_status");
    assert_eq!(v["ok"], true);
}
