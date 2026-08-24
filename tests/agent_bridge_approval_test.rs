//! `tests/agent_bridge_approval_test.rs` — bridge approval gate (fail-closed)
//! (plan-20260818 LB-05 / 成功定义+ADR-LB dangerous-action approval).

use libra::internal::{
    ai::agent_bridge::{
        authorization::{ActionClass, Approval, bind_actor, classify, enforce},
        ingress::{BridgeContext, dispatch as ingress_dispatch},
        mutations::dispatch as mutation_dispatch,
        protocol::parse_request_line,
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

#[test]
fn dangerous_actions_default_to_deny() {
    // Safe/bounded writes are Normal.
    for m in [
        "checkpoint.create",
        "checkpoint.list",
        "checkpoint.show",
        "evidence.append",
        "provenance.append",
        "context.get",
    ] {
        assert_eq!(classify(m), ActionClass::Normal, "{m} should be normal");
    }
    // Anything that can mutate working state / refs / remote is Dangerous.
    for m in [
        "checkpoint.restore",
        "commit.create",
        "review.run",
        "branch.switch",
        "reset",
        "push",
        "delete.worktree",
    ] {
        assert_eq!(
            classify(m),
            ActionClass::Dangerous,
            "{m} must default to dangerous"
        );
    }
    // Unknown actions fail closed (dangerous).
    assert_eq!(classify("totally.unknown"), ActionClass::Dangerous);
}

#[test]
fn enforce_denies_without_or_with_denied_approval() {
    // Normal actions need no approval.
    assert!(enforce("checkpoint.create", None).is_ok());
    // Dangerous actions with no approval -> denied.
    let err = enforce("checkpoint.restore", None).expect_err("must be denied");
    assert_eq!(err.stable_code, "LBR-AGENT-035");
    // Dangerous actions with explicit denied approval -> denied.
    let denied = Approval {
        decision: "denied".into(),
        approver: Some("alice".into()),
    };
    let err = enforce("checkpoint.restore", Some(&denied)).expect_err("denied approval");
    assert_eq!(err.stable_code, "LBR-AGENT-035");
    // Dangerous actions with approved approval -> allowed to admission.
    let approved = Approval {
        decision: "approved".into(),
        approver: Some("alice".into()),
    };
    assert!(enforce("checkpoint.restore", Some(&approved)).is_ok());
}

#[test]
fn bind_actor_rejects_spoof() {
    // No self-report -> trusted scope is authoritative.
    assert!(
        bind_actor(
            "commit.create",
            None,
            Some("deepseek-harness"),
            Some("agent-1")
        )
        .is_ok()
    );
    // Matching self-report -> ok.
    assert!(
        bind_actor(
            "commit.create",
            Some(("deepseek-harness", "agent-1")),
            Some("deepseek-harness"),
            Some("agent-1"),
        )
        .is_ok()
    );
    // Mismatched self-report -> scope_mismatch (fail closed).
    let err = bind_actor(
        "commit.create",
        Some(("system", "root")),
        Some("deepseek-harness"),
        Some("agent-1"),
    )
    .expect_err("spoof rejected");
    assert_eq!(err.stable_code, "LBR-AGENT-032");
}

#[test]
fn malformed_approval_is_invalid_params() {
    let params = Some(serde_json::json!({ "approval": { "decision": "maybe" } }));
    match Approval::from_params(&params) {
        Some(Err(e)) => assert_eq!(e.code, -32602),
        _ => panic!("malformed approval must be invalid_params"),
    }
}

/// The approval gate is enforced at dispatch: a dangerous mutation without
/// approval is refused (stable denied), and the denial is not swallowed.
#[tokio::test]
async fn dispatch_enforces_approval_for_dangerous_mutation() {
    let c = ctx().await;
    ingress_dispatch(
        &c,
        &parse_request_line(
            r#"{"jsonrpc":"2.0","method":"session.open","params":{"session_id":"s1"},"id":1}"#,
        )
        .unwrap(),
    )
    .await
    .expect("open");

    let err = mutation_dispatch(
        &c,
        &parse_request_line(r#"{"jsonrpc":"2.0","method":"checkpoint.restore","params":{"operation_id":"op-1","session_id":"s1","checkpoint_id":"cp-1"},"id":1}"#).unwrap(),
    )
    .await
    .expect_err("restore without approval must be denied");
    assert_eq!(err.stable_code, "LBR-AGENT-035");
}
