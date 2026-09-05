//! OL-02 focused coverage for the v1 -> v2 operation schema replacement.

use std::{collections::BTreeMap, path::Path};

use libra::internal::db;
use sea_orm::{ConnectionTrait, DbBackend, Statement};
use tempfile::TempDir;

async fn table_columns(conn: &sea_orm::DatabaseConnection, table: &str) -> Vec<String> {
    let statement =
        Statement::from_string(DbBackend::Sqlite, format!("PRAGMA table_info('{table}')"));
    let rows = conn
        .query_all_raw(statement)
        .await
        .expect("table_info query succeeds");
    rows.into_iter()
        .map(|row| row.try_get_by_index::<String>(1).expect("column name"))
        .collect()
}

async fn schema_signature(conn: &sea_orm::DatabaseConnection) -> BTreeMap<String, Vec<String>> {
    let rows = conn
        .query_all_raw(Statement::from_string(
            DbBackend::Sqlite,
            "SELECT name FROM sqlite_master WHERE type = 'table' AND name IN (\
             'operation', 'operation_parent', 'operation_head', 'operation_journal',\
             'change_identity', 'change_revision', 'change_predecessor', 'ai_operation_link',\
             'legacy_operation', 'legacy_operation_parent', 'legacy_operation_view',\
             'legacy_operation_view_ref', 'legacy_operation_view_workspace')\
              ORDER BY name",
        ))
        .await
        .expect("schema table query succeeds");
    let mut signature = BTreeMap::new();
    for row in rows {
        let name: String = row.try_get_by_index(0).expect("table name");
        signature.insert(name.clone(), table_columns(conn, &name).await);
    }
    signature
}

fn db_path(dir: &TempDir, name: &str) -> std::path::PathBuf {
    dir.path().join(name)
}

async fn make_legacy_database(path: &Path) -> sea_orm::DatabaseConnection {
    let conn = db::create_database(path.to_str().expect("UTF-8 database path"))
        .await
        .expect("create baseline database");
    conn.execute_unprepared(
        "DROP TABLE legacy_operation_view_workspace;\
          DROP TABLE legacy_operation_view_ref;\
          DROP TABLE legacy_operation_view;\
          DROP TABLE legacy_operation_parent;\
          DROP TABLE legacy_operation;\
          DROP TABLE operation_journal;\
          DROP TABLE operation_head;\
          DROP TABLE change_identity;\
          DROP TABLE change_revision;\
          DROP TABLE change_predecessor;\
          DROP TABLE ai_operation_link;\
          DROP TABLE operation_parent;\
          DROP TABLE operation;\
          CREATE TABLE operation (\
              op_id TEXT PRIMARY KEY, repo_id TEXT NOT NULL, view_id TEXT NOT NULL,\
              command_name TEXT NOT NULL, description TEXT NOT NULL, actor TEXT NOT NULL,\
              args_digest TEXT, start_ts INTEGER NOT NULL, end_ts INTEGER, status TEXT NOT NULL\
          );\
          CREATE TABLE operation_parent (\
              op_id TEXT NOT NULL, parent_op_id TEXT NOT NULL, PRIMARY KEY (op_id, parent_op_id)\
          );\
          CREATE TABLE operation_view (\
              view_id TEXT PRIMARY KEY, repo_id TEXT NOT NULL, head_kind TEXT NOT NULL,\
              head_target TEXT NOT NULL, created_at INTEGER NOT NULL\
          );\
          CREATE TABLE operation_view_ref (\
              view_id TEXT NOT NULL, ref_kind TEXT NOT NULL, ref_name TEXT NOT NULL,\
              ref_remote TEXT NOT NULL, target_oid TEXT NOT NULL,\
              PRIMARY KEY (view_id, ref_kind, ref_name, ref_remote)\
          );\
          CREATE TABLE operation_view_workspace (\
              view_id TEXT NOT NULL, pointer_kind TEXT NOT NULL, pointer_value TEXT NOT NULL,\
              PRIMARY KEY (view_id, pointer_kind)\
          );\
          INSERT INTO operation (op_id, repo_id, view_id, command_name, description, actor, args_digest, start_ts, end_ts, status)\
          VALUES ('legacy-op-1', 'repo-1', 'legacy-view-1', 'status', 'legacy row', 'jackie', 'digest', 10, 11, 'succeeded');\
          INSERT INTO operation_parent (op_id, parent_op_id) VALUES ('legacy-op-1', 'legacy-parent-1');\
          INSERT INTO operation_view (view_id, repo_id, head_kind, head_target, created_at)\
          VALUES ('legacy-view-1', 'repo-1', 'branch', 'main', 10);\
          INSERT INTO operation_view_ref (view_id, ref_kind, ref_name, ref_remote, target_oid)\
          VALUES ('legacy-view-1', 'branch', 'main', '', 'deadbeef');\
          INSERT INTO operation_view_workspace (view_id, pointer_kind, pointer_value)\
          VALUES ('legacy-view-1', 'head', 'deadbeef');\
          DELETE FROM schema_versions WHERE version = 2026090101",
     )
     .await
     .expect("install legacy operation schema");
    conn
}

#[tokio::test]
async fn fresh_and_legacy_databases_converge_to_the_same_v2_schema() {
    let dir = TempDir::new().expect("temporary schema directory");
    let fresh_path = db_path(&dir, "fresh.db");
    let legacy_path = db_path(&dir, "legacy.db");

    let fresh = db::create_database(fresh_path.to_str().expect("UTF-8 path"))
        .await
        .expect("fresh database initializes");
    let fresh_signature = schema_signature(&fresh).await;
    assert!(!fresh_signature.contains_key("operation_view"));
    assert_eq!(
        fresh_signature
            .get("operation")
            .and_then(|columns| columns.first())
            .map(String::as_str),
        Some("op_id")
    );
    assert!(fresh_signature["operation"].contains(&"pre_view_oid".to_string()));
    assert!(fresh_signature.contains_key("operation_head"));
    assert!(fresh_signature.contains_key("operation_journal"));
    assert!(fresh_signature.contains_key("ai_operation_link"));
    assert_eq!(
        fresh
            .query_one_raw(Statement::from_string(
                DbBackend::Sqlite,
                "SELECT COUNT(*) FROM sqlite_master WHERE type = 'index' AND name IN (+                 'idx_legacy_operation_dedup_scope', 'idx_legacy_operation_control_slot')",
            ))
            .await
            .expect("legacy index query")
            .expect("legacy index row")
            .try_get_by_index::<i64>(0)
            .expect("legacy index count"),
        2
    );
    assert_eq!(
        fresh
            .query_one_raw(Statement::from_string(
                DbBackend::Sqlite,
                "SELECT COUNT(*) FROM sqlite_master WHERE type = 'trigger' AND name LIKE 'legacy_operation_%_domain_%'",
            ))
            .await
            .expect("legacy trigger query")
            .expect("legacy trigger row")
            .try_get_by_index::<i64>(0)
            .expect("legacy trigger count"),
        4
    );
    drop(fresh);

    let legacy = make_legacy_database(&legacy_path).await;
    let legacy_signature = {
        db::upgrade_database_schema(&legacy_path)
            .await
            .expect("legacy database migrates forward");
        let upgraded = db::establish_connection(legacy_path.to_str().expect("UTF-8 path"))
            .await
            .expect("upgraded database opens");
        let signature = schema_signature(&upgraded).await;
        assert_eq!(
            upgraded
                .query_one_raw(Statement::from_string(
                    DbBackend::Sqlite,
                    "SELECT op_id, repo_id, view_id FROM legacy_operation",
                ))
                .await
                .expect("legacy row query")
                .expect("legacy row")
                .try_get_by_index::<String>(0)
                .expect("legacy op id"),
            "legacy-op-1"
        );
        assert_eq!(
            upgraded
                .query_one_raw(Statement::from_string(
                    DbBackend::Sqlite,
                    "SELECT COUNT(*) FROM operation",
                ))
                .await
                .expect("v2 row query")
                .expect("v2 count")
                .try_get_by_index::<i64>(0)
                .expect("v2 count value"),
            0
        );
        assert_eq!(
            upgraded
                .query_one_raw(Statement::from_string(
                    DbBackend::Sqlite,
                    "SELECT restorable FROM legacy_operation WHERE op_id = 'legacy-op-1'",
                ))
                .await
                .expect("legacy restorable query")
                .expect("legacy restorable row")
                .try_get_by_index::<i64>(0)
                .expect("legacy restorable value"),
            1
        );
        upgraded
            .execute_unprepared("DELETE FROM schema_versions WHERE version = 2026090101")
            .await
            .expect("remove migration marker for idempotence check");
        drop(upgraded);
        signature
    };
    drop(legacy);

    db::upgrade_database_schema(&legacy_path)
        .await
        .expect("re-running the operation migration is idempotent");
    let reopened = db::establish_connection(legacy_path.to_str().expect("UTF-8 path"))
        .await
        .expect("idempotently upgraded database reopens");
    assert_eq!(
        reopened
            .query_one_raw(Statement::from_string(
                DbBackend::Sqlite,
                "SELECT COUNT(*) FROM legacy_operation",
            ))
            .await
            .expect("legacy count query")
            .expect("legacy count row")
            .try_get_by_index::<i64>(0)
            .expect("legacy count"),
        1
    );
    drop(reopened);

    assert_eq!(fresh_signature, legacy_signature);
}

#[tokio::test]
async fn operation_v2_migration_rolls_back_schema_and_data_on_validation_failure() {
    let dir = TempDir::new().expect("temporary schema directory");
    let path = db_path(&dir, "rollback.db");
    let conn = make_legacy_database(&path).await;
    conn.execute_unprepared("UPDATE operation SET repo_id = ''")
        .await
        .expect("inject invalid legacy key");

    let error = db::upgrade_database_schema(&path)
        .await
        .expect_err("invalid legacy data must abort migration");
    assert!(
        error
            .to_string()
            .contains("Failed to run schema migrations")
    );

    let version = conn
        .query_one_raw(Statement::from_string(
            DbBackend::Sqlite,
            "SELECT MAX(version) FROM schema_versions",
        ))
        .await
        .expect("version query")
        .expect("version row")
        .try_get_by_index::<i64>(0)
        .expect("version value");
    assert_ne!(version, 2026090101);
    assert_eq!(
        conn.query_one_raw(Statement::from_string(
            DbBackend::Sqlite,
            "SELECT COUNT(*) FROM operation",
        ))
        .await
        .expect("v1 count query")
        .expect("v1 count row")
        .try_get_by_index::<i64>(0)
        .expect("v1 count"),
        1
    );
    assert_eq!(
        conn.query_one_raw(Statement::from_string(
            DbBackend::Sqlite,
            "SELECT COUNT(*) FROM sqlite_master WHERE type = 'table' AND name = 'legacy_operation'",
        ))
        .await
        .expect("legacy table query")
        .expect("legacy table row")
        .try_get_by_index::<i64>(0)
        .expect("legacy table count"),
        0
    );
}

#[tokio::test]
async fn operation_v2_migration_is_forward_only_and_versioned() {
    let dir = TempDir::new().expect("temporary schema directory");
    let path = db_path(&dir, "version.db");
    let conn = db::create_database(path.to_str().expect("UTF-8 path"))
        .await
        .expect("create database");
    let row = conn
        .query_one_raw(Statement::from_sql_and_values(
            DbBackend::Sqlite,
            "SELECT MAX(version) FROM schema_versions",
            [],
        ))
        .await
        .expect("schema version query")
        .expect("schema version row");
    let version: i64 = row.try_get_by_index(0).expect("schema version");
    assert_eq!(version, 2026090101);
    assert!(
        table_columns(&conn, "operation_parent")
            .await
            .contains(&"ordinal".to_string())
    );
}
