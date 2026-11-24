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

#[cfg(feature = "test-live-cloud")]
#[path = "helpers/reasoning_artifact.rs"]
mod reasoning_lifecycle_fixture;

/// Mandatory when selected: missing credentials fail rather than becoming a
/// passing skip. Only synthetic ciphertext, under this test's unique repo UUID.
#[cfg(feature = "test-live-cloud")]
#[tokio::test]
#[serial(cloud_live)]
async fn reasoning_artifact_mirrors_and_erases_with_checkpoint() {
    for name in [
        "LIBRA_D1_ACCOUNT_ID",
        "LIBRA_D1_API_TOKEN",
        "LIBRA_D1_DATABASE_ID",
        "LIBRA_STORAGE_ENDPOINT",
        "LIBRA_STORAGE_BUCKET",
        "LIBRA_STORAGE_ACCESS_KEY",
        "LIBRA_STORAGE_SECRET_KEY",
    ] {
        assert!(
            env_is_present(name),
            "RG-05 live gate requires configured {name}; this is not a passing skip"
        );
    }
    let repo = tempfile::tempdir().unwrap();
    let home = tempfile::tempdir().unwrap();
    let init = Command::new(env!("CARGO_BIN_EXE_libra"))
        .arg("init")
        .current_dir(repo.path())
        .env("LIBRA_TEST", "1")
        .env("LIBRA_TEST_HOME", home.path())
        .output()
        .unwrap();
    assert!(init.status.success());
    let conn = connect_repo_db(repo.path()).await;
    // init already owns a fresh UUID. Reuse it instead of INSERTing a second
    // multi-value libra.repoid: writer and cloud readers must share one key.
    let repo_id = libra::internal::config::ConfigKv::get_with_conn(&conn, "libra.repoid")
        .await
        .unwrap()
        .map(|entry| entry.value)
        .unwrap_or_else(|| Uuid::new_v4().to_string());
    assert_eq!(Uuid::parse_str(&repo_id).unwrap().to_string(), repo_id);
    libra::internal::config::ConfigKv::set_with_conn(&conn, "libra.repoid", &repo_id, false)
        .await
        .unwrap();
    assert_eq!(
        count(
            &conn,
            "SELECT COUNT(*) AS n FROM config_kv WHERE key = 'libra.repoid'"
        )
        .await,
        1
    );
    conn.close().await.unwrap();
    let (id, sha, oid) = reasoning_lifecycle_fixture::seed(repo.path()).await;
    let client = d1_client_from_env();
    let store: Arc<dyn object_store::ObjectStore> = Arc::new(
        object_store::aws::AmazonS3Builder::new()
            .with_bucket_name(std::env::var("LIBRA_STORAGE_BUCKET").unwrap())
            .with_region(std::env::var("LIBRA_STORAGE_REGION").unwrap_or_else(|_| "auto".into()))
            .with_endpoint(std::env::var("LIBRA_STORAGE_ENDPOINT").unwrap())
            .with_access_key_id(std::env::var("LIBRA_STORAGE_ACCESS_KEY").unwrap())
            .with_secret_access_key(std::env::var("LIBRA_STORAGE_SECRET_KEY").unwrap())
            .with_virtual_hosted_style_request(false)
            .build()
            .expect("build configured R2 client"),
    );
    let remote = libra::utils::storage::remote::RemoteStorage::new_with_prefix(
        store.clone(),
        repo_id.clone(),
    );
    use futures::FutureExt;
    let result = std::panic::AssertUnwindSafe(reasoning_artifact_live_scenario(
        repo.path(),
        home.path(),
        &client,
        &remote,
        &repo_id,
        (&id, &sha, oid),
    ))
    .catch_unwind()
    .await;
    // Cleanup precedes propagating ANY scenario assertion. Delete only this
    // owned UUID namespace; physically deleting R2 here is fixture teardown,
    // never an implementation claim about production session erasure.
    let cleanup = cleanup_reasoning_lifecycle_namespace(&client, store, &repo_id).await;
    cleanup.expect("clean unique RG-05 live fixture namespace");
    result
        .expect("RG-05 live scenario panicked after namespace cleanup")
        .expect("actual CLI/D1/R2 reasoning artifact lifecycle acceptance");
}

#[cfg(feature = "test-live-cloud")]
async fn reasoning_artifact_live_scenario(
    repo: &Path,
    home: &Path,
    client: &D1Client,
    remote: &libra::utils::storage::remote::RemoteStorage,
    repo_id: &str,
    artifact: (&str, &str, git_internal::hash::ObjectHash),
) -> anyhow::Result<()> {
    let (id, sha, oid) = artifact;
    use libra::utils::storage::Storage;
    let cli = |args: &[&str]| -> anyhow::Result<std::process::Output> {
        Ok(Command::new(env!("CARGO_BIN_EXE_libra"))
            .args(args)
            .current_dir(repo)
            .env("LIBRA_TEST", "1")
            .env("LIBRA_TEST_HOME", home)
            .output()?)
    };
    let show = cli(&["--json", "agent", "checkpoint", "show", id])?;
    anyhow::ensure!(show.status.success(), "synthetic artifact show failed");
    let summary: serde_json::Value = serde_json::from_slice(&show.stdout)?;
    anyhow::ensure!(
        summary["data"]["reasoning_artifacts"][0]["availability"] == "encrypted_unavailable"
    );
    let output = repo.join("synthetic-export");
    let export = cli(&[
        "agent",
        "checkpoint",
        "export",
        id,
        "--reasoning-artifact",
        sha,
        "--allow-raw",
        "--justification",
        "synthetic cloud lifecycle acceptance",
        "--output-path",
        output
            .to_str()
            .ok_or_else(|| anyhow::anyhow!("fixture path is not text"))?,
    ])?;
    anyhow::ensure!(export.status.success(), "synthetic artifact export failed");
    anyhow::ensure!(std::fs::read(output)? == reasoning_lifecycle_fixture::SYNTHETIC_BODY);
    let synced = cli(&["--json", "cloud", "sync"])?;
    if !synced.status.success() {
        let diagnostic = reasoning_lifecycle_fixture::index_diagnostics(repo).await;
        anyhow::bail!(
            "actual cloud sync failed: {}; safe fixture index metadata: {diagnostic}",
            String::from_utf8_lossy(&synced.stderr)
        );
    }
    anyhow::ensure!(
        synced.status.success(),
        "actual cloud sync failed: {}",
        String::from_utf8_lossy(&synced.stderr)
    );
    let report: serde_json::Value = serde_json::from_slice(&synced.stdout)?;
    anyhow::ensure!(
        report["data"]["agent_capture"]["status"] == "completed",
        "actual capture generation did not complete"
    );
    let generation = client
        .get_agent_capture_generation(repo_id)
        .await
        .map_err(|_| anyhow::anyhow!("RG-05 live fixture service operation failed"))?
        .ok_or_else(|| anyhow::anyhow!("actual D1 generation is absent"))?;
    anyhow::ensure!(generation.state == "complete");
    let indexes = client
        .get_object_indexes_by_oids(repo_id, &[oid.to_string()])
        .await
        .map_err(|_| anyhow::anyhow!("RG-05 live fixture service operation failed"))?;
    anyhow::ensure!(
        indexes.len() == 1
            && indexes[0].o_type == "blob"
            && indexes[0].o_size == reasoning_lifecycle_fixture::SYNTHETIC_BODY.len() as i64
            && indexes[0].is_synced == 1
    );
    let (body, kind) = remote
        .get_typed_bounded(
            &oid,
            reasoning_lifecycle_fixture::SYNTHETIC_BODY.len() as u64,
        )
        .await
        .map_err(|_| anyhow::anyhow!("RG-05 live fixture service operation failed"))?;
    anyhow::ensure!(
        kind == git_internal::internal::object::types::ObjectType::Blob
            && body == reasoning_lifecycle_fixture::SYNTHETIC_BODY
    );
    use sha2::{Digest, Sha256};
    anyhow::ensure!(hex::encode(Sha256::digest(&body)) == sha);
    let show = cli(&["--json", "agent", "checkpoint", "show", id])?;
    let summary: serde_json::Value = serde_json::from_slice(&show.stdout)?;
    anyhow::ensure!(
        summary["data"]["reasoning_artifacts"][0]["availability"] == "encrypted_unavailable"
    );
    reasoning_lifecycle_fixture::erase(repo).await;
    for args in [
        vec![
            "--json",
            "agent",
            "checkpoint",
            "list",
            "--session",
            "sess-x",
        ],
        vec!["--json", "agent", "checkpoint", "show", id],
        vec!["--json", "agent", "graph", "sess-x"],
    ] {
        let result = cli(&args)?;
        let wire = format!(
            "{}{}",
            String::from_utf8_lossy(&result.stdout),
            String::from_utf8_lossy(&result.stderr)
        );
        anyhow::ensure!(
            !wire.contains(sha)
                && !{
                    let needle = oid.to_string();
                    wire.contains(&needle)
                }
                && !wire.contains("reasoning/encrypted/")
        );
    }
    let erased_sync = cli(&["--json", "cloud", "sync"])?;
    anyhow::ensure!(
        erased_sync.status.success(),
        "actual erase cloud sync failed: {}",
        String::from_utf8_lossy(&erased_sync.stderr)
    );
    let generation = client
        .get_agent_capture_generation(repo_id)
        .await
        .map_err(|_| anyhow::anyhow!("RG-05 live fixture service operation failed"))?
        .ok_or_else(|| anyhow::anyhow!("erasure generation is absent"))?;
    anyhow::ensure!(generation.state == "complete");
    anyhow::ensure!(
        client
            .list_agent_sessions(repo_id)
            .await
            .map_err(|_| anyhow::anyhow!("RG-05 live fixture service operation failed"))?
            .is_empty()
    );
    anyhow::ensure!(
        client
            .list_agent_checkpoints(repo_id)
            .await
            .map_err(|_| anyhow::anyhow!("RG-05 live fixture service operation failed"))?
            .is_empty()
    );
    let restored = client
        .list_agent_capture_restore_catalog_rows(repo_id, true, 100)
        .await
        .map_err(|_| anyhow::anyhow!("RG-05 live fixture service operation failed"))?;
    anyhow::ensure!(
        restored.import_tombstones.len() == 1
            && restored.import_tombstones[0].provider_session_id == "p-x"
    );
    // Production physical R2 erasure is deferred; availability never becomes
    // decrypted/plaintext and erased readers have no artifact references.
    Ok(())
}

#[cfg(feature = "test-live-cloud")]
async fn cleanup_reasoning_lifecycle_namespace(
    client: &D1Client,
    store: Arc<dyn object_store::ObjectStore>,
    repo_id: &str,
) -> anyhow::Result<()> {
    use futures::TryStreamExt;
    use object_store::ObjectStoreExt;
    anyhow::ensure!(Uuid::parse_str(repo_id)?.to_string() == repo_id);
    let tables: Vec<serde_json::Value> = client
        .query(
            "SELECT name FROM sqlite_master WHERE type = 'table' AND sql LIKE '%repo_id%'",
            None,
        )
        .await
        .map_err(|_| anyhow::anyhow!("RG-05 live fixture service operation failed"))?;
    for table in tables {
        let name = table["name"]
            .as_str()
            .ok_or_else(|| anyhow::anyhow!("fixture cleanup table is invalid"))?;
        anyhow::ensure!(name.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'_'));
        client
            .execute(
                &format!("DELETE FROM \"{name}\" WHERE repo_id = ?1"),
                Some(vec![serde_json::json!(repo_id)]),
            )
            .await
            .map_err(|_| anyhow::anyhow!("RG-05 live fixture service operation failed"))?;
    }
    let prefix = object_store::path::Path::from(format!("{repo_id}/"));
    let objects: Vec<_> = store
        .list(Some(&prefix))
        .try_collect()
        .await
        .map_err(|_| anyhow::anyhow!("RG-05 live fixture service operation failed"))?;
    for object in objects {
        anyhow::ensure!(object.location.as_ref().starts_with(&format!("{repo_id}/")));
        store
            .delete(&object.location)
            .await
            .map_err(|_| anyhow::anyhow!("RG-05 live fixture service operation failed"))?;
    }
    Ok(())
}

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

/// The capture catalog once the capture store has opened, else `libra.db`.
async fn connect_catalog_db(repo: &Path) -> DatabaseConnection {
    let capture = repo.join(".libra").join("private").join("capture.db");
    if !capture.is_file() {
        return connect_repo_db(repo).await;
    }
    let mut opts = ConnectOptions::new(format!("sqlite://{}", capture.display()));
    opts.sqlx_logging(false)
        .connect_timeout(Duration::from_secs(5));
    Database::connect(opts).await.expect("connect capture db")
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
    let storage = Arc::new(ClientStorage::try_init(repo_dot.join("objects")).expect("storage"));
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
        catalog_tombstones[0].source_fingerprint, None,
        "the remote restore catalog must not re-export a legacy source fingerprint"
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

/// Append-only repair of one authorized legacy Claude checkpoint.
///
/// The revision key binds the old checkpoint and the ordinary transcript
/// bytes. A second repair and a deleted claim do not add a row or rewrite
/// the stored object. Mirror keeps the old tree reference. Erase writes the
/// tombstone, drops the revision, and a later repair does not revive it.
#[tokio::test]
async fn claude_legacy_revision_repair_is_idempotent_and_erases_with_the_old_checkpoint() {
    let canary = "CC08-THINKING-CANARY";
    let signature = "CC08-SIG";
    let transcript = format!(
        "{{\"type\":\"assistant\",\"uuid\":\"cc08-1\",\"timestamp\":\"2026-10-19T00:00:00Z\",\"message\":{{\"role\":\"assistant\",\"content\":[{{\"type\":\"thinking\",\"thinking\":\"{canary}\",\"signature\":\"{signature}\"}}]}}}}\n"
    );
    let repo = tempfile::tempdir().expect("repo tempdir");
    let init = Command::new(env!("CARGO_BIN_EXE_libra"))
        .arg("init")
        .current_dir(repo.path())
        .env("LIBRA_TEST", "1")
        .status()
        .expect("run libra init");
    assert!(init.success(), "libra init must succeed");
    let libra_dir = repo.path().join(".libra");
    let transcript_oid =
        libra::utils::object::write_git_object(&libra_dir, "blob", transcript.as_bytes())
            .expect("write legacy transcript")
            .to_string();
    let metadata_oid = libra::utils::object::write_git_object(
        &libra_dir,
        "blob",
        br#"{"agent_kind":"claude_code"}"#,
    )
    .expect("write metadata")
    .to_string();
    let checkpoint_id = "cc08ae75-d24c-465a-b890-a9f861a50cc8";
    let session_id = "claude__cc08-legacy";
    let transcript_tree = write_tree(&libra_dir, &[("100644", "claude_code", &transcript_oid)]);
    let inner_tree = write_tree(
        &libra_dir,
        &[
            ("100644", "metadata.json", &metadata_oid),
            ("40000", "transcript", &transcript_tree),
        ],
    );
    let prefix_tree = write_tree(&libra_dir, &[("40000", &checkpoint_id[2..], &inner_tree)]);
    let checkpoint_tree = write_tree(&libra_dir, &[("40000", &checkpoint_id[..2], &prefix_tree)]);
    let root_tree = write_tree(&libra_dir, &[("40000", "checkpoint", &checkpoint_tree)]);
    let object_path = libra_dir
        .join("objects")
        .join(&transcript_oid[..2])
        .join(&transcript_oid[2..]);
    let before = std::fs::read(&object_path).expect("read stored transcript object");

    let conn = connect_repo_db(repo.path()).await;
    seed_local_session(&conn, session_id).await;
    conn.execute_raw(Statement::from_sql_and_values(
        conn.get_database_backend(),
        "INSERT INTO agent_checkpoint (
            checkpoint_id, session_id, scope, parent_commit, tree_oid,
            metadata_blob_oid, traces_commit, created_at
         ) VALUES (?, ?, 'committed', NULL, ?, ?, ?, 1783206712)",
        vec![
            Value::from(checkpoint_id),
            Value::from(session_id),
            Value::from(root_tree.clone()),
            Value::from(metadata_oid.clone()),
            Value::from(metadata_oid.clone()),
        ],
    ))
    .await
    .expect("seed legacy checkpoint");

    let repair = run_libra(repo.path(), &["agent", "doctor", "--repair"]);
    let repair_text = format!(
        "{}{}",
        String::from_utf8_lossy(&repair.stdout),
        String::from_utf8_lossy(&repair.stderr)
    );
    assert!(
        !repair_text.contains(canary) && !repair_text.contains(signature),
        "doctor repair expanded the thinking body: {repair_text}"
    );
    // The repair opened the capture store, which moved the seeded catalog
    // into `capture.db`.
    let conn = connect_catalog_db(repo.path()).await;
    assert_eq!(
        count(
            &conn,
            &format!(
                "SELECT COUNT(*) AS n FROM agent_coverage_revision WHERE checkpoint_id = '{checkpoint_id}'"
            )
        )
        .await,
        1,
        "doctor --repair should append one revision: {repair_text}"
    );
    let digest_row = conn
        .query_one_raw(Statement::from_string(
            conn.get_database_backend(),
            format!(
                "SELECT coverage_digest, completeness FROM agent_coverage_revision WHERE checkpoint_id = '{checkpoint_id}'"
            ),
        ))
        .await
        .expect("read revision")
        .expect("revision row");
    let stored_digest: String = digest_row.try_get_by("coverage_digest").expect("digest");
    let completeness: String = digest_row.try_get_by("completeness").expect("completeness");
    use sha2::Digest;
    let expected_digest = hex::encode(sha2::Sha256::digest(transcript.as_bytes()));
    assert_eq!(stored_digest, expected_digest);
    assert_eq!(completeness, "incomplete");
    assert_eq!(
        std::fs::read(&object_path).expect("reread transcript object"),
        before,
        "repair must not rewrite the stored transcript object"
    );

    let show = run_libra(
        repo.path(),
        &["--json", "agent", "checkpoint", "show", checkpoint_id],
    );
    assert!(show.status.success(), "show failed: {}", cli_text(&show));
    let show_json: serde_json::Value = serde_json::from_slice(&show.stdout).expect("show json");
    assert_eq!(show_json["ok"], serde_json::json!(true));
    assert_eq!(
        show_json["data"]["reasoning"]["availability"],
        serde_json::json!("unproven")
    );
    assert_eq!(
        show_json["data"]["reasoning"]["readable"],
        serde_json::json!([canary])
    );
    let show_text = String::from_utf8_lossy(&show.stdout);
    assert!(
        !show_text.contains("native_artifact"),
        "ordinary show minted a native artifact: {show_text}"
    );
    for args in [
        ["agent", "checkpoint", "list", "--json"],
        ["agent", "session", "list", "--json"],
        ["agent", "graph", session_id, "--json"],
    ] {
        let listed = run_libra(repo.path(), &args);
        let text = cli_text(&listed);
        assert!(
            !text.contains(canary),
            "list or graph expanded the body: {text}"
        );
    }

    let again = run_libra(repo.path(), &["agent", "doctor", "--repair"]);
    assert_eq!(
        count(
            &conn,
            &format!(
                "SELECT COUNT(*) AS n FROM agent_coverage_revision WHERE checkpoint_id = '{checkpoint_id}'"
            )
        )
        .await,
        1,
        "a second repair must be idempotent: {}",
        cli_text(&again)
    );
    assert_eq!(
        std::fs::read(&object_path).expect("object after second repair"),
        before
    );

    conn.execute_raw(Statement::from_string(
        conn.get_database_backend(),
        format!("DELETE FROM agent_coverage_claim WHERE session_id = '{session_id}'"),
    ))
    .await
    .expect("drop the claim to simulate an interrupted repair");
    let resumed = run_libra(repo.path(), &["agent", "doctor", "--repair"]);
    assert_eq!(
        count(
            &conn,
            &format!(
                "SELECT COUNT(*) AS n FROM agent_coverage_revision WHERE checkpoint_id = '{checkpoint_id}'"
            )
        )
        .await,
        1,
        "re-entry must not append another revision: {}",
        cli_text(&resumed)
    );
    assert_eq!(
        count(
            &conn,
            &format!(
                "SELECT COUNT(*) AS n FROM agent_coverage_claim WHERE session_id = '{session_id}' AND state = 'catalog_committed'"
            )
        )
        .await,
        1,
        "re-entry must restore the claim: {}",
        cli_text(&resumed)
    );

    let mirrored = libra::command::cloud::claude_legacy_reasoning_mirror_refs(&conn)
        .await
        .expect("mirror refs");
    assert_eq!(mirrored.len(), 1);
    assert_eq!(mirrored[0].checkpoint_id, checkpoint_id);
    assert_eq!(mirrored[0].session_id, session_id);
    assert_eq!(mirrored[0].content_digest, expected_digest);
    assert_eq!(mirrored[0].tree_oid, root_tree);

    conn.execute_raw(Statement::from_string(
        conn.get_database_backend(),
        format!("DELETE FROM agent_checkpoint WHERE checkpoint_id = '{checkpoint_id}'"),
    ))
    .await
    .expect("hide the old checkpoint");
    let missing = libra::command::cloud::claude_legacy_reasoning_mirror_refs(&conn)
        .await
        .expect_err("replacement without the old checkpoint must be refused");
    assert!(
        missing.contains("old checkpoint"),
        "unexpected mirror error: {missing}"
    );
    conn.execute_raw(Statement::from_sql_and_values(
        conn.get_database_backend(),
        "INSERT INTO agent_checkpoint (
            checkpoint_id, session_id, scope, parent_commit, tree_oid,
            metadata_blob_oid, traces_commit, created_at
         ) VALUES (?, ?, 'committed', NULL, ?, ?, ?, 1783206712)",
        vec![
            Value::from(checkpoint_id),
            Value::from(session_id),
            Value::from(root_tree.clone()),
            Value::from(metadata_oid.clone()),
            Value::from(metadata_oid.clone()),
        ],
    ))
    .await
    .expect("restore the old checkpoint row");

    let storage = Arc::new(ClientStorage::try_init(libra_dir.join("objects")).expect("storage"));
    let history = HistoryManager::new_with_ref(
        storage,
        libra_dir.clone(),
        Arc::new(conn.clone()),
        TRACES_BRANCH,
    );
    history
        .erase_session_local(session_id)
        .await
        .expect("erase the legacy session");
    assert_eq!(
        count(
            &conn,
            &format!("SELECT COUNT(*) AS n FROM agent_session WHERE session_id = '{session_id}'")
        )
        .await,
        0
    );
    assert_eq!(
        count(
            &conn,
            &format!(
                "SELECT COUNT(*) AS n FROM agent_checkpoint WHERE checkpoint_id = '{checkpoint_id}'"
            )
        )
        .await,
        0
    );
    assert_eq!(
        count(
            &conn,
            &format!(
                "SELECT COUNT(*) AS n FROM agent_coverage_revision WHERE checkpoint_id = '{checkpoint_id}'"
            )
        )
        .await,
        0,
        "erase must drop the replacement revision with the old checkpoint"
    );
    assert_eq!(
        count(
            &conn,
            "SELECT COUNT(*) AS n FROM agent_import_tombstone WHERE erased_session_id = 'claude__cc08-legacy'"
        )
        .await,
        1,
        "erase must keep the tombstone"
    );
    assert_eq!(
        std::fs::read(&object_path).expect("loose object remains after erase"),
        before,
        "erase must not destroy the only stored transcript object"
    );
    let revived = conn
        .execute_raw(Statement::from_sql_and_values(
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
        .await;
    let revived_error = revived
        .expect_err("tombstone must block session revival")
        .to_string();
    assert!(
        revived_error.contains("agent session is protected by an erasure tombstone"),
        "unexpected revival error: {revived_error}"
    );
    assert_eq!(
        count(
            &conn,
            &format!(
                "SELECT COUNT(*) AS n FROM agent_coverage_revision WHERE checkpoint_id = '{checkpoint_id}'"
            )
        )
        .await,
        0
    );
    let after_erase = libra::command::cloud::claude_legacy_reasoning_mirror_refs(&conn)
        .await
        .expect("mirror after erase");
    assert!(
        after_erase.is_empty(),
        "erase must not leave a publishable replacement: {after_erase:?}"
    );
}

fn run_libra(repo: &Path, args: &[&str]) -> std::process::Output {
    Command::new(env!("CARGO_BIN_EXE_libra"))
        .args(args)
        .current_dir(repo)
        .env("LIBRA_TEST", "1")
        .output()
        .expect("run libra")
}

fn cli_text(output: &std::process::Output) -> String {
    format!(
        "status {:?}\nstdout {}\nstderr {}",
        output.status,
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    )
}

fn write_tree(libra_dir: &Path, entries: &[(&str, &str, &str)]) -> String {
    let mut body = Vec::new();
    for (mode, name, oid) in entries {
        body.extend_from_slice(mode.as_bytes());
        body.push(b' ');
        body.extend_from_slice(name.as_bytes());
        body.push(0);
        body.extend_from_slice(&hex::decode(oid).expect("valid oid hex"));
    }
    libra::utils::object::write_git_object(libra_dir, "tree", &body)
        .expect("write tree object")
        .to_string()
}
