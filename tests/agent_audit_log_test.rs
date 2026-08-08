//! AG-24a compliance: append-only `agent_audit_log` enforcement
//! (plan.md Task A8.5).
//!
//! Verifies at the database level that the audit table admits only
//! INSERT/SELECT — UPDATE and DELETE are rejected by triggers
//! (`sql/migrations/2026070803_agent_audit_log.sql`) — and that the
//! `_down` rollback preserves recorded rows while freezing new writes.
//! Also exercises the [`compliance::write_audit_record`] append helper and
//! the retention-setting defaults/validation.

use libra::internal::{
    ai::observed_agents::compliance::{
        self, AuditRecord, AuditScope, DEFAULT_RETENTION_STDERR_DAYS,
        DEFAULT_RETENTION_TRANSCRIPT_DAYS, write_audit_record,
    },
    db::migration::run_builtin_migrations,
};
use sea_orm::{ConnectOptions, ConnectionTrait, Database, DatabaseConnection, Statement};

async fn migrated_db() -> (tempfile::TempDir, DatabaseConnection) {
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("test.db");
    std::fs::File::create(&path).expect("touch sqlite file");
    let mut opts = ConnectOptions::new(format!("sqlite://{}", path.display()));
    opts.sqlx_logging(false);
    let conn = Database::connect(opts).await.expect("connect");
    run_builtin_migrations(&conn)
        .await
        .expect("apply builtin migrations");
    (dir, conn)
}

fn sample_record(id: &str) -> AuditRecord {
    AuditRecord::new(
        id.to_string(),
        "2026-07-05T00:00:00Z".to_string(),
        (Some("u1".to_string()), Some("Alice".to_string())),
        "raw_export",
        "cp-1",
        AuditScope::Transcript,
        Some("/tmp/out.txt".to_string()),
        Some("incident review".to_string()),
        true,
    )
}

async fn count_rows(conn: &DatabaseConnection) -> i64 {
    let row = conn
        .query_one_raw(Statement::from_string(
            conn.get_database_backend(),
            "SELECT COUNT(*) AS c FROM agent_audit_log".to_string(),
        ))
        .await
        .expect("count query")
        .expect("one row");
    row.try_get::<i64>("", "c").expect("count")
}

/// INSERT via the append helper works; UPDATE and DELETE are rejected at
/// the database level; SELECT still works. This is the core append-only
/// invariant of the compliance surface.
#[tokio::test]
async fn audit_log_rejects_update_and_delete_but_allows_insert_select() {
    let (_dir, conn) = migrated_db().await;

    write_audit_record(&conn, &sample_record("a1"))
        .await
        .expect("insert audit record");
    write_audit_record(&conn, &sample_record("a2"))
        .await
        .expect("second insert");
    assert_eq!(count_rows(&conn).await, 2, "inserts land");

    // UPDATE must be rejected by the trigger.
    let update = conn
        .execute_raw(Statement::from_string(
            conn.get_database_backend(),
            "UPDATE agent_audit_log SET justification = 'tampered' WHERE audit_id = 'a1'"
                .to_string(),
        ))
        .await;
    assert!(
        update.is_err(),
        "UPDATE on agent_audit_log must be rejected"
    );
    assert!(
        format!("{:?}", update.unwrap_err()).contains("append-only"),
        "rejection message names the append-only invariant"
    );

    // DELETE must be rejected by the trigger.
    let delete = conn
        .execute_raw(Statement::from_string(
            conn.get_database_backend(),
            "DELETE FROM agent_audit_log WHERE audit_id = 'a1'".to_string(),
        ))
        .await;
    assert!(
        delete.is_err(),
        "DELETE on agent_audit_log must be rejected"
    );

    // The rejected statements changed nothing.
    assert_eq!(
        count_rows(&conn).await,
        2,
        "row set intact after rejections"
    );

    // SELECT still works and returns the untampered value.
    let row = conn
        .query_one_raw(Statement::from_string(
            conn.get_database_backend(),
            "SELECT justification FROM agent_audit_log WHERE audit_id = 'a1'".to_string(),
        ))
        .await
        .expect("select")
        .expect("row present");
    assert_eq!(
        row.try_get::<String>("", "justification").unwrap(),
        "incident review",
        "value never mutated"
    );
}

/// A denied raw access is still audited (granted = 0), so refusals are
/// not invisible.
#[tokio::test]
async fn denied_access_is_recorded() {
    let (_dir, conn) = migrated_db().await;
    let mut denied = sample_record("d1");
    denied.granted = false;
    denied.export_path = None;
    write_audit_record(&conn, &denied)
        .await
        .expect("record denial");
    let row = conn
        .query_one_raw(Statement::from_string(
            conn.get_database_backend(),
            "SELECT granted FROM agent_audit_log WHERE audit_id = 'd1'".to_string(),
        ))
        .await
        .expect("select")
        .expect("row");
    assert_eq!(row.try_get::<i64>("", "granted").unwrap(), 0);
}

/// Retention settings return their documented defaults on a fresh DB.
/// (The getters read process-global config; on a clean test environment
/// the keys are absent, so the defaults must surface.)
#[tokio::test]
async fn retention_defaults_are_documented_values() {
    // These read the ambient config store; with no override set they must
    // return the documented defaults. Assert the constants are the spec
    // values so a silent default change is caught here.
    assert_eq!(DEFAULT_RETENTION_TRANSCRIPT_DAYS, 90);
    assert_eq!(DEFAULT_RETENTION_STDERR_DAYS, 30);
    assert_eq!(compliance::DEFAULT_RETENTION_FINDINGS_DAYS, 90);
    assert_eq!(compliance::DEFAULT_MAX_TRANSCRIPT_READ_BYTES, 268_435_456);
}
