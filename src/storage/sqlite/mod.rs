//! SQLite-backed implementations of `crate::storage` traits (`ADR-RB-04`).
//!
//! All SQLite storage adapters live under `src/storage/sqlite/` and are gated
//! behind `#[cfg(feature = "storage-sqlite")]`.

use sea_orm::DbErr;

use crate::{internal::db::schema::DatabaseRole, storage::error::StorageBackendError};

pub mod config_store;

pub use config_store::SqliteConfigStore;

/// Convert a SeaORM [`DbErr`] into a sanitized [`StorageBackendError`] without
/// leaking raw SQL parameters, private filesystem paths, or secret config values
/// (`GC-RB-03` / `ER-11`).
pub(crate) fn map_sqlite_db_err(
    role: DatabaseRole,
    context: &'static str,
    err: DbErr,
) -> StorageBackendError {
    let msg = err.to_string();
    if msg.contains("database is locked")
        || msg.contains("database table is locked")
        || msg.contains("SQLITE_BUSY")
    {
        return StorageBackendError::WriteLockBusy { role, waited_ms: 0 };
    }
    StorageBackendError::BackendInternal {
        detail: format!("sqlite {role} operation failed ({context})"),
    }
}
