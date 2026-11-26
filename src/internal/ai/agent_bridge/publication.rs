//! Operation-to-commit identity stored in the repository database.
//!
//! The bridge catalog (`agent_bridge_operation`) lives in the capture database
//! and is updated after `commit` returns. Matching a later HEAD by message,
//! tree, or timestamp cannot prove which operation published it. This table is
//! inserted in the same repository write transaction that moves HEAD (SQLite
//! transaction or `redb` `GitStore::update_ref_cas_with_bridge_publication`),
//! so a rolled-back ref update cannot leave a binding, and a committed ref
//! update cannot leave the operation anonymous. The commit message stays free
//! of bridge metadata.

use sea_orm::{ConnectionTrait, DatabaseBackend, DbErr, Statement};
use tokio::task_local;

use crate::storage::{
    BridgePublicationPhase, BridgePublicationRecord, GitStateStore, GitStore, RefCasOutcome,
    RefKey, RefTarget, ReflogWrite, StorageBackendKind,
};

task_local! {
    static PUBLICATION_OPERATION: String;
    static EXPECTED_HEAD: String;
    static EXPECTED_INDEX: Option<String>;
    static EXPECTED_WORKTREE: Option<String>;
    static BRIDGE_COMMIT_LOCK_REQUIRED: bool;
}

#[cfg(feature = "storage-sqlite")]
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
#[cfg(feature = "storage-sqlite")]
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

/// Active task-local bridge `operation_id`, if running inside [`with_operation`]
/// or [`with_operation_fence`].
pub fn current_operation_id() -> Option<String> {
    PUBLICATION_OPERATION.try_with(Clone::clone).ok()
}

/// Active task-local expected `HEAD` commit OID fence, if set.
pub fn expected_head_fence() -> Option<String> {
    EXPECTED_HEAD.try_with(Clone::clone).ok()
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

// ---------------------------------------------------------------------------
// Store-backed atomic HEAD & two-phase index publication (`RB-25`)
// ---------------------------------------------------------------------------

/// Record a `prepared` commit publication for `operation_id` via [`GitStore`].
pub async fn prepare_commit_with_store<S: GitStore + ?Sized>(
    git_store: &S,
    operation_id: &str,
    commit_oid: &str,
) -> Result<(), String> {
    if !operation_id_is_bindable(operation_id) {
        return Err("bridge publication operation id is empty or unsafe".to_string());
    }
    git_store
        .prepare_bridge_publication(operation_id, commit_oid)
        .await
        .map_err(|error| format!("record prepared commit: {error}"))
}

/// Record a `prepared` commit publication for the active task-local operation
/// via [`GitStore`]. No-op outside [`with_operation`] / [`with_operation_fence`].
pub async fn prepare_current_with_store<S: GitStore + ?Sized>(
    git_store: &S,
    commit_oid: &str,
) -> Result<(), String> {
    let Some(operation_id) = current_operation_id() else {
        return Ok(());
    };
    prepare_commit_with_store(git_store, &operation_id, commit_oid).await
}

/// Read the `prepared` commit OID bound to `operation_id` via [`GitStore`].
pub async fn prepared_commit_with_store<S: GitStore + ?Sized>(
    git_store: &S,
    operation_id: &str,
) -> Result<Option<String>, String> {
    if !operation_id_is_bindable(operation_id) {
        return Ok(None);
    }
    match git_store
        .get_bridge_publication(operation_id)
        .await
        .map_err(|error| format!("read publication: {error}"))?
    {
        Some(record) if record.phase == BridgePublicationPhase::Prepared => {
            Ok(Some(record.commit_oid))
        }
        _ => Ok(None),
    }
}

/// Read the `published` commit OID bound to `operation_id` via [`GitStore`].
pub async fn published_commit_with_store<S: GitStore + ?Sized>(
    git_store: &S,
    operation_id: &str,
) -> Result<Option<String>, String> {
    if !operation_id_is_bindable(operation_id) {
        return Ok(None);
    }
    match git_store
        .get_bridge_publication(operation_id)
        .await
        .map_err(|error| format!("read publication: {error}"))?
    {
        Some(record) if record.phase == BridgePublicationPhase::Published => {
            Ok(Some(record.commit_oid))
        }
        _ => Ok(None),
    }
}

/// Atomically move `key` (detached `LocalHead` or checked-out `LocalBranch`)
/// and bind `operation_id -> new_commit_oid` (`phase = published`) inside a
/// SINGLE [`GitStore::update_ref_cas_with_bridge_publication`] write
/// transaction. Any CAS conflict or conflicting existing binding rolls back the
/// entire transaction leaving zero orphan ref, reflog, or publication rows.
pub async fn publish_head_move_for_operation_with_store<S: GitStore + ?Sized>(
    git_store: &S,
    operation_id: &str,
    key: &RefKey,
    expected: Option<&RefTarget>,
    new_commit_oid: &str,
    reflogs: &[ReflogWrite],
) -> Result<RefCasOutcome, String> {
    if !operation_id_is_bindable(operation_id) {
        return Err(
            "bridge publication operation id is empty or unsafe; the ref was not moved".to_string(),
        );
    }
    let new_target = RefTarget::Direct(new_commit_oid.to_string());
    let publication = BridgePublicationRecord {
        operation_id: operation_id.to_string(),
        commit_oid: new_commit_oid.to_string(),
        phase: BridgePublicationPhase::Published,
    };
    git_store
        .update_ref_cas_with_bridge_publication(
            key,
            expected,
            Some(&new_target),
            reflogs,
            &publication,
        )
        .await
        .map_err(|error| format!("publish bridge HEAD move: {error}"))
}

/// Atomically move `key` via [`GitStore`], enforcing any active task-local
/// `EXPECTED_HEAD` fence and binding any active task-local `PUBLICATION_OPERATION`
/// in the same write transaction via
/// [`GitStore::update_ref_cas_with_bridge_publication`].
pub async fn publish_head_move_with_store<S: GitStore + ?Sized>(
    git_store: &S,
    key: &RefKey,
    expected: Option<&RefTarget>,
    new_commit_oid: &str,
    reflogs: &[ReflogWrite],
) -> Result<RefCasOutcome, String> {
    if let Some(expected_head) = expected_head_fence() {
        let requested_head = match expected {
            Some(RefTarget::Direct(oid)) => oid.as_str(),
            Some(RefTarget::Symbolic(_)) | None => "",
        };
        if requested_head != expected_head {
            return Err(STALE_HEAD_MARKER.to_string());
        }
    }
    if let Some(operation_id) = current_operation_id() {
        publish_head_move_for_operation_with_store(
            git_store,
            &operation_id,
            key,
            expected,
            new_commit_oid,
            reflogs,
        )
        .await
    } else {
        let new_target = RefTarget::Direct(new_commit_oid.to_string());
        git_store
            .update_ref_cas(key, expected, Some(&new_target), reflogs)
            .await
            .map_err(|error| format!("publish HEAD move: {error}"))
    }
}

/// Record a `prepared` two-phase index publication row via [`GitStateStore`].
pub async fn prepare_index_publication_with_store<S: GitStateStore + ?Sized>(
    state_store: &S,
    operation_id: &str,
    index_digest: &str,
) -> Result<(), String> {
    if !operation_id_is_bindable(operation_id) {
        return Err("bridge index publication operation id is empty or unsafe".into());
    }
    state_store
        .prepare_bridge_index_publication(operation_id, index_digest)
        .await
        .map_err(|error| format!("record prepared index publication: {error}"))?;
    Ok(())
}

/// Transition a `prepared` index publication row to `published` via
/// [`GitStateStore`].
pub async fn publish_index_publication_with_store<S: GitStateStore + ?Sized>(
    state_store: &S,
    operation_id: &str,
) -> Result<(), String> {
    if !operation_id_is_bindable(operation_id) {
        return Err("bridge index publication operation id is empty or unsafe".into());
    }
    state_store
        .publish_bridge_index_publication(operation_id)
        .await
        .map_err(|error| format!("record published index publication: {error}"))?;
    Ok(())
}

/// Read the recorded `(index_digest, phase)` for `operation_id` via
/// [`GitStateStore`].
pub async fn index_publication_with_store<S: GitStateStore + ?Sized>(
    state_store: &S,
    operation_id: &str,
) -> Result<Option<(String, String)>, String> {
    if !operation_id_is_bindable(operation_id) {
        return Ok(None);
    }
    let record = state_store
        .get_bridge_index_publication(operation_id)
        .await
        .map_err(|error| format!("read index publication: {error}"))?;
    Ok(record.map(|rec| (rec.index_digest, rec.phase.as_str().to_string())))
}

// ---------------------------------------------------------------------------
// Backend-dispatched ambient repository publication helpers
// ---------------------------------------------------------------------------

/// Create the publication table before the ref transaction starts.
///
/// `CREATE TABLE` is kept out of the HEAD transaction so a schema change cannot
/// implicitly commit that transaction. On `redb`, the table is initialized at
/// database creation time.
pub async fn ensure_table() -> Result<(), String> {
    let storage = crate::storage::runtime::current_repo_storage_dir()
        .map_err(|error| format!("resolve repository storage: {error}"))?;
    match crate::storage::runtime::detect_repo_backend(&storage)
        .map_err(|error| format!("detect repository backend: {error}"))?
    {
        StorageBackendKind::Redb => {
            #[cfg(feature = "storage-redb")]
            {
                Ok(())
            }
            #[cfg(not(feature = "storage-redb"))]
            {
                Err("this Libra binary was compiled without `storage-redb`".to_string())
            }
        }
        StorageBackendKind::Sqlite => {
            #[cfg(feature = "storage-sqlite")]
            {
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
            #[cfg(not(feature = "storage-sqlite"))]
            {
                Err("this Libra binary was compiled without `storage-sqlite`".to_string())
            }
        }
    }
}

/// Create the index publication table.
pub async fn ensure_index_table() -> Result<(), String> {
    let storage = crate::storage::runtime::current_repo_storage_dir()
        .map_err(|error| format!("resolve repository storage: {error}"))?;
    match crate::storage::runtime::detect_repo_backend(&storage)
        .map_err(|error| format!("detect repository backend: {error}"))?
    {
        StorageBackendKind::Redb => {
            #[cfg(feature = "storage-redb")]
            {
                Ok(())
            }
            #[cfg(not(feature = "storage-redb"))]
            {
                Err("this Libra binary was compiled without `storage-redb`".to_string())
            }
        }
        StorageBackendKind::Sqlite => {
            #[cfg(feature = "storage-sqlite")]
            {
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
            #[cfg(not(feature = "storage-sqlite"))]
            {
                Err("this Libra binary was compiled without `storage-sqlite`".to_string())
            }
        }
    }
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
    let storage = crate::storage::runtime::current_repo_storage_dir()
        .map_err(|error| format!("resolve repository storage: {error}"))?;
    match crate::storage::runtime::detect_repo_backend(&storage)
        .map_err(|error| format!("detect repository backend: {error}"))?
    {
        StorageBackendKind::Redb => {
            #[cfg(feature = "storage-redb")]
            {
                let store = crate::storage::runtime::open_git_state_store_read_write(&storage)
                    .await
                    .map_err(|error| format!("open repository git state store: {error}"))?;
                prepare_index_publication_with_store(store.as_ref(), operation_id, index_digest)
                    .await
            }
            #[cfg(not(feature = "storage-redb"))]
            {
                Err("this Libra binary was compiled without `storage-redb`".to_string())
            }
        }
        StorageBackendKind::Sqlite => {
            #[cfg(feature = "storage-sqlite")]
            {
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
            #[cfg(not(feature = "storage-sqlite"))]
            {
                Err("this Libra binary was compiled without `storage-sqlite`".to_string())
            }
        }
    }
}

/// Record that the prepared index replacement completed.
///
/// This row is the evidence that the operation published the index.
pub async fn publish_index_publication(operation_id: &str) -> Result<(), String> {
    let storage = crate::storage::runtime::current_repo_storage_dir()
        .map_err(|error| format!("resolve repository storage: {error}"))?;
    match crate::storage::runtime::detect_repo_backend(&storage)
        .map_err(|error| format!("detect repository backend: {error}"))?
    {
        StorageBackendKind::Redb => {
            #[cfg(feature = "storage-redb")]
            {
                let store = crate::storage::runtime::open_git_state_store_read_write(&storage)
                    .await
                    .map_err(|error| format!("open repository git state store: {error}"))?;
                publish_index_publication_with_store(store.as_ref(), operation_id).await
            }
            #[cfg(not(feature = "storage-redb"))]
            {
                Err("this Libra binary was compiled without `storage-redb`".to_string())
            }
        }
        StorageBackendKind::Sqlite => {
            #[cfg(feature = "storage-sqlite")]
            {
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
            #[cfg(not(feature = "storage-sqlite"))]
            {
                Err("this Libra binary was compiled without `storage-sqlite`".to_string())
            }
        }
    }
}

/// The recorded index publication for `operation_id`, if any.
///
/// A repository that never staged or unstaged anything has no table yet; that
/// is "no publication record", not an error.
pub async fn index_publication(operation_id: &str) -> Result<Option<(String, String)>, String> {
    let storage = crate::storage::runtime::current_repo_storage_dir()
        .map_err(|error| format!("resolve repository storage: {error}"))?;
    match crate::storage::runtime::detect_repo_backend(&storage)
        .map_err(|error| format!("detect repository backend: {error}"))?
    {
        StorageBackendKind::Redb => {
            #[cfg(feature = "storage-redb")]
            {
                let store = crate::storage::runtime::open_git_state_store_read_only(&storage)
                    .await
                    .map_err(|error| format!("open repository git state store: {error}"))?;
                index_publication_with_store(store.as_ref(), operation_id).await
            }
            #[cfg(not(feature = "storage-redb"))]
            {
                Err("this Libra binary was compiled without `storage-redb`".to_string())
            }
        }
        StorageBackendKind::Sqlite => {
            #[cfg(feature = "storage-sqlite")]
            {
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
            #[cfg(not(feature = "storage-sqlite"))]
            {
                Err("this Libra binary was compiled without `storage-sqlite`".to_string())
            }
        }
    }
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
    let storage = crate::storage::runtime::current_repo_storage_dir()
        .map_err(|error| format!("resolve repository storage: {error}"))?;
    match crate::storage::runtime::detect_repo_backend(&storage)
        .map_err(|error| format!("detect repository backend: {error}"))?
    {
        StorageBackendKind::Redb => {
            #[cfg(feature = "storage-redb")]
            {
                let store = crate::storage::runtime::open_git_store_read_write(&storage)
                    .await
                    .map_err(|error| format!("open repository git store: {error}"))?;
                prepare_commit_with_store(store.as_ref(), &operation_id, commit_oid).await
            }
            #[cfg(not(feature = "storage-redb"))]
            {
                Err("this Libra binary was compiled without `storage-redb`".to_string())
            }
        }
        StorageBackendKind::Sqlite => {
            #[cfg(feature = "storage-sqlite")]
            {
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
            #[cfg(not(feature = "storage-sqlite"))]
            {
                Err("this Libra binary was compiled without `storage-sqlite`".to_string())
            }
        }
    }
}

/// The commit object saved for `operation_id` whose ref update has not committed.
pub async fn prepared_commit(operation_id: &str) -> Result<Option<String>, String> {
    let storage = crate::storage::runtime::current_repo_storage_dir()
        .map_err(|error| format!("resolve repository storage: {error}"))?;
    match crate::storage::runtime::detect_repo_backend(&storage)
        .map_err(|error| format!("detect repository backend: {error}"))?
    {
        StorageBackendKind::Redb => {
            #[cfg(feature = "storage-redb")]
            {
                let store = crate::storage::runtime::open_git_store_read_only(&storage)
                    .await
                    .map_err(|error| format!("open repository git store: {error}"))?;
                prepared_commit_with_store(store.as_ref(), operation_id).await
            }
            #[cfg(not(feature = "storage-redb"))]
            {
                Err("this Libra binary was compiled without `storage-redb`".to_string())
            }
        }
        StorageBackendKind::Sqlite => {
            #[cfg(feature = "storage-sqlite")]
            {
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
            #[cfg(not(feature = "storage-sqlite"))]
            {
                Err("this Libra binary was compiled without `storage-sqlite`".to_string())
            }
        }
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
    let storage = crate::storage::runtime::current_repo_storage_dir()
        .map_err(|error| format!("resolve repository storage: {error}"))?;
    match crate::storage::runtime::detect_repo_backend(&storage)
        .map_err(|error| format!("detect repository backend: {error}"))?
    {
        StorageBackendKind::Redb => {
            #[cfg(feature = "storage-redb")]
            {
                let store = crate::storage::runtime::open_git_store_read_only(&storage)
                    .await
                    .map_err(|error| format!("open repository git store: {error}"))?;
                published_commit_with_store(store.as_ref(), operation_id).await
            }
            #[cfg(not(feature = "storage-redb"))]
            {
                Err("this Libra binary was compiled without `storage-redb`".to_string())
            }
        }
        StorageBackendKind::Sqlite => {
            #[cfg(feature = "storage-sqlite")]
            {
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
            #[cfg(not(feature = "storage-sqlite"))]
            {
                Err("this Libra binary was compiled without `storage-sqlite`".to_string())
            }
        }
    }
}

#[cfg(feature = "storage-sqlite")]
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

#[cfg(all(test, feature = "storage-redb"))]
mod tests {
    use std::sync::Arc;

    use serde_json::json;
    use tempfile::tempdir;
    use tokio::sync::Barrier;

    use super::*;
    use crate::{
        internal::ai::agent_bridge::{
            ingress::{self, StoreBridgeContext},
            methods,
            protocol::{BridgeRequest, JSONRPC_VERSION},
            storage as bridge_storage,
        },
        storage::{
            RedbRepairContext,
            runtime::{
                init_redb_repository_storage, open_capture_store_read_only,
                open_capture_store_read_write, open_git_state_store_read_only,
                open_git_state_store_read_write, open_git_store_read_only,
                open_git_store_read_write, open_repo_config_store_read_write,
            },
        },
    };

    fn test_oid(n: u64) -> String {
        format!("{n:040x}")
    }

    fn make_req(id: i64, method: &str, params: serde_json::Value) -> BridgeRequest {
        BridgeRequest {
            jsonrpc: JSONRPC_VERSION.to_string(),
            method: method.to_string(),
            params: Some(params),
            id: Some(json!(id)),
        }
    }

    fn make_reflog(ref_name: &str, old_n: u64, new_n: u64, msg: &str) -> ReflogWrite {
        ReflogWrite {
            ref_name: ref_name.to_string(),
            old_oid: test_oid(old_n),
            new_oid: test_oid(new_n),
            timestamp: 1_795_400_000 + i64::try_from(new_n).unwrap_or(0),
            committer_name: "Bridge Test".to_string(),
            committer_email: "bridge@example.com".to_string(),
            action: "commit".to_string(),
            message: msg.to_string(),
            worktree_id: None,
        }
    }

    /// `RB-25` (AC1–AC5): End-to-end pure-Redb verification of:
    /// - AC1: `agent_bridge/{ingress,methods,storage}.rs` sessions, contiguous
    ///   event sequence ack, association links, operation replay/terminal
    ///   immutability, and read methods without creating or opening SQLite.
    /// - AC2: Atomic `HEAD` movement + `agent_bridge_publication` commit inside
    ///   one `git.redb` transaction via `update_ref_cas_with_bridge_publication`,
    ///   and zero-orphan rollback on CAS conflict or conflicting `operation_id`.
    /// - AC3: Two-phase `agent_bridge_index_publication` (`prepared -> published`)
    ///   transitions on `git.redb`.
    /// - AC5: 8 concurrent read-only queries (`open_git_store_read_only`,
    ///   `open_git_state_store_read_only`, `open_capture_store_read_only`) during
    ///   active bridge writes complete with ZERO lock errors.
    #[tokio::test]
    #[serial_test::serial(cwd, env)]
    async fn redb_bridge_atomic_head_publication_and_index_two_phase() {
        let dir = tempdir().expect("create RB-25 tempdir");
        let storage = dir.path().join(".libra");
        std::fs::create_dir_all(&storage).expect("create .libra");

        init_redb_repository_storage(&storage)
            .await
            .expect("init redb repository storage");
        assert!(
            !storage.join("libra.db").exists(),
            "AC1: pure-Redb repo must not create libra.db"
        );
        assert!(
            !storage.join("private").join("capture.db").exists(),
            "AC1: pure-Redb repo must not create private/capture.db"
        );

        let git = open_git_store_read_write(&storage)
            .await
            .expect("open redb git store");
        let config = open_repo_config_store_read_write(&storage)
            .await
            .expect("open redb config store");
        let git_state = open_git_state_store_read_write(&storage)
            .await
            .expect("open redb git state store");
        let capture = open_capture_store_read_write(&storage, RedbRepairContext::WriteCommand)
            .await
            .expect("open redb capture store");

        let ctx = StoreBridgeContext {
            storage: storage.clone(),
            git: Arc::clone(&git),
            config: Arc::clone(&config),
            git_state: Arc::clone(&git_state),
            capture: Arc::clone(&capture),
            repository_id: "repo-rb25".to_string(),
            worktree_id: None,
        };

        // -----------------------------------------------------------------
        // AC1: ingress + storage + methods on pure Redb (zero SQLite).
        // -----------------------------------------------------------------
        let open_req = make_req(
            1,
            "session.open",
            json!({
                "session_id": "sess-rb25-1",
                "agent_id": "harness-alpha",
                "workspace_id": "ws-rb25",
            }),
        );
        let open_resp = ingress::dispatch_with_store(&ctx, &open_req)
            .await
            .expect("session.open succeeds")
            .expect("session.open returns response");
        let open_result = open_resp.result.expect("session.open result");
        assert_eq!(open_result["created"], json!(true));
        assert_eq!(open_result["last_acked_seq"], json!(0));

        // Idempotent replay with identical scope.
        let open_replay = ingress::dispatch_with_store(&ctx, &open_req)
            .await
            .expect("session.open replay succeeds")
            .expect("session.open replay response");
        assert_eq!(
            open_replay.result.expect("replay result")["created"],
            json!(false)
        );

        // Out-of-order append: seq=1 and seq=3 (gap at 2) => contiguous ack stays 1.
        let append_gap_req = make_req(
            2,
            "event.append",
            json!({
                "session_id": "sess-rb25-1",
                "events": [
                    { "seq": 1, "type": "turn.start", "payload": "{\"msg\":\"one\"}" },
                    { "seq": 3, "type": "turn.end", "payload": "{\"msg\":\"three\"}" },
                ]
            }),
        );
        let gap_resp = ingress::dispatch_with_store(&ctx, &append_gap_req)
            .await
            .expect("event.append gap succeeds")
            .expect("event.append gap response");
        let gap_res = gap_resp.result.expect("gap result");
        assert_eq!(gap_res["last_acked_seq"], json!(1));
        assert_eq!(gap_res["per_event"][0]["status"], json!("accepted"));
        assert_eq!(gap_res["per_event"][1]["status"], json!("accepted"));

        // Close gap at seq=2, replay seq=1 (duplicate), and conflict at seq=3.
        let append_close_req = make_req(
            3,
            "event.append",
            json!({
                "session_id": "sess-rb25-1",
                "events": [
                    { "seq": 1, "type": "turn.start", "payload": "{\"msg\":\"one\"}" },
                    { "seq": 2, "type": "turn.mid", "payload": "{\"msg\":\"two\"}" },
                    { "seq": 3, "type": "turn.end", "payload": "{\"msg\":\"tampered\"}" },
                ]
            }),
        );
        let close_resp = ingress::dispatch_with_store(&ctx, &append_close_req)
            .await
            .expect("event.append close gap succeeds")
            .expect("event.append close gap response");
        let close_res = close_resp.result.expect("close result");
        assert_eq!(close_res["last_acked_seq"], json!(3));
        assert_eq!(close_res["per_event"][0]["status"], json!("duplicate"));
        assert_eq!(close_res["per_event"][1]["status"], json!("accepted"));
        assert_eq!(close_res["per_event"][2]["status"], json!("conflict"));

        // Evidence link idempotency & conflict detection.
        let link_req = make_req(
            4,
            "evidence.append",
            json!({
                "session_id": "sess-rb25-1",
                "source_id": "ev-001",
                "target_type": "commit",
                "target_id": test_oid(1),
            }),
        );
        ingress::dispatch_with_store(&ctx, &link_req)
            .await
            .expect("evidence.append succeeds");
        ingress::dispatch_with_store(&ctx, &link_req)
            .await
            .expect("evidence.append exact replay succeeds");
        let link_conflict_req = make_req(
            5,
            "evidence.append",
            json!({
                "session_id": "sess-rb25-1",
                "source_id": "ev-001",
                "target_type": "commit",
                "target_id": test_oid(2),
            }),
        );
        let link_err = ingress::dispatch_with_store(&ctx, &link_conflict_req)
            .await
            .expect_err("conflicting relink must fail closed");
        assert_eq!(
            link_err.code,
            crate::internal::ai::agent_bridge::protocol::code::DIGEST_CONFLICT
        );

        // Operation replay & immutable terminal outcome via `storage::*_with_store`.
        let (fresh, existing) = bridge_storage::upsert_operation_with_store(
            ctx.capture.as_ref(),
            "op-replay-1",
            "sess-rb25-1",
            "vcs.commit",
            "digest-aaa",
            "repo-rb25",
            Some("ws-rb25"),
            1_000,
        )
        .await
        .expect("insert op-replay-1");
        assert!(fresh);
        assert!(existing.is_none());

        let (fresh2, existing2) = bridge_storage::upsert_operation_with_store(
            ctx.capture.as_ref(),
            "op-replay-1",
            "sess-rb25-1",
            "vcs.commit",
            "digest-aaa",
            "repo-rb25",
            Some("ws-rb25"),
            1_001,
        )
        .await
        .expect("replay op-replay-1");
        assert!(!fresh2);
        let existing2 = existing2.expect("existing op returned on replay");
        assert_eq!(existing2.method, "vcs.commit");
        assert_eq!(existing2.params_digest, "digest-aaa");

        let terminal_payload = r#"{"vcsWriteOutcome":"indeterminate","operationId":"op-replay-1"}"#;
        assert!(
            bridge_storage::finish_operation_terminal_with_store(
                ctx.capture.as_ref(),
                "op-replay-1",
                terminal_payload,
                1_002,
            )
            .await
            .expect("record terminal outcome")
        );
        assert!(
            !bridge_storage::finish_operation_unless_terminal_with_store(
                ctx.capture.as_ref(),
                "op-replay-1",
                "applied",
                Some("overwrite-attempt"),
                1_003,
            )
            .await
            .expect("terminal outcome must block overwrite")
        );
        let op_res =
            bridge_storage::get_operation_result_with_store(ctx.capture.as_ref(), "op-replay-1")
                .await
                .expect("get operation result")
                .expect("op exists");
        assert!(op_res.terminal_outcome);
        assert_eq!(op_res.result_digest.as_deref(), Some(terminal_payload));

        // Bridge checkpoint + read methods (`context.get`, `status.get`,
        // `history.search`, `checkpoint.list`, `checkpoint.show`).
        assert!(
            bridge_storage::insert_checkpoint_with_store(
                ctx.capture.as_ref(),
                "cp-rb25-1",
                "sess-rb25-1",
                None,
                Some(&test_oid(1)),
                1_005,
            )
            .await
            .expect("insert bridge checkpoint")
        );

        let status_resp = methods::dispatch_with_store(&ctx, &make_req(6, "status.get", json!({})))
            .await
            .expect("status.get succeeds")
            .expect("status.get response");
        let status_data = &status_resp.result.expect("status result")["data"];
        assert_eq!(status_data["bridge_sessions"], json!(1));
        assert_eq!(status_data["bridge_events"], json!(3));
        assert_eq!(status_data["bridge_checkpoints"], json!(1));

        let ctx_resp = methods::dispatch_with_store(&ctx, &make_req(7, "context.get", json!({})))
            .await
            .expect("context.get succeeds")
            .expect("context.get response");
        let ctx_data = &ctx_resp.result.expect("context result")["data"];
        assert_eq!(ctx_data["sessions"][0]["session_id"], json!("sess-rb25-1"));
        assert_eq!(ctx_data["sessions"][0]["last_acked_seq"], json!(3));
        assert_eq!(
            ctx_data["recent_checkpoints"][0]["checkpoint_id"],
            json!("cp-rb25-1")
        );

        let search_resp = methods::dispatch_with_store(
            &ctx,
            &make_req(8, "history.search", json!({ "query": "harness-alpha" })),
        )
        .await
        .expect("history.search succeeds")
        .expect("history.search response");
        assert_eq!(
            search_resp.result.expect("search result")["data"]["sessions"][0]["session_id"],
            json!("sess-rb25-1")
        );

        let cp_list_resp = methods::dispatch_with_store(
            &ctx,
            &make_req(9, "checkpoint.list", json!({ "session_id": "sess-rb25-1" })),
        )
        .await
        .expect("checkpoint.list succeeds")
        .expect("checkpoint.list response");
        assert_eq!(
            cp_list_resp.result.expect("cp list result")["data"]["checkpoints"][0]["checkpoint_id"],
            json!("cp-rb25-1")
        );

        let cp_show_resp = methods::dispatch_with_store(
            &ctx,
            &make_req(
                10,
                "checkpoint.show",
                json!({ "checkpoint_id": "cp-rb25-1" }),
            ),
        )
        .await
        .expect("checkpoint.show succeeds")
        .expect("checkpoint.show response");
        assert_eq!(
            cp_show_resp.result.expect("cp show result")["data"]["target_oid"],
            json!(test_oid(1))
        );

        let flush_resp = ingress::dispatch_with_store(
            &ctx,
            &make_req(11, "session.flush", json!({ "session_id": "sess-rb25-1" })),
        )
        .await
        .expect("session.flush succeeds")
        .expect("session.flush response");
        assert_eq!(
            flush_resp.result.expect("flush result")["last_acked_seq"],
            json!(3)
        );

        let close_sess_resp = ingress::dispatch_with_store(
            &ctx,
            &make_req(12, "session.close", json!({ "session_id": "sess-rb25-1" })),
        )
        .await
        .expect("session.close succeeds")
        .expect("session.close response");
        assert_eq!(
            close_sess_resp.result.expect("close result")["closed"],
            json!(true)
        );

        // -----------------------------------------------------------------
        // AC2: Atomic HEAD + agent_bridge_publication commit & CAS rollback.
        // -----------------------------------------------------------------
        let main_branch = RefKey::LocalBranch {
            name: "main".to_string(),
        };
        let local_head = RefKey::LocalHead { worktree_id: None };
        let c1 = test_oid(1);
        let c2 = test_oid(2);
        let c3 = test_oid(3);
        let c99 = test_oid(99);

        git.update_ref_cas(
            &main_branch,
            None,
            Some(&RefTarget::Direct(c1.clone())),
            &[],
        )
        .await
        .expect("seed refs/heads/main -> c1");
        git.update_ref_cas(
            &local_head,
            None,
            Some(&RefTarget::Symbolic("main".to_string())),
            &[],
        )
        .await
        .expect("seed HEAD -> refs/heads/main");

        // Prepare op-commit-1 before ref publication.
        with_operation(
            "op-commit-1",
            &c1,
            prepare_current_with_store(git.as_ref(), &c2),
        )
        .await
        .expect("prepare op-commit-1");
        assert_eq!(
            prepared_commit_with_store(git.as_ref(), "op-commit-1")
                .await
                .expect("read prepared commit"),
            Some(c2.clone())
        );
        assert_eq!(
            published_commit_with_store(git.as_ref(), "op-commit-1")
                .await
                .expect("read published commit before publish"),
            None
        );

        // CAS conflict rollback: stale expected c99 != live c1 -> ZERO orphan publication rows or reflogs!
        let stale_log = make_reflog("refs/heads/main", 99, 3, "stale commit");
        let conflict_outcome = with_operation(
            "op-commit-rollback",
            &c99,
            publish_head_move_with_store(
                git.as_ref(),
                &main_branch,
                Some(&RefTarget::Direct(c99.clone())),
                &c3,
                std::slice::from_ref(&stale_log),
            ),
        )
        .await
        .expect("CAS conflict returns Ok(CasConflict)");
        assert_eq!(
            conflict_outcome,
            RefCasOutcome::CasConflict {
                actual: Some(RefTarget::Direct(c1.clone())),
            }
        );
        assert_eq!(
            git.get_ref(&main_branch)
                .await
                .expect("main unchanged after rollback"),
            Some(RefTarget::Direct(c1.clone()))
        );
        assert_eq!(
            git.get_bridge_publication("op-commit-rollback")
                .await
                .expect("check rollback publication row"),
            None,
            "AC2: CAS rollback must leave zero orphan agent_bridge_publication rows"
        );
        assert!(
            git.list_reflog("refs/heads/main", None, None)
                .await
                .expect("reflog after rollback")
                .is_empty(),
            "AC2: CAS rollback must leave zero orphan reflog rows"
        );

        // Matching CAS: atomically advance main c1 -> c2 and promote op-commit-1 to published.
        let commit_log = make_reflog("refs/heads/main", 1, 2, "bridge commit 1");
        let head_log = make_reflog("HEAD", 1, 2, "bridge commit 1");
        let applied_outcome = with_operation(
            "op-commit-1",
            &c1,
            publish_head_move_with_store(
                git.as_ref(),
                &main_branch,
                Some(&RefTarget::Direct(c1.clone())),
                &c2,
                &[commit_log, head_log],
            ),
        )
        .await
        .expect("atomic HEAD + bridge publication commit");
        assert_eq!(
            applied_outcome,
            RefCasOutcome::Applied {
                previous: Some(RefTarget::Direct(c1.clone())),
                current: Some(RefTarget::Direct(c2.clone())),
            }
        );
        assert_eq!(
            git.get_ref(&main_branch)
                .await
                .expect("main advanced to c2"),
            Some(RefTarget::Direct(c2.clone()))
        );
        assert_eq!(
            published_commit_with_store(git.as_ref(), "op-commit-1")
                .await
                .expect("read published commit"),
            Some(c2.clone())
        );
        assert_eq!(
            prepared_commit_with_store(git.as_ref(), "op-commit-1")
                .await
                .expect("prepared cleared after publish"),
            None
        );

        // Reusing op-commit-1 for a DIFFERENT commit c3 must fail and roll back main.
        let dup_op_err = with_operation(
            "op-commit-1",
            &c2,
            publish_head_move_with_store(
                git.as_ref(),
                &main_branch,
                Some(&RefTarget::Direct(c2.clone())),
                &c3,
                &[],
            ),
        )
        .await
        .expect_err("reusing already-bound operation_id for a different commit must fail");
        assert!(!dup_op_err.is_empty());
        assert_eq!(
            git.get_ref(&main_branch)
                .await
                .expect("main rolled back to c2"),
            Some(RefTarget::Direct(c2.clone()))
        );
        assert_eq!(
            published_commit_with_store(git.as_ref(), "op-commit-1")
                .await
                .expect("op-commit-1 remains bound to c2"),
            Some(c2.clone())
        );

        // -----------------------------------------------------------------
        // AC3: Two-phase agent_bridge_index_publication (prepared -> published).
        // -----------------------------------------------------------------
        assert_eq!(
            index_publication_with_store(git_state.as_ref(), "op-idx-1")
                .await
                .expect("absent index publication"),
            None
        );
        prepare_index_publication_with_store(git_state.as_ref(), "op-idx-1", "sha256:idx-v1")
            .await
            .expect("prepare index publication");
        assert_eq!(
            index_publication_with_store(git_state.as_ref(), "op-idx-1")
                .await
                .expect("read prepared index publication"),
            Some(("sha256:idx-v1".to_string(), "prepared".to_string()))
        );
        assert!(
            prepare_index_publication_with_store(
                git_state.as_ref(),
                "op-idx-1",
                "sha256:idx-conflict",
            )
            .await
            .is_err(),
            "conflicting digest for existing prepared index publication must fail"
        );
        publish_index_publication_with_store(git_state.as_ref(), "op-idx-1")
            .await
            .expect("publish index publication");
        assert_eq!(
            index_publication_with_store(git_state.as_ref(), "op-idx-1")
                .await
                .expect("read published index publication"),
            Some(("sha256:idx-v1".to_string(), "published".to_string()))
        );

        // -----------------------------------------------------------------
        // AC5: 8 concurrent read-only queries during active long-lived bridge
        // writes must complete with ZERO lock errors.
        // -----------------------------------------------------------------
        ingress::dispatch_with_store(
            &ctx,
            &make_req(
                100,
                "session.open",
                json!({ "session_id": "sess-conc", "agent_id": "harness-conc" }),
            ),
        )
        .await
        .expect("open concurrent session");

        let barrier = Arc::new(Barrier::new(9));
        let writer_ctx = ctx.clone();
        let writer_barrier = Arc::clone(&barrier);
        let writer_handle = tokio::spawn(async move {
            writer_barrier.wait().await;
            let mut prev_oid = c2;
            for step in 1u64..=16u64 {
                let ev_req = make_req(
                    200 + i64::try_from(step).unwrap_or(0),
                    "event.append",
                    json!({
                        "session_id": "sess-conc",
                        "events": [{
                            "seq": step,
                            "type": "conc.tick",
                            "payload": format!("{{\"step\":{step}}}"),
                        }]
                    }),
                );
                ingress::dispatch_with_store(&writer_ctx, &ev_req)
                    .await
                    .map_err(|e| format!("writer event.append step {step}: {}", e.message))?;

                let idx_op = format!("op-conc-idx-{step}");
                let idx_digest = format!("sha256:conc-idx-{step}");
                prepare_index_publication_with_store(
                    writer_ctx.git_state.as_ref(),
                    &idx_op,
                    &idx_digest,
                )
                .await?;
                publish_index_publication_with_store(writer_ctx.git_state.as_ref(), &idx_op)
                    .await?;

                let next_oid = test_oid(100 + step);
                let commit_op = format!("op-conc-commit-{step}");
                let outcome = publish_head_move_for_operation_with_store(
                    writer_ctx.git.as_ref(),
                    &commit_op,
                    &RefKey::LocalBranch {
                        name: "main".to_string(),
                    },
                    Some(&RefTarget::Direct(prev_oid.clone())),
                    &next_oid,
                    &[],
                )
                .await?;
                if !matches!(outcome, RefCasOutcome::Applied { .. }) {
                    return Err(format!(
                        "writer CAS step {step} unexpected outcome: {outcome:?}"
                    ));
                }
                prev_oid = next_oid;
            }
            Ok::<(), String>(())
        });

        let mut reader_handles = Vec::with_capacity(8);
        for reader_id in 0..8usize {
            let reader_storage = storage.clone();
            let reader_barrier = Arc::clone(&barrier);
            reader_handles.push(tokio::spawn(async move {
                reader_barrier.wait().await;
                for iter in 0..8usize {
                    let ro_git = open_git_store_read_only(&reader_storage)
                        .await
                        .map_err(|e| format!("reader {reader_id} iter {iter} open git ro: {e}"))?;
                    let head_ref = ro_git
                        .get_ref(&RefKey::LocalBranch {
                            name: "main".to_string(),
                        })
                        .await
                        .map_err(|e| format!("reader {reader_id} iter {iter} get_ref: {e}"))?;
                    if head_ref.is_none() {
                        return Err(format!(
                            "reader {reader_id} iter {iter} observed missing main branch"
                        ));
                    }
                    let _ = ro_git
                        .get_bridge_publication("op-commit-1")
                        .await
                        .map_err(|e| format!("reader {reader_id} iter {iter} get pub: {e}"))?;

                    let ro_state = open_git_state_store_read_only(&reader_storage)
                        .await
                        .map_err(|e| {
                            format!("reader {reader_id} iter {iter} open state ro: {e}")
                        })?;
                    let _ = ro_state
                        .get_bridge_index_publication("op-idx-1")
                        .await
                        .map_err(|e| format!("reader {reader_id} iter {iter} get idx pub: {e}"))?;

                    let ro_cap = open_capture_store_read_only(&reader_storage)
                        .await
                        .map_err(|e| format!("reader {reader_id} iter {iter} open cap ro: {e}"))?;
                    let sess = ro_cap
                        .get_bridge_session("sess-conc")
                        .await
                        .map_err(|e| format!("reader {reader_id} iter {iter} get sess: {e}"))?;
                    if sess.is_none() {
                        return Err(format!(
                            "reader {reader_id} iter {iter} observed missing sess-conc"
                        ));
                    }
                    let _ = ro_cap
                        .list_bridge_events("sess-conc")
                        .await
                        .map_err(|e| format!("reader {reader_id} iter {iter} list events: {e}"))?;
                }
                Ok::<(), String>(())
            }));
        }

        writer_handle
            .await
            .expect("writer task join")
            .expect("writer task succeeded");
        for (idx, handle) in reader_handles.into_iter().enumerate() {
            handle
                .await
                .unwrap_or_else(|e| panic!("reader {idx} join failed: {e}"))
                .unwrap_or_else(|e| panic!("AC5: concurrent read-only reader {idx} failed: {e}"));
        }

        // Verify final durable state and zero SQLite creation.
        let final_sess = bridge_storage::get_session_with_store(ctx.capture.as_ref(), "sess-conc")
            .await
            .expect("read final sess-conc")
            .expect("sess-conc exists");
        assert_eq!(final_sess.last_acked_seq, 16);
        assert_eq!(
            published_commit_with_store(ctx.git.as_ref(), "op-conc-commit-16")
                .await
                .expect("read final concurrent published commit"),
            Some(test_oid(116))
        );
        assert_eq!(
            index_publication_with_store(ctx.git_state.as_ref(), "op-conc-idx-16")
                .await
                .expect("read final concurrent index publication"),
            Some(("sha256:conc-idx-16".to_string(), "published".to_string()))
        );
        assert!(
            !storage.join("libra.db").exists()
                && !storage.join("private").join("capture.db").exists(),
            "AC1: pure-Redb bridge execution must never create SQLite files"
        );
    }
}
