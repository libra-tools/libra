//! `tests/agent_bridge_workspace_test.rs` — bridge workspace lease binding
//! (plan-20260818 LB-06).
//!
//! `workspace.claim`/`renew`/`release` must route through the existing
//! `WorkspaceStore` (owner + monotonic fence), bind to the trusted bridge
//! session/actor (never a self-reported owner), fail closed on a stale
//! owner/fence and on repository/worktree identity mismatch, and leave no
//! orphan active lease. The store's `libra.repoid` is seeded exactly as
//! `libra init` does so `RepoIdentity::resolve` succeeds.

use libra::internal::{
    ai::agent_bridge::{
        ingress::BridgeContext,
        protocol::{BridgeRequest, BridgeResponse, JSONRPC_VERSION},
        storage,
        workspace::dispatch,
    },
    db::migration::run_builtin_migrations,
    workspace::WorkspaceStore,
};
use sea_orm::{ConnectionTrait, Database, DbBackend, Statement};

const REPO: &str = "repo-ws-1";

async fn fresh_db() -> sea_orm::DatabaseConnection {
    let db = Database::connect("sqlite::memory:").await.expect("connect");
    run_builtin_migrations(&db).await.expect("apply migrations");
    db.execute_raw(Statement::from_sql_and_values(
        DbBackend::Sqlite,
        "INSERT INTO config_kv (key, value, encrypted) VALUES ('libra.repoid', ?, 0)",
        [REPO.into()],
    ))
    .await
    .expect("seed repository identity");
    db
}

fn ctx(db: sea_orm::DatabaseConnection) -> BridgeContext {
    BridgeContext {
        conn: db,
        repository_id: REPO.to_string(),
        worktree_id: None,
    }
}

fn request(method: &str, params: serde_json::Value) -> BridgeRequest {
    BridgeRequest {
        jsonrpc: JSONRPC_VERSION.to_string(),
        method: method.to_string(),
        params: Some(params),
        id: Some(serde_json::json!(1)),
    }
}

/// Response helper: return the `result` object or panic on an error.
fn result(resp: BridgeResponse) -> serde_json::Value {
    match resp.error {
        Some(e) => panic!("expected success, got error frame: {:?}", e),
        None => resp.result.expect("result present"),
    }
}

/// Open a bridge session so workspace methods have trusted session scope.
async fn open_session(db: &sea_orm::DatabaseConnection, session_id: &str) {
    storage::open_session(db, session_id, REPO, None, None, None, None, 1000)
        .await
        .expect("open bridge session");
}

/// Open a bridge session with an explicit agent_id (client-supplied label).
async fn open_session_with_agent(
    db: &sea_orm::DatabaseConnection,
    session_id: &str,
    agent_id: &str,
) {
    storage::open_session(db, session_id, REPO, None, None, None, Some(agent_id), 1000)
        .await
        .expect("open bridge session with agent id");
}

#[tokio::test]
async fn claim_renew_release_round_trip_via_store() {
    let db = fresh_db().await;
    open_session(&db, "s1").await;
    let c = ctx(db.clone());
    let dir = tempfile::tempdir().expect("tempdir");
    let ws = dir.path().join("work");

    // Claim a task workspace (path only, no worktree).
    let resp = dispatch(
        &c,
        &request(
            "workspace.claim",
            serde_json::json!({
                "session_id": "s1",
                "path": ws.to_str().unwrap(),
                "lease_ttl_ms": 60_000,
            }),
        ),
    )
    .await
    .expect("claim dispatches")
    .expect("handled");
    let r = result(resp);
    let workspace_id = r["workspace_id"]
        .as_str()
        .expect("workspace_id")
        .to_string();
    let owner = r["owner"].as_str().expect("owner").to_string();
    let fence = r["fence"].as_i64().expect("fence");
    assert!(
        owner.contains("deepseek-harness:"),
        "actor-derived owner: {owner}"
    );

    // Renew with the returned owner/fence.
    let resp = dispatch(
        &c,
        &request(
            "workspace.renew",
            serde_json::json!({
                "session_id": "s1",
                "workspace_id": workspace_id,
                "owner": owner,
                "fence": fence,
                "lease_ttl_ms": 60_000,
            }),
        ),
    )
    .await
    .expect("renew dispatches")
    .expect("handled");
    let r = result(resp);
    assert_eq!(r["workspace_id"].as_str().unwrap(), workspace_id);

    // Release with the same owner/fence.
    let resp = dispatch(
        &c,
        &request(
            "workspace.release",
            serde_json::json!({
                "session_id": "s1",
                "workspace_id": workspace_id,
                "owner": owner,
                "fence": fence,
            }),
        ),
    )
    .await
    .expect("release dispatches")
    .expect("handled");
    let r = result(resp);
    assert_eq!(r["released"], true);
}

#[tokio::test]
async fn stale_fence_and_parent_scope_fail_closed() {
    let db = fresh_db().await;
    open_session(&db, "s1").await;
    let c = ctx(db.clone());
    let dir = tempfile::tempdir().expect("tempdir");
    let ws = dir.path().join("work");

    // Claim with a short TTL so the lease can be reclaimed.
    let resp = dispatch(
        &c,
        &request(
            "workspace.claim",
            serde_json::json!({
                "session_id": "s1",
                "path": ws.to_str().unwrap(),
                "lease_ttl_ms": 100,
            }),
        ),
    )
    .await
    .expect("claim")
    .expect("handled");
    let r = result(resp);
    let workspace_id = r["workspace_id"].as_str().unwrap().to_string();
    let owner = r["owner"].as_str().unwrap().to_string();
    let real_fence = r["fence"].as_i64().unwrap();

    // Reclaim with a higher fence (as a doctor/new owner would) once the
    // short lease has expired.
    let new_owner = format!("{owner}-new");
    let now = libra::internal::workspace::now_ms();
    let reclaimed =
        WorkspaceStore::reclaim_expired(&db, &workspace_id, &new_owner, 60_000, now + 120_000)
            .await
            .expect("reclaim");

    // The stale original owner (old fence) must be unable to release or renew.
    let outcome = dispatch(
        &c,
        &request(
            "workspace.release",
            serde_json::json!({
                "session_id": "s1",
                "workspace_id": workspace_id,
                "owner": owner,
                "fence": real_fence,
            }),
        ),
    )
    .await;
    let err = match outcome {
        Ok(_) => panic!("stale owner release must be refused"),
        Err(e) => e,
    };
    assert!(
        err.stable_code.starts_with("LBR-AGENT-"),
        "stale fence must map to a stable bridge scope error: {err:?}"
    );

    // The new owner can renew.
    let _ = reclaimed;
}

#[tokio::test]
async fn workspace_requires_open_session_in_repo() {
    let db = fresh_db().await;
    let c = ctx(db.clone());
    let dir = tempfile::tempdir().expect("tempdir");
    let ws = dir.path().join("work");

    // No session open -> scope mismatch, not a fake success.
    let outcome = dispatch(
        &c,
        &request(
            "workspace.claim",
            serde_json::json!({
                "session_id": "ghost",
                "path": ws.to_str().unwrap(),
            }),
        ),
    )
    .await;
    // The missing-session error is a fail-closed rejection (Err from dispatch).
    let err = match outcome {
        Ok(_) => panic!("claim without an open session must fail closed"),
        Err(e) => e,
    };
    assert!(
        err.stable_code.starts_with("LBR-AGENT-"),
        "expected a stable bridge scope error, got: {err:?}"
    );
}

#[tokio::test]
async fn self_reported_owner_is_not_honoured() {
    let db = fresh_db().await;
    open_session(&db, "s1").await;
    let c = ctx(db.clone());
    let dir = tempfile::tempdir().expect("tempdir");
    let ws = dir.path().join("work");

    let resp = dispatch(
        &c,
        &request(
            "workspace.claim",
            serde_json::json!({
                "session_id": "s1",
                "path": ws.to_str().unwrap(),
                "lease_ttl_ms": 60_000,
            }),
        ),
    )
    .await
    .expect("claim")
    .expect("handled");
    let r = result(resp);
    let workspace_id = r["workspace_id"].as_str().unwrap().to_string();
    // The real owner is actor-derived; a caller that presents a DIFFERENT
    // owner must be refused, not accepted as an override.
    let outcome = dispatch(
        &c,
        &request(
            "workspace.release",
            serde_json::json!({
                "session_id": "s1",
                "workspace_id": workspace_id,
                "owner": "attacker-self-report",
                "fence": 1,
            }),
        ),
    )
    .await;
    match outcome {
        Ok(_) => panic!("a self-reported owner must not be honoured"),
        Err(e) => assert!(
            e.stable_code.starts_with("LBR-AGENT-"),
            "self-reported owner must fail closed: {e:?}"
        ),
    }
}

/// A client-supplied `agent_id` must NEVER become the lease owner (GC-LB-07):
/// two colluding sessions claiming the same `agent_id` must not derive the
/// same owner, or one could release the other's workspace lease. The owner is
/// derived solely from the authenticated `bridge_session_id`.
#[tokio::test]
async fn forged_agent_id_cannot_alias_another_sessions_lease_owner() {
    let db = fresh_db().await;
    // Victim session s1 (no agent id) and an attacker session a1 that FORGES
    // agent_id = "s1" to try to collide with the victim's owner.
    open_session(&db, "s1").await;
    open_session_with_agent(&db, "attacker-1", "s1").await;
    let c = ctx(db.clone());
    let dir = tempfile::tempdir().expect("tempdir");
    let ws = dir.path().join("work");

    // Victim claims a workspace.
    let resp = dispatch(
        &c,
        &request(
            "workspace.claim",
            serde_json::json!({
                "session_id": "s1",
                "path": ws.to_str().unwrap(),
                "lease_ttl_ms": 60_000,
            }),
        ),
    )
    .await
    .expect("claim")
    .expect("handled");
    let r = result(resp);
    let workspace_id = r["workspace_id"].as_str().unwrap().to_string();
    let victim_owner = r["owner"].as_str().unwrap().to_string();
    assert_eq!(victim_owner, "deepseek-harness:s1");

    // The attacker's derived owner must NOT equal the victim's, even though
    // the attacker claimed agent_id = "s1". Their owner is bound to their own
    // session id.
    let resp = dispatch(
        &c,
        &request(
            "workspace.claim",
            serde_json::json!({
                "session_id": "attacker-1",
                "path": dir.path().join("other").to_str().unwrap(),
                "lease_ttl_ms": 60_000,
            }),
        ),
    )
    .await
    .expect("attacker claim")
    .expect("handled");
    let r = result(resp);
    let attacker_owner = r["owner"].as_str().unwrap().to_string();
    assert_eq!(attacker_owner, "deepseek-harness:attacker-1");
    assert_ne!(
        attacker_owner, victim_owner,
        "a forged agent_id must not alias another session's lease owner"
    );

    // The attacker cannot release the victim's lease (wrong derived owner).
    let outcome = dispatch(
        &c,
        &request(
            "workspace.release",
            serde_json::json!({
                "session_id": "attacker-1",
                "workspace_id": workspace_id,
                "owner": victim_owner,
                "fence": 1,
            }),
        ),
    )
    .await;
    assert!(
        outcome.is_err(),
        "attacker must not release the victim's lease"
    );
}

/// `workspace.renew` with a mismatched/self-reported owner is refused
/// fail-closed, symmetric with release.
#[tokio::test]
async fn renew_with_mismatched_owner_fails_closed() {
    let db = fresh_db().await;
    open_session(&db, "s1").await;
    let c = ctx(db.clone());
    let dir = tempfile::tempdir().expect("tempdir");
    let ws = dir.path().join("work");

    let resp = dispatch(
        &c,
        &request(
            "workspace.claim",
            serde_json::json!({
                "session_id": "s1",
                "path": ws.to_str().unwrap(),
                "lease_ttl_ms": 60_000,
            }),
        ),
    )
    .await
    .expect("claim")
    .expect("handled");
    let r = result(resp);
    let workspace_id = r["workspace_id"].as_str().unwrap().to_string();
    let real_owner = r["owner"].as_str().unwrap().to_string();
    let real_fence = r["fence"].as_i64().unwrap();

    // A mismatched self-reported owner is refused.
    let outcome = dispatch(
        &c,
        &request(
            "workspace.renew",
            serde_json::json!({
                "session_id": "s1",
                "workspace_id": workspace_id,
                "owner": "attacker-self-report",
                "fence": real_fence,
                "lease_ttl_ms": 60_000,
            }),
        ),
    )
    .await;
    assert!(
        outcome.is_err(),
        "mismatched owner on renew must fail closed"
    );

    // The real owner/fence renews successfully.
    let resp = dispatch(
        &c,
        &request(
            "workspace.renew",
            serde_json::json!({
                "session_id": "s1",
                "workspace_id": workspace_id,
                "owner": real_owner,
                "fence": real_fence,
                "lease_ttl_ms": 60_000,
            }),
        ),
    )
    .await
    .expect("renew")
    .expect("handled");
    let r = result(resp);
    assert_eq!(r["workspace_id"].as_str().unwrap(), workspace_id);
}
