//! Read-only identity checks for the operation namespace convergence barrier.

use sea_orm::{
    ConnectionTrait, Database, DatabaseConnection, DatabaseTransaction, DbErr, QueryResult,
    Statement,
};

use self::metadata::{validate_columns, validate_required_objects};

mod metadata;
mod sql;

const V1_TABLES: &[&str] = &[
    "operation",
    "operation_parent",
    "operation_view",
    "operation_view_ref",
    "operation_view_workspace",
];

const V2_TABLES: &[&str] = &[
    "operation",
    "operation_parent",
    "operation_head",
    "operation_journal",
    "change_identity",
    "change_revision",
    "change_predecessor",
    "ai_operation_link",
    "legacy_operation",
    "legacy_operation_parent",
    "legacy_operation_view",
    "legacy_operation_view_ref",
    "legacy_operation_view_workspace",
];

pub(super) async fn validate_operation_v2_schema(
    txn: &DatabaseTransaction,
    canonical_sql: &str,
    legacy_sql: &str,
) -> Result<(), DbErr> {
    let reference = reference_database(&[canonical_sql, legacy_sql]).await?;
    let result = async {
        for table in V2_TABLES {
            validate_columns(txn, &reference, table).await?;
            validate_required_objects(txn, &reference, table).await?;
        }
        Ok(())
    }
    .await;
    close_reference(reference, result).await
}

pub(super) async fn validate_operation_v1_schema(
    txn: &DatabaseTransaction,
    expected_sql: &str,
) -> Result<(), DbErr> {
    let reference = reference_database(&[expected_sql]).await?;
    let result = async {
        for table in V1_TABLES {
            validate_columns(txn, &reference, table).await?;
        }
        Ok(())
    }
    .await;
    close_reference(reference, result).await
}

async fn reference_database(definitions: &[&str]) -> Result<DatabaseConnection, DbErr> {
    let reference = Database::connect("sqlite::memory:")
        .await
        .map_err(|error| schema_error("reference database", error))?;
    // Only this isolated database receives DDL; the repository transaction is read-only here.
    for definition in definitions {
        if let Err(error) = reference.execute_unprepared(definition).await {
            let _ = reference.close().await;
            return Err(schema_error("reference DDL", error));
        }
    }
    Ok(reference)
}

async fn close_reference(
    reference: DatabaseConnection,
    result: Result<(), DbErr>,
) -> Result<(), DbErr> {
    let close = reference
        .close()
        .await
        .map_err(|error| schema_error("reference database close", error));
    result.and(close)
}

fn schema_error(resource: &str, detail: impl std::fmt::Display) -> DbErr {
    DbErr::Custom(format!(
        "operation schema validation failed for {resource}: {detail}; no schema repair was attempted"
    ))
}

fn quote_identifier(identifier: &str) -> String {
    format!("\"{}\"", identifier.replace('"', "\"\""))
}

async fn query<C: ConnectionTrait>(
    connection: &C,
    resource: &str,
    sql: String,
) -> Result<Vec<QueryResult>, DbErr> {
    connection
        .query_all_raw(Statement::from_string(
            connection.get_database_backend(),
            sql,
        ))
        .await
        .map_err(|error| schema_error(resource, error))
}

async fn object_definition<C: ConnectionTrait>(
    connection: &C,
    name: &str,
    kind: &str,
    table: &str,
) -> Result<String, DbErr> {
    let row = connection
        .query_one_raw(Statement::from_sql_and_values(
            connection.get_database_backend(),
            "SELECT type, tbl_name, sql FROM main.sqlite_schema WHERE name = ? AND type = ?",
            [name.into(), kind.into()],
        ))
        .await
        .map_err(|error| schema_error(name, error))?
        .ok_or_else(|| schema_error(name, format!("required {kind} is missing")))?;
    let actual_kind: String = row.try_get_by_index(0)?;
    let actual_table: String = row.try_get_by_index(1)?;
    let definition: Option<String> = row.try_get_by_index(2)?;
    if actual_kind != kind || actual_table != table {
        return Err(schema_error(
            name,
            format!("expected {kind} on {table}, found {actual_kind} on {actual_table}"),
        ));
    }
    definition.ok_or_else(|| schema_error(name, "required object has no SQL definition"))
}
