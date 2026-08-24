//! `tests/agent_bridge_checkpoint_test.rs` — bridge checkpoint create then
//! list/show round-trip (plan-20260818 LB-05).

use libra::internal::{
    ai::agent_bridge::{
        ingress::{BridgeContext, dispatch as ingress_dispatch},
        methods::dispatch as read_dispatch,
        mutations::dispatch as mutation_dispatch,
        protocol::{BridgeRequest, parse_request_line},
    },
    db::migration::run_builtin_migrations,
};
use sea_orm::Database;

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

#[tokio::test]
async fn checkpoint_create_list_show_round_trip() {
    let c = ctx().await;
    ingress_dispatch(
        &c,
        &request(
            r#"{"jsonrpc":"2.0","method":"session.open","params":{"session_id":"s1"},"id":1}"#,
        ),
    )
    .await
    .expect("open");

    // Create a checkpoint through the mutation registry.
    let r = mutation_dispatch(
        &c,
        &request(r#"{"jsonrpc":"2.0","method":"checkpoint.create","params":{"operation_id":"op-1","session_id":"s1","checkpoint_id":"cp-1","agent_checkpoint_id":"agent-cp-1","target_oid":"oid-1"},"id":2}"#),
    )
    .await
    .expect("checkpoint.create")
    .expect("response");
    let result = r.result.as_ref().expect("result");
    assert_eq!(result["created"], true);
    assert_eq!(result["checkpoint_id"], "cp-1");
    assert_eq!(result["provenance"]["session_id"], "s1");

    // It is visible through the read registry (checkpoint.list).
    let r = read_dispatch(
        &c,
        &request(
            r#"{"jsonrpc":"2.0","method":"checkpoint.list","params":{"session_id":"s1"},"id":3}"#,
        ),
    )
    .await
    .expect("checkpoint.list")
    .expect("response");
    let list = r.result.as_ref().expect("result");
    assert_eq!(list["data"]["checkpoints"][0]["checkpoint_id"], "cp-1");
    assert_eq!(
        list["data"]["checkpoints"][0]["agent_checkpoint_id"],
        "agent-cp-1"
    );

    // And through checkpoint.show.
    let r = read_dispatch(
        &c,
        &request(r#"{"jsonrpc":"2.0","method":"checkpoint.show","params":{"checkpoint_id":"cp-1"},"id":4}"#),
    )
    .await
    .expect("checkpoint.show")
    .expect("response");
    let show = r.result.as_ref().expect("result");
    assert_eq!(show["data"]["checkpoint_id"], "cp-1");
    assert_eq!(show["data"]["target_oid"], "oid-1");
}

#[tokio::test]
async fn checkpoint_create_requires_open_session_in_repo() {
    let c = ctx().await;
    // No session opened -> scope_mismatch (fail closed, not empty success).
    let err = mutation_dispatch(
        &c,
        &request(r#"{"jsonrpc":"2.0","method":"checkpoint.create","params":{"operation_id":"op-1","session_id":"ghost","checkpoint_id":"cp-1"},"id":1}"#),
    )
    .await
    .expect_err("missing session must fail closed");
    assert_eq!(err.stable_code, "LBR-AGENT-032");
}

/// Checkpoint reads are repository-scoped: a checkpoint created under one
/// repository identity is invisible to a query scoped to a different
/// repository on the same database (GC-LB-06 / R3-P1-2 regression guard).
#[tokio::test]
async fn checkpoint_reads_are_repository_scoped() {
    let conn = Database::connect("sqlite::memory:").await.expect("connect");
    run_builtin_migrations(&conn)
        .await
        .expect("apply migrations");
    let c1 = BridgeContext {
        conn: conn.clone(),
        repository_id: "repo-1".into(),
        worktree_id: None,
    };
    let c2 = BridgeContext {
        conn: conn.clone(),
        repository_id: "repo-2".into(),
        worktree_id: None,
    };

    // Open a session and create a checkpoint in repo-1.
    ingress_dispatch(
        &c1,
        &request(
            r#"{"jsonrpc":"2.0","method":"session.open","params":{"session_id":"s1"},"id":1}"#,
        ),
    )
    .await
    .expect("open");
    mutation_dispatch(
        &c1,
        &request(r#"{"jsonrpc":"2.0","method":"checkpoint.create","params":{"operation_id":"op-1","session_id":"s1","checkpoint_id":"cp-1"},"id":2}"#),
    )
    .await
    .expect("checkpoint.create in repo-1");

    // repo-2's checkpoint.list must NOT see cp-1.
    let r = read_dispatch(
        &c2,
        &request(
            r#"{"jsonrpc":"2.0","method":"checkpoint.list","params":{"session_id":"s1"},"id":3}"#,
        ),
    )
    .await
    .expect("checkpoint.list in repo-2")
    .expect("response");
    let list = r.result.as_ref().expect("result");
    assert!(
        list["data"]["checkpoints"]
            .as_array()
            .expect("array")
            .is_empty(),
        "repo-2 must not see repo-1's checkpoint"
    );

    // checkpoint.show for cp-1 from repo-2 must fail closed (not found in scope).
    let err = read_dispatch(
        &c2,
        &request(r#"{"jsonrpc":"2.0","method":"checkpoint.show","params":{"checkpoint_id":"cp-1"},"id":4}"#),
    )
    .await
    .expect_err("repo-2 must not see repo-1's checkpoint via show");
    assert_eq!(err.code, 1006);
}
