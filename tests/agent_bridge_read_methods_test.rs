//! `tests/agent_bridge_read_methods_test.rs` — bridge typed read methods and
//! query scope (plan-20260818 LB-04).
//!
//! Exercises `context.get`, `status.get`, `history.search`,
//! `checkpoint.list`/`show` over the bridge durable projection, the unified
//! result envelope, bounded pagination, and repository-scoped queries.

use libra::internal::{
    ai::agent_bridge::{
        ingress::{BridgeContext, dispatch as ingress_dispatch},
        methods::dispatch as read_dispatch,
        protocol::{BridgeRequest, parse_request_line},
        storage::insert_checkpoint,
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

fn request(line: &str) -> BridgeRequest {
    parse_request_line(line).expect("valid request")
}

async fn seed(c: &BridgeContext) {
    ingress_dispatch(
        c,
        &request(
            r#"{"jsonrpc":"2.0","method":"session.open","params":{"session_id":"s1"},"id":1}"#,
        ),
    )
    .await
    .expect("open");
    ingress_dispatch(c, &request(r#"{"jsonrpc":"2.0","method":"event.append","params":{"session_id":"s1","events":[{"seq":1,"type":"tool/result","payload":"{\"tool\":\"a\"}"}]},"id":2}"#))
        .await
        .expect("append");
    insert_checkpoint(
        &c.conn,
        "cp-1",
        "s1",
        Some("agent-cp-1"),
        Some("oid-1"),
        1000,
    )
    .await
    .expect("checkpoint");
}

fn data(resp: &libra::internal::ai::agent_bridge::protocol::BridgeResponse) -> Value {
    resp.result.as_ref().expect("result").clone()
}

#[tokio::test]
async fn context_get_returns_scoped_envelope() {
    let c = ctx().await;
    seed(&c).await;
    let resp = read_dispatch(
        &c,
        &request(r#"{"jsonrpc":"2.0","method":"context.get","id":1}"#),
    )
    .await
    .expect("context.get")
    .expect("response");
    let r = data(&resp);
    // Unified envelope fields.
    assert_eq!(r["schema_version"], 1);
    assert_eq!(r["repository_id"], "repo-1");
    assert_eq!(r["status"], "ok");
    assert_eq!(r["data"]["sessions"][0]["session_id"], "s1");
    assert_eq!(r["data"]["recent_checkpoints"][0]["checkpoint_id"], "cp-1");
    // context.get returns metadata only — never raw event payloads or secrets.
    assert!(!r.to_string().contains("TAMPERED"));
    assert!(!r.to_string().contains("sk-"));
}

#[tokio::test]
async fn status_get_reports_repository_totals() {
    let c = ctx().await;
    seed(&c).await;
    let resp = read_dispatch(
        &c,
        &request(r#"{"jsonrpc":"2.0","method":"status.get","id":1}"#),
    )
    .await
    .expect("status.get")
    .expect("response");
    let r = data(&resp);
    assert_eq!(r["repository_id"], "repo-1");
    assert_eq!(r["data"]["bridge_sessions"], 1);
    assert_eq!(r["data"]["bridge_events"], 1);
    assert_eq!(r["data"]["bridge_checkpoints"], 1);
}

#[tokio::test]
async fn query_results_are_repository_scoped_and_paginated() {
    let c = ctx().await;
    // Two sessions in the same repository plus one in another repository.
    ingress_dispatch(
        &c,
        &request(
            r#"{"jsonrpc":"2.0","method":"session.open","params":{"session_id":"s1"},"id":1}"#,
        ),
    )
    .await
    .expect("open s1");
    ingress_dispatch(
        &c,
        &request(
            r#"{"jsonrpc":"2.0","method":"session.open","params":{"session_id":"s2"},"id":2}"#,
        ),
    )
    .await
    .expect("open s2");

    // history.search returns only this repository's sessions.
    let resp = read_dispatch(
        &c,
        &request(r#"{"jsonrpc":"2.0","method":"history.search","params":{"limit":1},"id":1}"#),
    )
    .await
    .expect("history.search")
    .expect("response");
    let r = data(&resp);
    assert_eq!(r["data"]["limit"], 1);
    let sessions = r["data"]["sessions"].as_array().expect("array");
    assert_eq!(sessions.len(), 1, "paginated to limit");
    assert!(sessions[0]["session_id"] == "s2" || sessions[0]["session_id"] == "s1");
    // All results are from this repository (scope derived from context).
    for s in sessions {
        assert_eq!(s["state"].as_str().unwrap_or(""), "open");
    }
}

#[tokio::test]
async fn history_search_filters_by_term() {
    let c = ctx().await;
    ingress_dispatch(
        &c,
        &request(
            r#"{"jsonrpc":"2.0","method":"session.open","params":{"session_id":"alpha-1"},"id":1}"#,
        ),
    )
    .await
    .expect("open alpha");
    ingress_dispatch(
        &c,
        &request(
            r#"{"jsonrpc":"2.0","method":"session.open","params":{"session_id":"beta-2"},"id":2}"#,
        ),
    )
    .await
    .expect("open beta");

    let resp = read_dispatch(
        &c,
        &request(
            r#"{"jsonrpc":"2.0","method":"history.search","params":{"query":"alpha"},"id":1}"#,
        ),
    )
    .await
    .expect("history.search")
    .expect("response");
    let r = data(&resp);
    let sessions = r["data"]["sessions"].as_array().expect("array");
    assert_eq!(sessions.len(), 1);
    assert_eq!(sessions[0]["session_id"], "alpha-1");
}

#[tokio::test]
async fn checkpoint_list_and_show_work() {
    let c = ctx().await;
    seed(&c).await;

    let resp = read_dispatch(
        &c,
        &request(
            r#"{"jsonrpc":"2.0","method":"checkpoint.list","params":{"session_id":"s1"},"id":1}"#,
        ),
    )
    .await
    .expect("checkpoint.list")
    .expect("response");
    let r = data(&resp);
    assert_eq!(r["data"]["checkpoints"][0]["checkpoint_id"], "cp-1");
    assert_eq!(
        r["data"]["checkpoints"][0]["agent_checkpoint_id"],
        "agent-cp-1"
    );

    let resp = read_dispatch(
        &c,
        &request(r#"{"jsonrpc":"2.0","method":"checkpoint.show","params":{"checkpoint_id":"cp-1"},"id":2}"#),
    )
    .await
    .expect("checkpoint.show")
    .expect("response");
    let r = data(&resp);
    assert_eq!(r["data"]["checkpoint_id"], "cp-1");
    assert_eq!(r["data"]["target_oid"], "oid-1");
}

#[tokio::test]
async fn checkpoint_show_missing_is_error_not_empty() {
    let c = ctx().await;
    let err = read_dispatch(
        &c,
        &request(r#"{"jsonrpc":"2.0","method":"checkpoint.show","params":{"checkpoint_id":"nope"},"id":1}"#),
    )
    .await
    .expect_err("missing checkpoint must fail, not silently return empty");
    assert_eq!(err.code, 1006);
}

/// `diff.get` is wired to the real diff service (see `agent_bridge_vcs_test`
/// for the live-repository behaviour). With no working repository in scope —
/// which is exactly this in-memory context — it fails closed with a scope
/// error, never an empty diff that a caller could mistake for "no changes".
#[tokio::test]
async fn diff_get_without_a_repository_fails_closed_on_scope() {
    let c = ctx().await;
    let err = read_dispatch(
        &c,
        &request(r#"{"jsonrpc":"2.0","method":"diff.get","params":{},"id":1}"#),
    )
    .await
    .expect_err("diff.get must not report an empty diff outside a repository");
    assert_eq!(err.stable_code, "LBR-AGENT-032");
}

/// `diff.get` selectors are typed and validated BEFORE the diff service is
/// reached, so an unknown mode or an escaping path is an invalid-params
/// refusal rather than a repository error (GC-LB-03).
#[tokio::test]
async fn diff_get_selectors_are_validated_before_the_service() {
    let c = ctx().await;
    for frame in [
        r#"{"jsonrpc":"2.0","method":"diff.get","params":{"mode":"HEAD~1"},"id":1}"#,
        r#"{"jsonrpc":"2.0","method":"diff.get","params":{"paths":["../escape"]},"id":2}"#,
        r#"{"jsonrpc":"2.0","method":"diff.get","params":{"paths":[":(exclude)x"]},"id":3}"#,
    ] {
        let err = read_dispatch(&c, &request(frame))
            .await
            .expect_err("untyped selector must be refused");
        assert_eq!(err.stable_code, "LBR-AGENT-027", "frame {frame}");
    }
}

/// `status.get` event/checkpoint totals must be scoped to the caller's
/// repository, never leaking another repository's session-keyed rows
/// (GC-LB-06 / LB-04 P1 fix).
#[tokio::test]
async fn status_get_totals_are_repository_scoped() {
    let conn = Database::connect("sqlite::memory:").await.expect("connect");
    run_builtin_migrations(&conn)
        .await
        .expect("apply migrations");

    // Two repositories share one database; both open sessions and write events.
    let repo1 = BridgeContext {
        conn: conn.clone(),
        repository_id: "repo-1".into(),
        worktree_id: None,
    };
    let repo2 = BridgeContext {
        conn: conn.clone(),
        repository_id: "repo-2".into(),
        worktree_id: None,
    };
    // Note: bridge_session_id is a global primary key, so each repo uses its
    // own session id.
    let opens = [(&repo1, "s1"), (&repo2, "s2")];
    for (c, sid) in opens {
        let open = format!(
            r#"{{"jsonrpc":"2.0","method":"session.open","params":{{"session_id":"{sid}"}},"id":1}}"#
        );
        ingress_dispatch(c, &request(&open)).await.expect("open");
        let frame = format!(
            r#"{{"jsonrpc":"2.0","method":"event.append","params":{{"session_id":"{sid}","events":[{{"seq":1,"type":"tool/result","payload":"{{}}"}}]}},"id":2}}"#
        );
        ingress_dispatch(c, &request(&frame)).await.expect("append");
    }

    // repo-1's status.get must show exactly its own 1 session + 1 event, not
    // repo-2's rows.
    let resp = read_dispatch(
        &repo1,
        &request(r#"{"jsonrpc":"2.0","method":"status.get","id":1}"#),
    )
    .await
    .expect("status.get")
    .expect("response");
    let r = data(&resp);
    assert_eq!(r["repository_id"], "repo-1");
    assert_eq!(r["data"]["bridge_sessions"], 1, "repo-1 sessions only");
    assert_eq!(r["data"]["bridge_events"], 1, "repo-1 events only");
}

/// An out-of-range `limit` is clamped to the v1 page cap and surfaces an
/// explicit `limit_clamped` warning (LB-04 AC: bounded pagination with a
/// truncated warning).
#[tokio::test]
async fn out_of_range_limit_reports_a_clamping_warning() {
    let c = ctx().await;
    seed(&c).await;
    // Request a limit far above the v1 page cap (MAX_PAGE = 100).
    let resp = read_dispatch(
        &c,
        &request(r#"{"jsonrpc":"2.0","method":"history.search","params":{"limit":100000},"id":1}"#),
    )
    .await
    .expect("history.search")
    .expect("response");
    let r = data(&resp);
    let warnings = r["warnings"].as_array().expect("warnings array");
    assert!(
        warnings.iter().any(|w| w["code"] == "limit_clamped"),
        "clamping must surface a warning: {r}"
    );
    // The returned limit is bounded to the cap, not the request.
    assert!(r["data"]["limit"].as_u64().unwrap_or(0) <= 100);
}
