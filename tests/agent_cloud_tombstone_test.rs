//! A0-10 cloud-mirror tombstone propagation for agent capture data.
//!
//! Ground truth (verified against `src/command/cloud.rs`): the D1 agent-capture
//! MIRROR is live — `libra cloud sync` publishes fenced `agent_session` /
//! `agent_checkpoint` to D1 on every sync (`sync_agent_capture_tables`) and
//! `libra cloud restore` reads them back (`restore_agent_capture_from_d1`).
//! Ordinary checkpoint retention propagates a durable D1 prune fence, and —
//! since plan-20260714 PD-03 — SESSION erasure propagates too:
//! [`HistoryManager::erase_session_local`] writes the local
//! `agent_import_tombstone`, `libra cloud sync` publishes it to D1 under the
//! generation fence (`sync_agent_import_tombstones_batch`) and cascade-deletes
//! the erased session's mirror rows, and `libra cloud restore` is
//! tombstone-first. Only R2 physical payload deletion stays deferred.
//!
//! This test proves the propagation contract against a real D1 endpoint: it
//! mirrors a session + checkpoint to D1 (as a sync would), performs a real
//! local erase of the SAME session, publishes the resulting tombstone exactly
//! as `cloud sync` does, and asserts the D1 mirror rows are gone, the restore
//! catalog carries the tombstone, and a republish is idempotent.
//!
//! **Layer:** L3 — runs only with `--features test-live-cloud` AND `LIBRA_D1_*`
//! credentials; otherwise it prints `skipped` and returns without failing. The
//! session-row mirror is a D1 concern, so the gate is D1-only.

use std::{path::Path, process::Command, sync::Arc, time::Duration};

use libra::{
    internal::{ai::history::HistoryManager, branch::TRACES_BRANCH},
    utils::{
        client_storage::ClientStorage,
        d1_client::{
            AgentCheckpointPruneTombstoneRow, AgentCheckpointV2Row, AgentImportTombstoneRow,
            AgentSessionV2Row, D1Client,
        },
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
    conn.execute_raw(Statement::from_sql_and_values(
        conn.get_database_backend(),
        "INSERT INTO config_kv (key, value, encrypted) VALUES ('libra.repoid', ?, 0)",
        vec![Value::from(repo_id)],
    ))
    .await
    .expect("pin libra.repoid");
}

async fn seed_local_session(conn: &DatabaseConnection, session_id: &str) {
    conn.execute_raw(Statement::from_sql_and_values(
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
    conn.execute_raw(Statement::from_sql_and_values(
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
        .query_one_raw(Statement::from_string(
            conn.get_database_backend(),
            sql.to_string(),
        ))
        .await
        .expect("count query")
        .expect("row");
    row.try_get_by::<i64, _>("n").expect("decode count")
}

fn sample_session_row(session_id: &str) -> AgentSessionV2Row {
    AgentSessionV2Row {
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
        sync_revision: 1,
    }
}

fn sample_checkpoint_row(checkpoint_id: &str, session_id: &str) -> AgentCheckpointV2Row {
    AgentCheckpointV2Row {
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
        sync_revision: 1,
    }
}

/// PD-03 propagation contract: a real local `erase_session_local` writes the
/// tombstone, the sync-shaped publication removes the D1 mirror rows and
/// records the tombstone remotely (idempotently), and the restore catalog is
/// tombstone-first. Runs against a throwaway `repo_id` so it never touches
/// real capture data, and cleans up the D1 rows before any value assertion
/// can panic.
#[tokio::test]
#[serial(cloud_live)]
async fn cloud_tombstone_propagates_for_agent_capture() {
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
        .ensure_agent_capture_generation_table()
        .await
        .expect("ensure fenced capture generation table on D1");
    client
        .ensure_agent_checkpoint_prune_tombstone_table()
        .await
        .expect("ensure checkpoint prune fences on D1");
    client
        .ensure_agent_import_tombstone_table()
        .await
        .expect("ensure session erasure tombstones on D1");
    client
        .ensure_agent_subagent_content_tables()
        .await
        .expect("ensure companion tables used by prune cleanup");
    let publish_token = Uuid::new_v4().to_string();
    client
        .begin_agent_capture_generation(
            &repo_id,
            &publish_token,
            libra::utils::d1_client::AgentCaptureGenerationManifest {
                object_index_digest: "live-tombstone-fixture-no-objects",
                object_index_count: 0,
                object_index_scope: "checkpoint_projection",
                object_index_generation: 0,
                traces_head: None,
            },
        )
        .await
        .expect("begin fenced capture generation");
    client
        .sync_agent_sessions_batch(&repo_id, &publish_token, &[sample_session_row(&session_id)])
        .await
        .expect("mirror one agent_session row to D1");
    client
        .sync_agent_checkpoints_batch(
            &repo_id,
            &publish_token,
            &[sample_checkpoint_row("cp-tomb", &session_id)],
        )
        .await
        .expect("mirror one agent_checkpoint row to D1");
    client
        .complete_agent_capture_generation(&repo_id, &publish_token, 0)
        .await
        .expect("complete fenced capture generation");
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

    // PD-03: publish the local session tombstone exactly as `cloud sync`
    // does — fenced UPSERT + cascade delete of the erased session's rows.
    let tombstone_row = conn
        .query_one_raw(Statement::from_sql_and_values(
            conn.get_database_backend(),
            "SELECT agent_kind, provider_session_id, erased_session_id,
                    source_fingerprint, erased_at
             FROM agent_import_tombstone WHERE erased_session_id = ?",
            [Value::from(session_id.clone())],
        ))
        .await
        .expect("read local tombstone")
        .expect("local erase wrote the tombstone");
    let published = AgentImportTombstoneRow {
        agent_kind: tombstone_row.try_get_by("agent_kind").expect("agent_kind"),
        provider_session_id: tombstone_row
            .try_get_by("provider_session_id")
            .expect("provider_session_id"),
        erased_session_id: tombstone_row
            .try_get_by("erased_session_id")
            .expect("erased_session_id"),
        source_fingerprint: tombstone_row
            .try_get_by("source_fingerprint")
            .expect("source_fingerprint"),
        erased_at: tombstone_row.try_get_by("erased_at").expect("erased_at"),
    };
    let tombstone_token = Uuid::new_v4().to_string();
    client
        .begin_agent_capture_generation(
            &repo_id,
            &tombstone_token,
            libra::utils::d1_client::AgentCaptureGenerationManifest {
                object_index_digest: "live-tombstone-propagation-no-objects",
                object_index_count: 0,
                object_index_scope: "checkpoint_projection",
                object_index_generation: 0,
                traces_head: None,
            },
        )
        .await
        .expect("begin tombstone publication generation");
    client
        .sync_agent_import_tombstones_batch(
            &repo_id,
            &tombstone_token,
            std::slice::from_ref(&published),
        )
        .await
        .expect("publish session tombstone to D1");
    // Idempotent republish inside the same generation.
    client
        .sync_agent_import_tombstones_batch(
            &repo_id,
            &tombstone_token,
            std::slice::from_ref(&published),
        )
        .await
        .expect("republish session tombstone (idempotent)");
    client
        .complete_agent_capture_generation(&repo_id, &tombstone_token, 0)
        .await
        .expect("complete tombstone publication generation");
    let sessions_after_propagation = client
        .list_agent_sessions(&repo_id)
        .await
        .expect("list sessions after tombstone publication")
        .len();
    let checkpoints_after_propagation = client
        .list_agent_checkpoints(&repo_id)
        .await
        .expect("list checkpoints after tombstone publication")
        .len();
    let restore_catalog = client
        .list_agent_capture_restore_catalog_rows(&repo_id, false, 10_000)
        .await
        .expect("read restore catalog after tombstone publication");
    let catalog_tombstones = restore_catalog.import_tombstones.clone();
    let catalog_sessions = restore_catalog.sessions.len();

    // Ordinary retention is intentionally different from session erasure:
    // its durable D1 fence removes the checkpoint and rejects a stale clone.
    let prune_token = Uuid::new_v4().to_string();
    client
        .begin_agent_capture_generation(
            &repo_id,
            &prune_token,
            libra::utils::d1_client::AgentCaptureGenerationManifest {
                object_index_digest: "live-prune-fixture-no-objects",
                object_index_count: 0,
                object_index_scope: "checkpoint_projection",
                object_index_generation: 0,
                traces_head: None,
            },
        )
        .await
        .expect("begin ordinary prune generation");
    client
        .sync_agent_checkpoint_prune_tombstones_batch(
            &repo_id,
            &prune_token,
            &[AgentCheckpointPruneTombstoneRow {
                checkpoint_id: "cp-tomb".to_string(),
                session_id: session_id.clone(),
                pruned_at: 4,
            }],
        )
        .await
        .expect("publish ordinary checkpoint prune fence");
    client
        .complete_agent_capture_generation(&repo_id, &prune_token, 0)
        .await
        .expect("complete ordinary prune generation");
    let checkpoints_after_ordinary_prune = client
        .list_agent_checkpoints(&repo_id)
        .await
        .expect("list checkpoints after ordinary prune")
        .len();
    let stale_token = Uuid::new_v4().to_string();
    client
        .begin_agent_capture_generation(
            &repo_id,
            &stale_token,
            libra::utils::d1_client::AgentCaptureGenerationManifest {
                object_index_digest: "live-stale-fixture-no-objects",
                object_index_count: 0,
                object_index_scope: "checkpoint_projection",
                object_index_generation: 0,
                traces_head: None,
            },
        )
        .await
        .expect("begin stale-clone generation");
    let stale_reinsert = client
        .sync_agent_checkpoints_batch(
            &repo_id,
            &stale_token,
            &[sample_checkpoint_row("cp-tomb", &session_id)],
        )
        .await;

    // Cleanup runs BEFORE any value assertion, so a failing assert never leaks
    // the throwaway mirror rows.
    client
        .execute(
            "DELETE FROM agent_capture_checkpoint_v2 WHERE repo_id = ?1",
            Some(vec![serde_json::json!(repo_id)]),
        )
        .await
        .expect("cleanup throwaway checkpoint mirror rows");
    client
        .execute(
            "DELETE FROM agent_capture_session_v2 WHERE repo_id = ?1",
            Some(vec![serde_json::json!(repo_id)]),
        )
        .await
        .expect("cleanup throwaway session mirror rows");
    client
        .execute(
            "DELETE FROM agent_checkpoint_prune_tombstone WHERE repo_id = ?1",
            Some(vec![serde_json::json!(repo_id)]),
        )
        .await
        .expect("cleanup throwaway checkpoint prune fences");
    client
        .execute(
            "DELETE FROM agent_import_tombstone WHERE repo_id = ?1",
            Some(vec![serde_json::json!(repo_id)]),
        )
        .await
        .expect("cleanup throwaway session tombstones");
    client
        .execute(
            "DELETE FROM agent_capture_generation WHERE repo_id = ?1",
            Some(vec![serde_json::json!(repo_id)]),
        )
        .await
        .expect("cleanup throwaway capture generation");
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
        "a local erase alone does not touch D1 (propagation happens on the \
         next cloud sync)",
    );
    assert_eq!(
        checkpoints_after, 1,
        "a local erase alone does not touch the D1 checkpoint mirror either",
    );
    assert_eq!(
        sessions_after_propagation, 0,
        "PD-03: publishing the tombstone cascade-deletes the session mirror row"
    );
    assert_eq!(
        checkpoints_after_propagation, 0,
        "PD-03: publishing the tombstone cascade-deletes the checkpoint mirror row"
    );
    assert_eq!(
        catalog_tombstones.len(),
        1,
        "the restore catalog carries the session tombstone (tombstone-first restore)"
    );
    assert_eq!(
        catalog_tombstones[0].erased_session_id, session_id,
        "the catalog tombstone names the erased session"
    );
    assert_eq!(
        catalog_tombstones[0].erased_at, published.erased_at,
        "the idempotent republish kept the original erased_at"
    );
    assert_eq!(
        catalog_sessions, 0,
        "no erased session survives into the restore catalog"
    );
    assert_eq!(
        checkpoints_after_ordinary_prune, 0,
        "ordinary checkpoint retention propagates its D1 deletion fence"
    );
    assert!(
        stale_reinsert
            .expect_err("stale clone must not reinsert a pruned checkpoint")
            .message
            .contains("fenced"),
        "stale checkpoint write should report its prune fence"
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
