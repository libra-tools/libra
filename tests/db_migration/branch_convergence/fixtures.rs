//! Faithful branch-history databases and loss-sensitive snapshots.

use std::{collections::BTreeMap, path::PathBuf};

use sea_orm::{ConnectionTrait, DatabaseConnection, DbBackend, Statement};
use tempfile::TempDir;

use super::super::{
    MigrationRunner, builtin_migrations, builtin_runner, connect, fresh_db_url,
    historical_bootstrap,
};

pub(super) const OPERATION_V2: i64 = 2026090101;
pub(super) const CONFIG_REPAIR: i64 = 2026090601;
pub(super) const CONVERGENCE: i64 = 2026090801;

pub(super) async fn branch_database(tip: i64) -> (TempDir, PathBuf, DatabaseConnection) {
    assert!([OPERATION_V2, CONFIG_REPAIR].contains(&tip));
    let (dir, url, path) = fresh_db_url();
    let conn = connect(&url).await;
    historical_bootstrap::initialize(&conn).await;
    builtin_runner().unwrap().run_pending(&conn).await.unwrap();
    historical_bootstrap::assert_pre_v2_history(&conn).await;
    conn.execute_unprepared(
        "INSERT INTO operation (op_id,repo_id,view_id,command_name,description,actor,args_digest,start_ts,end_ts,status,scope_kind) \
         VALUES ('old-op','repo','view','status','retained legacy row','fixture','digest',10,11,'succeeded','main'); \
         INSERT INTO operation_parent VALUES ('old-op','old-parent'); \
         INSERT INTO operation_view VALUES ('view','repo','branch','main',10); \
         INSERT INTO operation_view_ref VALUES ('view','branch','main','','deadbeef'); \
         INSERT INTO operation_view_workspace VALUES ('view','head','deadbeef'); \
         INSERT INTO config (configuration,name,key,value) VALUES ('remote','origin','url','https://example.test/origin'); \
         INSERT INTO config_kv (key,value,encrypted) VALUES ('init.defaultBranch','trunk',0);"
    ).await.unwrap();
    let mut branch = MigrationRunner::new();
    branch
        .extend(
            builtin_migrations()
                .into_iter()
                .filter(|migration| migration.version < OPERATION_V2 || migration.version == tip),
        )
        .unwrap();
    assert_eq!(branch.run_pending(&conn).await.unwrap(), vec![tip]);
    let branch_receipts = receipts(&conn).await;
    assert_eq!(branch_receipts.len(), 58, "fixture branch receipt count");
    assert_eq!(branch_receipts.last().unwrap().0, tip, "fixture branch MAX");
    conn.execute_raw(Statement::from_sql_and_values(
        DbBackend::Sqlite,
        "UPDATE schema_versions SET applied_at = '2026-09-06T04:05:06+00:00' WHERE version = ?",
        [tip.into()],
    ))
    .await
    .unwrap();
    (dir, path, conn)
}

pub(super) async fn receipts(conn: &DatabaseConnection) -> Vec<(i64, String, String)> {
    conn.query_all_raw(Statement::from_string(
        DbBackend::Sqlite,
        "SELECT version,name,applied_at FROM schema_versions ORDER BY version",
    ))
    .await
    .unwrap()
    .into_iter()
    .map(|row| {
        (
            row.try_get_by_index(0).unwrap(),
            row.try_get_by_index(1).unwrap(),
            row.try_get_by_index(2).unwrap(),
        )
    })
    .collect()
}

// Sort columns as well as rows: namespace migration may reorder columns, but
// changing any stored value or SQLite value type must fail the comparison.
pub(super) async fn rows(conn: &DatabaseConnection, table: &str) -> Vec<String> {
    let quoted = format!("\"{}\"", table.replace('"', "\"\""));
    let mut columns: Vec<String> = conn
        .query_all_raw(Statement::from_string(
            DbBackend::Sqlite,
            format!("PRAGMA table_info({quoted})"),
        ))
        .await
        .unwrap()
        .into_iter()
        .map(|row| row.try_get_by_index(1).unwrap())
        .collect();
    assert!(!columns.is_empty(), "expected table {table}");
    columns.sort();
    let columns = columns
        .iter()
        .map(|column| format!("\"{}\"", column.replace('"', "\"\"")))
        .collect::<Vec<_>>()
        .join(",");
    conn.query_all_raw(Statement::from_string(
        DbBackend::Sqlite,
        format!("SELECT json_array({columns}) AS payload FROM {quoted} ORDER BY payload"),
    ))
    .await
    .unwrap()
    .into_iter()
    .map(|row| row.try_get_by_index(0).unwrap())
    .collect()
}

pub(super) async fn operation_rows(conn: &DatabaseConnection, prefix: &str) -> Vec<Vec<String>> {
    let mut captured = Vec::new();
    for table in [
        "operation",
        "operation_parent",
        "operation_view",
        "operation_view_ref",
        "operation_view_workspace",
    ] {
        captured.push(rows(conn, &format!("{prefix}{table}")).await);
    }
    captured
}

#[derive(Debug, PartialEq, Eq)]
pub(super) struct DatabaseSnapshot {
    schema: Vec<String>,
    data: BTreeMap<String, Vec<String>>,
}

pub(super) async fn snapshot(conn: &DatabaseConnection) -> DatabaseSnapshot {
    let schema = conn
        .query_all_raw(Statement::from_string(
            DbBackend::Sqlite,
            "SELECT json_array(type,name,tbl_name,sql) FROM sqlite_master ORDER BY type,name",
        ))
        .await
        .unwrap()
        .into_iter()
        .map(|row| row.try_get_by_index(0).unwrap())
        .collect();
    let tables = conn
        .query_all_raw(Statement::from_string(
            DbBackend::Sqlite,
            "SELECT name FROM sqlite_master WHERE type='table' ORDER BY name",
        ))
        .await
        .unwrap();
    let mut data = BTreeMap::new();
    for row in tables {
        let name: String = row.try_get_by_index(0).unwrap();
        data.insert(name.clone(), rows(conn, &name).await);
    }
    DatabaseSnapshot { schema, data }
}
