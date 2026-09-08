//! Forward barrier for the independently shipped operation-v2 and config tips.

use chrono::Utc;
use sea_orm::{ConnectionTrait, DatabaseTransaction, DbErr, Statement};

use super::{
    OPERATION_V2_MIGRATION_VERSION, apply_operation_v2_migration, legacy_operation_namespace_ddl,
    operation_v2_schema::{validate_operation_v1_schema, validate_operation_v2_schema},
};

pub(super) const VERSION: i64 = 2026090801;
const OPERATION_V2_SQL: &str =
    include_str!("../../../../sql/migrations/2026090101_operation_v2.sql");
const LEGACY_TABLES: [&str; 5] = [
    "legacy_operation",
    "legacy_operation_parent",
    "legacy_operation_view",
    "legacy_operation_view_ref",
    "legacy_operation_view_workspace",
];
const V2_ONLY_TABLES: [&str; 6] = [
    "operation_head",
    "operation_journal",
    "change_identity",
    "change_revision",
    "change_predecessor",
    "ai_operation_link",
];

/// Runs after the runner has claimed 0801 and acquired its existing writer lock.
pub(super) async fn apply(txn: &DatabaseTransaction, barrier_sql: &str) -> Result<(), DbErr> {
    let receipt = txn
        .query_one_raw(Statement::from_sql_and_values(
            txn.get_database_backend(),
            "SELECT name FROM schema_versions WHERE version = ?",
            [OPERATION_V2_MIGRATION_VERSION.into()],
        ))
        .await?;
    if let Some(receipt) = receipt {
        let name: String = receipt.try_get_by_index(0)?;
        if name != "operation_v2" {
            return Err(DbErr::Custom(format!(
                "migration {VERSION} refuses migration {OPERATION_V2_MIGRATION_VERSION} receipt named {name:?}; expected operation_v2"
            )));
        }
        reject_mixed_namespace(txn, true).await?;
        validate_operation_v2_schema(txn, OPERATION_V2_SQL, &legacy_operation_namespace_ddl())
            .await?;
    } else {
        // Only this known branch hole may be filled. An unreceipted v2 or
        // partially copied namespace is ambiguous, not permission to recopy.
        reject_mixed_namespace(txn, false).await?;
        validate_operation_v1_schema(txn, &v1_reference_sql()).await?;
        apply_operation_v2_migration(txn, OPERATION_V2_SQL).await?;
        reject_mixed_namespace(txn, true).await?;
        validate_operation_v2_schema(txn, OPERATION_V2_SQL, &legacy_operation_namespace_ddl())
            .await?;
        txn.execute_raw(Statement::from_sql_and_values(
            txn.get_database_backend(),
            "INSERT INTO schema_versions (version, name, applied_at) VALUES (?, ?, ?)",
            [
                OPERATION_V2_MIGRATION_VERSION.into(),
                "operation_v2".into(),
                Utc::now().to_rfc3339().into(),
            ],
        ))
        .await?;
    }
    txn.execute_unprepared(barrier_sql).await?;
    Ok(())
}

pub(super) fn v1_reference_sql() -> String {
    // 2026072902 defaulted new v1 rows to declared; the copied legacy
    // namespace deliberately defaults future writes to unknown instead.
    legacy_operation_namespace_ddl()
        .replace("legacy_", "")
        .replace(
            "scope_provenance TEXT NOT NULL DEFAULT 'unknown'",
            "scope_provenance TEXT NOT NULL DEFAULT 'declared'",
        )
}

async fn reject_mixed_namespace(txn: &DatabaseTransaction, has_receipt: bool) -> Result<(), DbErr> {
    let objects = txn
        .query_all_raw(Statement::from_string(
            txn.get_database_backend(),
            "SELECT name FROM sqlite_master WHERE type IN ('table', 'view')",
        ))
        .await?;
    for object in objects {
        let name: String = object.try_get_by_index(0)?;
        let unexpected = if has_receipt {
            [
                "operation_view",
                "operation_view_ref",
                "operation_view_workspace",
            ]
            .contains(&name.as_str())
                || (name.starts_with("legacy_operation") && !LEGACY_TABLES.contains(&name.as_str()))
        } else {
            name.starts_with("legacy_operation") || V2_ONLY_TABLES.contains(&name.as_str())
        };
        if unexpected || (name.starts_with("operation") && name.ends_with("__staging")) {
            return Err(DbErr::Custom(format!(
                "migration {VERSION} refuses mixed operation schema containing {name}; restore a consistent database backup before retrying"
            )));
        }
    }
    Ok(())
}
