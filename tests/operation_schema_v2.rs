//! OL-02 focused coverage for the v1 -> v2 operation schema replacement.

use std::{collections::BTreeMap, path::Path};

use libra::internal::db::{
    self,
    migration::{MigrationRunner, builtin_migrations},
};
use sea_orm::{
    ConnectOptions, ConnectionTrait, Database, DatabaseConnection, DbBackend, Statement,
};
use tempfile::TempDir;

#[path = "db_migration/branch_convergence/historical_bootstrap.rs"]
mod historical_bootstrap;

async fn table_columns(conn: &DatabaseConnection, table: &str) -> Vec<String> {
    values(
        conn,
        &format!("SELECT name FROM pragma_table_info('{table}') ORDER BY cid"),
    )
    .await
}

async fn schema_signature(conn: &DatabaseConnection) -> BTreeMap<String, Vec<String>> {
    let names = values::<String>(
        conn,
        "SELECT name FROM sqlite_master WHERE type = 'table' AND name IN (\
             'operation', 'operation_parent', 'operation_view', 'operation_view_ref',\
             'operation_view_workspace', 'operation_head', 'operation_journal',\
             'change_identity', 'change_revision', 'change_predecessor', 'ai_operation_link',\
             'legacy_operation', 'legacy_operation_parent', 'legacy_operation_view',\
             'legacy_operation_view_ref', 'legacy_operation_view_workspace')\
              ORDER BY name",
    )
    .await;
    let mut signature = BTreeMap::new();
    for name in names {
        signature.insert(name.clone(), table_columns(conn, &name).await);
    }
    signature
}

async fn values<T: sea_orm::TryGetable>(conn: &DatabaseConnection, sql: &str) -> Vec<T> {
    conn.query_all_raw(Statement::from_string(DbBackend::Sqlite, sql))
        .await
        .expect("fixture query succeeds")
        .into_iter()
        .map(|row| row.try_get_by_index(0).expect("fixture value"))
        .collect()
}

async fn receipts(conn: &DatabaseConnection) -> BTreeMap<i64, (String, String)> {
    conn.query_all_raw(Statement::from_string(
        DbBackend::Sqlite,
        "SELECT version, name, applied_at FROM schema_versions ORDER BY version",
    ))
    .await
    .expect("migration receipts query")
    .into_iter()
    .map(|row| {
        let version = row.try_get_by_index(0).expect("receipt version");
        let name = row.try_get_by_index(1).expect("receipt name");
        let applied_at = row.try_get_by_index(2).expect("receipt timestamp");
        (version, (name, applied_at))
    })
    .collect()
}

async fn operation_rows(conn: &DatabaseConnection, prefix: &str) -> BTreeMap<String, Vec<String>> {
    let mut rows = BTreeMap::new();
    for suffix in ["", "_parent", "_view", "_view_ref", "_view_workspace"] {
        let table = format!("{prefix}operation{suffix}");
        let projection = table_columns(conn, &table)
            .await
            .iter()
            .map(|column| format!("quote(\"{column}\")"))
            .collect::<Vec<_>>()
            .join(" || ',' || ");
        let sql = format!("SELECT {projection} FROM {table} ORDER BY {projection}");
        rows.insert(suffix.to_string(), values(conn, &sql).await);
    }
    rows
}

async fn make_legacy_database(path: &Path) -> DatabaseConnection {
    std::fs::File::create(path).expect("create historical database file");
    let mut options = ConnectOptions::new(format!("sqlite://{}", path.display()));
    options.sqlx_logging(false);
    let conn = Database::connect(options)
        .await
        .expect("connect historical database");
    historical_bootstrap::initialize(&conn).await;
    let mut runner = MigrationRunner::new();
    runner
        .extend(
            builtin_migrations()
                .into_iter()
                .filter(|migration| migration.version < 2026090101),
        )
        .expect("register pre-v2 migrations");
    runner
        .run_pending(&conn)
        .await
        .expect("apply pre-v2 migrations");
    historical_bootstrap::assert_pre_v2_history(&conn).await;
    conn.execute_unprepared(
        "INSERT INTO operation (op_id, repo_id, view_id, command_name, description, actor, args_digest, start_ts, end_ts, status,\
          worktree_id, scope_provenance, restorable, control_slot, claim_owner, scope_kind)\
          VALUES ('legacy-op-1', 'repo-1', 'legacy-view-1', 'status', 'legacy row', 'jackie', 'digest', 10, 11, 'succeeded',\
          'worktree-1', 'declared', 1, 'control-1', 'owner-1', 'linked');\
          INSERT INTO operation_parent (op_id, parent_op_id) VALUES ('legacy-op-1', 'legacy-parent-1');\
          INSERT INTO operation_view (view_id, repo_id, head_kind, head_target, created_at)\
          VALUES ('legacy-view-1', 'repo-1', 'branch', 'main', 10);\
          INSERT INTO operation_view_ref (view_id, ref_kind, ref_name, ref_remote, target_oid)\
          VALUES ('legacy-view-1', 'branch', 'main', '', 'deadbeef');\
          INSERT INTO operation_view_workspace (view_id, pointer_kind, pointer_value)\
          VALUES ('legacy-view-1', 'head', 'deadbeef');",
     )
     .await
     .expect("populate controlled legacy operation rows");
    conn
}

#[tokio::test]
async fn fresh_and_legacy_databases_converge_to_the_same_v2_schema() {
    // Given fresh and faithfully migrated pre-v2 databases with controlled rows.
    let dir = TempDir::new().expect("temporary schema directory");
    let legacy_path = dir.path().join("legacy.db");
    let fresh = db::create_database(dir.path().join("fresh.db").to_str().expect("UTF-8 path"))
        .await
        .expect("fresh database initializes");
    let fresh_signature = schema_signature(&fresh).await;
    assert!(!fresh_signature.contains_key("operation_view"));
    assert_eq!(fresh_signature["operation"][0], "op_id");
    assert!(fresh_signature["operation"].contains(&"pre_view_oid".to_string()));
    assert!(fresh_signature.contains_key("operation_head"));
    assert!(fresh_signature.contains_key("operation_journal"));
    assert!(fresh_signature.contains_key("ai_operation_link"));
    assert_eq!(
        values::<i64>(
            &fresh,
            "SELECT COUNT(*) FROM sqlite_master WHERE type = 'index' AND name IN (\
            'idx_legacy_operation_dedup_scope', 'idx_legacy_operation_control_slot')"
        )
        .await,
        [2]
    );
    assert_eq!(
        values::<i64>(&fresh, "SELECT COUNT(*) FROM sqlite_master WHERE type = 'trigger' AND name LIKE 'legacy_operation_%_domain_%'").await,
        [4]
    );
    let legacy = make_legacy_database(&legacy_path).await;
    let original_rows = operation_rows(&legacy, "").await;
    let original_receipts = receipts(&legacy).await;
    assert!(
        original_receipts
            .keys()
            .all(|version| *version < 2026090101)
    );

    // When the public upgrade entry point advances the true historical fixture.
    let report = db::upgrade_database_schema(&legacy_path)
        .await
        .expect("legacy database migrates forward");
    let expected_versions: Vec<_> = builtin_migrations()
        .into_iter()
        .filter(|migration| migration.version >= 2026090101)
        .map(|migration| migration.version)
        .collect();
    assert_eq!(report.applied_versions, expected_versions);
    let upgraded_receipts = receipts(&legacy).await;
    assert_eq!(upgraded_receipts[&2026090101].0, "operation_v2");
    for (version, receipt) in original_receipts {
        assert_eq!(upgraded_receipts.get(&version), Some(&receipt));
    }
    // Then the canonical schema and every legacy column survive convergence.
    assert_eq!(schema_signature(&legacy).await, fresh_signature);
    assert_eq!(operation_rows(&legacy, "legacy_").await, original_rows);
    assert_eq!(
        values::<i64>(&legacy, "SELECT COUNT(*) FROM operation").await,
        [0]
    );

    let repeated = db::upgrade_database_schema(&legacy_path)
        .await
        .expect("repeated upgrade succeeds without deleting receipts");
    assert!(repeated.applied_versions.is_empty());
    assert_eq!(repeated.previous_version, report.current_version);
    assert_eq!(repeated.current_version, report.current_version);
    let reopened = db::establish_connection(legacy_path.to_str().expect("UTF-8 path"))
        .await
        .expect("upgraded database reopens");
    assert_eq!(receipts(&reopened).await, upgraded_receipts);
    assert_eq!(operation_rows(&reopened, "legacy_").await, original_rows);
    assert_eq!(schema_signature(&reopened).await, fresh_signature);
}

#[tokio::test]
async fn operation_v2_migration_rolls_back_schema_and_data_on_validation_failure() {
    // Given a true pre-v2 schema with one invalid legacy key.
    let dir = TempDir::new().expect("temporary schema directory");
    let path = dir.path().join("rollback.db");
    let conn = make_legacy_database(&path).await;
    conn.execute_unprepared("UPDATE operation SET repo_id = ''")
        .await
        .expect("inject invalid legacy key");
    let original_rows = operation_rows(&conn, "").await;
    let original_receipts = receipts(&conn).await;
    let original_signature = schema_signature(&conn).await;
    let schema_sql = "SELECT type || ':' || name || ':' || COALESCE(sql, '') \
        FROM sqlite_master WHERE tbl_name GLOB 'operation*' OR tbl_name GLOB 'legacy_operation*' \
        ORDER BY type, name";
    let original_schema = values::<String>(&conn, schema_sql).await;

    // When validation rejects the attempted upgrade.
    let error = db::upgrade_database_schema(&path)
        .await
        .expect_err("invalid legacy data must abort migration");
    assert!(
        error
            .to_string()
            .contains("Failed to run schema migrations")
    );

    // Then no new receipt, copied table, or mutation to existing rows survives.
    let failed_receipts = receipts(&conn).await;
    assert!(!failed_receipts.contains_key(&2026090101));
    assert_eq!(failed_receipts, original_receipts);
    assert_eq!(schema_signature(&conn).await, original_signature);
    assert_eq!(values::<String>(&conn, schema_sql).await, original_schema);
    assert_eq!(operation_rows(&conn, "").await, original_rows);
    assert!(
        !schema_signature(&conn)
            .await
            .contains_key("legacy_operation")
    );
}

#[tokio::test]
async fn operation_v2_migration_is_forward_only_and_versioned() {
    // Given the current complete registry, including the original v2 migration.
    let dir = TempDir::new().expect("temporary schema directory");
    let path = dir.path().join("version.db");
    let migrations = builtin_migrations();
    let v2 = migrations
        .iter()
        .find(|migration| migration.version == 2026090101)
        .expect("original operation-v2 migration remains registered");
    assert!(v2.down.is_none());

    // When a fresh database is initialized by the public entry point.
    let conn = db::create_database(path.to_str().expect("UTF-8 path"))
        .await
        .expect("create database");
    // Then the tip follows the registry and the original v2 receipt is present.
    let applied = receipts(&conn).await;
    assert_eq!(applied[&2026090101].0, v2.name);
    assert_eq!(
        applied.last_key_value().map(|(version, _)| *version),
        migrations.last().map(|migration| migration.version)
    );
    assert_eq!(
        applied.keys().copied().collect::<Vec<_>>(),
        migrations
            .iter()
            .map(|migration| migration.version)
            .collect::<Vec<_>>()
    );
    assert!(
        table_columns(&conn, "operation_parent")
            .await
            .contains(&"ordinal".to_string())
    );
}
