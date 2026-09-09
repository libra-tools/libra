//! Bootstrap prerequisites for a historical, bounded migration registry.

use libra::internal::db;
use sea_orm::{ConnectionTrait, DatabaseConnection, DbBackend, Statement};

pub(super) async fn initialize(conn: &DatabaseConnection) {
    conn.execute_unprepared(include_str!("../../../sql/sqlite_20260309_init.sql"))
        .await
        .expect("apply historical bootstrap");
    // The bootstrap retains the old eight-column rebase table. Real opens
    // normalized these lazy-era columns before the July static rebuild;
    // a bounded MigrationRunner does not invoke that runtime hook.
    conn.execute_unprepared(
        "ALTER TABLE rebase_state ADD COLUMN autosquash INTEGER NOT NULL DEFAULT 0; \
         ALTER TABLE rebase_state ADD COLUMN todo_actions TEXT NOT NULL DEFAULT ''; \
         ALTER TABLE rebase_state ADD COLUMN empty_mode TEXT NOT NULL DEFAULT 'keep';",
    )
    .await
    .expect("establish historical lazy rebase columns");
    db::ensure_ai_runtime_contract_schema(conn)
        .await
        .expect("establish historical runtime contract tables");
}

pub(super) async fn assert_pre_v2_history(conn: &DatabaseConnection) {
    let receipt = conn
        .query_one_raw(Statement::from_string(
            DbBackend::Sqlite,
            "SELECT COUNT(*), MAX(version) FROM schema_versions",
        ))
        .await
        .expect("inspect historical fixture receipts")
        .expect("historical receipt aggregate");
    assert_eq!(
        receipt.try_get_by_index::<i64>(0).unwrap(),
        57,
        "fixture must contain the complete pre-v2 history"
    );
    assert_eq!(
        receipt.try_get_by_index::<i64>(1).unwrap(),
        2026082401,
        "fixture must stop before either September branch"
    );
    let columns: Vec<String> = conn
        .query_all_raw(Statement::from_string(
            DbBackend::Sqlite,
            "SELECT name FROM pragma_table_info('operation') ORDER BY name",
        ))
        .await
        .expect("inspect historical operation shape")
        .into_iter()
        .map(|row| row.try_get_by_index(0).unwrap())
        .collect();
    assert_eq!(
        columns,
        [
            "actor",
            "args_digest",
            "claim_owner",
            "command_name",
            "control_slot",
            "description",
            "end_ts",
            "op_id",
            "repo_id",
            "restorable",
            "scope_kind",
            "scope_provenance",
            "start_ts",
            "status",
            "view_id",
            "worktree_id",
        ],
        "fixture must have the real pre-v2 operation shape"
    );
    assert!(
        conn.query_all_raw(Statement::from_string(
            DbBackend::Sqlite,
            "SELECT name FROM sqlite_master WHERE type='table' AND \
             (name LIKE 'legacy_operation%' OR name IN ('operation_head','operation_journal'))",
        ))
        .await
        .expect("inspect absence of operation v2 tables")
        .is_empty(),
        "historical fixture must not already contain operation v2"
    );
}
