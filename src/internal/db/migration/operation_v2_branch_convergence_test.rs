//! Adversarial receipts and transaction boundaries for the forward barrier.

use sea_orm::{ConnectionTrait, Database, DatabaseConnection, Statement, TransactionTrait};

use super::{
    MigrationRunner, OPERATION_V2_MIGRATION_VERSION, apply_operation_v2_migration,
    builtin_migrations, ensure_schema_versions_table,
    operation_v2_branch_convergence::{VERSION, v1_reference_sql},
};

async fn v1_database() -> DatabaseConnection {
    let conn = Database::connect("sqlite::memory:").await.unwrap();
    ensure_schema_versions_table(&conn).await.unwrap();
    conn.execute_unprepared(&v1_reference_sql()).await.unwrap();
    conn.execute_unprepared(
        "INSERT INTO schema_versions VALUES (2026090601,'legacy_config_table','2026-09-06T00:00:00Z');
         CREATE TABLE untouched (id TEXT PRIMARY KEY);
         INSERT INTO untouched VALUES ('keep');
         INSERT INTO operation (op_id,repo_id,view_id,command_name,description,actor,start_ts,status)
         VALUES ('op','repo','view','status','history','actor',1,'succeeded');",
    )
    .await
    .unwrap();
    conn
}

fn barrier_runner() -> MigrationRunner {
    let mut runner = MigrationRunner::new();
    runner
        .extend(
            builtin_migrations()
                .into_iter()
                .filter(|m| m.version == VERSION),
        )
        .unwrap();
    runner
}

async fn receipt(conn: &DatabaseConnection, name: &str) {
    conn.execute_raw(Statement::from_sql_and_values(
        conn.get_database_backend(),
        "INSERT INTO schema_versions VALUES (?, ?, '2026-09-01T00:00:00Z')",
        [OPERATION_V2_MIGRATION_VERSION.into(), name.into()],
    ))
    .await
    .unwrap();
}

async fn snapshot(conn: &DatabaseConnection) -> Vec<String> {
    let mut captured: Vec<String> = conn
        .query_all_raw(Statement::from_string(
            conn.get_database_backend(),
            "SELECT json_array(type,name,tbl_name,sql) FROM sqlite_master",
        ))
        .await
        .unwrap()
        .into_iter()
        .map(|row| row.try_get_by_index(0).unwrap())
        .collect();
    let tables = conn
        .query_all_raw(Statement::from_string(
            conn.get_database_backend(),
            "SELECT name FROM sqlite_master WHERE type='table'",
        ))
        .await
        .unwrap();
    for table in tables {
        let name: String = table.try_get_by_index(0).unwrap();
        let quoted = format!("\"{}\"", name.replace('"', "\"\""));
        let columns = conn
            .query_all_raw(Statement::from_string(
                conn.get_database_backend(),
                format!("PRAGMA table_info({quoted})"),
            ))
            .await
            .unwrap()
            .into_iter()
            .map(|row| {
                let name: String = row.try_get_by_index(1).unwrap();
                format!("\"{}\"", name.replace('"', "\"\""))
            })
            .collect::<Vec<_>>()
            .join(",");
        for row in conn
            .query_all_raw(Statement::from_string(
                conn.get_database_backend(),
                format!("SELECT json_array({columns}) FROM {quoted}"),
            ))
            .await
            .unwrap()
        {
            let values: String = row.try_get_by_index(0).unwrap();
            captured.push(format!("{name}:{values}"));
        }
    }
    captured.sort();
    captured
}

#[tokio::test]
async fn wrong_operation_receipt_name_refuses_without_writes() {
    let conn = v1_database().await;
    receipt(&conn, "not_operation_v2").await;
    let before = snapshot(&conn).await;
    let error = barrier_runner().run_pending(&conn).await.unwrap_err();
    assert!(error.to_string().contains("receipt named"), "{error}");
    assert_eq!(snapshot(&conn).await, before);
}

#[tokio::test]
async fn operation_receipt_with_v1_schema_refuses_without_writes() {
    let conn = v1_database().await;
    receipt(&conn, "operation_v2").await;
    let before = snapshot(&conn).await;
    barrier_runner().run_pending(&conn).await.unwrap_err();
    assert_eq!(snapshot(&conn).await, before);
}

#[tokio::test]
async fn unreceipted_mixed_namespace_refuses_without_writes() {
    let conn = v1_database().await;
    conn.execute_unprepared("CREATE TABLE legacy_operation__staging (op_id TEXT)")
        .await
        .unwrap();
    let before = snapshot(&conn).await;
    barrier_runner().run_pending(&conn).await.unwrap_err();
    assert_eq!(snapshot(&conn).await, before);
}

#[tokio::test]
async fn missing_unrelated_history_is_not_replayed() {
    let conn = v1_database().await;
    let mut runner = MigrationRunner::new();
    runner
        .register(super::Migration {
            version: 2026080101,
            name: "unrelated_missing_receipt",
            up: "INSERT INTO untouched VALUES ('must-not-run');",
            down: None,
        })
        .unwrap();
    runner
        .extend(
            builtin_migrations()
                .into_iter()
                .filter(|m| m.version == VERSION),
        )
        .unwrap();
    assert_eq!(runner.run_pending(&conn).await.unwrap(), vec![VERSION]);
    let row = conn
        .query_one_raw(Statement::from_string(
            conn.get_database_backend(),
            "SELECT (SELECT COUNT(*) FROM untouched),
         (SELECT COUNT(*) FROM schema_versions WHERE version=2026080101),
         (SELECT applied_at FROM schema_versions WHERE version=2026090601)",
        ))
        .await
        .unwrap()
        .unwrap();
    assert_eq!(row.try_get_by_index::<i64>(0).unwrap(), 1);
    assert_eq!(row.try_get_by_index::<i64>(1).unwrap(), 0);
    assert_eq!(
        row.try_get_by_index::<String>(2).unwrap(),
        "2026-09-06T00:00:00Z"
    );
}

#[tokio::test]
async fn failed_original_receipt_insert_rolls_back_completed_copy_and_barrier() {
    let conn = v1_database().await;
    conn.execute_unprepared(
        "CREATE TRIGGER reject_v2_receipt BEFORE INSERT ON schema_versions
         WHEN NEW.version=2026090101
         BEGIN SELECT RAISE(ABORT,'receipt write rejected'); END;",
    )
    .await
    .unwrap();
    let before = snapshot(&conn).await;
    let error = barrier_runner().run_pending(&conn).await.unwrap_err();
    assert!(
        error.to_string().contains("receipt write rejected"),
        "{error}"
    );
    assert_eq!(snapshot(&conn).await, before);
}

#[tokio::test]
async fn receipted_v2_with_legacy_v1_table_refuses_without_writes() {
    let conn = v1_database().await;
    let txn = conn.begin().await.unwrap();
    apply_operation_v2_migration(
        &txn,
        include_str!("../../../../sql/migrations/2026090101_operation_v2.sql"),
    )
    .await
    .unwrap();
    txn.commit().await.unwrap();
    receipt(&conn, "operation_v2").await;
    conn.execute_unprepared("CREATE TABLE operation_view (view_id TEXT PRIMARY KEY)")
        .await
        .unwrap();
    let before = snapshot(&conn).await;
    barrier_runner().run_pending(&conn).await.unwrap_err();
    assert_eq!(snapshot(&conn).await, before);
}

#[tokio::test]
async fn missing_v1_companion_table_refuses_without_writes() {
    let conn = v1_database().await;
    conn.execute_unprepared("DROP TABLE operation_view_ref")
        .await
        .unwrap();
    let before = snapshot(&conn).await;
    barrier_runner().run_pending(&conn).await.unwrap_err();
    assert_eq!(snapshot(&conn).await, before);
}

#[tokio::test]
async fn complete_v2_without_original_receipt_is_not_adopted() {
    let conn = v1_database().await;
    let txn = conn.begin().await.unwrap();
    apply_operation_v2_migration(
        &txn,
        include_str!("../../../../sql/migrations/2026090101_operation_v2.sql"),
    )
    .await
    .unwrap();
    txn.commit().await.unwrap();
    let before = snapshot(&conn).await;
    barrier_runner().run_pending(&conn).await.unwrap_err();
    assert_eq!(snapshot(&conn).await, before);
}
