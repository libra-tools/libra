//! AG-24a local session erasure (plan.md Task A8.5):
//! [`HistoryManager::erase_session_local`] makes the three local faces
//! consistent — `refs/libra/traces` rewrite + `agent_checkpoint` /
//! `agent_session` row deletion + `object_index` cleanup — and never
//! touches the append-only `agent_audit_log`.

use std::{path::Path, sync::Arc, time::Duration};

use libra::{
    internal::{ai::history::HistoryManager, branch::TRACES_BRANCH},
    utils::client_storage::ClientStorage,
};
use sea_orm::{ConnectOptions, ConnectionTrait, Database, DatabaseConnection, Statement, Value};

use super::init_repo_via_cli;

async fn connect_repo_db(repo: &Path) -> DatabaseConnection {
    let db_path = repo.join(".libra").join("libra.db");
    let mut opts = ConnectOptions::new(format!("sqlite://{}", db_path.display()));
    opts.sqlx_logging(false)
        .connect_timeout(Duration::from_secs(5));
    Database::connect(opts).await.expect("connect repo db")
}

async fn seed_session(conn: &DatabaseConnection, session_id: &str) {
    conn.execute_raw(Statement::from_sql_and_values(
        conn.get_database_backend(),
        "INSERT INTO agent_session (
            session_id, agent_kind, provider_session_id, state, working_dir,
            metadata_json, redaction_report, started_at, last_event_at, stopped_at
         ) VALUES (?, 'claude_code', ?, 'stopped', '/tmp/x', '{}', '{}', 1, 2, 3)",
        vec![
            Value::from(session_id),
            Value::from(format!("provider-{session_id}")),
        ],
    ))
    .await
    .expect("insert agent_session");
}

/// DB-only checkpoint (empty traces ref) — erasure removes the catalog
/// row via the prune engine's no-ref-rewrite path.
async fn seed_checkpoint(conn: &DatabaseConnection, id: &str, session: &str, created_at: i64) {
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
    .expect("insert agent_checkpoint");
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

#[tokio::test]
async fn erase_legacy_generation_zero_session_advances_to_fresh_incarnation() {
    let repo = tempfile::tempdir().expect("repo tempdir");
    init_repo_via_cli(repo.path());
    let conn = connect_repo_db(repo.path()).await;
    seed_session(&conn, "sess-legacy-generation").await;
    conn.execute_raw(Statement::from_string(
        conn.get_database_backend(),
        "UPDATE agent_session SET sync_revision = 0
         WHERE session_id = 'sess-legacy-generation'"
            .to_string(),
    ))
    .await
    .expect("model a session restored from the legacy cloud catalog");

    let libra_dir = repo.path().join(".libra");
    let history = HistoryManager::new_with_ref(
        Arc::new(ClientStorage::init(libra_dir.join("objects"))),
        libra_dir,
        Arc::new(conn.clone()),
        TRACES_BRANCH,
    );
    let outcome = history
        .erase_session_local("sess-legacy-generation")
        .await
        .expect("erase generation-zero legacy session");
    assert!(outcome.session_deleted);
    assert_eq!(
        count(
            &conn,
            "SELECT next_session_sync_revision AS n
             FROM agent_capture_incarnation
             WHERE agent_kind = 'claude_code'
               AND provider_session_id = 'provider-sess-legacy-generation'"
        )
        .await,
        2,
        "legacy generation zero must skip to the first post-migration incarnation"
    );
}

#[tokio::test]
async fn erase_session_local_removes_rows_and_preserves_audit_log() {
    let repo = tempfile::tempdir().expect("repo tempdir");
    init_repo_via_cli(repo.path());
    let conn = connect_repo_db(repo.path()).await;

    seed_session(&conn, "sess-erase").await;
    seed_session(&conn, "sess-keep").await;
    seed_checkpoint(&conn, "cp-a", "sess-erase", 100).await;
    seed_checkpoint(&conn, "cp-b", "sess-erase", 101).await;
    seed_checkpoint(&conn, "cp-keep", "sess-keep", 102).await;

    // An audit row that must outlive erasure.
    conn.execute_raw(Statement::from_sql_and_values(
        conn.get_database_backend(),
        "INSERT INTO agent_audit_log (audit_id, timestamp, action, checkpoint_id, scope, granted) \
         VALUES ('aud-1', '2026-07-05T00:00:00Z', 'raw_export', 'cp-a', 'transcript', 1)",
        Vec::<Value>::new(),
    ))
    .await
    .expect("seed audit");
    for session_id in ["sess-erase", "sess-keep"] {
        conn.execute_raw(Statement::from_sql_and_values(
            conn.get_database_backend(),
            "INSERT INTO metadata_kv (
                scope, target, key, value, value_type, created_at, updated_at
             ) VALUES ('agent_import_index_repair', ?, 'object-index-v1', '{}',
                       'text', '2026-07-05T00:00:00Z', '2026-07-05T00:00:00Z')",
            [session_id.into()],
        ))
        .await
        .expect("seed import object-index repair marker");
    }

    let repo_path = repo.path().join(".libra");
    let storage = Arc::new(ClientStorage::init(repo_path.join("objects")));
    let history =
        HistoryManager::new_with_ref(storage, repo_path, Arc::new(conn.clone()), TRACES_BRANCH);

    let outcome = history
        .erase_session_local("sess-erase")
        .await
        .expect("erase session");
    assert!(outcome.session_deleted, "the session row was deleted");
    assert_eq!(outcome.removed_checkpoints, 2, "both checkpoints removed");

    // Face 1 + 2: erased session and its checkpoints are gone; the other
    // session and its checkpoint survive.
    assert_eq!(
        count(
            &conn,
            "SELECT COUNT(*) AS n FROM agent_session WHERE session_id = 'sess-erase'"
        )
        .await,
        0,
        "erased agent_session row gone"
    );
    assert_eq!(
        count(
            &conn,
            "SELECT COUNT(*) AS n FROM agent_checkpoint WHERE session_id = 'sess-erase'"
        )
        .await,
        0,
        "erased session's checkpoints gone"
    );
    assert_eq!(
        count(
            &conn,
            "SELECT COUNT(*) AS n FROM agent_import_tombstone \
             WHERE erased_session_id = 'sess-erase' \
               AND provider_session_id = 'provider-sess-erase'"
        )
        .await,
        1,
        "erasure must retain the local anti-resurrection identity"
    );
    assert_eq!(
        count(
            &conn,
            "SELECT COUNT(*) AS n FROM agent_session WHERE session_id = 'sess-keep'"
        )
        .await,
        1,
        "other session untouched"
    );
    assert_eq!(
        count(
            &conn,
            "SELECT COUNT(*) AS n FROM agent_checkpoint WHERE checkpoint_id = 'cp-keep'"
        )
        .await,
        1,
        "other session's checkpoint untouched"
    );
    assert_eq!(
        count(
            &conn,
            "SELECT COUNT(*) AS n FROM metadata_kv
             WHERE scope = 'agent_import_index_repair' AND target = 'sess-erase'"
        )
        .await,
        0,
        "erasure must remove the obsolete replay-repair marker"
    );
    assert_eq!(
        count(
            &conn,
            "SELECT COUNT(*) AS n FROM metadata_kv
             WHERE scope = 'agent_import_index_repair' AND target = 'sess-keep'"
        )
        .await,
        1,
        "erasure must preserve other sessions' repair markers"
    );

    // Face 3 (audit): the append-only log is never touched by erasure.
    assert_eq!(
        count(
            &conn,
            "SELECT COUNT(*) AS n FROM agent_audit_log WHERE audit_id = 'aud-1'"
        )
        .await,
        1,
        "erasure must never delete audit rows"
    );
}

/// A0-10 local tombstone contract, upgraded for PD-03 propagation: the
/// tombstone `erase_session_local` writes locally is exactly the row
/// `libra cloud sync` later publishes to D1 (`agent_kind` +
/// `provider_session_id` identity, `erased_session_id`, an `erased_at`
/// timestamp), so this test pins those propagation-carrying fields. The
/// erase itself still succeeds with no D1/R2 mirror configured, and
/// re-erasing an already-gone session is idempotent (a deleted session
/// is never revived by a second erase).
#[tokio::test]
async fn agent_erasure_local_tombstone() {
    let repo = tempfile::tempdir().expect("repo tempdir");
    init_repo_via_cli(repo.path());
    let conn = connect_repo_db(repo.path()).await;

    seed_session(&conn, "sess-tomb").await;
    seed_checkpoint(&conn, "cp-t1", "sess-tomb", 200).await;
    seed_checkpoint(&conn, "cp-t2", "sess-tomb", 201).await;

    // Audit row that must outlive the tombstone.
    conn.execute_raw(Statement::from_sql_and_values(
        conn.get_database_backend(),
        "INSERT INTO agent_audit_log (audit_id, timestamp, action, checkpoint_id, scope, granted) \
         VALUES ('aud-tomb', '2026-07-09T00:00:00Z', 'raw_export', 'cp-t1', 'transcript', 1)",
        Vec::<Value>::new(),
    ))
    .await
    .expect("seed audit");

    let repo_path = repo.path().join(".libra");
    let storage = Arc::new(ClientStorage::init(repo_path.join("objects")));
    let history =
        HistoryManager::new_with_ref(storage, repo_path, Arc::new(conn.clone()), TRACES_BRANCH);

    // First erase: the session is tombstoned locally with NO cloud mirror in
    // scope — proving the local tombstone does not depend on D1/R2.
    let first = history
        .erase_session_local("sess-tomb")
        .await
        .expect("first erase (local-only, no cloud mirror)");
    assert!(first.session_deleted, "session tombstoned on first erase");
    assert_eq!(first.removed_checkpoints, 2, "both checkpoints removed");

    assert_eq!(
        count(
            &conn,
            "SELECT COUNT(*) AS n FROM agent_session WHERE session_id = 'sess-tomb'"
        )
        .await,
        0,
        "tombstoned session row gone"
    );
    assert_eq!(
        count(
            &conn,
            "SELECT COUNT(*) AS n FROM agent_checkpoint WHERE session_id = 'sess-tomb'"
        )
        .await,
        0,
        "tombstoned session's checkpoints gone"
    );
    assert_eq!(
        count(
            &conn,
            "SELECT COUNT(*) AS n FROM agent_import_tombstone \
             WHERE erased_session_id = 'sess-tomb'"
        )
        .await,
        1,
        "local import tombstone must survive deletion of agent_session"
    );
    // PD-03: the tombstone row carries everything the cloud publication
    // needs — the provider identity key and a positive erased_at.
    assert_eq!(
        count(
            &conn,
            "SELECT COUNT(*) AS n FROM agent_import_tombstone \
             WHERE erased_session_id = 'sess-tomb' \
               AND agent_kind = 'claude_code' \
               AND provider_session_id = 'provider-sess-tomb' \
               AND erased_at > 0"
        )
        .await,
        1,
        "the tombstone carries the propagation identity and timestamp"
    );
    assert_eq!(
        count(
            &conn,
            "SELECT COUNT(*) AS n FROM agent_capture_incarnation
             WHERE agent_kind = 'claude_code'
               AND provider_session_id = 'provider-sess-tomb'
               AND next_session_sync_revision = 2
               AND length(source_namespace) = 32"
        )
        .await,
        1,
        "local erasure must preserve the next cloud replication incarnation"
    );
    assert_eq!(
        count(
            &conn,
            "SELECT COUNT(*) AS n FROM agent_checkpoint_prune_tombstone
             WHERE session_id = 'sess-tomb'"
        )
        .await,
        0,
        "session erasure must not opt into deferred cross-device checkpoint deletion"
    );

    // Idempotency: re-erasing a session that is already gone is a no-op —
    // the tombstone is stable and never revives the deleted rows.
    let second = history
        .erase_session_local("sess-tomb")
        .await
        .expect("re-erase is idempotent");
    assert!(
        !second.session_deleted,
        "re-erase must not report a fresh deletion"
    );
    assert_eq!(
        second.removed_checkpoints, 0,
        "re-erase removes nothing (tombstone is stable)"
    );
    assert_eq!(
        count(
            &conn,
            "SELECT COUNT(*) AS n FROM agent_import_tombstone \
             WHERE erased_session_id = 'sess-tomb'"
        )
        .await,
        1,
        "idempotent re-erase must preserve the existing tombstone"
    );

    // The append-only audit log survives every erase.
    assert_eq!(
        count(
            &conn,
            "SELECT COUNT(*) AS n FROM agent_audit_log WHERE audit_id = 'aud-tomb'"
        )
        .await,
        1,
        "audit row survives the local tombstone and its idempotent re-run"
    );
}
