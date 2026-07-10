//! A0-10 cloud-mirror tombstone propagation for agent capture data.
//!
//! Ground truth (verified against `src/command/cloud.rs`): the D1 agent-capture
//! MIRROR is live — `libra cloud sync` upserts `agent_session` /
//! `agent_checkpoint` to D1 on every sync (`sync_agent_capture_tables`) and
//! `libra cloud restore` reads them back (`restore_agent_capture_from_d1`).
//! What is DEFERRED is delete/tombstone propagation: local agent-capture
//! erasure ([`HistoryManager::erase_session_local`]) rewrites `refs/libra/traces`
//! and deletes the LOCAL `agent_session` / `agent_checkpoint` rows +
//! `object_index`, but does **not** delete the D1 mirror rows or write a
//! tombstone. A subsequent `libra cloud restore` can therefore REVIVE erased
//! capture.
//!
//! This test characterizes that deferred contract against a real D1 endpoint:
//! it mirrors a session row to D1 (as a sync would), performs a real local
//! erase of the SAME session, and asserts the D1 mirror row still exists (no
//! tombstone was propagated). When propagation lands, this assertion must be
//! flipped and the doc row in `docs/development/tracing/agent.md` updated in the
//! same change.
//!
//! **Layer:** L3 — runs only with `--features test-live-cloud` AND `LIBRA_D1_*`
//! credentials; otherwise it prints `skipped` and returns without failing. The
//! session-row mirror is a D1 concern, so the gate is D1-only.

use std::{path::Path, process::Command, sync::Arc, time::Duration};

use libra::{
    internal::{ai::history::HistoryManager, branch::TRACES_BRANCH},
    utils::{
        client_storage::ClientStorage,
        d1_client::{AgentCheckpointRow, AgentSessionRow, D1Client},
    },
};
use sea_orm::{ConnectOptions, ConnectionTrait, Database, DatabaseConnection, Statement, Value};
use serial_test::serial;
use uuid::Uuid;

fn env_is_present(name: &str) -> bool {
    std::env::var(name).is_ok_and(|value| !value.is_empty())
}

fn live_d1_tests_enabled() -> bool {
    cfg!(feature = "test-live-cloud")
        && [
            "LIBRA_D1_ACCOUNT_ID",
            "LIBRA_D1_API_TOKEN",
            "LIBRA_D1_DATABASE_ID",
        ]
        .iter()
        .all(|name| env_is_present(name))
}

fn d1_client_from_env() -> D1Client {
    D1Client::new(
        std::env::var("LIBRA_D1_ACCOUNT_ID").expect("LIBRA_D1_ACCOUNT_ID"),
        std::env::var("LIBRA_D1_API_TOKEN").expect("LIBRA_D1_API_TOKEN"),
        std::env::var("LIBRA_D1_DATABASE_ID").expect("LIBRA_D1_DATABASE_ID"),
    )
}

async fn connect_repo_db(repo: &Path) -> DatabaseConnection {
    let db_path = repo.join(".libra").join("libra.db");
    let mut opts = ConnectOptions::new(format!("sqlite://{}", db_path.display()));
    opts.sqlx_logging(false)
        .connect_timeout(Duration::from_secs(5));
    Database::connect(opts).await.expect("connect repo db")
}

/// Pin the local repo id so the local repo and the D1 mirror row share it — a
/// future erase keyed by `repo_id` would then target this exact D1 row.
async fn set_repo_id(conn: &DatabaseConnection, repo_id: &str) {
    conn.execute(Statement::from_sql_and_values(
        conn.get_database_backend(),
        "INSERT INTO config_kv (key, value, encrypted) VALUES ('libra.repoid', ?, 0)",
        vec![Value::from(repo_id)],
    ))
    .await
    .expect("pin libra.repoid");
}

async fn seed_local_session(conn: &DatabaseConnection, session_id: &str) {
    conn.execute(Statement::from_sql_and_values(
        conn.get_database_backend(),
        "INSERT INTO agent_session (
            session_id, agent_kind, provider_session_id, state, working_dir,
            metadata_json, redaction_report, started_at, last_event_at, stopped_at
         ) VALUES (?, 'claude_code', ?, 'stopped', '/tmp/agent-tombstone-guard', '{}', '{}', 1, 2, 3)",
        vec![
            Value::from(session_id),
            Value::from(format!("provider-{session_id}")),
        ],
    ))
    .await
    .expect("seed local agent_session");
}

async fn seed_local_checkpoint(
    conn: &DatabaseConnection,
    id: &str,
    session: &str,
    created_at: i64,
) {
    conn.execute(Statement::from_sql_and_values(
        conn.get_database_backend(),
        "INSERT INTO agent_checkpoint (
            checkpoint_id, session_id, scope, parent_commit, tree_oid,
            metadata_blob_oid, traces_commit, created_at
         ) VALUES (?, ?, 'committed', ?, ?, ?, ?, ?)",
        vec![
            Value::from(id),
            Value::from(session),
            Value::from(format!("{created_at:040x}")),
            Value::from(format!("{:040x}", created_at + 1)),
            Value::from(format!("{:040x}", created_at + 2)),
            Value::from(String::new()),
            Value::from(created_at),
        ],
    ))
    .await
    .expect("seed local agent_checkpoint");
}

async fn count(conn: &DatabaseConnection, sql: &str) -> i64 {
    let row = conn
        .query_one(Statement::from_string(
            conn.get_database_backend(),
            sql.to_string(),
        ))
        .await
        .expect("count query")
        .expect("row");
    row.try_get_by::<i64, _>("n").expect("decode count")
}

fn sample_session_row(session_id: &str) -> AgentSessionRow {
    AgentSessionRow {
        session_id: session_id.to_string(),
        agent_kind: "claude_code".to_string(),
        provider_session_id: format!("provider-{session_id}"),
        state: "stopped".to_string(),
        working_dir: "/tmp/agent-tombstone-guard".to_string(),
        worktree_id: None,
        parent_commit: None,
        parent_session_id: None,
        metadata_json: "{}".to_string(),
        redaction_report: "{}".to_string(),
        started_at: 1,
        last_event_at: 2,
        stopped_at: Some(3),
        schema_version: 1,
    }
}

fn sample_checkpoint_row(checkpoint_id: &str, session_id: &str) -> AgentCheckpointRow {
    AgentCheckpointRow {
        checkpoint_id: checkpoint_id.to_string(),
        session_id: session_id.to_string(),
        parent_checkpoint_id: None,
        scope: "committed".to_string(),
        parent_commit: Some(format!("{:040x}", 900)),
        tree_oid: format!("{:040x}", 901),
        metadata_blob_oid: format!("{:040x}", 902),
        traces_commit: format!("{:040x}", 903),
        tool_use_id: None,
        subagent_session_id: None,
        description: None,
        created_at: 900,
    }
}

/// The deferred contract: a real local `erase_session_local` deletes the LOCAL
/// rows but leaves the D1 mirror row intact, because Libra propagates no cloud
/// tombstone. Runs against a throwaway `repo_id` so it never touches real
/// capture data, and cleans up the D1 row before any value assertion can panic.
#[tokio::test]
#[serial(cloud_live)]
async fn cloud_tombstone_propagation_is_deferred_for_agent_capture() {
    if !live_d1_tests_enabled() {
        eprintln!(
            "skipped (set --features test-live-cloud and LIBRA_D1_ACCOUNT_ID/\
             LIBRA_D1_API_TOKEN/LIBRA_D1_DATABASE_ID)"
        );
        return;
    }

    // A real local repo, with the repo id and one agent session pinned.
    let repo = tempfile::tempdir().expect("repo tempdir");
    let init = Command::new(env!("CARGO_BIN_EXE_libra"))
        .arg("init")
        .current_dir(repo.path())
        .status()
        .expect("run libra init");
    assert!(init.success(), "libra init must succeed");

    let conn = connect_repo_db(repo.path()).await;
    let repo_id = format!("agent-tombstone-guard-{}", Uuid::new_v4());
    let session_id = format!("sess-{}", Uuid::new_v4());
    set_repo_id(&conn, &repo_id).await;
    seed_local_session(&conn, &session_id).await;
    seed_local_checkpoint(&conn, "cp-tomb", &session_id, 900).await;

    // Mirror the same session AND checkpoint to D1, exactly as `libra cloud
    // sync` does (it mirrors both tables).
    let client = d1_client_from_env();
    client
        .ensure_agent_session_table()
        .await
        .expect("ensure agent_session table on D1");
    client
        .ensure_agent_checkpoint_table()
        .await
        .expect("ensure agent_checkpoint table on D1");
    client
        .upsert_agent_session(&repo_id, &sample_session_row(&session_id))
        .await
        .expect("mirror one agent_session row to D1");
    client
        .upsert_agent_checkpoint(&repo_id, &sample_checkpoint_row("cp-tomb", &session_id))
        .await
        .expect("mirror one agent_checkpoint row to D1");
    let mirrored_sessions = client
        .list_agent_sessions(&repo_id)
        .await
        .expect("list mirrored sessions")
        .len();
    let mirrored_checkpoints = client
        .list_agent_checkpoints(&repo_id)
        .await
        .expect("list mirrored checkpoints")
        .len();

    // Real local erase of the SAME session.
    let repo_dot = repo.path().join(".libra");
    let storage = Arc::new(ClientStorage::init(repo_dot.join("objects")));
    let history =
        HistoryManager::new_with_ref(storage, repo_dot, Arc::new(conn.clone()), TRACES_BRANCH);
    let outcome = history
        .erase_session_local(&session_id)
        .await
        .expect("local erase");
    let local_remaining = count(
        &conn,
        &format!("SELECT COUNT(*) AS n FROM agent_session WHERE session_id = '{session_id}'"),
    )
    .await;

    // The deferral: BOTH D1 mirror rows are untouched by a local erase.
    let sessions_after = client
        .list_agent_sessions(&repo_id)
        .await
        .expect("re-list sessions after local erase")
        .len();
    let checkpoints_after = client
        .list_agent_checkpoints(&repo_id)
        .await
        .expect("re-list checkpoints after local erase")
        .len();

    // Cleanup runs BEFORE any value assertion, so a failing assert never leaks
    // the throwaway mirror rows.
    client
        .execute(
            "DELETE FROM agent_session WHERE repo_id = ?1",
            Some(vec![serde_json::json!(repo_id)]),
        )
        .await
        .expect("cleanup throwaway session mirror rows");
    client
        .execute(
            "DELETE FROM agent_checkpoint WHERE repo_id = ?1",
            Some(vec![serde_json::json!(repo_id)]),
        )
        .await
        .expect("cleanup throwaway checkpoint mirror rows");
    let residual_sessions = client
        .list_agent_sessions(&repo_id)
        .await
        .expect("confirm session cleanup")
        .len();
    let residual_checkpoints = client
        .list_agent_checkpoints(&repo_id)
        .await
        .expect("confirm checkpoint cleanup")
        .len();

    assert_eq!(
        mirrored_sessions, 1,
        "the session mirror write landed on D1"
    );
    assert_eq!(
        mirrored_checkpoints, 1,
        "the checkpoint mirror write landed on D1"
    );
    assert!(outcome.session_deleted, "the local session row was deleted");
    assert_eq!(
        local_remaining, 0,
        "the local session row is gone after erase"
    );
    assert_eq!(
        sessions_after, 1,
        "cloud tombstone propagation is deferred: a local erase does not delete \
         the D1 session mirror row (see agent.md 还未实现的功能; cloud restore would revive it)",
    );
    assert_eq!(
        checkpoints_after, 1,
        "cloud tombstone propagation is deferred: a local erase does not delete \
         the D1 checkpoint mirror row either",
    );
    assert_eq!(
        residual_sessions, 0,
        "cleanup removed the session mirror row"
    );
    assert_eq!(
        residual_checkpoints, 0,
        "cleanup removed the checkpoint mirror row"
    );
}
