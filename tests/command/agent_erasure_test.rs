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
    conn.execute(Statement::from_sql_and_values(
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
    .expect("insert agent_checkpoint");
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
    conn.execute(Statement::from_sql_and_values(
        conn.get_database_backend(),
        "INSERT INTO agent_audit_log (audit_id, timestamp, action, checkpoint_id, scope, granted) \
         VALUES ('aud-1', '2026-07-05T00:00:00Z', 'raw_export', 'cp-a', 'transcript', 1)",
        Vec::<Value>::new(),
    ))
    .await
    .expect("seed audit");

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

/// A0-10 local tombstone contract: `erase_session_local` is a self-contained
/// LOCAL tombstone. It succeeds with no D1/R2 cloud mirror configured (cloud
/// tombstone propagation is an explicit, documented deferral — see
/// `docs/development/tracing/agent.md` "还未实现的功能" row), and re-erasing a
/// session that is already gone is idempotent (the tombstone is stable — a
/// deleted session is never revived by a second erase).
#[tokio::test]
async fn agent_erasure_local_tombstone() {
    let repo = tempfile::tempdir().expect("repo tempdir");
    init_repo_via_cli(repo.path());
    let conn = connect_repo_db(repo.path()).await;

    seed_session(&conn, "sess-tomb").await;
    seed_checkpoint(&conn, "cp-t1", "sess-tomb", 200).await;
    seed_checkpoint(&conn, "cp-t2", "sess-tomb", 201).await;

    // Audit row that must outlive the tombstone.
    conn.execute(Statement::from_sql_and_values(
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
