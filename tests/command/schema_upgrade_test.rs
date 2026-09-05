//! CLI coverage for repository database schema upgrades.

use std::{path::Path, time::Duration};

use libra::internal::db::migration::{
    MigrationError, MigrationRunner, builtin_migrations, builtin_runner as current_builtin_runner,
};
use sea_orm::{ConnectOptions, ConnectionTrait, Database, DatabaseConnection, Statement};
use tempfile::tempdir;

use super::{assert_cli_success, init_repo_via_cli, run_libra_command};

async fn connect_raw_repo_db(repo: &Path) -> DatabaseConnection {
    let db_path = repo.join(".libra").join("libra.db");
    let mut opts = ConnectOptions::new(format!("sqlite://{}", db_path.display()));
    opts.sqlx_logging(false)
        .connect_timeout(Duration::from_secs(5));
    Database::connect(opts)
        .await
        .expect("connect raw repository database")
}

/// Reconstruct the real pre-OL-02 operation shape for rollback fixtures.
/// Production v2 is intentionally forward-only, so a fixture that exercises
/// older migration downs must not run those downs against the v2 `operation`
/// table and pretend that it is v1.
async fn restore_v1_operation_shape(conn: &DatabaseConnection) {
    for table in [
        "ai_operation_link",
        "change_predecessor",
        "change_revision",
        "change_identity",
        "operation_journal",
        "operation_head",
        "operation_parent",
        "operation",
    ] {
        conn.execute_raw(Statement::from_string(
            conn.get_database_backend(),
            format!("DROP TABLE IF EXISTS `{table}`"),
        ))
        .await
        .unwrap_or_else(|error| panic!("drop v2 fixture table {table}: {error}"));
    }
    for trigger in [
        "legacy_operation_scope_provenance_domain_insert",
        "legacy_operation_scope_provenance_domain_update",
        "legacy_operation_scope_kind_domain_insert",
        "legacy_operation_scope_kind_domain_update",
    ] {
        conn.execute_raw(Statement::from_string(
            conn.get_database_backend(),
            format!("DROP TRIGGER IF EXISTS {trigger}"),
        ))
        .await
        .unwrap_or_else(|error| panic!("drop v2 fixture trigger {trigger}: {error}"));
    }
    for index in [
        "idx_legacy_operation_repo_order",
        "idx_legacy_operation_dedup_scope",
        "idx_legacy_operation_control_slot",
        "idx_legacy_operation_parent_parent",
        "idx_legacy_operation_view_repo_created",
    ] {
        conn.execute_raw(Statement::from_string(
            conn.get_database_backend(),
            format!("DROP INDEX IF EXISTS {index}"),
        ))
        .await
        .unwrap_or_else(|error| panic!("drop v2 fixture index {index}: {error}"));
    }
    for (legacy, v1) in [
        ("legacy_operation", "operation"),
        ("legacy_operation_parent", "operation_parent"),
        ("legacy_operation_view", "operation_view"),
        ("legacy_operation_view_ref", "operation_view_ref"),
        (
            "legacy_operation_view_workspace",
            "operation_view_workspace",
        ),
    ] {
        conn.execute_raw(Statement::from_string(
            conn.get_database_backend(),
            format!("ALTER TABLE `{legacy}` RENAME TO `{v1}`"),
        ))
        .await
        .unwrap_or_else(|error| panic!("restore {legacy} as {v1}: {error}"));
    }
}

async fn stale_repo_at_approved_permission() -> tempfile::TempDir {
    let repo = tempdir().expect("create repository root");
    init_repo_via_cli(repo.path());

    let conn = connect_raw_repo_db(repo.path()).await;
    let runner = historical_builtin_runner().expect("historical migration registry");
    conn.execute_raw(Statement::from_string(
        conn.get_database_backend(),
        "DELETE FROM schema_versions WHERE version = 2026090101".to_string(),
    ))
    .await
    .expect("remove forward-only v2 version marker before rollback fixture");
    restore_v1_operation_shape(&conn).await;
    runner
        .rollback_to(&conn, 2026050601)
        .await
        .expect("roll back latest migration");
    conn.close().await.expect("close raw connection");
    repo
}

fn historical_builtin_runner() -> Result<MigrationRunner, MigrationError> {
    let mut runner = MigrationRunner::new();
    runner.extend(
        builtin_migrations()
            .into_iter()
            .filter(|migration| migration.version < 2026090101),
    )?;
    Ok(runner)
}

async fn max_schema_version(conn: &DatabaseConnection) -> Option<i64> {
    let row = conn
        .query_one_raw(Statement::from_string(
            conn.get_database_backend(),
            "SELECT MAX(version) FROM schema_versions",
        ))
        .await
        .expect("query schema version")
        .expect("schema version row");
    row.try_get_by_index(0).expect("decode schema version")
}

async fn index_exists(conn: &DatabaseConnection, name: &str) -> bool {
    conn.query_one_raw(Statement::from_sql_and_values(
        conn.get_database_backend(),
        "SELECT 1 FROM sqlite_master WHERE type = ? AND name = ? LIMIT 1",
        ["index".into(), name.into()],
    ))
    .await
    .expect("query sqlite_master")
    .is_some()
}

async fn column_exists(conn: &DatabaseConnection, table: &str, column: &str) -> bool {
    let escaped_table = table.replace('`', "``");
    let rows = conn
        .query_all_raw(Statement::from_string(
            conn.get_database_backend(),
            format!("PRAGMA table_info(`{escaped_table}`)"),
        ))
        .await
        .expect("query table_info");
    rows.iter().any(|row| {
        let name: String = row.try_get_by_index(1).expect("column name");
        name == column
    })
}

#[tokio::test]
async fn normal_command_auto_upgrades_stale_schema() {
    let repo = stale_repo_at_approved_permission().await;

    // A plain command opens the repository database, which now auto-applies any
    // pending migrations on connect — no explicit upgrade step is required.
    let output = run_libra_command(&["status"], repo.path());
    assert_cli_success(&output, "libra status on a stale-schema repository");

    let conn = connect_raw_repo_db(repo.path()).await;
    let latest = current_builtin_runner()
        .expect("built-in migration registry")
        .max_registered_version();
    assert_eq!(
        max_schema_version(&conn).await,
        latest,
        "opening the database should bring the schema up to the latest version"
    );
    assert!(
        column_exists(&conn, "agent_usage_stats", "agent_name").await,
        "auto-upgrade should apply the agent_name migration"
    );
    assert!(
        index_exists(&conn, "idx_agent_usage_stats_agent_name_provider_model").await,
        "auto-upgrade should recreate the agent_name/provider/model index"
    );
}

#[tokio::test]
async fn hash_object_read_only_skips_stale_schema_guard() {
    let repo = stale_repo_at_approved_permission().await;
    std::fs::write(repo.path().join("hello.txt"), b"hello world\n").expect("write fixture");

    let output = run_libra_command(&["hash-object", "hello.txt"], repo.path());
    assert_cli_success(
        &output,
        "read-only hash-object should not trigger a schema upgrade",
    );
    assert_eq!(
        String::from_utf8_lossy(&output.stdout).trim(),
        "3b18e512dba79e4c8300dd08aeb37f8e728b8dad"
    );

    let conn = connect_raw_repo_db(repo.path()).await;
    assert_eq!(max_schema_version(&conn).await, Some(2026050601));
    assert!(
        !column_exists(&conn, "agent_usage_stats", "agent_name").await,
        "read-only hash-object preflight must not apply pending migrations"
    );
}

#[tokio::test]
async fn hash_object_read_only_defaults_sha1_when_config_kv_is_missing() {
    let repo = stale_repo_at_approved_permission().await;
    std::fs::write(repo.path().join("hello.txt"), b"hello world\n").expect("write fixture");

    let conn = connect_raw_repo_db(repo.path()).await;
    conn.execute_raw(Statement::from_string(
        conn.get_database_backend(),
        "DROP TABLE config_kv",
    ))
    .await
    .expect("drop config_kv table");
    conn.close().await.expect("close raw connection");

    let output = run_libra_command(&["hash-object", "hello.txt"], repo.path());
    assert_cli_success(
        &output,
        "read-only hash-object should not require config_kv schema",
    );
    assert_eq!(
        String::from_utf8_lossy(&output.stdout).trim(),
        "3b18e512dba79e4c8300dd08aeb37f8e728b8dad"
    );
}
