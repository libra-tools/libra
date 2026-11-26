//! Strict copy verification before a legacy source table is removed.

use sea_orm::{ConnectionTrait, DatabaseTransaction, DbErr, Statement};

pub(super) async fn validate_copy(
    txn: &DatabaseTransaction,
    source: &str,
    staging: &str,
    nonempty_fields: &[&str],
    comparison_fields: &[&str],
) -> Result<(), DbErr> {
    let source_table = quote_identifier(source);
    let staging_table = quote_identifier(staging);
    let source_count =
        query_count(txn, source, format!("SELECT COUNT(*) FROM {source_table}")).await?;
    let staging_count = query_count(
        txn,
        staging,
        format!("SELECT COUNT(*) FROM {staging_table}"),
    )
    .await?;
    if source_count != staging_count {
        return Err(DbErr::Custom(format!(
            "copy-first migration row-count mismatch for {source}: source={source_count}, staging={staging_count}"
        )));
    }
    let invalid_predicate = nonempty_fields
        .iter()
        .map(|field| format!("COALESCE(TRIM(t.{}), '') = ''", quote_identifier(field)))
        .collect::<Vec<_>>()
        .join(" OR ");
    if query_count(
        txn,
        staging,
        format!("SELECT COUNT(*) FROM {staging_table} AS t WHERE {invalid_predicate}"),
    )
    .await?
        != 0
    {
        return Err(DbErr::Custom(format!(
            "copy-first migration found an empty key in {source}"
        )));
    }
    // Qualifying columns also makes a missing quoted column an error instead
    // of SQLite's legacy double-quoted-string fallback, even on empty tables.
    let projection = |alias: &str| {
        comparison_fields
            .iter()
            .map(|field| format!("{alias}.{}", quote_identifier(field)))
            .collect::<Vec<_>>()
            .join(", ")
    };
    let source_projection = projection("s");
    let staging_projection = projection("t");
    let source_rows = format!("SELECT {source_projection} FROM {source_table} AS s");
    let staging_rows = format!("SELECT {staging_projection} FROM {staging_table} AS t");
    let source_missing = query_count(
        txn,
        source,
        format!("SELECT COUNT(*) FROM ({source_rows} EXCEPT {staging_rows})"),
    )
    .await?;
    let staging_missing = query_count(
        txn,
        staging,
        format!("SELECT COUNT(*) FROM ({staging_rows} EXCEPT {source_rows})"),
    )
    .await?;
    if source_missing != 0 || staging_missing != 0 {
        return Err(DbErr::Custom(format!(
            "copy-first migration key-set mismatch for {source}: source_missing={source_missing}, staging_missing={staging_missing}"
        )));
    }
    Ok(())
}

fn quote_identifier(identifier: &str) -> String {
    format!("\"{}\"", identifier.replace('"', "\"\""))
}

async fn query_count(txn: &DatabaseTransaction, table: &str, sql: String) -> Result<i64, DbErr> {
    let row = txn
        .query_one_raw(Statement::from_string(txn.get_database_backend(), sql))
        .await
        .map_err(|error| {
            DbErr::Custom(format!(
                "cannot verify copy-first migration table {table}: {error}"
            ))
        })?
        .ok_or_else(|| {
            DbErr::Custom(format!(
                "cannot verify copy-first migration table {table}: count query returned no row"
            ))
        })?;
    row.try_get_by_index(0).map_err(|error| {
        DbErr::Custom(format!(
            "cannot decode copy-first migration count for {table}: {error}"
        ))
    })
}
