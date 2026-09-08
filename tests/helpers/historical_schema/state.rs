//! Receipt and physical-schema observations for historical CLI fixtures.

use libra::internal::db::migration::builtin_migrations;
use sea_orm::{ConnectionTrait, DatabaseConnection, DbBackend, Statement};

async fn values(conn: &DatabaseConnection, sql: &str) -> Vec<String> {
    conn.query_all_raw(Statement::from_string(DbBackend::Sqlite, sql))
        .await
        .unwrap()
        .into_iter()
        .map(|row| row.try_get_by_index(0).unwrap())
        .collect()
}

async fn columns(conn: &DatabaseConnection, table: &str) -> Vec<String> {
    values(
        conn,
        &format!("SELECT name FROM pragma_table_info('{table}') ORDER BY name"),
    )
    .await
}

async fn receipts(conn: &DatabaseConnection) -> Vec<(i64, String, String)> {
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

pub(crate) async fn assert_history(conn: &DatabaseConnection, tip: i64) {
    let expected_count = match tip {
        2026050601 => 5,
        2026072304 => 38,
        2026073101 => 51,
        _ => panic!("unregistered historical fixture cutoff"),
    };
    let actual = receipts(conn).await;
    let expected: Vec<_> = builtin_migrations()
        .into_iter()
        .filter(|migration| migration.version <= tip)
        .map(|migration| (migration.version, migration.name.to_string()))
        .collect();
    assert_eq!(
        actual.len(),
        expected_count,
        "complete historical receipt count"
    );
    assert_eq!(actual.last().unwrap().0, tip, "historical MAX before CLI");
    assert_eq!(
        actual
            .iter()
            .map(|(version, name, _)| (*version, name.clone()))
            .collect::<Vec<_>>(),
        expected
    );
    assert!(actual.iter().all(|(_, _, timestamp)| !timestamp.is_empty()));

    let mut expected_columns = vec![
        "op_id",
        "repo_id",
        "view_id",
        "command_name",
        "description",
        "actor",
        "args_digest",
        "start_ts",
        "end_ts",
        "status",
    ];
    if tip >= 2026072201 {
        expected_columns.push("worktree_id");
    }
    if tip >= 2026072902 {
        expected_columns.push("scope_provenance");
    }
    if tip >= 2026073003 {
        expected_columns.extend(["restorable", "control_slot", "claim_owner"]);
    }
    if tip >= 2026073004 {
        expected_columns.push("scope_kind");
    }
    expected_columns.sort();
    assert_eq!(
        columns(conn, "operation").await,
        expected_columns,
        "exact historical operation shape"
    );
    assert_eq!(
        columns(conn, "reference")
            .await
            .contains(&"worktree_id".to_string()),
        tip >= 2026070801
    );
    assert_eq!(
        columns(conn, "agent_usage_stats")
            .await
            .contains(&"agent_name".to_string()),
        tip >= 2026050801
    );
    assert_eq!(
        !columns(conn, "worktree_registry_capability")
            .await
            .is_empty(),
        tip >= 2026072401
    );
    let rebase_columns = columns(conn, "rebase_state").await;
    for column in ["autosquash", "todo_actions", "empty_mode", "worktree_id"] {
        assert_eq!(
            rebase_columns.contains(&column.to_string()),
            tip >= 2026072101,
            "historical rebase column {column}"
        );
    }
    assert!(
        !columns(conn, "agent_session")
            .await
            .contains(&"workspace_id".to_string()),
        "the August capture-scope migration must still be pending"
    );
    assert!(
        values(
            conn,
            "SELECT name FROM sqlite_master WHERE type='table' AND \
         (name LIKE 'legacy_operation%' OR name IN ('operation_head','operation_journal'))"
        )
        .await
        .is_empty(),
        "history must not already contain operation v2"
    );
}

pub(crate) async fn assert_current(conn: &DatabaseConnection) {
    let actual = receipts(conn).await;
    let expected: Vec<_> = builtin_migrations()
        .into_iter()
        .map(|migration| (migration.version, migration.name.to_string()))
        .collect();
    assert_eq!(
        actual
            .iter()
            .map(|(version, name, _)| (*version, name.clone()))
            .collect::<Vec<_>>(),
        expected
    );
    for version in [2026090101, 2026090601, 2026090801] {
        assert!(actual.iter().any(|(applied, _, _)| *applied == version));
    }
    assert!(
        columns(conn, "operation")
            .await
            .contains(&"format_version".to_string())
    );
    assert!(
        columns(conn, "agent_session")
            .await
            .contains(&"workspace_id".to_string()),
        "the pending capture-scope migration must actually apply"
    );
    for table in ["legacy_operation", "operation_head", "operation_journal"] {
        assert!(
            !columns(conn, table).await.is_empty(),
            "current schema must contain {table}"
        );
    }
}

#[derive(Debug, PartialEq, Eq)]
pub(crate) struct SchemaSnapshot {
    receipts: Vec<(i64, String, String)>,
    schema: Vec<String>,
}

pub(crate) async fn snapshot(conn: &DatabaseConnection) -> SchemaSnapshot {
    SchemaSnapshot {
        receipts: receipts(conn).await,
        schema: values(
            conn,
            "SELECT json_array(type,name,tbl_name,sql) FROM sqlite_master ORDER BY type,name",
        )
        .await,
    }
}
