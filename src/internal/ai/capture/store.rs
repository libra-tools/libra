//! The only production entry that opens `.libra/private/capture.db`.
//!
//! Repository commands keep using `.libra/libra.db`. This module never
//! `ATTACH`es the two files onto one connection. The catalog copy reads
//! primary-key batches of at most 500 rows so a transcript column is not
//! loaded for the whole table at once.

use std::{
    fs,
    io::{self, ErrorKind},
    path::{Path, PathBuf},
    sync::atomic::{AtomicUsize, Ordering},
    time::Duration,
};

use sea_orm::{
    ConnectionTrait, Database, DatabaseConnection, DbErr, Statement, TransactionTrait,
    sqlx::{Row, ValueRef},
};

use crate::{
    internal::db::{
        self,
        schema::{self, DatabaseRole},
    },
    utils::util,
};

/// Rows copied from one capture table in a single read. The catalog copy
/// never issues a larger `SELECT` of table data.
pub const CAPTURE_COPY_BATCH: usize = 500;

/// Highest data-row batch observed by the copy. Integration tests read this
/// to prove the shipped loop stays within [`CAPTURE_COPY_BATCH`].
#[doc(hidden)]
pub static CAPTURE_COPY_BATCH_PEAK: AtomicUsize = AtomicUsize::new(0);

const COPY_RECEIPT_TABLE: &str = "capture_catalog_copy_receipt";

/// `.libra/private/capture.db` for a repository storage directory.
pub fn capture_database_path(storage_root: &Path) -> PathBuf {
    storage_root.join("private").join("capture.db")
}

/// Busy timeout for command-path capture opens.
pub const COMMAND_BUSY_TIMEOUT: Duration = Duration::from_secs(30);

/// Open the capture database for the current repository.
///
/// The storage root is the parent of [`crate::utils::path::database`].
pub async fn open_current() -> io::Result<DatabaseConnection> {
    let database = crate::utils::path::database();
    let Some(storage) = database.parent() else {
        return Err(io::Error::new(
            ErrorKind::NotFound,
            "repository database path has no parent directory",
        ));
    };
    open(storage, COMMAND_BUSY_TIMEOUT).await
}

/// Open the capture database for `storage_root` (the `.libra` directory).
///
/// Creates `.libra/private` (mode `0700` on Unix) and `capture.db` when they
/// are missing, applies the capture schema, and copies catalog rows out of
/// `libra.db` once. A capture schema newer than this binary rejects only
/// this file.
pub async fn open(storage_root: &Path, busy_timeout: Duration) -> io::Result<DatabaseConnection> {
    let private_dir = storage_root.join("private");
    ensure_private_dir(&private_dir)?;
    let db_path = private_dir.join("capture.db");
    reject_symlink(&db_path)?;
    let conn = connect_capture(&db_path, busy_timeout).await?;
    ensure_capture_schema(&conn).await?;
    let repository = storage_root.join(util::DATABASE);
    if !copy_receipt_present(&conn).await? {
        if repository.is_file() && !repository_is_symlink(&repository)? {
            copy_catalog_from_repository(&conn, &repository).await?;
        }
        write_copy_receipt(&conn).await?;
    }
    if repository.is_file() && !repository_is_symlink(&repository)? {
        super::detach::detach_copied_catalog(&conn, &repository).await?;
    }
    Ok(conn)
}

/// Open an existing capture database without creating it or copying rows.
///
/// A missing file is [`ErrorKind::NotFound`]. Schema repair and the catalog
/// copy stay on [`open`]. This is still the capture-store entry: callers do
/// not open `.libra/private/capture.db` themselves.
pub async fn open_readonly(
    storage_root: &Path,
    busy_timeout: Duration,
) -> io::Result<DatabaseConnection> {
    let db_path = capture_database_path(storage_root);
    if !db_path.is_file() {
        return Err(io::Error::new(
            ErrorKind::NotFound,
            "capture database is not present",
        ));
    }
    reject_symlink(&db_path)?;
    connect_capture_file(&db_path, busy_timeout, true).await
}

/// Open an existing capture database for a catalog write.
///
/// Does not copy rows and does not detach. Detach takes an immediate lock on
/// `libra.db`, and the caller may already hold the traces-ref transaction on
/// that file. The two databases are never attached.
pub async fn open_existing_writer(
    storage_root: &Path,
    busy_timeout: Duration,
) -> io::Result<DatabaseConnection> {
    let db_path = capture_database_path(storage_root);
    if !db_path.is_file() {
        return Err(io::Error::new(
            ErrorKind::NotFound,
            "capture database is not present",
        ));
    }
    reject_symlink(&db_path)?;
    let conn = connect_capture(&db_path, busy_timeout).await?;
    ensure_capture_schema(&conn).await?;
    Ok(conn)
}

/// Copy capture-catalog rows from a repository database into an open capture
/// database. A second call inserts no additional rows. Primary keys are read
/// at most [`CAPTURE_COPY_BATCH`] at a time.
pub async fn copy_catalog_from_repository(
    capture: &DatabaseConnection,
    repository_db: &Path,
) -> io::Result<()> {
    if !repository_db.is_file() {
        return Err(io::Error::new(
            ErrorKind::NotFound,
            "capture catalog copy could not find the repository database; create the repository, then retry",
        ));
    }
    reject_symlink(repository_db)?;
    let source = schema::open_readonly_connection_for_role(
        repository_db,
        Duration::from_secs(30),
        DatabaseRole::Repository,
    )
    .await
    .map_err(|error| {
        io::Error::other(format!(
            "capture catalog copy could not read the repository database: {error}"
        ))
    })?;
    assert_single_database(&source, "repository").await?;
    assert_single_database(capture, "capture").await?;
    let tables = declared_capture_tables();
    set_foreign_keys(capture, false).await?;
    let copy_result = async {
        for table in tables {
            if !table_exists(&source, &table).await? {
                continue;
            }
            if !table_exists(capture, &table).await? {
                return Err(io::Error::other(format!(
                    "capture database is missing table '{table}'; its schema is incomplete. Retry with this Libra version"
                )));
            }
            copy_table(&source, capture, &table).await?;
        }
        Ok(())
    }
    .await;
    let restored = set_foreign_keys(capture, true).await;
    source.close().await.map_err(|error| {
        io::Error::other(format!(
            "capture catalog copy could not close the repository database: {error}"
        ))
    })?;
    copy_result?;
    restored?;
    Ok(())
}

pub(crate) fn declared_capture_tables() -> Vec<String> {
    let mut tables = Vec::new();
    for line in schema::CAPTURE_DATABASE_SQL.lines() {
        let trimmed = line.trim();
        let rest = trimmed
            .strip_prefix("CREATE TABLE IF NOT EXISTS ")
            .or_else(|| trimmed.strip_prefix("CREATE TABLE "));
        let Some(rest) = rest else {
            continue;
        };
        let name = rest
            .trim_start_matches('`')
            .trim_start_matches('"')
            .split(['`', '"', ' ', '('])
            .next()
            .unwrap_or("");
        if name.is_empty()
            || name == "schema_versions"
            || name == COPY_RECEIPT_TABLE
            || !safe_ident(name)
        {
            continue;
        }
        if !tables.iter().any(|existing| existing == name) {
            tables.push(name.to_owned());
        }
    }
    // Tombstone triggers reject session and checkpoint inserts that match an
    // existing tombstone. Copy those guarding rows after the rows they protect.
    if let Some(index) = tables
        .iter()
        .position(|name| name == "agent_import_tombstone")
    {
        let tombstone = tables.remove(index);
        tables.push(tombstone);
    }
    tables
}

async fn copy_table(
    source: &DatabaseConnection,
    capture: &DatabaseConnection,
    table: &str,
) -> io::Result<()> {
    let columns = table_columns(source, table).await?;
    if columns.is_empty() {
        return Err(io::Error::other(format!(
            "capture catalog copy found no columns on table '{table}'"
        )));
    }
    let quoted_columns = columns
        .iter()
        .map(|column| format!("\"{column}\""))
        .collect::<Vec<_>>()
        .join(", ");
    let without_rowid = table_sql(source, table)
        .await?
        .is_some_and(|sql| sql.to_ascii_uppercase().contains("WITHOUT ROWID"));
    if without_rowid {
        copy_without_rowid(source, capture, table, &columns, &quoted_columns).await
    } else {
        copy_by_rowid(source, capture, table, &columns, &quoted_columns).await
    }
}

async fn copy_by_rowid(
    source: &DatabaseConnection,
    capture: &DatabaseConnection,
    table: &str,
    columns: &[String],
    quoted_columns: &str,
) -> io::Result<()> {
    let mut last_rowid = 0_i64;
    loop {
        let keys = query_i64s(
            source,
            &format!(
                "SELECT rowid FROM \"{table}\" WHERE rowid > ? ORDER BY rowid LIMIT {CAPTURE_COPY_BATCH}"
            ),
            last_rowid,
        )
        .await?;
        let Some(batch_last) = keys.last().copied() else {
            return Ok(());
        };
        last_rowid = batch_last;
        let placeholders = vec!["?"; keys.len()].join(", ");
        let sql =
            format!("SELECT {quoted_columns} FROM \"{table}\" WHERE rowid IN ({placeholders})");
        let rows = fetch_cells(source, &sql, &keys).await?;
        note_batch(rows.len())?;
        insert_rows(capture, table, columns, &rows).await?;
        if keys.len() < CAPTURE_COPY_BATCH {
            return Ok(());
        }
    }
}

async fn copy_without_rowid(
    source: &DatabaseConnection,
    capture: &DatabaseConnection,
    table: &str,
    columns: &[String],
    quoted_columns: &str,
) -> io::Result<()> {
    let primary_key = primary_key_columns(source, table).await?;
    if primary_key.is_empty() {
        return Err(io::Error::other(format!(
            "capture catalog copy cannot page table '{table}' because it has no primary key"
        )));
    }
    let order = primary_key
        .iter()
        .map(|column| format!("\"{column}\""))
        .collect::<Vec<_>>()
        .join(", ");
    let mut offset = 0_i64;
    loop {
        let key_sql = format!(
            "SELECT {order} FROM \"{table}\" ORDER BY {order} LIMIT {CAPTURE_COPY_BATCH} OFFSET {offset}"
        );
        let key_rows = fetch_cells(source, &key_sql, &[]).await?;
        if key_rows.is_empty() {
            return Ok(());
        }
        offset += i64::try_from(key_rows.len()).unwrap_or(i64::MAX);
        // The key query already loaded only key columns. Fetch the matching
        // data rows, still capped at the batch size, so transcript bodies
        // from later keys stay on disk.
        let mut predicate = String::new();
        let mut binds = Vec::new();
        for key in &key_rows {
            if !predicate.is_empty() {
                predicate.push_str(" OR ");
            }
            predicate.push('(');
            for (index, column) in primary_key.iter().enumerate() {
                if index > 0 {
                    predicate.push_str(" AND ");
                }
                predicate.push_str(&format!("\"{column}\" IS ?"));
                binds.push(key.get(index).cloned().unwrap_or(Cell::Null));
            }
            predicate.push(')');
        }
        let sql = format!("SELECT {quoted_columns} FROM \"{table}\" WHERE {predicate}");
        let rows = fetch_bound_cells(source, &sql, &binds).await?;
        note_batch(rows.len())?;
        insert_rows(capture, table, columns, &rows).await?;
        if key_rows.len() < CAPTURE_COPY_BATCH {
            return Ok(());
        }
    }
}

fn note_batch(len: usize) -> io::Result<()> {
    if len > CAPTURE_COPY_BATCH {
        return Err(io::Error::other(format!(
            "capture catalog copy loaded {len} rows in one batch; the limit is {CAPTURE_COPY_BATCH}"
        )));
    }
    let mut current = CAPTURE_COPY_BATCH_PEAK.load(Ordering::Relaxed);
    while current < len {
        match CAPTURE_COPY_BATCH_PEAK.compare_exchange_weak(
            current,
            len,
            Ordering::Relaxed,
            Ordering::Relaxed,
        ) {
            Ok(_) => break,
            Err(observed) => current = observed,
        }
    }
    Ok(())
}

async fn insert_rows(
    capture: &DatabaseConnection,
    table: &str,
    columns: &[String],
    rows: &[Vec<Cell>],
) -> io::Result<()> {
    if rows.is_empty() {
        return Ok(());
    }
    let quoted = columns
        .iter()
        .map(|column| format!("\"{column}\""))
        .collect::<Vec<_>>()
        .join(", ");
    let placeholders = vec!["?"; columns.len()].join(", ");
    let sql = format!("INSERT OR IGNORE INTO \"{table}\" ({quoted}) VALUES ({placeholders})");
    let pool = capture.get_sqlite_connection_pool().clone();
    let mut connection = pool.acquire().await.map_err(sqlx_io)?;
    for row in rows {
        if row.len() != columns.len() {
            return Err(io::Error::other(format!(
                "capture catalog copy read an unexpected column count from table '{table}'"
            )));
        }
        let mut query = sea_orm::sqlx::query(sea_orm::sqlx::AssertSqlSafe(sql.clone()));
        for cell in row {
            query = bind_cell(query, cell);
        }
        query.execute(&mut *connection).await.map_err(|error| {
            io::Error::other(format!(
                "capture catalog copy could not write table '{table}': {error}"
            ))
        })?;
    }
    Ok(())
}

fn bind_cell<'q>(
    query: sea_orm::sqlx::query::Query<
        'q,
        sea_orm::sqlx::Sqlite,
        sea_orm::sqlx::sqlite::SqliteArguments,
    >,
    cell: &Cell,
) -> sea_orm::sqlx::query::Query<'q, sea_orm::sqlx::Sqlite, sea_orm::sqlx::sqlite::SqliteArguments>
{
    match cell {
        Cell::Null => query.bind(Option::<i64>::None),
        Cell::Integer(value) => query.bind(*value),
        Cell::Real(value) => query.bind(*value),
        Cell::Text(value) => query.bind(value.clone()),
        Cell::Blob(value) => query.bind(value.clone()),
    }
}

#[derive(Clone, Debug)]
enum Cell {
    Null,
    Integer(i64),
    Real(f64),
    Text(String),
    Blob(Vec<u8>),
}

async fn fetch_cells(
    source: &DatabaseConnection,
    sql: &str,
    rowids: &[i64],
) -> io::Result<Vec<Vec<Cell>>> {
    let pool = source.get_sqlite_connection_pool().clone();
    let mut connection = pool.acquire().await.map_err(sqlx_io)?;
    let mut query = sea_orm::sqlx::query(sea_orm::sqlx::AssertSqlSafe(sql.to_owned()));
    for rowid in rowids {
        query = query.bind(*rowid);
    }
    let rows = query.fetch_all(&mut *connection).await.map_err(|error| {
        io::Error::other(format!("capture catalog copy could not read rows: {error}"))
    })?;
    rows_to_cells(&rows)
}

async fn fetch_bound_cells(
    source: &DatabaseConnection,
    sql: &str,
    binds: &[Cell],
) -> io::Result<Vec<Vec<Cell>>> {
    let pool = source.get_sqlite_connection_pool().clone();
    let mut connection = pool.acquire().await.map_err(sqlx_io)?;
    let mut query = sea_orm::sqlx::query(sea_orm::sqlx::AssertSqlSafe(sql.to_owned()));
    for cell in binds {
        query = bind_cell(query, cell);
    }
    let rows = query.fetch_all(&mut *connection).await.map_err(|error| {
        io::Error::other(format!(
            "capture catalog copy could not read keyed rows: {error}"
        ))
    })?;
    rows_to_cells(&rows)
}

fn rows_to_cells(rows: &[sea_orm::sqlx::sqlite::SqliteRow]) -> io::Result<Vec<Vec<Cell>>> {
    let mut decoded = Vec::with_capacity(rows.len());
    for row in rows {
        let mut cells = Vec::with_capacity(row.len());
        for index in 0..row.len() {
            cells.push(decode_cell(row, index)?);
        }
        decoded.push(cells);
    }
    Ok(decoded)
}

fn decode_cell(row: &sea_orm::sqlx::sqlite::SqliteRow, index: usize) -> io::Result<Cell> {
    let type_name = {
        let raw = row.try_get_raw(index).map_err(|error| {
            io::Error::other(format!(
                "capture catalog copy could not decode a column: {error}"
            ))
        })?;
        if raw.is_null() {
            return Ok(Cell::Null);
        }
        raw.type_info().to_string()
    };
    let decode_error = |kind: &str| {
        io::Error::other(format!(
            "capture catalog copy could not decode a {kind} column"
        ))
    };
    match type_name.as_str() {
        "NULL" => Ok(Cell::Null),
        "INTEGER" | "INT4" => row
            .try_get(index)
            .map(Cell::Integer)
            .map_err(|_| decode_error("integer")),
        "BOOLEAN" => row
            .try_get::<bool, _>(index)
            .map(|value| Cell::Integer(i64::from(value)))
            .map_err(|_| decode_error("boolean")),
        "REAL" => row
            .try_get(index)
            .map(Cell::Real)
            .map_err(|_| decode_error("real")),
        "TEXT" | "DATE" | "TIME" | "DATETIME" => row
            .try_get(index)
            .map(Cell::Text)
            .map_err(|_| decode_error("text")),
        "BLOB" => row
            .try_get(index)
            .map(Cell::Blob)
            .map_err(|_| decode_error("blob")),
        "NUMERIC" => {
            if let Ok(value) = row.try_get::<i64, _>(index) {
                Ok(Cell::Integer(value))
            } else if let Ok(value) = row.try_get::<f64, _>(index) {
                Ok(Cell::Real(value))
            } else {
                row.try_get(index)
                    .map(Cell::Text)
                    .map_err(|_| decode_error("numeric"))
            }
        }
        other => Err(io::Error::other(format!(
            "capture catalog copy found an unsupported SQLite type '{other}'"
        ))),
    }
}

async fn query_i64s(source: &DatabaseConnection, sql: &str, bound: i64) -> io::Result<Vec<i64>> {
    let pool = source.get_sqlite_connection_pool().clone();
    let mut connection = pool.acquire().await.map_err(sqlx_io)?;
    sea_orm::sqlx::query_scalar::<_, i64>(sea_orm::sqlx::AssertSqlSafe(sql.to_owned()))
        .bind(bound)
        .fetch_all(&mut *connection)
        .await
        .map_err(|error| {
            io::Error::other(format!(
                "capture catalog copy could not read primary keys: {error}"
            ))
        })
}

async fn table_columns(source: &DatabaseConnection, table: &str) -> io::Result<Vec<String>> {
    let sql = format!("SELECT name FROM pragma_table_info('{table}') ORDER BY cid");
    text_column(source, &sql).await
}

async fn primary_key_columns(source: &DatabaseConnection, table: &str) -> io::Result<Vec<String>> {
    let sql = format!("SELECT name FROM pragma_table_info('{table}') WHERE pk > 0 ORDER BY pk");
    text_column(source, &sql).await
}

async fn table_sql(source: &DatabaseConnection, table: &str) -> io::Result<Option<String>> {
    let row = source
        .query_one_raw(Statement::from_sql_and_values(
            source.get_database_backend(),
            "SELECT sql FROM sqlite_master WHERE type = 'table' AND name = ?",
            [table.into()],
        ))
        .await
        .map_err(|error| {
            io::Error::other(format!(
                "capture catalog copy could not read the schema of table '{table}': {error}"
            ))
        })?;
    match row {
        Some(row) => row.try_get_by_index::<Option<String>>(0).map_err(|error| {
            io::Error::other(format!(
                "capture catalog copy could not read the schema text of table '{table}': {error}"
            ))
        }),
        None => Ok(None),
    }
}

async fn text_column(source: &DatabaseConnection, sql: &str) -> io::Result<Vec<String>> {
    let rows = source
        .query_all_raw(Statement::from_string(
            source.get_database_backend(),
            sql.to_owned(),
        ))
        .await
        .map_err(|error| {
            io::Error::other(format!(
                "capture catalog copy could not read table metadata: {error}"
            ))
        })?;
    rows.into_iter()
        .map(|row| {
            row.try_get_by_index::<String>(0).map_err(|error| {
                io::Error::other(format!(
                    "capture catalog copy could not read a column name: {error}"
                ))
            })
        })
        .collect()
}

async fn table_exists(conn: &DatabaseConnection, table: &str) -> io::Result<bool> {
    db::sqlite_schema_contains(conn, "table", table)
        .await
        .map_err(|error| {
            io::Error::other(format!(
                "capture catalog copy could not inspect table '{table}': {error}"
            ))
        })
}

async fn ensure_capture_schema(conn: &DatabaseConnection) -> io::Result<()> {
    let current = schema::current_schema_version_for_role(conn, DatabaseRole::Capture).await?;
    let latest = schema::latest_schema_version_for_role(DatabaseRole::Capture)?;
    if let Some(current) = current
        && latest.is_none_or(|latest| current > latest)
    {
        return Err(io::Error::other(format!(
            "capture database schema version {current} is newer than this Libra binary supports (latest supported: {}); install a newer Libra binary. The repository database was not changed",
            schema_version_text(latest)
        )));
    }
    if current == latest && current.is_some() {
        return Ok(());
    }
    conn.transaction::<_, _, DbErr>(|txn| {
        Box::pin(async move {
            let backend = txn.get_database_backend();
            txn.execute_raw(Statement::from_string(
                backend,
                schema::CAPTURE_DATABASE_SQL.to_owned(),
            ))
            .await?;
            txn.execute_raw(Statement::from_sql_and_values(
                backend,
                "INSERT OR IGNORE INTO schema_versions (version, name, applied_at) VALUES (?, ?, ?)",
                [
                    schema::CAPTURE_SCHEMA_VERSION.into(),
                    schema::CAPTURE_SCHEMA_NAME.into(),
                    chrono::Utc::now().to_rfc3339().into(),
                ],
            ))
            .await?;
            Ok(())
        })
    })
    .await
    .map_err(|error| {
        io::Error::other(format!(
            "failed to apply the capture database schema; the transaction was rolled back: {error}"
        ))
    })?;
    Ok(())
}

async fn copy_receipt_present(conn: &DatabaseConnection) -> io::Result<bool> {
    if !table_exists(conn, COPY_RECEIPT_TABLE).await? {
        return Ok(false);
    }
    let row = conn
        .query_one_raw(Statement::from_string(
            conn.get_database_backend(),
            format!("SELECT COUNT(*) FROM {COPY_RECEIPT_TABLE}"),
        ))
        .await
        .map_err(|error| {
            io::Error::other(format!(
                "capture database copy receipt could not be read: {error}"
            ))
        })?;
    let row = row.ok_or_else(|| {
        io::Error::other("capture database copy receipt query returned no result")
    })?;
    let count: i64 = row.try_get_by_index(0).map_err(|error| {
        io::Error::other(format!(
            "capture database copy receipt count was invalid: {error}"
        ))
    })?;
    Ok(count > 0)
}

async fn write_copy_receipt(conn: &DatabaseConnection) -> io::Result<()> {
    conn.execute_raw(Statement::from_sql_and_values(
        conn.get_database_backend(),
        "INSERT OR IGNORE INTO capture_catalog_copy_receipt (id, copied_at) VALUES (1, ?)",
        [chrono::Utc::now().to_rfc3339().into()],
    ))
    .await
    .map_err(|error| {
        io::Error::other(format!(
            "capture database copy receipt could not be recorded: {error}"
        ))
    })?;
    Ok(())
}

async fn set_foreign_keys(conn: &DatabaseConnection, enabled: bool) -> io::Result<()> {
    let sql = if enabled {
        "PRAGMA foreign_keys = ON"
    } else {
        "PRAGMA foreign_keys = OFF"
    };
    conn.execute_unprepared(sql).await.map_err(|error| {
        io::Error::other(format!(
            "capture database could not set foreign key enforcement: {error}"
        ))
    })?;
    let row = conn
        .query_one_raw(Statement::from_string(
            conn.get_database_backend(),
            "PRAGMA foreign_keys".to_owned(),
        ))
        .await
        .map_err(|error| {
            io::Error::other(format!(
                "capture database could not read foreign key enforcement: {error}"
            ))
        })?;
    let value: i64 = row
        .ok_or_else(|| io::Error::other("capture database foreign key query returned no result"))?
        .try_get_by_index(0)
        .map_err(|error| {
            io::Error::other(format!(
                "capture database foreign key setting was invalid: {error}"
            ))
        })?;
    let expected = i64::from(enabled);
    if value != expected {
        return Err(io::Error::other(format!(
            "capture database foreign key enforcement stayed {value}; expected {expected}"
        )));
    }
    Ok(())
}

async fn assert_single_database(conn: &DatabaseConnection, role: &str) -> io::Result<()> {
    let row = conn
        .query_one_raw(Statement::from_string(
            conn.get_database_backend(),
            "SELECT COUNT(*) FROM pragma_database_list".to_owned(),
        ))
        .await
        .map_err(|error| {
            io::Error::other(format!(
                "could not list databases on the {role} connection: {error}"
            ))
        })?;
    let count: i64 = row
        .ok_or_else(|| io::Error::other(format!("{role} database list returned no result")))?
        .try_get_by_index(0)
        .map_err(|error| {
            io::Error::other(format!("{role} database list count was invalid: {error}"))
        })?;
    if count != 1 {
        return Err(io::Error::other(format!(
            "the {role} connection is attached to {count} databases; capture copy refuses ATTACH"
        )));
    }
    Ok(())
}

async fn connect_capture(db_path: &Path, busy_timeout: Duration) -> io::Result<DatabaseConnection> {
    connect_capture_file(db_path, busy_timeout, false).await
}

async fn connect_capture_file(
    db_path: &Path,
    busy_timeout: Duration,
    read_only: bool,
) -> io::Result<DatabaseConnection> {
    if !read_only && !db_path.exists() {
        create_private_file(db_path)?;
    }
    let filename = std::path::absolute(db_path).map_err(|error| {
        io::Error::new(
            error.kind(),
            format!(
                "cannot resolve capture database path '{}': {error}",
                db_path.display()
            ),
        )
    })?;
    if filename.to_str().is_none() {
        return Err(io::Error::new(
            ErrorKind::InvalidInput,
            format!(
                "capture database path '{}' is not valid UTF-8; use a UTF-8 path",
                db_path.display()
            ),
        ));
    }
    let mut options = sea_orm::ConnectOptions::new("sqlite://capture-database");
    options.max_connections(1);
    options.sqlx_logging(false);
    options.map_sqlx_sqlite_pool_opts(db::sqlite_pool_options);
    options.map_sqlx_sqlite_opts(move |opts| {
        let opts = opts
            .filename(&filename)
            .read_only(read_only)
            .create_if_missing(false)
            .busy_timeout(busy_timeout)
            .foreign_keys(true);
        // A read-only open cannot apply journal or synchronous pragmas.
        if read_only {
            opts
        } else {
            opts.synchronous(sea_orm::sqlx::sqlite::SqliteSynchronous::Full)
                .journal_mode(sea_orm::sqlx::sqlite::SqliteJournalMode::Delete)
        }
    });
    Database::connect(options).await.map_err(|error| {
        io::Error::other(format!(
            "failed to open capture database '{}': {error}; check the file and its permissions",
            db_path.display()
        ))
    })
}

fn schema_version_text(version: Option<i64>) -> String {
    version
        .map(|value| value.to_string())
        .unwrap_or_else(|| "none".to_string())
}

fn sqlx_io(error: sea_orm::sqlx::Error) -> io::Error {
    io::Error::other(format!("capture database connection failed: {error}"))
}

fn ensure_private_dir(path: &Path) -> io::Result<()> {
    if let Ok(metadata) = fs::symlink_metadata(path) {
        if metadata.file_type().is_symlink() {
            return Err(io::Error::new(
                ErrorKind::AlreadyExists,
                "capture database directory '.libra/private' is a symlink; remove it and retry",
            ));
        }
        if !metadata.is_dir() {
            return Err(io::Error::new(
                ErrorKind::AlreadyExists,
                "capture database path '.libra/private' is not a directory",
            ));
        }
        return Ok(());
    }
    fs::create_dir(path).map_err(|error| {
        io::Error::new(
            error.kind(),
            format!("cannot create capture database directory '.libra/private': {error}"),
        )
    })?;
    set_private_dir_mode(path)
}

fn set_private_dir_mode(path: &Path) -> io::Result<()> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(path, fs::Permissions::from_mode(0o700)).map_err(|error| {
            io::Error::other(format!(
                "cannot set permissions on capture database directory '.libra/private': {error}"
            ))
        })?;
    }
    #[cfg(not(unix))]
    {
        let _ = path;
    }
    Ok(())
}

fn create_private_file(path: &Path) -> io::Result<()> {
    let mut options = fs::OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    options.open(path).map(|_| ()).or_else(|error| {
        if error.kind() == ErrorKind::AlreadyExists {
            Ok(())
        } else {
            Err(io::Error::new(
                error.kind(),
                format!("cannot create capture database '.libra/private/capture.db': {error}"),
            ))
        }
    })
}

fn reject_symlink(path: &Path) -> io::Result<()> {
    if let Ok(metadata) = fs::symlink_metadata(path)
        && metadata.file_type().is_symlink()
    {
        return Err(io::Error::new(
            ErrorKind::AlreadyExists,
            "capture database path is a symlink; remove it and retry",
        ));
    }
    Ok(())
}

fn repository_is_symlink(path: &Path) -> io::Result<bool> {
    match fs::symlink_metadata(path) {
        Ok(metadata) if metadata.file_type().is_symlink() => Ok(true),
        Ok(_) => Ok(false),
        Err(error) if error.kind() == ErrorKind::NotFound => Ok(false),
        Err(error) => Err(error),
    }
}

fn safe_ident(name: &str) -> bool {
    let mut chars = name.chars();
    match chars.next() {
        Some(first) if first.is_ascii_alphabetic() || first == '_' => {}
        _ => return false,
    }
    chars.all(|ch| ch.is_ascii_alphanumeric() || ch == '_')
}
