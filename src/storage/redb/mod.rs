//! `redb 5.0`-backed implementations of `crate::storage` traits (`ADR-RB-03`, `ADR-RB-04`).
//!
//! All `redb` storage adapters live under `src/storage/redb/` and are gated
//! behind `#[cfg(feature = "storage-redb")]`.

use redb::{CommitError, DatabaseError, StorageError, TableError};

use crate::{internal::db::schema::DatabaseRole, storage::error::StorageBackendError};

pub mod config_store;

pub use config_store::RedbConfigStore;

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
