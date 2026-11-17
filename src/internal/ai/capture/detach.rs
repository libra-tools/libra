//! Remove capture-catalog tables from `libra.db` after proving `capture.db`
//! has every primary key. The proof reads keys in batches of at most
//! [`CAPTURE_COPY_BATCH`] and never selects transcript columns. A missing key
//! or a missing capture table deletes nothing. Repository `schema_versions`
//! is not updated.

use std::{
    io::{self, ErrorKind},
    path::Path,
};

use sea_orm::{ConnectionTrait, DatabaseConnection, Statement};

use super::store::{self, CAPTURE_COPY_BATCH};

/// Drop the repository's capture tables when every primary key is already
/// in `capture`. This does not insert rows: a second insert would trip
/// erasure triggers for sessions that were copied before their tombstone.
///
/// A second call, after those tables are gone, succeeds and does not drop
/// repository tables such as `config_kv`.
pub async fn detach_copied_catalog(
    capture: &DatabaseConnection,
    repository_db: &Path,
) -> io::Result<()> {
    if !repository_db.is_file() {
        return Err(io::Error::new(
            ErrorKind::NotFound,
            "capture catalog detach could not find the repository database",
        ));
    }
    let path = repository_db.to_str().ok_or_else(|| {
        io::Error::new(
            ErrorKind::InvalidInput,
            "capture catalog detach requires a UTF-8 repository database path",
        )
    })?;
    let repo = crate::internal::db::open_connection_without_schema_management(
        path,
        std::time::Duration::from_secs(30),
    )
    .await?;
    repo.execute_unprepared("PRAGMA foreign_keys = OFF")
        .await
        .map_err(|error| {
            io::Error::other(format!(
                "capture catalog detach could not suspend foreign keys: {error}"
            ))
        })?;
    repo.execute_unprepared("BEGIN IMMEDIATE")
        .await
        .map_err(|error| {
            io::Error::other(format!(
                "capture catalog detach could not lock the repository database: {error}"
            ))
        })?;
    let result = drop_proven_tables(&repo, capture).await;
    let finished = if result.is_ok() {
        repo.execute_unprepared("COMMIT")
            .await
            .map(|_| ())
            .map_err(|error| {
                io::Error::other(format!("capture catalog detach could not commit: {error}"))
            })
    } else {
        let _ = repo.execute_unprepared("ROLLBACK").await;
        Ok(())
    };
    result?;
    finished?;
    let _ = repo.close().await;
    Ok(())
}

async fn drop_proven_tables(
    repo: &DatabaseConnection,
    capture: &DatabaseConnection,
) -> io::Result<()> {
    let mut present = Vec::new();
    for table in store::declared_capture_tables() {
        if !safe_ident(&table) {
            return Err(io::Error::other(format!(
                "capture catalog detach refused table '{table}'"
            )));
        }
        if !table_exists(repo, &table).await? {
            continue;
        }
        if !table_exists(capture, &table).await? {
            return Err(io::Error::other(format!(
                "capture catalog detach refused because table '{table}' is missing from the capture database"
            )));
        }
        let keys = key_columns(repo, &table).await?;
        let missing = missing_key_count(repo, capture, &table, &keys).await?;
        if missing > 0 {
            return Err(io::Error::other(format!(
                "capture catalog detach refused because table '{table}' is missing {missing} copied primary keys"
            )));
        }
        present.push(table);
    }
    for table in &present {
        repo.execute_unprepared(&format!("DROP TABLE \"{table}\""))
            .await
            .map_err(|error| {
                io::Error::other(format!(
                    "capture catalog detach could not drop table '{table}': {error}"
                ))
            })?;
    }
    Ok(())
}

async fn missing_key_count(
    repo: &DatabaseConnection,
    capture: &DatabaseConnection,
    table: &str,
    keys: &[String],
) -> io::Result<u64> {
    let quoted = keys
        .iter()
        .map(|column| format!("CAST(\"{column}\" AS TEXT)"))
        .collect::<Vec<_>>()
        .join(", ");
    let mut offset = 0_i64;
    loop {
        let sql = format!(
            "SELECT {quoted} FROM \"{table}\" ORDER BY {quoted} LIMIT {CAPTURE_COPY_BATCH} OFFSET {offset}"
        );
        let rows = text_rows(repo, &sql).await?;
        if rows.is_empty() {
            return Ok(0);
        }
        if rows.len() > CAPTURE_COPY_BATCH {
            return Err(io::Error::other(format!(
                "capture catalog detach loaded {} keys in one batch; the limit is {CAPTURE_COPY_BATCH}",
                rows.len()
            )));
        }
        let found = count_keys(capture, table, keys, &rows).await?;
        if found != rows.len() as i64 {
            let missing = (rows.len() as i64).saturating_sub(found).max(0) as u64;
            return Ok(missing.max(1));
        }
        if rows.len() < CAPTURE_COPY_BATCH {
            return Ok(0);
        }
        offset += i64::try_from(rows.len()).unwrap_or(i64::MAX);
    }
}

async fn count_keys(
    capture: &DatabaseConnection,
    table: &str,
    keys: &[String],
    rows: &[Vec<Option<String>>],
) -> io::Result<i64> {
    let mut predicate = String::new();
    let mut binds = Vec::new();
    for row in rows {
        if !predicate.is_empty() {
            predicate.push_str(" OR ");
        }
        predicate.push('(');
        for (index, column) in keys.iter().enumerate() {
            if index > 0 {
                predicate.push_str(" AND ");
            }
            predicate.push_str(&format!("CAST(\"{column}\" AS TEXT) IS ?"));
            binds.push(row.get(index).cloned().flatten());
        }
        predicate.push(')');
    }
    let sql = format!("SELECT COUNT(*) AS n FROM \"{table}\" WHERE {predicate}");
    let mut values = Vec::with_capacity(binds.len());
    for value in binds {
        values.push(value.into());
    }
    let row = capture
        .query_one_raw(Statement::from_sql_and_values(
            capture.get_database_backend(),
            sql,
            values,
        ))
        .await
        .map_err(|error| {
            io::Error::other(format!(
                "capture catalog detach could not compare table '{table}': {error}"
            ))
        })?
        .ok_or_else(|| {
            io::Error::other(format!(
                "capture catalog detach could not compare table '{table}'"
            ))
        })?;
    row.try_get_by_index(0).map_err(|error| {
        io::Error::other(format!(
            "capture catalog detach could not read a key count for table '{table}': {error}"
        ))
    })
}

async fn key_columns(conn: &DatabaseConnection, table: &str) -> io::Result<Vec<String>> {
    let primary = text_column(
        conn,
        &format!("SELECT name FROM pragma_table_info('{table}') WHERE pk > 0 ORDER BY pk"),
    )
    .await?;
    if !primary.is_empty() {
        return Ok(primary);
    }
    let indexes = text_column(
        conn,
        &format!(
            "SELECT name FROM pragma_index_list('{table}') WHERE \"unique\" = 1 AND partial = 0"
        ),
    )
    .await?;
    for index in indexes {
        if !safe_ident(&index) {
            continue;
        }
        let columns = text_column(
            conn,
            &format!("SELECT name FROM pragma_index_info('{index}') ORDER BY seqno"),
        )
        .await?;
        if !columns.is_empty() && columns.iter().all(|column| safe_ident(column)) {
            return Ok(columns);
        }
    }
    Err(io::Error::other(format!(
        "capture catalog detach cannot compare table '{table}' because it has no primary key"
    )))
}

async fn table_exists(conn: &DatabaseConnection, table: &str) -> io::Result<bool> {
    crate::internal::db::sqlite_schema_contains(conn, "table", table)
        .await
        .map_err(|error| {
            io::Error::other(format!(
                "capture catalog detach could not inspect table '{table}': {error}"
            ))
        })
}

async fn text_column(conn: &DatabaseConnection, sql: &str) -> io::Result<Vec<String>> {
    let rows = text_rows(conn, sql).await?;
    Ok(rows
        .into_iter()
        .filter_map(|mut row| row.pop().flatten())
        .collect())
}

async fn text_rows(conn: &DatabaseConnection, sql: &str) -> io::Result<Vec<Vec<Option<String>>>> {
    let rows = conn
        .query_all_raw(Statement::from_string(
            conn.get_database_backend(),
            sql.to_owned(),
        ))
        .await
        .map_err(|error| {
            io::Error::other(format!(
                "capture catalog detach could not read keys: {error}"
            ))
        })?;
    let mut decoded = Vec::with_capacity(rows.len());
    for row in rows {
        let mut cells = Vec::new();
        for index in 0..8 {
            match row.try_get_by_index::<Option<String>>(index) {
                Ok(value) => cells.push(value),
                Err(_) if index > 0 => break,
                Err(error) => {
                    return Err(io::Error::other(format!(
                        "capture catalog detach could not read a key column: {error}"
                    )));
                }
            }
        }
        decoded.push(cells);
    }
    Ok(decoded)
}

fn safe_ident(name: &str) -> bool {
    let mut chars = name.chars();
    match chars.next() {
        Some(first) if first.is_ascii_alphabetic() || first == '_' => {}
        _ => return false,
    }
    chars.all(|ch| ch.is_ascii_alphanumeric() || ch == '_')
}
