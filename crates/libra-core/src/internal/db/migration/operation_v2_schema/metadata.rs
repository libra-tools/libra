use std::collections::BTreeMap;

use sea_orm::{ConnectionTrait, DatabaseConnection, DatabaseTransaction, DbErr, Statement};

use super::{object_definition, query, quote_identifier, schema_error, sql};

#[derive(Debug, PartialEq, Eq)]
struct Column {
    declared_type: String,
    not_null: i64,
    default: Option<String>,
    primary_key_position: i64,
    hidden: i64,
}

pub(super) async fn validate_columns(
    txn: &DatabaseTransaction,
    reference: &DatabaseConnection,
    table: &str,
) -> Result<(), DbErr> {
    let expected = columns(reference, table).await?;
    let actual = columns(txn, table).await?;
    if actual != expected {
        return Err(schema_error(
            table,
            "column names, types, nullability, defaults, primary key or hidden columns differ from the canonical schema",
        ));
    }
    Ok(())
}

async fn columns<C: ConnectionTrait>(
    connection: &C,
    table: &str,
) -> Result<BTreeMap<String, Column>, DbErr> {
    let definition = object_definition(connection, table, "table", table).await?;
    sql::validate_table_constraints(&definition).map_err(|error| schema_error(table, error))?;
    let rows = query(
        connection,
        table,
        format!("PRAGMA main.table_xinfo({})", quote_identifier(table)),
    )
    .await?;
    let mut columns = BTreeMap::new();
    for row in rows {
        columns.insert(
            row.try_get_by_index(1)?,
            Column {
                declared_type: row.try_get_by_index::<String>(2)?.to_ascii_uppercase(),
                not_null: row.try_get_by_index(3)?,
                default: row.try_get_by_index(4)?,
                primary_key_position: row.try_get_by_index(5)?,
                hidden: row.try_get_by_index(6)?,
            },
        );
    }
    if columns.is_empty() {
        return Err(schema_error(table, "table_xinfo returned no columns"));
    }
    Ok(columns)
}

pub(super) async fn validate_required_objects(
    txn: &DatabaseTransaction,
    reference: &DatabaseConnection,
    table: &str,
) -> Result<(), DbErr> {
    let expected = reference
        .query_all_raw(Statement::from_sql_and_values(
            reference.get_database_backend(),
            "SELECT name, type, sql FROM main.sqlite_schema \
             WHERE tbl_name = ? AND type IN ('index', 'trigger') AND sql IS NOT NULL \
             ORDER BY type, name",
            [table.into()],
        ))
        .await
        .map_err(|error| schema_error(table, error))?;
    // Additional user indexes/triggers are allowed; every canonical name must keep its identity.
    for row in expected {
        let name: String = row.try_get_by_index(0)?;
        let kind: String = row.try_get_by_index(1)?;
        let expected_sql: String = row.try_get_by_index(2)?;
        let actual_sql = object_definition(txn, &name, &kind, table).await?;
        let matches = if kind == "index" {
            index(reference, table, &name).await? == index(txn, table, &name).await?
                && sql::predicate(&expected_sql)? == sql::predicate(&actual_sql)?
        } else {
            sql::tokens(&expected_sql)? == sql::tokens(&actual_sql)?
        };
        if !matches {
            return Err(schema_error(
                &name,
                format!("required {kind} definition differs from the canonical schema"),
            ));
        }
    }
    Ok(())
}

#[derive(Debug, PartialEq, Eq)]
struct Index {
    unique: i64,
    partial: i64,
    keys: Vec<IndexKey>,
}

#[derive(Debug, PartialEq, Eq)]
struct IndexKey {
    name: Option<String>,
    descending: i64,
    collation: Option<String>,
}

async fn index<C: ConnectionTrait>(
    connection: &C,
    table: &str,
    name: &str,
) -> Result<Index, DbErr> {
    let rows = query(
        connection,
        name,
        format!("PRAGMA main.index_list({})", quote_identifier(table)),
    )
    .await?;
    let mut flags = None;
    for row in rows {
        if row.try_get_by_index::<String>(1)? == name {
            flags = Some((row.try_get_by_index(2)?, row.try_get_by_index(4)?));
            break;
        }
    }
    let (unique, partial) = flags
        .ok_or_else(|| schema_error(name, format!("index is not attached to table {table}")))?;
    let rows = query(
        connection,
        name,
        format!("PRAGMA main.index_xinfo({})", quote_identifier(name)),
    )
    .await?;
    let mut keys = BTreeMap::new();
    for row in rows {
        if row.try_get_by_index::<i64>(5)? != 0 {
            keys.insert(
                row.try_get_by_index::<i64>(0)?,
                IndexKey {
                    name: row.try_get_by_index(2)?,
                    descending: row.try_get_by_index(3)?,
                    collation: row.try_get_by_index(4)?,
                },
            );
        }
    }
    Ok(Index {
        unique,
        partial,
        // Column ids and auxiliary rowid entries depend on physical table layout, not key identity.
        keys: keys.into_values().collect(),
    })
}
