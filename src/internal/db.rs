//! SQLite connection bootstrapping and schema migration.
//!
//! Responsibilities:
//! - Open SQLite databases under `.libra/libra.db` (per-repo) and
//!   `~/.libra/config.db` (global), cached by path.
//! - Bootstrap the schema from the embedded `sqlite_20260309_init.sql`.
//! - Run idempotent schema upgrades only from explicit creation / upgrade paths:
//!   - [`ensure_config_kv_schema`] adds the `config_kv` table to old DBs.
//!   - [`ensure_ai_projection_schema`] adds the AI projection tables (using
//!     the bootstrap section delimited by `BEGIN/END AI PROJECTION SCHEMA`).
//!   - [`ensure_ai_runtime_contract_schema`] applies Phase 0 contract DDL.
//!   - [`migration::run_builtin_migrations`] (CEX-12.5) applies every
//!     versioned migration registered in
//!     [`migration::builtin_migrations`]. Future schema changes (CEX-13b /
//!     CEX-15 / CEX-16) **must** add a [`migration::Migration`] there
//!     instead of introducing a new ad-hoc `ensure_*_schema` helper.
//! - Provide a process-wide cache ([`TEST_DB_CONNECTIONS`]) keyed by absolute
//!   path so concurrent callers share a single sea-orm `DbConn` per database
//!   (matching SQLite's "one writer at a time" model).
//!
//! Hash and ref invariants are not enforced here; that work lives in the
//! `reference` model and `branch`/`tag` modules.

pub mod migration;

use std::{
    io,
    io::{Error as IOError, ErrorKind},
    path::Path,
    time::Duration,
};

use sea_orm::{
    ConnectOptions, ConnectionTrait, Database, DatabaseConnection, DbConn, DbErr, Statement,
    TransactionError, TransactionTrait,
};

use crate::utils::path;

/// Result of applying repository database schema upgrades.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SchemaUpgradeReport {
    pub previous_version: Option<i64>,
    pub current_version: Option<i64>,
    pub latest_version: Option<i64>,
    pub applied_versions: Vec<i64>,
}

/// Compatibility between an on-disk database and this Libra build.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SchemaCompatibility {
    Compatible {
        current_version: Option<i64>,
        latest_version: Option<i64>,
    },
    UpgradeRequired {
        current_version: Option<i64>,
        latest_version: i64,
    },
    UnsupportedFuture {
        current_version: i64,
        latest_version: Option<i64>,
    },
}

// #[cfg(not(test))]
// use tokio::sync::OnceCell;

/// Normalize a file path for use in a SQLite connection string.
///
/// Boundary conditions:
/// - On Windows, strips the `\\?\` extended-length prefix and converts
///   backslashes to forward slashes so sqlx accepts the URL.
/// - On Unix, returns the input unchanged (allocation-only).
fn normalize_path_for_sqlite(db_path: &str) -> String {
    #[cfg(windows)]
    {
        // Remove Windows extended-length path prefix if present
        let path = db_path.strip_prefix(r"\\?\").unwrap_or(db_path);
        // Convert backslashes to forward slashes for SQLite URL
        path.replace('\\', "/")
    }
    #[cfg(not(windows))]
    {
        db_path.to_string()
    }
}

/// Establish a connection to the database with the default 30-second busy timeout.
///
/// Functional scope: opens the file and brings its schema up to date,
/// automatically applying any pending built-in migrations (see
/// [`ensure_database_schema_is_current`]). Opening the database is therefore
/// sufficient to upgrade it — there is no separate explicit upgrade step.
///
/// Boundary conditions:
/// - Returns `IOError(NotFound)` if the database file does not exist on disk.
/// - A schema *newer* than this binary supports is surfaced as `IOError::other`
///   with an "install a newer Libra binary" hint (it cannot be migrated down).
#[allow(dead_code)]
pub async fn establish_connection(db_path: &str) -> Result<DatabaseConnection, IOError> {
    establish_connection_with_busy_timeout(db_path, Duration::from_secs(30)).await
}

/// Establish a SQLite connection with a caller-specified busy timeout.
///
/// This is useful for best-effort/background jobs that should fail fast on lock
/// contention instead of waiting for long periods. Like
/// [`establish_connection`], it brings the schema up to date by applying any
/// pending migrations on open.
#[allow(dead_code)]
pub async fn establish_connection_with_busy_timeout(
    db_path: &str,
    busy_timeout: Duration,
) -> Result<DatabaseConnection, IOError> {
    let conn = open_connection_without_schema_management(db_path, busy_timeout).await?;
    ensure_database_schema_is_current(&conn).await?;
    Ok(conn)
}

/// Open a SQLite connection WITHOUT inspecting or migrating the schema.
///
/// For best-effort READ paths that must never mutate the store they read
/// (W5-09 / Codex r12: the fresh-connection config-cascade reader opens
/// per-lookup connections with a 200 ms busy timeout — letting that path
/// apply pending migrations under such a timeout could half-apply or
/// spuriously fail against a concurrent writer). Callers own the
/// consequence: a query may fail against an incompatible schema and must
/// treat that as a read failure, not as "no value".
pub(crate) async fn open_connection_without_schema_management(
    db_path: &str,
    busy_timeout: Duration,
) -> Result<DatabaseConnection, IOError> {
    if !Path::new(db_path).exists() {
        return Err(IOError::new(
            ErrorKind::NotFound,
            "Database file does not exist.",
        ));
    }

    let normalized_path = normalize_path_for_sqlite(db_path);
    let mut option = ConnectOptions::new(format!("sqlite://{normalized_path}"));
    option.sqlx_logging(false); // TODO use better option
    // Recovery-critical durability (lore.md 2.6 / _general.md §12): the
    // sequencer, refs, reflog, and config all live here, so every commit MUST
    // reach disk — pin `synchronous = FULL` explicitly rather than relying on
    // SQLite's journal-mode-dependent default (a future WAL adoption would
    // silently drop it to NORMAL). The `--sync-data` switch never weakens it.
    // POOL SIZE — an invariant callers must know about. sea-orm pins SQLite
    // pools to `max_connections(1)` whenever the caller leaves it unset, and
    // this code leaves it unset deliberately: one connection is what makes
    // read-then-write sequences safe without a second serialization layer
    // (see `begin_write_transaction`). The consequence is that the single
    // connection is a process-wide resource, so:
    //
    //   NEVER make a BLOCKING call on a runtime worker thread while a pooled
    //   connection may be checked out or in the middle of being returned.
    //
    // sqlx returns a connection by SPAWNING a task; a spawn from inside a
    // poll lands in that worker's non-stealable LIFO slot, so blocking the
    // worker strands the return and every other DB user in the process waits
    // out the acquire timeout. `libra service`'s dirty-mark handler wedged
    // exactly this way (it took a blocking `flock` right after a query) and
    // now takes that lock via `spawn_blocking`. Raising the pool size would
    // change SQLite read-then-write contention semantics product-wide and is
    // deliberately NOT done as a side effect of that fix.
    option.map_sqlx_sqlite_opts(move |sqlx_opts| {
        sqlx_opts
            .busy_timeout(busy_timeout)
            .synchronous(sea_orm::sqlx::sqlite::SqliteSynchronous::Full)
    });
    Database::connect(option)
        .await
        .map_err(|err| IOError::other(format!("Database connection error: {err:?}")))
}
// #[cfg(not(test))]
// static DB_CONN: OnceCell<DbConn> = OnceCell::const_new();

// /// Get global database connection instance (singleton)
// #[cfg(not(test))]
// pub async fn get_db_conn_instance() -> &'static DbConn {
//     DB_CONN
//         .get_or_init(|| async { get_db_conn().await.unwrap() })
//         .await
// }

// #[cfg(test)]
// #[cfg(test)]
use std::collections::HashMap;
//#[cfg(test)]
//use std::ops::Deref;
// #[cfg(test)]
use std::path::PathBuf;

use once_cell::sync::Lazy;
// #[cfg(test)]
use tokio::sync::Mutex;

/// Shared sea-orm connections cached by absolute database path.
///
/// Despite the historical `TEST_` prefix, this cache is used in production
/// too. Sharing one connection per file matches SQLite's lock model and lets
/// callers run multiple concurrent reads without re-opening the file.
static TEST_DB_CONNECTIONS: Lazy<Mutex<HashMap<PathBuf, DbConn>>> =
    Lazy::new(|| Mutex::new(HashMap::new()));

/// Lookup-or-create routine for [`TEST_DB_CONNECTIONS`].
///
/// Functional scope:
/// - Verifies the file exists; missing files evict any stale cache entry and
///   return `IOError(NotFound)`.
/// - Returns a clone of the cached `DbConn` on hit.
/// - On miss, opens a new connection — automatically upgrading the schema to
///   this build via [`establish_connection`] — and re-acquires the lock to
///   publish it. The double-check pattern is used to avoid two threads racing
///   to install the same connection.
async fn get_or_init_db_conn_instance(db_path: PathBuf) -> io::Result<DbConn> {
    let mut connections = TEST_DB_CONNECTIONS.lock().await;

    if !db_path.exists() {
        connections.remove(&db_path);
        return Err(IOError::new(
            ErrorKind::NotFound,
            format!("Database file does not exist: {}", db_path.display()),
        ));
    }

    if let Some(conn) = connections.get(&db_path) {
        let conn = conn.clone();
        drop(connections);
        // The fence must hold for CACHED connections too (W2 r5 #6): a
        // long-lived process (the `libra code` server) whose cache predates
        // another process's migration would otherwise keep writing under a
        // schema its code does not know. One indexed MAX() on a tiny table
        // per hit; an upgrade requirement or a future schema evicts the
        // entry and re-resolves.
        //
        // SCOPE of the guarantee: this fences every acquisition THROUGH THIS
        // CACHE. A different (old) binary is fenced by its own open-time
        // check at its next connect; a connection handle an old process
        // already holds is inherently beyond any new binary's reach — the
        // migration protocol's claim-first transaction is what bounds the
        // damage there (a schema rebuild fails the old writer's statements
        // rather than accepting them).
        match inspect_database_schema_for_connection(&conn).await {
            Ok(SchemaCompatibility::Compatible { .. }) => return Ok(conn),
            // PROVEN incompatibility: evict, and CLOSE the shared pool so
            // every already-returned clone is refused too (W2 r7 #4). The
            // close is spawned: `Pool::close` marks the pool closed at once
            // (clones fail from that moment) but then WAITS for outstanding
            // connections — awaiting that here while another task holds an
            // open transaction on this pool would deadlock the process.
            Ok(_) => {
                let mut connections = TEST_DB_CONNECTIONS.lock().await;
                let evicted = connections.remove(&db_path);
                drop(connections);
                // The pool must be MARKED closed before this function
                // returns, or a concurrent holder of an evicted clone can
                // still execute SQL in the window before a spawned task runs
                // (W2 r8 #3). `Pool::close` marks the pool on its FIRST poll
                // and only then waits for outstanding connections — so a
                // zero-duration timeout polls it exactly once: the marking is
                // synchronous with this call, every clone's next statement is
                // refused from here on, and the drain (which can only end
                // when open transactions conclude — awaiting it inline
                // deadlocks) is abandoned to the connections' own drops.
                if let Some(stale) = evicted {
                    let _ =
                        tokio::time::timeout(std::time::Duration::from_millis(0), stale.close())
                            .await;
                }
                let _ =
                    tokio::time::timeout(std::time::Duration::from_millis(0), conn.close()).await;
                // Fall through: re-establish, which re-checks and either
                // upgrades or reports the future schema with its hint.
            }
            // A FAILED inspection is not evidence of incompatibility — a
            // legitimate writer's open transaction makes this read report
            // BUSY, and evicting (let alone closing) on that would tear the
            // pool out from under the writer. Hand back the cached
            // connection; the next acquisition re-checks.
            Err(error) => {
                tracing::debug!(
                    db_path = %db_path.display(),
                    error = %error,
                    "schema re-check on a cached connection failed transiently; keeping it"
                );
                return Ok(conn);
            }
        }
    } else {
        drop(connections);
    }

    let conn = get_db_conn_for_path(&db_path).await?;

    let mut connections = TEST_DB_CONNECTIONS.lock().await;
    if let Some(existing) = connections.get(&db_path) {
        return Ok(existing.clone());
    }
    connections.insert(db_path, conn.clone());
    Ok(conn)
}

/// The one statement that turns SQLite's DEFERRED `BEGIN` into a write
/// transaction without writing anything: any DML acquires the database write
/// lock before it evaluates its `WHERE`, so a clause that matches no row still
/// takes the lock and still changes nothing. `config_kv` is created by the
/// bootstrap schema, so it exists in every repository database.
const ACQUIRE_WRITE_LOCK_SQL: &str = "UPDATE `config_kv` SET `id` = `id` WHERE 0";

/// Begin a transaction that already holds SQLite's write lock.
///
/// SQLite will NOT run the busy handler for a transaction that holds a read
/// lock and then asks for the write lock: two such upgraders could deadlock
/// each other, so it returns `SQLITE_BUSY` ("database is locked") immediately
/// and `busy_timeout` never applies. A transaction that reads before it writes
/// — the natural shape of "look up the ref, then move it" — therefore FAILS
/// outright whenever another process happens to be writing, instead of waiting
/// its turn. Two worktrees rebasing at the same moment hit exactly this.
///
/// sea-orm cannot emit `BEGIN IMMEDIATE` for SQLite (`set_transaction_config`
/// warns and ignores both isolation level and access mode), so the equivalent
/// is a no-op write as the transaction's FIRST statement: the lock is acquired
/// while no read lock is held, which is the case the busy handler DOES cover,
/// so a losing writer queues for its `busy_timeout` instead of erroring.
///
/// Use this for any transaction that reads before it writes. A transaction
/// whose first statement is already a write needs nothing.
pub async fn begin_write_transaction<
    C: TransactionTrait<Transaction = sea_orm::DatabaseTransaction>,
>(
    db: &C,
) -> Result<sea_orm::DatabaseTransaction, DbErr> {
    let txn = db.begin().await?;
    // Every failure propagates, including a missing `config_kv`. Swallowing
    // that one would silently downgrade to a deferred transaction in exactly
    // the database that most needs reporting: the bootstrap creates the table
    // and the upgrade path ensures it before any migration runs, so a
    // current-schema repository without it is corrupt — and a
    // schema-compatible open returns early without repairing it. A named
    // failure beats intermittent "database is locked" on every read-then-write
    // command.
    if let Err(err) = txn.execute_unprepared(ACQUIRE_WRITE_LOCK_SQL).await {
        let _ = txn.rollback().await;
        return Err(err);
    }
    Ok(txn)
}

/// `ConnectionTrait::transaction` with the write lock taken up front.
///
/// Same shape and same `TransactionError` mapping as sea-orm's own, so a call
/// site converts by replacing `db.transaction(...)` with
/// `db::write_transaction(&db, ...)`. Use it wherever the closure reads before
/// it writes — see [`begin_write_transaction`] for why that ordering fails
/// instead of waiting.
pub async fn write_transaction<C, F, T, E>(db: &C, callback: F) -> Result<T, TransactionError<E>>
where
    C: TransactionTrait<Transaction = sea_orm::DatabaseTransaction>,
    F: for<'c> FnOnce(
            &'c sea_orm::DatabaseTransaction,
        ) -> std::pin::Pin<
            Box<dyn std::future::Future<Output = Result<T, E>> + Send + 'c>,
        > + Send,
    T: Send,
    E: std::error::Error + Send,
{
    let txn = begin_write_transaction(db)
        .await
        .map_err(TransactionError::Connection)?;
    match callback(&txn).await {
        Ok(value) => {
            txn.commit().await.map_err(TransactionError::Connection)?;
            Ok(value)
        }
        Err(err) => {
            let _ = txn.rollback().await;
            Err(TransactionError::Transaction(err))
        }
    }
}

/// Get global database connection instance for the current repository.
///
/// Functional scope: discovers `.libra/libra.db` via [`path::database`] and
/// returns a shared sea-orm connection from the process-wide cache.
///
/// Boundary conditions:
/// - **Panics** when the database is missing or cannot be opened. This is the
///   convenience entry point used by every command after `libra init`; the
///   panic message includes the resolved path and the underlying error.
/// - TODO(error): migrate legacy call sites to `get_db_conn_instance_for_path`
///   and make this wrapper return `io::Result` instead of panicking.
pub async fn get_db_conn_instance() -> DbConn {
    let db_path = path::database();
    get_db_conn_instance_for_path(&db_path)
        .await
        .unwrap_or_else(|err| panic!("Failed to open database {}: {}", db_path.display(), err))
}

/// Get a shared database connection instance for an explicit SQLite file path.
///
/// The connection is cached, so concurrent callers see the same handle.
/// Returns `Err(IOError)` when the file is missing or the schema migrations
/// fail.
pub async fn get_db_conn_instance_for_path(db_path: &Path) -> io::Result<DbConn> {
    get_or_init_db_conn_instance(db_path.to_path_buf()).await
}

/// Drop a cached shared connection for an explicit SQLite file path.
///
/// Used by tests and by `libra config` flows that recreate the underlying
/// database. Logs (but does not surface) any errors raised while closing the
/// connection — the cache entry is removed regardless.
pub async fn reset_db_conn_instance_for_path(db_path: &Path) {
    let mut connections = TEST_DB_CONNECTIONS.lock().await;
    let removed = connections.remove(db_path);
    drop(connections);

    if let Some(conn) = removed
        && let Err(err) = conn.close().await
    {
        tracing::warn!(
            db_path = %db_path.display(),
            error = %err,
            "Failed to close cached database connection during reset"
        );
    }
}

/// Internal: convert a `Path` to a UTF-8 string and call [`establish_connection`].
async fn get_db_conn_for_path(db_path: &Path) -> io::Result<DatabaseConnection> {
    let db_path = db_path.to_str().ok_or_else(|| {
        IOError::new(
            ErrorKind::InvalidData,
            format!("Database path is not valid UTF-8: {}", db_path.display()),
        )
    })?;
    establish_connection(db_path).await
}

/// Open an existing SQLite database without applying any schema changes.
///
/// Used by read-only inspection paths (e.g. schema-version queries and the
/// read-only `hash-object` hash-kind preflight) that must observe the schema
/// exactly as stored, without triggering the automatic upgrade that
/// [`establish_connection`] performs.
pub async fn open_database_without_migrations(db_path: &Path) -> io::Result<DatabaseConnection> {
    if !db_path.exists() {
        return Err(IOError::new(
            ErrorKind::NotFound,
            format!("Database file does not exist: {}", db_path.display()),
        ));
    }
    let db_path = db_path.to_str().ok_or_else(|| {
        IOError::new(
            ErrorKind::InvalidData,
            format!("Database path is not valid UTF-8: {}", db_path.display()),
        )
    })?;
    connect_database(db_path).await
}

/// Inspect whether an existing repository DB can be used by this Libra build.
///
/// This function is intentionally read-only. It does not create
/// `schema_versions`, run idempotent DDL, or apply pending migrations.
pub async fn inspect_database_schema(db_path: &Path) -> io::Result<SchemaCompatibility> {
    let conn = open_database_without_migrations(db_path).await?;
    inspect_database_schema_for_connection(&conn).await
}

/// Explicitly upgrade an existing repository database to the schema known by this
/// Libra build.
pub async fn upgrade_database_schema(db_path: &Path) -> io::Result<SchemaUpgradeReport> {
    let conn = open_database_without_migrations(db_path).await?;
    apply_database_schema_upgrades(&conn).await
}

async fn inspect_database_schema_for_connection(
    conn: &DatabaseConnection,
) -> io::Result<SchemaCompatibility> {
    let current = migration::current_builtin_schema_version_readonly(conn)
        .await
        .map_err(|err| IOError::other(format!("Failed to read schema version: {err}")))?;
    let latest = migration::latest_builtin_schema_version()
        .map_err(|err| IOError::other(format!("Failed to inspect built-in migrations: {err}")))?;

    match (current, latest) {
        (_, None) => Ok(SchemaCompatibility::Compatible {
            current_version: current,
            latest_version: latest,
        }),
        (Some(current), Some(latest)) if current == latest => Ok(SchemaCompatibility::Compatible {
            current_version: Some(current),
            latest_version: Some(latest),
        }),
        (Some(current), Some(latest)) if current > latest => {
            Ok(SchemaCompatibility::UnsupportedFuture {
                current_version: current,
                latest_version: Some(latest),
            })
        }
        (current, Some(latest)) => Ok(SchemaCompatibility::UpgradeRequired {
            current_version: current,
            latest_version: latest,
        }),
    }
}

/// Bring the connected database's schema up to date, applying any pending
/// built-in migrations automatically.
///
/// This is the single point where an older repository is migrated forward:
/// every pooled connection passes through here, so simply opening the database
/// for any command upgrades it in place (there is no separate `libra db
/// upgrade` step). A schema that is *newer* than this binary understands cannot
/// be migrated down and remains a hard error directing the user to install a
/// newer Libra.
async fn ensure_database_schema_is_current(conn: &DatabaseConnection) -> io::Result<()> {
    match inspect_database_schema_for_connection(conn).await? {
        SchemaCompatibility::Compatible { .. } => Ok(()),
        SchemaCompatibility::UpgradeRequired {
            current_version,
            latest_version,
        } => {
            tracing::info!(
                current = ?current_version,
                latest = latest_version,
                "repository database schema is out of date; applying pending migrations"
            );
            apply_database_schema_upgrades(conn).await?;
            Ok(())
        }
        SchemaCompatibility::UnsupportedFuture {
            current_version,
            latest_version,
        } => Err(IOError::other(format!(
            "repository database schema version {current_version} is newer than this Libra binary supports (latest supported: {})",
            format_schema_version(latest_version)
        ))),
    }
}

fn format_schema_version(version: Option<i64>) -> String {
    version
        .map(|value| value.to_string())
        .unwrap_or_else(|| "none".to_string())
}

/// Embedded canonical SQLite schema. Compiled into the binary via `include_str!`.
const BOOTSTRAP_SQL: &str = include_str!("../../sql/sqlite_20260309_init.sql");
/// Phase 0 AI runtime contract migration; safe to run repeatedly.
const AI_RUNTIME_CONTRACT_MIGRATION_SQL: &str =
    include_str!("../../sql/sqlite_20260415_ai_runtime_contract.sql");
const AI_PROJECTION_SCHEMA_START: &str = "-- BEGIN AI PROJECTION SCHEMA";
/// Marker delimiting the end of the AI projection schema inside `BOOTSTRAP_SQL`.
const AI_PROJECTION_SCHEMA_END: &str = "-- END AI PROJECTION SCHEMA";

/// Apply the entire bootstrap SQL to a fresh database in a single transaction.
///
/// Used by [`create_database`]. Existing databases use the idempotent
/// `ensure_*` migrators instead.
async fn setup_database_sql(conn: &DatabaseConnection) -> Result<(), TransactionError<DbErr>> {
    conn.transaction::<_, _, DbErr>(|txn| {
        Box::pin(async move {
            let backend = txn.get_database_backend();

            // `include_str!` will expand the file while compiling, so `.sql` is not needed after that
            txn.execute_raw(Statement::from_string(backend, BOOTSTRAP_SQL))
                .await?;
            Ok(())
        })
    })
    .await
}

/// Extract the AI projection section from `BOOTSTRAP_SQL`.
///
/// Functional scope: locates the `BEGIN/END AI PROJECTION SCHEMA` markers and
/// returns the text between them, trimmed.
///
/// Boundary conditions:
/// - Returns `IOError(InvalidData)` if either marker is missing or the section
///   is empty (which would indicate a corrupt bootstrap SQL file).
fn ai_projection_sql() -> io::Result<&'static str> {
    let start = BOOTSTRAP_SQL
        .find(AI_PROJECTION_SCHEMA_START)
        .ok_or_else(|| {
            IOError::new(
                ErrorKind::InvalidData,
                format!("Bootstrap schema is missing marker: {AI_PROJECTION_SCHEMA_START}"),
            )
        })?;
    let start = start + AI_PROJECTION_SCHEMA_START.len();
    let end = BOOTSTRAP_SQL[start..]
        .find(AI_PROJECTION_SCHEMA_END)
        .ok_or_else(|| {
            IOError::new(
                ErrorKind::InvalidData,
                format!("Bootstrap schema is missing marker: {AI_PROJECTION_SCHEMA_END}"),
            )
        })?;
    let sql = BOOTSTRAP_SQL[start..start + end].trim();
    if sql.is_empty() {
        return Err(IOError::new(
            ErrorKind::InvalidData,
            "Bootstrap schema AI projection section is empty.",
        ));
    }

    Ok(sql)
}

async fn sqlite_schema_contains(
    conn: &DatabaseConnection,
    entry_type: &str,
    name: &str,
) -> Result<bool, DbErr> {
    let backend = conn.get_database_backend();
    let stmt = Statement::from_sql_and_values(
        backend,
        "SELECT 1 FROM sqlite_master WHERE type = ? AND name = ? LIMIT 1",
        [entry_type.into(), name.into()],
    );
    let row = conn.query_one_raw(stmt).await?;
    Ok(row.is_some())
}

/// Ensure the `config_kv` table exists in the database.
/// Existing databases (created before this table was added) will have the table
/// created on first connection, similar to the AI projection schema pattern.
async fn ensure_config_kv_schema(conn: &DatabaseConnection) -> Result<(), IOError> {
    if sqlite_schema_contains(conn, "table", "config_kv")
        .await
        .map_err(|err| IOError::other(format!("Failed to inspect config_kv schema: {err}")))?
    {
        return Ok(());
    }

    let backend = conn.get_database_backend();
    let ddl = r#"
 CREATE TABLE IF NOT EXISTS `config_kv` (
     `id` INTEGER PRIMARY KEY AUTOINCREMENT,
     `key` TEXT NOT NULL,
     `value` TEXT NOT NULL,
     `encrypted` INTEGER NOT NULL DEFAULT 0
 );
 CREATE INDEX IF NOT EXISTS idx_config_kv_key ON config_kv(`key`);
 "#;
    conn.execute_raw(Statement::from_string(backend, ddl))
        .await
        .map_err(|err| IOError::other(format!("Failed to create config_kv table: {err}")))?;
    Ok(())
}

async fn ensure_ai_projection_schema(conn: &DatabaseConnection) -> Result<(), IOError> {
    if !sqlite_schema_contains(conn, "table", "object_index")
        .await
        .map_err(|err| IOError::other(format!("Failed to inspect core schema: {err}")))?
    {
        let backend = conn.get_database_backend();
        conn.execute_raw(Statement::from_string(backend, BOOTSTRAP_SQL))
            .await
            .map_err(|err| IOError::other(format!("Failed to bootstrap SQLite schema: {err}")))?;
        return Ok(());
    }

    let has_ai_table = sqlite_schema_contains(conn, "table", "ai_index_intent_context_frame")
        .await
        .map_err(|err| IOError::other(format!("Failed to inspect AI schema: {err}")))?;
    let has_ai_index = sqlite_schema_contains(conn, "index", "uq_ai_thread_intent_intent")
        .await
        .map_err(|err| IOError::other(format!("Failed to inspect AI schema: {err}")))?;

    if has_ai_table && has_ai_index {
        return Ok(());
    }

    let backend = conn.get_database_backend();
    conn.execute_raw(Statement::from_string(backend, ai_projection_sql()?))
        .await
        .map_err(|err| IOError::other(format!("Failed to apply AI projection schema: {err}")))?;
    Ok(())
}

/// Ensure Phase 0 AI runtime contract read-model tables exist.
///
/// This migration is safe to run repeatedly. Fresh databases get the same DDL
/// from the bootstrap schema; deployed databases get the idempotent migration
/// here on first connection.
pub async fn ensure_ai_runtime_contract_schema(conn: &DatabaseConnection) -> Result<(), IOError> {
    let backend = conn.get_database_backend();
    conn.execute_raw(Statement::from_string(
        backend,
        AI_RUNTIME_CONTRACT_MIGRATION_SQL,
    ))
    .await
    .map_err(|err| IOError::other(format!("Failed to apply AI runtime contract schema: {err}")))?;
    Ok(())
}

async fn connect_database(db_path: &str) -> io::Result<DatabaseConnection> {
    let normalized_path = normalize_path_for_sqlite(db_path);
    let mut option = ConnectOptions::new(format!("sqlite://{normalized_path}"));
    option.sqlx_logging(false); // TODO use better option
    Database::connect(option)
        .await
        .map_err(|err| IOError::other(format!("Database connection error: {err:?}")))
}

async fn apply_database_schema_upgrades(
    conn: &DatabaseConnection,
) -> io::Result<SchemaUpgradeReport> {
    let previous_version = migration::current_builtin_schema_version_readonly(conn)
        .await
        .map_err(|err| IOError::other(format!("Failed to read schema version: {err}")))?;
    let latest_version = migration::latest_builtin_schema_version()
        .map_err(|err| IOError::other(format!("Failed to inspect built-in migrations: {err}")))?;

    ensure_config_kv_schema(conn)
        .await
        .map_err(|err| IOError::other(format!("Failed to ensure config_kv schema: {err}")))?;
    ensure_ai_projection_schema(conn)
        .await
        .map_err(|err| IOError::other(format!("Failed to ensure AI projection schema: {err}")))?;
    ensure_ai_runtime_contract_schema(conn)
        .await
        .map_err(|err| {
            IOError::other(format!(
                "Failed to ensure AI runtime contract schema: {err}"
            ))
        })?;
    // CEX-12.5: apply every migration registered in
    // `migration::builtin_migrations`. The runner is idempotent — on a
    // fresh DB or a legacy DB it ensures the `schema_versions` tracking
    // table and runs only the migrations whose `version` is not already
    // recorded. Future persistence-touching CEXes plug in by adding to
    // `builtin_migrations`; no new `ensure_*_schema` helpers should be
    // added here.
    let applied_versions = migration::run_builtin_migrations(conn)
        .await
        .map_err(|err| IOError::other(format!("Failed to run schema migrations: {err}")))?;
    let current_version = migration::current_builtin_schema_version_readonly(conn)
        .await
        .map_err(|err| IOError::other(format!("Failed to read schema version: {err}")))?;

    Ok(SchemaUpgradeReport {
        previous_version,
        current_version,
        latest_version,
        applied_versions,
    })
}

/// Create a new SQLite database file at the specified path.
/// **should only be called in init or test**
/// - `db_path` is the path to the SQLite database file.
/// - Returns `Ok(())` if the database file was created and the schema was set up successfully.
/// - Returns an `IOError` if the database file already exists, or if there was an error creating the file or setting up the schema.
#[allow(dead_code)]
pub async fn create_database(db_path: &str) -> io::Result<DatabaseConnection> {
    if Path::new(db_path).exists() {
        return Err(IOError::new(
            ErrorKind::AlreadyExists,
            "Database file already exists.",
        ));
    }

    std::fs::File::create(db_path)
        .map_err(|err| IOError::other(format!("Failed to create database file: {err:?}")))?;

    // Connect to the new database and set up the schema.
    match connect_database(db_path).await {
        Ok(conn) => {
            setup_database_sql(&conn)
                .await
                .map_err(|err| IOError::other(format!("Failed to setup database: {err:?}")))?;
            // CEX-12.5 P1#2 fix (Codex r3): the fresh-init path must run
            // the migration runner so freshly created databases have the
            // `schema_versions` bookkeeping table and any registered
            // built-in migrations applied. Without this call, callers like
            // `libra init` would create a DB whose schema diverges from a
            // reconnected DB until the first `establish_connection` ran
            // the migrations belatedly. The acceptance criterion in
            // `docs/development/tracing/agent.md` line 313 requires fresh and
            // existing repos to converge to the same schema after init.
            apply_database_schema_upgrades(&conn).await.map_err(|err| {
                IOError::other(format!(
                    "Failed to run schema migrations on fresh database: {err}"
                ))
            })?;
            Ok(conn)
        }
        _ => Err(IOError::other("Failed to connect to new database.")),
    }
}

#[cfg(test)]
mod tests {
    use std::{fs, path::PathBuf, sync::Arc};

    use sea_orm::{
        ActiveModelTrait, ActiveValue::NotSet, ColumnTrait, EntityTrait, QueryFilter, Set,
    };
    use tokio::sync::Barrier;

    use super::*;
    use crate::internal::model::{
        config, object_index,
        reference::{self, ConfigKind},
    };

    /// TestDbPath is a helper struct create and delete test database file
    struct TestDbPath(String);
    impl Drop for TestDbPath {
        fn drop(&mut self) {
            if Path::new(&self.0).exists() {
                let _ = fs::remove_file(&self.0);
            }
        }
    }
    impl TestDbPath {
        async fn new(name: &str) -> Self {
            let mut db_path = std::env::temp_dir();
            db_path.push("test_db");
            if !db_path.exists() {
                let _ = fs::create_dir_all(&db_path);
            }
            db_path.push(name);
            let db_path_str = db_path.to_str().unwrap().to_string();
            if db_path.exists() {
                let _ = fs::remove_file(&db_path);
            }
            let rt = TestDbPath(db_path_str);
            create_database(rt.0.as_str()).await.unwrap();
            rt
        }
    }

    /// The defect `begin_write_transaction` exists for, and the fix, on ONE
    /// database file through two independent connections — which is what two
    /// processes in two worktrees are.
    ///
    /// Both halves must run: without the first, the second proves nothing (a
    /// transaction that never contends succeeds either way), and without the
    /// second, the first is just a description of SQLite.
    #[tokio::test(flavor = "multi_thread", worker_threads = 3)]
    async fn a_read_then_write_transaction_cannot_wait_but_a_write_first_one_can() {
        let test_db = TestDbPath::new("write_lock_upgrade.db").await;
        let holder = establish_connection(&test_db.0).await.unwrap();
        let waiter = establish_connection(&test_db.0).await.unwrap();

        // The holder takes the write lock and keeps it.
        let held = holder.begin().await.unwrap();
        held.execute_unprepared("INSERT INTO `config_kv` (`key`, `value`) VALUES ('a', '1')")
            .await
            .unwrap();

        // A DEFERRED transaction that reads first: the busy timeout is 30
        // seconds, and it still fails at once, because SQLite does not run the
        // busy handler for a read→write upgrade.
        let upgrading = waiter.begin().await.unwrap();
        upgrading
            .query_all_raw(Statement::from_string(
                sea_orm::DatabaseBackend::Sqlite,
                "SELECT `id` FROM `config_kv`",
            ))
            .await
            .unwrap();
        let started = std::time::Instant::now();
        let upgrade = upgrading
            .execute_unprepared("INSERT INTO `config_kv` (`key`, `value`) VALUES ('b', '2')")
            .await;
        let elapsed = started.elapsed();
        let error = upgrade.expect_err("the upgrade cannot take a lock another writer holds");
        assert!(
            error.to_string().to_ascii_lowercase().contains("locked"),
            "and it fails BECAUSE of the lock: {error}"
        );
        assert!(
            elapsed < Duration::from_secs(5),
            "immediately, not after the 30s busy timeout — that is the defect: {elapsed:?}"
        );
        let _ = upgrading.rollback().await;

        // Release the lock shortly, so the write-first transaction below has
        // something to wait FOR rather than something to walk into.
        let release = tokio::spawn(async move {
            tokio::time::sleep(Duration::from_millis(300)).await;
            held.commit().await.unwrap();
        });

        // The same read-then-write body, with the lock taken up front: it
        // queues behind the holder and succeeds.
        let started = std::time::Instant::now();
        let txn = begin_write_transaction(&waiter).await.unwrap();
        txn.query_all_raw(Statement::from_string(
            sea_orm::DatabaseBackend::Sqlite,
            "SELECT `id` FROM `config_kv`",
        ))
        .await
        .unwrap();
        txn.execute_unprepared("INSERT INTO `config_kv` (`key`, `value`) VALUES ('b', '2')")
            .await
            .unwrap();
        txn.commit().await.unwrap();
        assert!(
            started.elapsed() >= Duration::from_millis(250),
            "it WAITED for the holder rather than racing past it"
        );
        release.await.unwrap();

        let rows = waiter
            .query_all_raw(Statement::from_string(
                sea_orm::DatabaseBackend::Sqlite,
                "SELECT `key` FROM `config_kv` WHERE `key` IN ('a', 'b') ORDER BY `key`",
            ))
            .await
            .unwrap();
        assert_eq!(rows.len(), 2, "both writers' rows are present");
    }

    /// W2 r6 #5: the REAL lifecycle of the cache fence — a connection this
    /// process already cached is refused (and evicted) the moment the
    /// database's recorded schema moves past what this binary registers.
    #[tokio::test]
    async fn a_cached_connection_is_refused_after_the_schema_moves_ahead() {
        let test_db = TestDbPath::new("cache_fence_lifecycle.db").await;
        let path = std::path::PathBuf::from(&test_db.0);

        // Populate the cache.
        let cached = get_db_conn_instance_for_path(&path)
            .await
            .expect("first acquisition caches");

        // Another actor moves the schema ahead of this binary. The write goes
        // through the CACHED handle on purpose: the file is the same either
        // way, and this proves the eviction is not relying on a fresh open.
        cached
            .execute_unprepared(
                "INSERT INTO schema_versions (version, name, applied_at) \
                  VALUES (2126010101, 'from-the-future', datetime('now'))",
            )
            .await
            .expect("plant a future version");

        // The NEXT acquisition through the cache must refuse, not hand the
        // stale-schema handle back.
        let error = get_db_conn_instance_for_path(&path)
            .await
            .expect_err("a cache hit over a future schema must refuse");
        assert!(
            error
                .to_string()
                .contains("newer than this Libra binary supports"),
            "the refusal says why: {error}"
        );

        // And it stays refused — the eviction did not accidentally re-cache.
        get_db_conn_instance_for_path(&path)
            .await
            .expect_err("still refused on the following acquisition");

        // The RETAINED clone is dead too (W2 r7 #4): eviction closed the
        // shared pool, so the old handle cannot keep writing under the stale
        // schema from inside this process.
        cached
            .execute_unprepared("INSERT INTO config_kv (key, value) VALUES ('x', 'y')")
            .await
            .expect_err("the pre-eviction clone must be refused as well");
        reset_db_conn_instance_for_path(&path).await;
    }

    #[tokio::test]
    async fn test_create_database() {
        let mut db_path_buf = std::env::temp_dir();
        db_path_buf.push("test_create_database.db");
        let db_path = db_path_buf.to_str().unwrap();

        if Path::new(db_path).exists() {
            fs::remove_file(db_path).unwrap();
        }
        let conn = create_database(db_path).await.unwrap();
        assert!(Path::new(db_path).exists());

        let result = create_database(db_path).await;
        assert!(result.is_err());

        conn.close().await.unwrap();
        fs::remove_file(db_path).unwrap();
    }

    #[tokio::test]
    #[allow(deprecated)]
    async fn test_insert_config() {
        // insert into config_entry & config_section, check foreign key constraint
        let test_db = TestDbPath::new("test_insert_config.db").await;
        let db_path = test_db.0.as_str();

        let conn = establish_connection(db_path).await.unwrap();
        // test insert config without name
        {
            let entries = [
                ("repositoryformatversion", "0"),
                ("filemode", "true"),
                ("bare", "false"),
                ("logallrefupdates", "true"),
            ];
            for (key, value) in entries.iter() {
                let entry = config::ActiveModel {
                    configuration: Set("core".to_string()),
                    name: Set(None),
                    key: Set(key.to_string()),
                    value: Set(value.to_string()),
                    ..Default::default()
                };
                let config = entry.save(&conn).await.unwrap();
                assert_eq!(config.key.unwrap(), key.to_string());
            }
            let result = config::Entity::find().all(&conn).await.unwrap();
            assert_eq!(result.len(), entries.len(), "config_section count is not 1");
        }
        // test insert config with name
        {
            let entry = config::ActiveModel {
                id: NotSet,
                configuration: Set("remote".to_string()),
                name: Set(Some("origin".to_string())),
                key: Set("url".to_string()),
                value: Set("https://localhost".to_string()),
            };
            let config = entry.save(&conn).await.unwrap();
            assert_ne!(config.id.unwrap(), 0);
        }

        // test search config
        {
            let result = config::Entity::find()
                .filter(config::Column::Configuration.eq("core"))
                .all(&conn)
                .await
                .unwrap();
            assert_eq!(result.len(), 4, "config_section count is not 5");
        }
    }

    #[tokio::test]
    async fn test_insert_reference() {
        // insert into reference, check foreign key constraint
        let test_db = TestDbPath::new("test_insert_reference.db").await;
        let db_path = test_db.0.as_str();

        let conn = establish_connection(db_path).await.unwrap();
        // test insert reference
        let entries = [
            (Some("master"), ConfigKind::Head, None, None), // attached head
            (Some("master"), ConfigKind::Branch, Some("2019"), None), // local branch
            (Some("release1"), ConfigKind::Tag, Some("2019"), None), // tag (remote tag store same as local tag)
            (
                Some("main"),
                ConfigKind::Head,
                None,
                Some("origin".to_string()),
            ), // remote head — a remote HEAD is not a scope's HEAD
            (
                Some("main"),
                ConfigKind::Branch,
                Some("a"),
                Some("origin".to_string()),
            ),
        ];
        for (name, kind, commit, remote) in entries.iter() {
            let entry = reference::ActiveModel {
                name: Set(name.map(|s| s.to_string())),
                kind: Set(kind.clone()),
                commit: Set(commit.map(|s| s.to_string())),
                remote: Set(remote.clone()),
                ..Default::default()
            };
            let reference_entry = entry.save(&conn).await.unwrap();
            assert_eq!(reference_entry.name.unwrap(), name.map(|s| s.to_string()));
        }

        // plan-20260714 W0 (§C.4.1, migration 2026072901): a scope has exactly
        // ONE local HEAD row. This insert used to be part of the list above —
        // a detached HEAD landing NEXT TO the attached one — and two HEAD rows
        // for the main worktree is precisely the ambiguity the partial unique
        // index exists to prevent, so it must now be refused rather than
        // stored. Updating the existing row is how a detach is recorded.
        let second_main_head = reference::ActiveModel {
            name: Set(None),
            kind: Set(ConfigKind::Head),
            commit: Set(Some("2019".to_string())),
            remote: Set(None),
            ..Default::default()
        };
        let refused = second_main_head.save(&conn).await;
        assert!(
            refused.is_err(),
            "a second local HEAD row for the main scope must be refused"
        );
    }

    #[tokio::test]
    #[serial_test::serial]
    async fn test_reference_check() {
        // test reference check
        let test_db = TestDbPath::new("test_reference_check.db").await;
        let db_path = test_db.0.as_str();

        let conn = establish_connection(db_path).await.unwrap();

        // test `remote`` can't be ''
        let entry = reference::ActiveModel {
            name: Set(Some("master".to_string())),
            kind: Set(ConfigKind::Head),
            commit: Set(Some("2019922235".to_string())),
            remote: Set(Some("".to_string())),
            ..Default::default()
        };
        let result = entry.save(&conn).await;
        assert!(
            result.is_err(),
            "reference check `remote` can't be '' failed"
        );

        // test `name`` can't be ''
        let entry = reference::ActiveModel {
            name: Set(Some("".to_string())),
            kind: Set(ConfigKind::Head),
            commit: Set(Some("2019922235".to_string())),
            remote: Set(Some("origin".to_string())),
            ..Default::default()
        };
        let result = entry.save(&conn).await;
        assert!(result.is_err(), "reference check `name` can't be '' failed");

        // test `remote` must be None for tag
        let entry = reference::ActiveModel {
            name: Set(Some("master".to_string())),
            kind: Set(ConfigKind::Tag),
            commit: Set(Some("2019922235".to_string())),
            remote: Set(Some("origin".to_string())),
            ..Default::default()
        };
        let result = entry.save(&conn).await;
        assert!(
            result.is_err(),
            "reference check `remote` must be None for tag failed"
        );

        // test (`name`, `type`) can't be duplicated when `remote` is None
        let entry = reference::ActiveModel {
            name: Set(Some("test_branch".to_string())),
            kind: Set(ConfigKind::Branch),
            ..Default::default()
        };
        let result = entry.clone().save(&conn).await;
        assert!(result.is_ok());
        let result = entry.save(&conn).await;
        assert!(result.is_err(), "reference check duplicated failed");

        // test (`name`, `type`) can't be duplicated when `remote` is not None
        let entry = reference::ActiveModel {
            name: Set(Some("test_branch".to_string())),
            kind: Set(ConfigKind::Branch),
            remote: Set(Some("origin".to_string())),
            ..Default::default()
        };
        let result = entry.clone().save(&conn).await;
        assert!(result.is_ok()); // not duplicated because remote is different
        let result = entry.save(&conn).await;
        assert!(result.is_err(), "reference check duplicated failed");
    }

    #[tokio::test]
    async fn test_object_index_crud() {
        // Test CRUD operations on object_index table
        let test_db = TestDbPath::new("test_object_index_crud.db").await;
        let db_path = test_db.0.as_str();

        let conn = establish_connection(db_path).await.unwrap();

        // Test insert
        let repo_id = "test-repo-uuid-1234";
        let obj_hash = "e69de29bb2d1d6434b8b29ae775ad8c2e48c5391";
        let entry = object_index::ActiveModel {
            o_id: Set(obj_hash.to_string()),
            o_type: Set("blob".to_string()),
            o_size: Set(0),
            repo_id: Set(repo_id.to_string()),
            created_at: Set(chrono::Utc::now().timestamp()),
            is_synced: Set(0),
            ..Default::default()
        };
        let result = entry.save(&conn).await;
        assert!(result.is_ok(), "Failed to insert object_index");

        // Test query by repo_id
        let results = object_index::Entity::find()
            .filter(object_index::Column::RepoId.eq(repo_id))
            .all(&conn)
            .await
            .unwrap();
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].o_id, obj_hash);
        assert_eq!(results[0].o_type, "blob");
        assert_eq!(results[0].is_synced, 0);

        // Test update is_synced
        let mut active: object_index::ActiveModel = results[0].clone().into();
        active.is_synced = Set(1);
        let updated = active.update(&conn).await.unwrap();
        assert_eq!(updated.is_synced, 1);

        // Test query unsynced objects
        let unsynced = object_index::Entity::find()
            .filter(object_index::Column::RepoId.eq(repo_id))
            .filter(object_index::Column::IsSynced.eq(0))
            .all(&conn)
            .await
            .unwrap();
        assert_eq!(
            unsynced.len(),
            0,
            "Should have no unsynced objects after update"
        );

        // Test unique constraint on (repo_id, o_id)
        let duplicate_entry = object_index::ActiveModel {
            o_id: Set(obj_hash.to_string()),
            o_type: Set("tree".to_string()),
            o_size: Set(100),
            repo_id: Set(repo_id.to_string()),
            created_at: Set(chrono::Utc::now().timestamp()),
            is_synced: Set(0),
            ..Default::default()
        };
        let result = duplicate_entry.insert(&conn).await;
        assert!(
            result.is_err(),
            "Should fail due to unique constraint on o_id"
        );

        // Test insert different object types
        let types = ["tree", "commit", "tag"];
        for (i, obj_type) in types.iter().enumerate() {
            let entry = object_index::ActiveModel {
                o_id: Set(format!("hash_{i}_{obj_type}")),
                o_type: Set(obj_type.to_string()),
                o_size: Set((i * 100) as i64),
                repo_id: Set(repo_id.to_string()),
                created_at: Set(chrono::Utc::now().timestamp()),
                is_synced: Set(0),
                ..Default::default()
            };
            entry.insert(&conn).await.unwrap();
        }

        // Verify all objects in repo
        let all_objects = object_index::Entity::find()
            .filter(object_index::Column::RepoId.eq(repo_id))
            .all(&conn)
            .await
            .unwrap();
        assert_eq!(all_objects.len(), 4, "Should have 4 objects total");
    }

    #[tokio::test]
    async fn test_upgrade_database_schema_backfills_ai_projection_tables() {
        let mut db_path_buf = std::env::temp_dir();
        db_path_buf.push("test_ai_projection_backfill.db");
        let db_path = db_path_buf.to_str().unwrap();

        if Path::new(db_path).exists() {
            fs::remove_file(db_path).unwrap();
        }

        fs::File::create(db_path).unwrap();

        upgrade_database_schema(Path::new(db_path)).await.unwrap();
        let conn = open_database_without_migrations(Path::new(db_path))
            .await
            .unwrap();
        let backend = conn.get_database_backend();
        let stmt = Statement::from_sql_and_values(
            backend,
            "SELECT name FROM sqlite_master WHERE type = 'table' AND name = ?",
            ["ai_thread".into()],
        );
        let row = conn.query_one_raw(stmt).await.unwrap();

        assert!(row.is_some(), "expected ai_thread table to exist");

        conn.close().await.unwrap();
        fs::remove_file(db_path).unwrap();
    }

    #[test]
    fn test_ai_projection_sql_only_contains_ai_schema() {
        let ai_sql = ai_projection_sql().unwrap();

        assert!(ai_sql.contains("CREATE TABLE IF NOT EXISTS `ai_thread`"));
        assert!(ai_sql.contains("CREATE TABLE IF NOT EXISTS `ai_scheduler_state`"));
        assert!(!ai_sql.contains("CREATE TABLE IF NOT EXISTS `config`"));
        assert!(!ai_sql.contains("CREATE TABLE IF NOT EXISTS `reference`"));
        assert!(!ai_sql.contains("CREATE TABLE IF NOT EXISTS `object_index`"));
    }

    #[tokio::test]
    async fn test_upgrade_database_schema_backfills_ai_projection_schema_for_core_only_db() {
        let mut db_path_buf = std::env::temp_dir();
        db_path_buf.push("test_ai_projection_backfill_core_only.db");
        let db_path = db_path_buf.to_str().unwrap();

        if Path::new(db_path).exists() {
            fs::remove_file(db_path).unwrap();
        }

        fs::File::create(db_path).unwrap();

        let conn = connect_database(db_path).await.unwrap();
        let core_sql_end = BOOTSTRAP_SQL.find(AI_PROJECTION_SCHEMA_START).unwrap();
        let core_sql = BOOTSTRAP_SQL[..core_sql_end].trim();
        let backend = conn.get_database_backend();
        conn.execute_raw(Statement::from_string(backend, core_sql))
            .await
            .unwrap();
        conn.close().await.unwrap();

        upgrade_database_schema(Path::new(db_path)).await.unwrap();
        let conn = open_database_without_migrations(Path::new(db_path))
            .await
            .unwrap();
        let backend = conn.get_database_backend();

        let ai_stmt = Statement::from_sql_and_values(
            backend,
            "SELECT name FROM sqlite_master WHERE type = 'table' AND name = ?",
            ["ai_thread".into()],
        );
        let ai_row = conn.query_one_raw(ai_stmt).await.unwrap();
        assert!(ai_row.is_some(), "expected ai_thread table to exist");

        let core_stmt = Statement::from_sql_and_values(
            backend,
            "SELECT name FROM sqlite_master WHERE type = 'table' AND name = ?",
            ["object_index".into()],
        );
        let core_row = conn.query_one_raw(core_stmt).await.unwrap();
        assert!(
            core_row.is_some(),
            "expected object_index table to remain present"
        );

        conn.close().await.unwrap();
        fs::remove_file(db_path).unwrap();
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn test_get_db_conn_instance_for_path_caches_requested_path_under_race() {
        let test_db =
            TestDbPath::new("test_get_db_conn_instance_for_path_reuses_under_race.db").await;
        let db_path = PathBuf::from(&test_db.0);

        reset_db_conn_instance_for_path(&db_path).await;

        let barrier = Arc::new(Barrier::new(8));
        let mut tasks = Vec::new();
        for _ in 0..8 {
            let barrier = Arc::clone(&barrier);
            let db_path = db_path.clone();
            tasks.push(tokio::spawn(async move {
                barrier.wait().await;
                get_db_conn_instance_for_path(&db_path).await
            }));
        }

        for task in tasks {
            let conn = task.await.unwrap().unwrap();
            let backend = conn.get_database_backend();
            let stmt = Statement::from_sql_and_values(backend, "SELECT 1", []);
            let row = conn.query_one_raw(stmt).await.unwrap();
            assert!(row.is_some());
        }

        let connections = TEST_DB_CONNECTIONS.lock().await;
        let cached = connections.get(&db_path);
        assert!(cached.is_some());
        assert_eq!(
            connections.keys().filter(|path| *path == &db_path).count(),
            1
        );
    }

    #[tokio::test]
    async fn test_reset_db_conn_instance_for_path_drops_cached_connection() {
        let test_db = TestDbPath::new("test_reset_db_conn_instance_for_path.db").await;
        let db_path = PathBuf::from(&test_db.0);

        let _conn = get_db_conn_instance_for_path(&db_path).await.unwrap();
        {
            let connections = TEST_DB_CONNECTIONS.lock().await;
            assert!(connections.contains_key(&db_path));
        }

        reset_db_conn_instance_for_path(&db_path).await;

        let connections = TEST_DB_CONNECTIONS.lock().await;
        assert!(!connections.contains_key(&db_path));
    }
}
