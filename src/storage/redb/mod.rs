//! `redb 5.0`-backed implementations of `crate::storage` traits (`ADR-RB-03`, `ADR-RB-04`).
//!
//! All `redb` storage adapters live under `src/storage/redb/` and are gated
//! behind `#[cfg(feature = "storage-redb")]`.

use redb::{CommitError, DatabaseError, StorageError, TableError};

use crate::{internal::db::schema::DatabaseRole, storage::error::StorageBackendError};

pub mod config_store;

pub use config_store::RedbConfigStore;

pub use crate::storage::capture_store::RedbCaptureStore;

/// Construct a sanitized fail-closed error when `agent_import_tombstone`
/// blocks re-importing a previously erased session or checkpoint (`RB-06c`).
///
/// Deliberately omits raw provider session IDs and paths (`GC-RB-03` / `ER-11`).
pub(crate) fn map_tombstone_protected_err(entity: &'static str) -> StorageBackendError {
    StorageBackendError::BackendInternal {
        detail: format!(
            "{}: {entity} is protected by an erasure tombstone",
            crate::storage::capture_store::CAPTURE_TOMBSTONE_ERROR_PREFIX
        ),
    }
}

/// Construct a sanitized fail-closed error when an overwrite or delete is
/// attempted against an append-only capture audit table (`RB-06c`).
pub(crate) fn map_append_only_err(table: &'static str, op: &'static str) -> StorageBackendError {
    StorageBackendError::BackendInternal {
        detail: format!(
            "{}: {op} is not permitted on {table}",
            crate::storage::capture_store::CAPTURE_APPEND_ONLY_ERROR_PREFIX
        ),
    }
}

/// Construct a sanitized fail-closed error when Unix `O_NOFOLLOW` rejects a
/// symlinked capture private directory or database file (`RB-06d`).
///
/// Deliberately omits raw paths so private repository locations never leak
/// through error messages (`GC-RB-03` / `ER-11`).
#[cfg(unix)]
pub(crate) fn map_capture_symlink_err(target: &'static str) -> StorageBackendError {
    StorageBackendError::BackendInternal {
        detail: format!(
            "{}: {target} must not be a symlink (O_NOFOLLOW)",
            crate::storage::capture_store::CAPTURE_SECURITY_SYMLINK_ERROR_PREFIX
        ),
    }
}

/// Construct a sanitized fail-closed error when a capture private directory or
/// database file is not owned by the current effective Unix UID (`RB-06d`).
///
/// Deliberately omits raw paths and UIDs (`GC-RB-03` / `ER-11`).
#[cfg(unix)]
pub(crate) fn map_capture_owner_mismatch_err(target: &'static str) -> StorageBackendError {
    StorageBackendError::BackendInternal {
        detail: format!(
            "{}: {target} is not owned by the current effective user",
            crate::storage::capture_store::CAPTURE_SECURITY_OWNER_ERROR_PREFIX
        ),
    }
}

/// Map a `redb::StorageError` to a sanitized [`StorageBackendError`] without
/// echoing raw paths or secret values (`GC-RB-03` / `ER-11`).
pub(crate) fn map_redb_storage_err(
    err: StorageError,
    context: &'static str,
) -> StorageBackendError {
    match err {
        StorageError::Io(io_err) => StorageBackendError::io(context, io_err),
        StorageError::Corrupted(_) => StorageBackendError::BackendInternal {
            detail: format!("{context}: redb storage reported corruption"),
        },
        StorageError::ValueTooLarge(_) => StorageBackendError::BackendInternal {
            detail: format!("{context}: redb value exceeded maximum size"),
        },
        StorageError::PreviousIo => StorageBackendError::BackendInternal {
            detail: format!(
                "{context}: redb database entered read-only state after prior I/O error"
            ),
        },
        StorageError::LockPoisoned(_) => StorageBackendError::BackendInternal {
            detail: format!("{context}: internal lock poisoned"),
        },
        _ => StorageBackendError::BackendInternal {
            detail: format!("{context}: unexpected redb storage error"),
        },
    }
}

/// Map a `redb::TableError` to a sanitized [`StorageBackendError`].
pub(crate) fn map_redb_table_err(err: TableError, context: &'static str) -> StorageBackendError {
    match err {
        TableError::Storage(storage_err) => map_redb_storage_err(storage_err, context),
        TableError::TableTypeMismatch { .. } => StorageBackendError::BackendInternal {
            detail: format!("{context}: redb table type mismatch"),
        },
        TableError::TableAlreadyOpen(_, _) => StorageBackendError::BackendInternal {
            detail: format!("{context}: redb table is already open"),
        },
        TableError::TableDoesNotExist(_) => StorageBackendError::BackendInternal {
            detail: format!("{context}: redb table does not exist"),
        },
        TableError::TableIsNotMultimap(_) | TableError::TableIsMultimap(_) => {
            StorageBackendError::BackendInternal {
                detail: format!("{context}: redb table multimap mismatch"),
            }
        }
        _ => StorageBackendError::BackendInternal {
            detail: format!("{context}: unexpected redb table error"),
        },
    }
}

/// Map a `redb::CommitError` to a sanitized [`StorageBackendError`].
pub(crate) fn map_redb_commit_err(err: CommitError, context: &'static str) -> StorageBackendError {
    match err {
        CommitError::Storage(storage_err) => map_redb_storage_err(storage_err, context),
        _ => StorageBackendError::BackendInternal {
            detail: format!("{context}: redb commit failed"),
        },
    }
}

/// Map a `redb::DatabaseError` to a sanitized [`StorageBackendError`].
pub(crate) fn map_redb_database_err(
    err: DatabaseError,
    role: DatabaseRole,
    context: &'static str,
) -> StorageBackendError {
    match err {
        DatabaseError::Storage(storage_err) => map_redb_storage_err(storage_err, context),
        DatabaseError::DatabaseAlreadyOpen => StorageBackendError::BackendInternal {
            detail: format!("{context}: {role} redb database is already open exclusively"),
        },
        DatabaseError::RepairAborted => StorageBackendError::RepairAborted {
            role,
            detail: format!("{context}: database requires explicit repair"),
        },
        DatabaseError::UpgradeRequired(ver) => StorageBackendError::SchemaTooNew {
            role,
            found: u32::from(ver),
            max_supported: 2,
        },
        _ => StorageBackendError::BackendInternal {
            detail: format!("{context}: redb database operation failed"),
        },
    }
}
