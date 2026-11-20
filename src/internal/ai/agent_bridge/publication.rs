//! Operation-to-commit identity stored in the repository database.
//!
//! The bridge catalog (`agent_bridge_operation`) lives in the capture database
//! and is updated after `commit` returns. Matching a later HEAD by message,
//! tree, or timestamp cannot prove which operation published it. This table is
//! inserted in the same SQLite transaction that moves HEAD, so a rolled-back
//! ref update cannot leave a binding, and a committed ref update cannot leave
//! the operation anonymous. The commit message stays free of bridge metadata.

use sea_orm::{ConnectionTrait, DatabaseBackend, DbErr, Statement};
use tokio::task_local;

task_local! {
    static PUBLICATION_OPERATION: String;
    static EXPECTED_HEAD: String;
    static EXPECTED_INDEX: Option<String>;
    static EXPECTED_WORKTREE: Option<String>;
    static BRIDGE_COMMIT_LOCK_REQUIRED: bool;
}

const CREATE_SQL: &str = "\
CREATE TABLE IF NOT EXISTS agent_bridge_publication ( \
    operation_id TEXT PRIMARY KEY, \
    commit_oid TEXT NOT NULL, \
    phase TEXT NOT NULL DEFAULT 'published' \
)";

/// The index publication table records what a stage or unstage published.
///
/// The index is a FILE, so it cannot be published inside the repository
/// transaction that moves a ref. Two rows are therefore written around the
/// replacement: `prepared` before it and `published` after it. Only the
/// `published` row proves that this operation installed the index; a
/// `prepared` row alone leaves the outcome indeterminate.
const CREATE_INDEX_SQL: &str = "\
CREATE TABLE IF NOT EXISTS agent_bridge_index_publication ( \
    operation_id TEXT PRIMARY KEY, \
    index_digest TEXT NOT NULL, \
    phase TEXT NOT NULL DEFAULT 'prepared' \
)";

/// Run `future` as the publisher of `operation_id`.
///
/// Only the task inside this scope binds a commit. Ordinary `libra commit`
/// leaves the table untouched.
pub async fn with_operation<T>(
    operation_id: &str,
    expected_head: &str,
    future: impl std::future::Future<Output = T>,
) -> T {
    with_operation_fence(operation_id, expected_head, None, None, future).await
}

pub async fn with_operation_fence<T>(
    operation_id: &str,
    expected_head: &str,
    expected_index: Option<&str>,
    expected_worktree: Option<&str>,
    future: impl std::future::Future<Output = T>,
) -> T {
    BRIDGE_COMMIT_LOCK_REQUIRED
        .scope(
            true,
            PUBLICATION_OPERATION.scope(
                operation_id.to_string(),
                EXPECTED_HEAD.scope(
                    expected_head.to_string(),
                    EXPECTED_INDEX.scope(
                        expected_index.map(str::to_string),
                        EXPECTED_WORKTREE.scope(expected_worktree.map(str::to_string), future),
                    ),
                ),
            ),
        )
        .await
}

/// Mark an in-process bridge commit that has no durable operation id but still
/// needs the same post-hook publication fence as protocol commits.
pub async fn with_commit_fence<T>(
    expected_head: &str,
    expected_index: Option<&str>,
    expected_worktree: Option<&str>,
    future: impl std::future::Future<Output = T>,
) -> T {
    BRIDGE_COMMIT_LOCK_REQUIRED
        .scope(
            true,
            EXPECTED_HEAD.scope(
                expected_head.to_string(),
                EXPECTED_INDEX.scope(
                    expected_index.map(str::to_string),
                    EXPECTED_WORKTREE.scope(expected_worktree.map(str::to_string), future),
                ),
            ),
        )
        .await
}

pub(crate) fn bridge_commit_lock_required() -> bool {
    BRIDGE_COMMIT_LOCK_REQUIRED
        .try_with(|required| *required)
        .unwrap_or(false)
}

pub(crate) fn expected_commit_fence() -> Option<(String, Option<String>, Option<String>)> {
    Some((
        EXPECTED_HEAD.try_with(Clone::clone).ok()?,
        EXPECTED_INDEX.try_with(Clone::clone).ok().flatten(),
        EXPECTED_WORKTREE.try_with(Clone::clone).ok().flatten(),
    ))
}

pub(crate) const STALE_HEAD_MARKER: &str = "bridge HEAD fence changed before publication";

/// Check the admitted HEAD under the same transaction that publishes the ref.
pub(crate) async fn check_expected_head_in_txn<C: ConnectionTrait>(conn: &C) -> Result<(), DbErr> {
    let Ok(expected) = EXPECTED_HEAD.try_with(Clone::clone) else {
        return Ok(());
    };
    let actual = crate::internal::head::Head::current_commit_result_with_conn(conn)
        .await
        .map_err(|error| DbErr::Custom(format!("cannot read HEAD before publication: {error}")))?
        .map(|oid| oid.to_string())
        .unwrap_or_default();
    if actual != expected {
        return Err(DbErr::Custom(STALE_HEAD_MARKER.to_string()));
    }
    Ok(())
}

pub fn operation_id_is_bindable(operation_id: &str) -> bool {
    !operation_id.is_empty()
        && operation_id.len() <= 128
        && operation_id
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-' | b'.' | b':'))
}

/// Create the publication table before the ref transaction starts.
///
/// `CREATE TABLE` is kept out of the HEAD transaction so a schema change cannot
/// implicitly commit that transaction.
pub async fn ensure_table() -> Result<(), String> {
    let db = crate::internal::worktree_scope::request_db()
        .await
        .map_err(|error| format!("open repository database: {error}"))?;
    db.execute_raw(Statement::from_string(
        DatabaseBackend::Sqlite,
        CREATE_SQL.to_string(),
    ))
    .await
    .map_err(|error| format!("create publication table: {error}"))?;
    let row = db
        .query_one_raw(Statement::from_string(
            DatabaseBackend::Sqlite,
            "SELECT COUNT(*) FROM pragma_table_info('agent_bridge_publication') WHERE name = 'phase'"
                .to_string(),
        ))
        .await
        .map_err(|error| format!("inspect publication table: {error}"))?;
    let count: i64 = row
        .ok_or_else(|| "inspect publication table: no result".to_string())?
        .try_get_by_index(0)
        .map_err(|error| format!("inspect publication table: {error}"))?;
    if count == 0 {
        db.execute_raw(Statement::from_string(
            DatabaseBackend::Sqlite,
            "ALTER TABLE agent_bridge_publication ADD COLUMN phase TEXT NOT NULL DEFAULT 'published'"
                .to_string(),
        ))
        .await
        .map_err(|error| format!("add publication phase column: {error}"))?;
    }
    Ok(())
}

/// Create the index publication table.
pub async fn ensure_index_table() -> Result<(), String> {
    let db = crate::internal::worktree_scope::request_db()
        .await
        .map_err(|error| format!("open repository database: {error}"))?;
    db.execute_raw(Statement::from_string(
        DatabaseBackend::Sqlite,
        CREATE_INDEX_SQL.to_string(),
    ))
    .await
    .map_err(|error| format!("create index publication table: {error}"))?;
    Ok(())
}

/// Record that `operation_id` is about to install the index named by
/// `index_digest`.
///
/// The row is written BEFORE the replacement. On its own it proves only that
/// the operation was prepared, never that it published.
pub async fn prepare_index_publication(
    operation_id: &str,
    index_digest: &str,
) -> Result<(), String> {
    if !operation_id_is_bindable(operation_id) {
        return Err("bridge index publication operation id is empty or unsafe".into());
    }
    let db = crate::internal::worktree_scope::request_db()
        .await
        .map_err(|error| format!("open repository database: {error}"))?;
    db.execute_raw(Statement::from_sql_and_values(
        DatabaseBackend::Sqlite,
        "INSERT INTO agent_bridge_index_publication (operation_id, index_digest, phase) \
         VALUES (?, ?, 'prepared') \
         ON CONFLICT(operation_id) DO UPDATE SET index_digest = excluded.index_digest, \
         phase = 'prepared'",
        [operation_id.into(), index_digest.into()],
    ))
    .await
    .map_err(|error| format!("record prepared index publication: {error}"))?;
    Ok(())
}

/// Record that the prepared index replacement completed.
///
/// This row is the evidence that the operation published the index.
pub async fn publish_index_publication(operation_id: &str) -> Result<(), String> {
    let db = crate::internal::worktree_scope::request_db()
        .await
        .map_err(|error| format!("open repository database: {error}"))?;
    db.execute_raw(Statement::from_sql_and_values(
        DatabaseBackend::Sqlite,
        "UPDATE agent_bridge_index_publication SET phase = 'published' WHERE operation_id = ?",
        [operation_id.into()],
    ))
    .await
    .map_err(|error| format!("record published index publication: {error}"))?;
    Ok(())
}

/// The recorded index publication for `operation_id`, if any.
///
/// A repository that never staged or unstaged anything has no table yet; that
/// is "no publication record", not an error.
pub async fn index_publication(operation_id: &str) -> Result<Option<(String, String)>, String> {
    let db = crate::internal::worktree_scope::request_db()
        .await
        .map_err(|error| format!("open repository database: {error}"))?;
    if !table_exists(&db, "agent_bridge_index_publication").await? {
        return Ok(None);
    }
    let row = db
        .query_one_raw(Statement::from_sql_and_values(
            DatabaseBackend::Sqlite,
            "SELECT index_digest, phase FROM agent_bridge_index_publication WHERE operation_id = ?",
            [operation_id.into()],
        ))
        .await
        .map_err(|error| format!("read index publication: {error}"))?;
    let Some(row) = row else {
        return Ok(None);
    };
    let digest: String = row
        .try_get_by_index(0)
        .map_err(|error| format!("index publication digest is undecodable: {error}"))?;
    let phase: String = row
        .try_get_by_index(1)
        .map_err(|error| format!("index publication phase is undecodable: {error}"))?;
    Ok(Some((digest, phase)))
}

/// Record `operation_id -> commit_oid` inside the caller's ref transaction.
///
/// No active publication scope is a no-op, so non-bridge commits are unchanged.
/// A second commit for an operation that already names a different oid fails
/// the transaction and rolls the ref update back.
pub async fn bind_in_txn<C: ConnectionTrait>(conn: &C, commit_oid: &str) -> Result<(), DbErr> {
    let Ok(operation_id) = PUBLICATION_OPERATION.try_with(|id| id.clone()) else {
        return Ok(());
    };
    if !operation_id_is_bindable(&operation_id) {
        return Err(DbErr::Custom(
            "bridge publication operation id is empty or unsafe; the ref was not moved".into(),
        ));
    }
    let existing = publication_row(conn, &operation_id).await?;
    match existing {
        Some((oid, _)) if oid == commit_oid => {
            conn.execute_raw(Statement::from_sql_and_values(
                DatabaseBackend::Sqlite,
                "UPDATE agent_bridge_publication SET phase = 'published' \
                 WHERE operation_id = ? AND commit_oid = ?",
                [operation_id.into(), commit_oid.into()],
            ))
            .await?;
            return Ok(());
        }
        Some((oid, _)) => {
            return Err(DbErr::Custom(format!(
                "bridge operation '{operation_id}' is already bound to commit {oid}; refusing to publish {commit_oid}"
            )));
        }
        None => {}
    }
    conn.execute_raw(Statement::from_sql_and_values(
        DatabaseBackend::Sqlite,
        "INSERT INTO agent_bridge_publication (operation_id, commit_oid, phase) VALUES (?, ?, 'published')",
        [operation_id.into(), commit_oid.into()],
    ))
    .await?;
    Ok(())
}

/// Remember a commit object that has been written but whose ref update has not
/// committed. A crash here must not be reported as a published commit.
pub async fn prepare_current(commit_oid: &str) -> Result<(), String> {
    let Ok(operation_id) = PUBLICATION_OPERATION.try_with(|id| id.clone()) else {
        return Ok(());
    };
    if !operation_id_is_bindable(&operation_id) {
        return Err("bridge publication operation id is empty or unsafe".to_string());
    }
    let db = crate::internal::worktree_scope::request_db()
        .await
        .map_err(|error| format!("open repository database: {error}"))?;
    match publication_row(&db, &operation_id)
        .await
        .map_err(|error| format!("read publication: {error}"))?
    {
        Some((oid, phase)) if oid == commit_oid && phase == "published" => Ok(()),
        Some((oid, _)) if oid == commit_oid => Ok(()),
        Some((oid, _)) => Err(format!(
            "bridge operation '{operation_id}' is already bound to commit {oid}"
        )),
        None => {
            db.execute_raw(Statement::from_sql_and_values(
                DatabaseBackend::Sqlite,
                "INSERT INTO agent_bridge_publication (operation_id, commit_oid, phase) \
                 VALUES (?, ?, 'prepared')",
                [operation_id.into(), commit_oid.into()],
            ))
            .await
            .map_err(|error| format!("record prepared commit: {error}"))?;
            Ok(())
        }
    }
}

/// The commit object saved for `operation_id` whose ref update has not committed.
pub async fn prepared_commit(operation_id: &str) -> Result<Option<String>, String> {
    let db = crate::internal::worktree_scope::request_db()
        .await
        .map_err(|error| format!("open repository database: {error}"))?;
    if !table_exists(&db, "agent_bridge_publication").await? {
        return Ok(None);
    }
    match publication_row(&db, operation_id)
        .await
        .map_err(|error| format!("read publication: {error}"))?
    {
        Some((oid, phase)) if phase == "prepared" => Ok(Some(oid)),
        _ => Ok(None),
    }
}

/// Inside the ref transaction, the prepared object for this task, if any.
pub async fn prepared_oid_in_txn<C: ConnectionTrait>(conn: &C) -> Result<Option<String>, DbErr> {
    let Ok(operation_id) = PUBLICATION_OPERATION.try_with(|id| id.clone()) else {
        return Ok(None);
    };
    match publication_row(conn, &operation_id).await? {
        Some((oid, phase)) if phase == "prepared" => Ok(Some(oid)),
        _ => Ok(None),
    }
}

/// The commit bound to `operation_id`, if this repository has published one.
pub async fn published_commit(operation_id: &str) -> Result<Option<String>, String> {
    let db = crate::internal::worktree_scope::request_db()
        .await
        .map_err(|error| format!("open repository database: {error}"))?;
    if !table_exists(&db, "agent_bridge_publication").await? {
        return Ok(None);
    }
    match publication_row(&db, operation_id)
        .await
        .map_err(|error| format!("read publication: {error}"))?
    {
        Some((oid, phase)) if phase == "published" => Ok(Some(oid)),
        _ => Ok(None),
    }
}

async fn table_exists<C: ConnectionTrait>(conn: &C, table: &str) -> Result<bool, String> {
    let row = conn
        .query_one_raw(Statement::from_sql_and_values(
            DatabaseBackend::Sqlite,
            "SELECT 1 FROM sqlite_master WHERE type = 'table' AND name = ?",
            [table.into()],
        ))
        .await
        .map_err(|error| format!("inspect publication table: {error}"))?;
    Ok(row.is_some())
}

async fn publication_row<C: ConnectionTrait>(
    conn: &C,
    operation_id: &str,
) -> Result<Option<(String, String)>, DbErr> {
    let row = conn
        .query_one_raw(Statement::from_sql_and_values(
            DatabaseBackend::Sqlite,
            "SELECT commit_oid, phase FROM agent_bridge_publication WHERE operation_id = ?",
            [operation_id.into()],
        ))
        .await?;
    let Some(row) = row else {
        return Ok(None);
    };
    let oid: String = row.try_get_by_index(0).map_err(|error| {
        DbErr::Custom(format!("publication commit oid is undecodable: {error}"))
    })?;
    let phase: String = row
        .try_get_by_index(1)
        .map_err(|error| DbErr::Custom(format!("publication phase is undecodable: {error}")))?;
    Ok(Some((oid, phase)))
}
