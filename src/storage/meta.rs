//! Per-database `META_SCHEMA` ledger, role/version verification, and handle
//! migration tombstone fence (`ADR-RB-03` #3, `ADR-RB-04` #4, `GC-RB-02`).
//!
//! Every persistent Libra `.redb` file (`Repository`, `Capture`, `GlobalConfig`,
//! `SystemConfig`) embeds a single `META_SCHEMA` table record encoded with the
//! versioned postcard envelope (`crate::storage::codec`). Every read and write
//! transaction entry point must validate `META_SCHEMA` as step 1:
//!
//! - Rejects `GC-13` database role mismatches (`RoleMismatch`).
//! - Immediately aborts transactions on tombstoned handles
//!   (`min_reader_version == u32::MAX`) with [`StorageBackendError::RepositoryMigrated`],
//!   physically blocking stale open file descriptors after reverse migration.
//! - Rejects files whose `min_reader_version` exceeds this binary's maximum
//!   supported reader version (`SchemaTooNew`, mapping `Capture` to
//!   `LBR-AGENT-048`, `GlobalConfig`/`SystemConfig` to `LBR-CONFIG-001`, and
//!   `Repository` to `LBR-IO-001`).

use std::{sync::Arc, time::Duration};

use redb::{
    Database, ReadTransaction, ReadableDatabase, ReadableTable, StorageError, TableDefinition,
    TableError, TableHandle, TransactionError, WriteTransaction,
};
use serde::{Deserialize, Serialize};

use crate::{
    internal::db::schema::DatabaseRole,
    storage::{
        codec::{decode_versioned, encode_versioned},
        error::{StorageBackendError, StorageResult},
        opener::{begin_write_bounded, begin_write_bounded_blocking},
    },
};

/// `redb` table storing the singleton `META_SCHEMA` ledger record for each database.
pub const META_SCHEMA_TABLE: TableDefinition<'static, &str, &[u8]> =
    TableDefinition::new("meta_schema");

/// Singleton key within [`META_SCHEMA_TABLE`].
pub const META_SCHEMA_SINGLETON_KEY: &str = "singleton";

/// Current `postcard` envelope version for [`MetaSchemaRecord`].
pub const META_SCHEMA_CODEC_VERSION: u16 = 1;

/// Current Libra `redb` database format version written to newly created stores.
pub const CURRENT_REDB_FORMAT_VERSION: u32 = 1;

/// Highest `min_reader_version` this binary knows how to read safely.
pub const MAX_SUPPORTED_READER_VERSION: u32 = 1;

/// Sentinel `min_reader_version` written into an obsolete `.redb` file during
/// migration cutover to fence already-open reader and writer file descriptors
/// (`ADR-RB-03` #3).
pub const MIGRATION_TOMBSTONE_MIN_READER_VERSION: u32 = u32::MAX;

/// Persistent `GC-13` database roles permitted to own a `.redb` file (`GC-RB-02`).
///
/// [`DatabaseRole::Derived`] is deliberately excluded and always rejected.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum MetaSchemaRole {
    /// Repository Git metadata store (`.libra/redb/git.gen-<id>.redb`).
    Repository,
    /// User-level global configuration store (`<config_dir>/libra/config.redb`).
    GlobalConfig,
    /// Machine-wide system configuration store (`/etc/libra/config.redb`).
    SystemConfig,
    /// Private AI capture catalog store (`.libra/private/capture.gen-<id>.redb`).
    Capture,
}

impl MetaSchemaRole {
    /// Convert a persistent `MetaSchemaRole` back into [`DatabaseRole`].
    #[must_use]
    pub const fn into_database_role(self) -> DatabaseRole {
        match self {
            Self::Repository => DatabaseRole::Repository,
            Self::GlobalConfig => DatabaseRole::GlobalConfig,
            Self::SystemConfig => DatabaseRole::SystemConfig,
            Self::Capture => DatabaseRole::Capture,
        }
    }

    /// Convert a [`DatabaseRole`] into a persistent [`MetaSchemaRole`],
    /// rejecting [`DatabaseRole::Derived`] (`GC-RB-02`).
    pub fn try_from_database_role(role: DatabaseRole) -> StorageResult<Self> {
        match role {
            DatabaseRole::Repository => Ok(Self::Repository),
            DatabaseRole::GlobalConfig => Ok(Self::GlobalConfig),
            DatabaseRole::SystemConfig => Ok(Self::SystemConfig),
            DatabaseRole::Capture => Ok(Self::Capture),
            DatabaseRole::Derived => Err(StorageBackendError::BackendInternal {
                detail:
                    "derived database role is forbidden from persistent redb storage (GC-RB-02)"
                        .to_string(),
            }),
        }
    }
}

/// On-disk `META_SCHEMA` ledger record stored under [`META_SCHEMA_SINGLETON_KEY`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct MetaSchemaRecord {
    /// Authoritative `GC-13` owner role of this `.redb` file.
    pub role: MetaSchemaRole,
    /// Schema format version written by the creating/migrating binary.
    pub format_version: u32,
    /// Minimum reader version required to interpret this file, or
    /// [`MIGRATION_TOMBSTONE_MIN_READER_VERSION`] (`u32::MAX`) when fenced.
    pub min_reader_version: u32,
}

impl MetaSchemaRecord {
    /// Construct a fresh `META_SCHEMA` record for `role` at the current format version.
    pub fn current_for(role: DatabaseRole) -> StorageResult<Self> {
        Ok(Self {
            role: MetaSchemaRole::try_from_database_role(role)?,
            format_version: CURRENT_REDB_FORMAT_VERSION,
            min_reader_version: CURRENT_REDB_FORMAT_VERSION,
        })
    }

    /// Returns `true` when this record carries the `u32::MAX` migration tombstone fence.
    #[must_use]
    pub const fn is_migration_tombstone(&self) -> bool {
        self.min_reader_version == MIGRATION_TOMBSTONE_MIN_READER_VERSION
    }
}

/// Validate a decoded [`MetaSchemaRecord`] against `expected_role`, migration
/// tombstone sentinel, and supported reader version bounds.
pub fn validate_meta_schema(
    record: &MetaSchemaRecord,
    expected_role: DatabaseRole,
) -> StorageResult<()> {
    let _ = MetaSchemaRole::try_from_database_role(expected_role)?;

    let found_role = record.role.into_database_role();
    if found_role != expected_role {
        return Err(StorageBackendError::RoleMismatch {
            expected: expected_role,
            found: found_role,
        });
    }

    // Check migration tombstone sentinel BEFORE future-version comparison so
    // `u32::MAX` always reports `RepositoryMigrated` rather than `SchemaTooNew`.
    if record.is_migration_tombstone() {
        return Err(StorageBackendError::RepositoryMigrated {
            role: expected_role,
        });
    }

    if record.min_reader_version > MAX_SUPPORTED_READER_VERSION {
        return Err(StorageBackendError::SchemaTooNew {
            role: expected_role,
            found: record.min_reader_version,
            max_supported: MAX_SUPPORTED_READER_VERSION,
        });
    }

    if record.format_version == 0 || record.min_reader_version == 0 {
        return Err(StorageBackendError::BackendInternal {
            detail: format!(
                "invalid {expected_role} META_SCHEMA record: format_version={} min_reader_version={}",
                record.format_version, record.min_reader_version
            ),
        });
    }

    Ok(())
}

/// Initialize `META_SCHEMA` in a write transaction for a genuinely new `.redb` file.
///
/// - If `META_SCHEMA` already exists with a valid singleton record, validates
///   it against `role` (failing closed on role mismatch, tombstone fence, or
///   future version) and returns the existing record without overwriting it.
/// - If `META_SCHEMA` exists without its singleton record, or if the database
///   already contains other tables without a `META_SCHEMA` ledger, fails closed
///   so a damaged or pre-existing database can never be silently relabeled.
/// - Only writes a fresh `META_SCHEMA` record when the database has no existing
///   tables or multimap tables.
pub fn init_meta_schema(
    wtx: &WriteTransaction,
    role: DatabaseRole,
) -> StorageResult<MetaSchemaRecord> {
    let fresh = MetaSchemaRecord::current_for(role)?;

    let mut has_meta_table = false;
    let mut has_other_table = false;
    for handle in wtx
        .list_tables()
        .map_err(|err| map_storage_error(err, "list tables before META_SCHEMA initialization"))?
    {
        if handle.name() == META_SCHEMA_TABLE.name() {
            has_meta_table = true;
        } else {
            has_other_table = true;
        }
    }
    let has_multimap_table = wtx
        .list_multimap_tables()
        .map_err(|err| {
            map_storage_error(
                err,
                "list multimap tables before META_SCHEMA initialization",
            )
        })?
        .next()
        .is_some();

    if has_meta_table {
        let table = wtx
            .open_table(META_SCHEMA_TABLE)
            .map_err(|err| map_table_error(err, "open existing META_SCHEMA"))?;
        let existing_bytes = table
            .get(META_SCHEMA_SINGLETON_KEY)
            .map_err(|err| map_storage_error(err, "read existing META_SCHEMA"))?
            .map(|guard| guard.value().to_vec())
            .ok_or_else(|| StorageBackendError::BackendInternal {
                detail: format!(
                    "META_SCHEMA table exists without singleton record in {role} redb database; refusing to relabel damaged database"
                ),
            })?;
        let existing: MetaSchemaRecord =
            decode_versioned(&existing_bytes, META_SCHEMA_CODEC_VERSION)?;
        validate_meta_schema(&existing, role)?;
        return Ok(existing);
    }

    if has_other_table || has_multimap_table {
        return Err(StorageBackendError::BackendInternal {
            detail: format!(
                "refusing to initialize META_SCHEMA for {role}: database already contains tables without a META_SCHEMA ledger"
            ),
        });
    }

    let mut table = wtx
        .open_table(META_SCHEMA_TABLE)
        .map_err(|err| map_table_error(err, "create META_SCHEMA table"))?;
    let encoded = encode_versioned(META_SCHEMA_CODEC_VERSION, &fresh)?;
    table
        .insert(META_SCHEMA_SINGLETON_KEY, encoded.as_slice())
        .map_err(|err| map_storage_error(err, "write META_SCHEMA"))?;
    Ok(fresh)
}

/// Write the `min_reader_version == u32::MAX` migration tombstone sentinel into
/// `META_SCHEMA` inside an active write transaction (`ADR-RB-03` #3).
///
/// Refuses to fence an uninitialized database, a database of a different role,
/// or a non-tombstone database whose `min_reader_version` is newer than this
/// binary supports. Idempotent if the matching role is already tombstoned.
pub fn write_migration_tombstone(
    wtx: &WriteTransaction,
    expected_role: DatabaseRole,
) -> StorageResult<MetaSchemaRecord> {
    let _ = MetaSchemaRole::try_from_database_role(expected_role)?;

    let has_meta_table = wtx
        .list_tables()
        .map_err(|err| map_storage_error(err, "list tables before writing migration tombstone"))?
        .any(|handle| handle.name() == META_SCHEMA_TABLE.name());
    if !has_meta_table {
        return Err(StorageBackendError::BackendInternal {
            detail: format!(
                "missing META_SCHEMA table in {expected_role} redb database; cannot write migration tombstone"
            ),
        });
    }

    let mut table = wtx
        .open_table(META_SCHEMA_TABLE)
        .map_err(|err| map_table_error(err, "open META_SCHEMA for migration tombstone"))?;

    let existing_bytes = table
        .get(META_SCHEMA_SINGLETON_KEY)
        .map_err(|err| map_storage_error(err, "read META_SCHEMA before writing tombstone"))?
        .map(|guard| guard.value().to_vec())
        .ok_or_else(|| StorageBackendError::BackendInternal {
            detail: format!(
                "missing META_SCHEMA record in {expected_role} redb database; cannot write migration tombstone"
            ),
        })?;

    let existing: MetaSchemaRecord = decode_versioned(&existing_bytes, META_SCHEMA_CODEC_VERSION)?;
    let found_role = existing.role.into_database_role();
    if found_role != expected_role {
        return Err(StorageBackendError::RoleMismatch {
            expected: expected_role,
            found: found_role,
        });
    }

    if existing.is_migration_tombstone() {
        return Ok(existing);
    }

    if existing.min_reader_version > MAX_SUPPORTED_READER_VERSION {
        return Err(StorageBackendError::SchemaTooNew {
            role: expected_role,
            found: existing.min_reader_version,
            max_supported: MAX_SUPPORTED_READER_VERSION,
        });
    }

    let tombstoned = MetaSchemaRecord {
        role: existing.role,
        format_version: existing.format_version,
        min_reader_version: MIGRATION_TOMBSTONE_MIN_READER_VERSION,
    };
    let encoded = encode_versioned(META_SCHEMA_CODEC_VERSION, &tombstoned)?;
    table
        .insert(META_SCHEMA_SINGLETON_KEY, encoded.as_slice())
        .map_err(|err| map_storage_error(err, "write META_SCHEMA migration tombstone"))?;
    Ok(tombstoned)
}

/// Verify `META_SCHEMA` at the entry of a [`ReadTransaction`].
pub fn verify_meta_schema_read(
    rtx: &ReadTransaction,
    expected_role: DatabaseRole,
) -> StorageResult<MetaSchemaRecord> {
    let _ = MetaSchemaRole::try_from_database_role(expected_role)?;

    let table = match rtx.open_table(META_SCHEMA_TABLE) {
        Ok(table) => table,
        Err(TableError::TableDoesNotExist(_)) => {
            return Err(StorageBackendError::BackendInternal {
                detail: format!("missing META_SCHEMA table in {expected_role} redb database"),
            });
        }
        Err(other) => {
            return Err(map_table_error(
                other,
                "open META_SCHEMA in read transaction",
            ));
        }
    };

    let raw = table
        .get(META_SCHEMA_SINGLETON_KEY)
        .map_err(|err| map_storage_error(err, "read META_SCHEMA in read transaction"))?
        .ok_or_else(|| StorageBackendError::BackendInternal {
            detail: format!("missing META_SCHEMA record in {expected_role} redb database"),
        })?;

    let record: MetaSchemaRecord = decode_versioned(raw.value(), META_SCHEMA_CODEC_VERSION)?;
    validate_meta_schema(&record, expected_role)?;
    Ok(record)
}

/// Verify `META_SCHEMA` at the entry of a [`WriteTransaction`] without creating
/// the `META_SCHEMA` table if it is absent.
///
/// Callers must abort/drop the transaction immediately if this returns `Err`.
pub fn verify_meta_schema_write(
    wtx: &WriteTransaction,
    expected_role: DatabaseRole,
) -> StorageResult<MetaSchemaRecord> {
    let _ = MetaSchemaRole::try_from_database_role(expected_role)?;

    let has_meta_table = wtx
        .list_tables()
        .map_err(|err| map_storage_error(err, "list tables before META_SCHEMA verification"))?
        .any(|handle| handle.name() == META_SCHEMA_TABLE.name());
    if !has_meta_table {
        return Err(StorageBackendError::BackendInternal {
            detail: format!("missing META_SCHEMA table in {expected_role} redb database"),
        });
    }

    let table = wtx
        .open_table(META_SCHEMA_TABLE)
        .map_err(|err| map_table_error(err, "open META_SCHEMA in write transaction"))?;

    let raw = table
        .get(META_SCHEMA_SINGLETON_KEY)
        .map_err(|err| map_storage_error(err, "read META_SCHEMA in write transaction"))?
        .ok_or_else(|| StorageBackendError::BackendInternal {
            detail: format!("missing META_SCHEMA record in {expected_role} redb database"),
        })?;

    let record: MetaSchemaRecord = decode_versioned(raw.value(), META_SCHEMA_CODEC_VERSION)?;
    validate_meta_schema(&record, expected_role)?;
    Ok(record)
}

/// Begin a [`ReadTransaction`] and validate `META_SCHEMA` as its first step.
///
/// Immediately aborts/drops the read transaction if role verification,
/// `min_reader_version` bounds, or the migration tombstone fence fails.
pub fn begin_checked_read(
    db: &impl ReadableDatabase,
    expected_role: DatabaseRole,
) -> StorageResult<ReadTransaction> {
    let _ = MetaSchemaRole::try_from_database_role(expected_role)?;
    let rtx = db
        .begin_read()
        .map_err(|err| map_transaction_error(err, "begin checked redb read transaction"))?;
    verify_meta_schema_read(&rtx, expected_role)?;
    Ok(rtx)
}

/// Synchronously begin a bounded [`WriteTransaction`] and validate `META_SCHEMA`
/// as its first step (`ADR-RB-03` #3/#4).
///
/// Immediately aborts/drops the write transaction on any validation or tombstone
/// failure so no writes can ever be committed to a mismatched or migrated inode.
pub fn begin_checked_write_blocking(
    db: &Database,
    expected_role: DatabaseRole,
    timeout: Duration,
) -> StorageResult<WriteTransaction> {
    let _ = MetaSchemaRole::try_from_database_role(expected_role)?;
    let wtx = begin_write_bounded_blocking(db, expected_role, timeout)?;
    verify_meta_schema_write(&wtx, expected_role)?;
    Ok(wtx)
}

/// Asynchronously begin a bounded [`WriteTransaction`] off the Tokio executor
/// and validate `META_SCHEMA` as its first step (`ADR-RB-03` #3/#4).
///
/// Immediately aborts/drops the write transaction off the executor if role,
/// version, or migration tombstone validation fails.
pub async fn begin_checked_write(
    db: Arc<Database>,
    expected_role: DatabaseRole,
    timeout: Duration,
) -> StorageResult<WriteTransaction> {
    let _ = MetaSchemaRole::try_from_database_role(expected_role)?;
    let wtx = begin_write_bounded(db, expected_role, timeout).await?;
    tokio::task::spawn_blocking(move || {
        verify_meta_schema_write(&wtx, expected_role)?;
        Ok(wtx)
    })
    .await
    .map_err(|err| StorageBackendError::BackendInternal {
        detail: format!("redb META_SCHEMA verification task failed: {err}"),
    })?
}

fn map_storage_error(err: StorageError, context: &'static str) -> StorageBackendError {
    match err {
        StorageError::Io(io_err) => StorageBackendError::io(context, io_err),
        StorageError::Corrupted(_) => StorageBackendError::BackendInternal {
            detail: format!("{context}: redb storage reported corruption"),
        },
        StorageError::ValueTooLarge(_) => StorageBackendError::BackendInternal {
            detail: format!("{context}: redb value exceeded maximum supported size"),
        },
        StorageError::PreviousIo => StorageBackendError::BackendInternal {
            detail: format!(
                "{context}: redb database entered read-only error state after prior I/O failure"
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

fn map_table_error(err: TableError, context: &'static str) -> StorageBackendError {
    match err {
        TableError::Storage(storage_err) => map_storage_error(storage_err, context),
        TableError::TableDoesNotExist(_) => StorageBackendError::BackendInternal {
            detail: format!("{context}: expected redb table does not exist"),
        },
        TableError::TableTypeMismatch { .. } => StorageBackendError::BackendInternal {
            detail: format!("{context}: redb table type mismatch"),
        },
        _ => StorageBackendError::BackendInternal {
            detail: format!("{context}: unexpected redb table error"),
        },
    }
}

fn map_transaction_error(err: TransactionError, context: &'static str) -> StorageBackendError {
    match err {
        TransactionError::Storage(storage_err) => map_storage_error(storage_err, context),
        TransactionError::ReadTransactionStillInUse(_) => StorageBackendError::BackendInternal {
            detail: format!("{context}: read transaction still has live table handles"),
        },
        TransactionError::WouldBlock => StorageBackendError::BackendInternal {
            detail: format!("{context}: write transaction would block"),
        },
        _ => StorageBackendError::BackendInternal {
            detail: format!("{context}: unexpected redb transaction error"),
        },
    }
}

#[cfg(test)]
mod tests {
    use std::{env, fs, path::PathBuf, process::Command, sync::Arc, time::Duration};

    use redb::{MultimapTableDefinition, TableDefinition};
    use tempfile::tempdir;

    use super::*;
    use crate::storage::opener::{
        RedbRepairContext, commit_write_bounded, create_redb_writable, open_redb_read_only,
        open_redb_writable,
    };

    const DATA_KV: TableDefinition<&str, &str> = TableDefinition::new("meta_test_kv");
    const MULTIMAP_KV: MultimapTableDefinition<&str, &str> =
        MultimapTableDefinition::new("meta_test_multimap");

    fn seed_db_with_record(path: &std::path::Path, record: &MetaSchemaRecord) {
        let db = create_redb_writable(path, DatabaseRole::Repository, RedbRepairContext::Migrate)
            .expect("create test redb file");
        let wtx =
            begin_write_bounded_blocking(&db, DatabaseRole::Repository, Duration::from_secs(2))
                .expect("begin seed write");
        {
            let mut table = wtx
                .open_table(META_SCHEMA_TABLE)
                .expect("open META_SCHEMA table");
            let encoded = encode_versioned(META_SCHEMA_CODEC_VERSION, record)
                .expect("encode META_SCHEMA record");
            table
                .insert(META_SCHEMA_SINGLETON_KEY, encoded.as_slice())
                .expect("insert META_SCHEMA record");
        }
        wtx.commit().expect("commit seed META_SCHEMA");
    }

    #[test]
    fn meta_schema_role_future_version_and_migration_tombstone_fence() {
        // Isolated child-process mode simulating `libra-migrate` writing the
        // `u32::MAX` tombstone fence from a separate OS process (and on Unix,
        // unlinking the path while the parent still holds open file descriptors).
        if let Some(mode) = env::var_os("LIBRA_RB02B_CHILD_MODE") {
            assert_eq!(mode.to_str(), Some("migrator_fence"));
            let db_path = PathBuf::from(env::var_os("LIBRA_RB02B_DB_PATH").expect("db path"));
            let db = open_redb_writable(
                &db_path,
                DatabaseRole::Repository,
                RedbRepairContext::Migrate,
            )
            .expect("migrator child opens redb writable");
            let wtx =
                begin_write_bounded_blocking(&db, DatabaseRole::Repository, Duration::from_secs(5))
                    .expect("migrator child acquires write lock");
            let fenced = write_migration_tombstone(&wtx, DatabaseRole::Repository)
                .expect("migrator writes u32::MAX migration tombstone");
            assert!(fenced.is_migration_tombstone());
            assert_eq!(
                fenced.min_reader_version,
                MIGRATION_TOMBSTONE_MIN_READER_VERSION
            );
            // Calling write_migration_tombstone a second time is idempotent.
            let fenced_again = write_migration_tombstone(&wtx, DatabaseRole::Repository)
                .expect("repeated tombstone write must be idempotent");
            assert_eq!(fenced_again, fenced);
            wtx.commit().expect("migrator commits tombstone fence");
            drop(db);

            #[cfg(unix)]
            {
                fs::remove_file(&db_path)
                    .expect("migrator unlinks fenced redb path on Unix after commit");
            }
            return;
        }

        let dir = tempdir().expect("create tempdir");
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("build tokio runtime");

        // AC1: Role mismatch rejection (e.g., opening a `Capture` DB as `Repository`,
        // `GlobalConfig`, or `SystemConfig`), plus `DatabaseRole::Derived` rejection.
        let capture_path = dir.path().join("capture.gen-00000001.redb");
        {
            let cap_db = create_redb_writable(
                &capture_path,
                DatabaseRole::Capture,
                RedbRepairContext::WriteCommand,
            )
            .expect("create capture redb");
            let wtx = begin_write_bounded_blocking(
                &cap_db,
                DatabaseRole::Capture,
                Duration::from_secs(2),
            )
            .expect("begin capture init write");
            let rec =
                init_meta_schema(&wtx, DatabaseRole::Capture).expect("init capture META_SCHEMA");
            assert_eq!(rec.role, MetaSchemaRole::Capture);
            assert_eq!(rec.format_version, CURRENT_REDB_FORMAT_VERSION);
            assert_eq!(rec.min_reader_version, CURRENT_REDB_FORMAT_VERSION);

            // Idempotent re-init with matching role succeeds; mismatched role fails closed.
            let same = init_meta_schema(&wtx, DatabaseRole::Capture)
                .expect("idempotent init with same role must succeed");
            assert_eq!(same, rec);
            let wrong_init = init_meta_schema(&wtx, DatabaseRole::Repository)
                .expect_err("init_meta_schema must reject mismatched role");
            assert!(matches!(
                wrong_init,
                StorageBackendError::RoleMismatch {
                    expected: DatabaseRole::Repository,
                    found: DatabaseRole::Capture,
                }
            ));
            assert_eq!(wrong_init.stable_code_str(), "LBR-IO-001");

            // `write_migration_tombstone` must refuse to tombstone a wrong-role DB.
            let wrong_fence = write_migration_tombstone(&wtx, DatabaseRole::Repository)
                .expect_err("write_migration_tombstone must reject wrong role");
            assert!(matches!(
                wrong_fence,
                StorageBackendError::RoleMismatch {
                    expected: DatabaseRole::Repository,
                    found: DatabaseRole::Capture,
                }
            ));

            wtx.commit().expect("commit capture META_SCHEMA");
        }

        let cap_ro = open_redb_read_only(&capture_path, DatabaseRole::Capture)
            .expect("open capture read-only");
        let cap_rw = Arc::new(
            open_redb_writable(
                &capture_path,
                DatabaseRole::Capture,
                RedbRepairContext::WriteCommand,
            )
            .expect("open capture writable"),
        );

        // Matching role succeeds for both read and write entry points.
        let ok_rtx = begin_checked_read(&cap_ro, DatabaseRole::Capture)
            .expect("matching Capture role read must succeed");
        drop(ok_rtx);
        let ok_wtx =
            begin_checked_write_blocking(&cap_rw, DatabaseRole::Capture, Duration::from_secs(2))
                .expect("matching Capture role write must succeed");
        drop(ok_wtx);

        // Mismatched roles are rejected on read, blocking write, and async write.
        for wrong_role in [
            DatabaseRole::Repository,
            DatabaseRole::GlobalConfig,
            DatabaseRole::SystemConfig,
        ] {
            let ro_err = begin_checked_read(&cap_ro, wrong_role)
                .expect_err("read with mismatched role must fail");
            assert!(matches!(
                ro_err,
                StorageBackendError::RoleMismatch {
                    expected,
                    found: DatabaseRole::Capture,
                } if expected == wrong_role
            ));
            assert_eq!(ro_err.stable_code_str(), "LBR-IO-001");

            let rw_err = begin_checked_write_blocking(&cap_rw, wrong_role, Duration::from_secs(2))
                .err()
                .expect("blocking write with mismatched role must fail");
            assert!(matches!(
                rw_err,
                StorageBackendError::RoleMismatch {
                    expected,
                    found: DatabaseRole::Capture,
                } if expected == wrong_role
            ));

            let async_err = rt
                .block_on(begin_checked_write(
                    Arc::clone(&cap_rw),
                    wrong_role,
                    Duration::from_secs(2),
                ))
                .err()
                .expect("async write with mismatched role must fail");
            assert!(matches!(
                async_err,
                StorageBackendError::RoleMismatch {
                    expected,
                    found: DatabaseRole::Capture,
                } if expected == wrong_role
            ));
        }

        // `DatabaseRole::Derived` is forbidden from persistent redb storage (GC-RB-02).
        let derived_err = MetaSchemaRecord::current_for(DatabaseRole::Derived)
            .expect_err("Derived role must be rejected");
        assert_eq!(derived_err.stable_code_str(), "LBR-IO-001");
        drop(cap_ro);
        drop(cap_rw);

        // AC2: Future `min_reader_version` rejection with per-role stable error codes
        // (`Capture` -> `LBR-AGENT-048`, `GlobalConfig`/`SystemConfig` -> `LBR-CONFIG-001`,
        // `Repository` -> `LBR-IO-001`).
        for (name, role, expected_code) in [
            ("future-repo.redb", DatabaseRole::Repository, "LBR-IO-001"),
            ("future-cap.redb", DatabaseRole::Capture, "LBR-AGENT-048"),
            (
                "future-global.redb",
                DatabaseRole::GlobalConfig,
                "LBR-CONFIG-001",
            ),
            (
                "future-system.redb",
                DatabaseRole::SystemConfig,
                "LBR-CONFIG-001",
            ),
        ] {
            let p = dir.path().join(name);
            let future_record = MetaSchemaRecord {
                role: MetaSchemaRole::try_from_database_role(role).expect("persistent role"),
                format_version: MAX_SUPPORTED_READER_VERSION + 1,
                min_reader_version: MAX_SUPPORTED_READER_VERSION + 1,
            };
            seed_db_with_record(&p, &future_record);

            let ro = open_redb_read_only(&p, role).expect("open future db read-only");
            let ro_err = begin_checked_read(&ro, role)
                .expect_err("read must reject future min_reader_version");
            assert!(matches!(
                ro_err,
                StorageBackendError::SchemaTooNew {
                    role: r,
                    found,
                    max_supported,
                } if r == role
                    && found == MAX_SUPPORTED_READER_VERSION + 1
                    && max_supported == MAX_SUPPORTED_READER_VERSION
            ));
            assert_eq!(ro_err.stable_code_str(), expected_code);

            let rw = Arc::new(
                open_redb_writable(&p, role, RedbRepairContext::WriteCommand)
                    .expect("open future db writable"),
            );
            let rw_err = begin_checked_write_blocking(&rw, role, Duration::from_secs(2))
                .err()
                .expect("blocking write must reject future min_reader_version");
            assert!(matches!(
                rw_err,
                StorageBackendError::SchemaTooNew { role: r, .. } if r == role
            ));
            assert_eq!(rw_err.stable_code_str(), expected_code);

            let async_err = rt
                .block_on(begin_checked_write(
                    Arc::clone(&rw),
                    role,
                    Duration::from_secs(2),
                ))
                .err()
                .expect("async write must reject future min_reader_version");
            assert!(matches!(
                async_err,
                StorageBackendError::SchemaTooNew { role: r, .. } if r == role
            ));
            assert_eq!(async_err.stable_code_str(), expected_code);
        }

        // Missing META_SCHEMA table / corrupt META_SCHEMA bytes also fail closed.
        let uninit_path = dir.path().join("uninitialized.redb");
        {
            let db = create_redb_writable(
                &uninit_path,
                DatabaseRole::Repository,
                RedbRepairContext::WriteCommand,
            )
            .expect("create uninitialized redb");
            let ro = open_redb_read_only(&uninit_path, DatabaseRole::Repository)
                .expect("open uninit read-only");
            let missing_ro = begin_checked_read(&ro, DatabaseRole::Repository)
                .expect_err("read on uninitialized DB must fail closed");
            assert_eq!(missing_ro.stable_code_str(), "LBR-IO-001");
            let missing_rw =
                begin_checked_write_blocking(&db, DatabaseRole::Repository, Duration::from_secs(2))
                    .err()
                    .expect("write on uninitialized DB must fail closed");
            assert_eq!(missing_rw.stable_code_str(), "LBR-IO-001");

            // Corrupt META_SCHEMA bytes must also fail closed on both read and write entry.
            let wtx =
                begin_write_bounded_blocking(&db, DatabaseRole::Repository, Duration::from_secs(2))
                    .expect("begin corrupt seed write");
            {
                let mut tbl = wtx.open_table(META_SCHEMA_TABLE).expect("open table");
                tbl.insert(META_SCHEMA_SINGLETON_KEY, b"\x01\x00\xff\xfe".as_slice())
                    .expect("insert corrupt meta bytes");
            }
            wtx.commit().expect("commit corrupt meta bytes");
            let corrupt_ro = begin_checked_read(&ro, DatabaseRole::Repository)
                .expect_err("corrupt META_SCHEMA read must fail closed");
            assert_eq!(corrupt_ro.stable_code_str(), "LBR-IO-001");
            let corrupt_rw =
                begin_checked_write_blocking(&db, DatabaseRole::Repository, Duration::from_secs(2))
                    .err()
                    .expect("corrupt META_SCHEMA write must fail closed");
            assert_eq!(corrupt_rw.stable_code_str(), "LBR-IO-001");

            // Damaged database where `META_SCHEMA` table exists without its
            // singleton record must NOT be silently relabeled by `init_meta_schema`.
            let wtx =
                begin_write_bounded_blocking(&db, DatabaseRole::Repository, Duration::from_secs(2))
                    .expect("begin delete-singleton write");
            {
                let mut tbl = wtx.open_table(META_SCHEMA_TABLE).expect("open table");
                tbl.remove(META_SCHEMA_SINGLETON_KEY)
                    .expect("remove singleton key");
            }
            wtx.commit().expect("commit empty META_SCHEMA table");

            let wtx =
                begin_write_bounded_blocking(&db, DatabaseRole::Repository, Duration::from_secs(2))
                    .expect("begin relabel-attempt write");
            let empty_tbl_err = init_meta_schema(&wtx, DatabaseRole::Repository)
                .expect_err("init_meta_schema must refuse empty existing META_SCHEMA table");
            assert_eq!(empty_tbl_err.stable_code_str(), "LBR-IO-001");
            assert!(
                empty_tbl_err
                    .to_string()
                    .contains("without singleton record"),
                "unexpected error for empty META_SCHEMA table: {empty_tbl_err}"
            );
            drop(wtx);
        }

        // Pre-existing database containing other tables without a META_SCHEMA ledger
        // must also fail closed rather than being relabeled by `init_meta_schema`.
        let populated_no_meta_path = dir.path().join("populated-no-meta.redb");
        {
            let db = create_redb_writable(
                &populated_no_meta_path,
                DatabaseRole::Repository,
                RedbRepairContext::WriteCommand,
            )
            .expect("create populated-no-meta redb");
            let wtx =
                begin_write_bounded_blocking(&db, DatabaseRole::Repository, Duration::from_secs(2))
                    .expect("begin seed data table");
            {
                let mut kv = wtx.open_table(DATA_KV).expect("open kv");
                kv.insert("existing", "row").expect("insert existing row");
            }
            wtx.commit().expect("commit data table without META_SCHEMA");

            let wtx =
                begin_write_bounded_blocking(&db, DatabaseRole::Repository, Duration::from_secs(2))
                    .expect("begin init on populated db");
            let populated_err = init_meta_schema(&wtx, DatabaseRole::Capture)
                .expect_err("init_meta_schema must refuse database with existing tables");
            assert_eq!(populated_err.stable_code_str(), "LBR-IO-001");
            assert!(
                populated_err
                    .to_string()
                    .contains("already contains tables"),
                "unexpected error for populated db without META_SCHEMA: {populated_err}"
            );
            assert!(
                !wtx.list_tables()
                    .expect("list tables after rejected init")
                    .any(|h| h.name() == META_SCHEMA_TABLE.name()),
                "rejected init_meta_schema must not create META_SCHEMA table"
            );
            drop(wtx);
        }

        // Pre-existing multimap-only database (zero regular tables) without a
        // META_SCHEMA ledger must also fail closed and leave no META_SCHEMA created.
        let multimap_only_path = dir.path().join("multimap-only-no-meta.redb");
        {
            let db = create_redb_writable(
                &multimap_only_path,
                DatabaseRole::Repository,
                RedbRepairContext::WriteCommand,
            )
            .expect("create multimap-only redb");
            let wtx =
                begin_write_bounded_blocking(&db, DatabaseRole::Repository, Duration::from_secs(2))
                    .expect("begin seed multimap table");
            {
                let mut mm = wtx
                    .open_multimap_table(MULTIMAP_KV)
                    .expect("open multimap table");
                mm.insert("tag", "v1").expect("insert multimap entry");
            }
            wtx.commit().expect("commit multimap-only database");

            let wtx =
                begin_write_bounded_blocking(&db, DatabaseRole::Repository, Duration::from_secs(2))
                    .expect("begin init on multimap-only db");
            let mm_err = init_meta_schema(&wtx, DatabaseRole::Capture)
                .expect_err("init_meta_schema must refuse multimap-only database");
            assert_eq!(mm_err.stable_code_str(), "LBR-IO-001");
            assert!(
                mm_err.to_string().contains("already contains tables"),
                "unexpected error for multimap-only db without META_SCHEMA: {mm_err}"
            );
            assert!(
                !wtx.list_tables()
                    .expect("list tables after rejected multimap init")
                    .any(|h| h.name() == META_SCHEMA_TABLE.name()),
                "rejected init_meta_schema on multimap-only db must not create META_SCHEMA table"
            );
            drop(wtx);

            let ro = open_redb_read_only(&multimap_only_path, DatabaseRole::Capture)
                .expect("open multimap-only db read-only");
            let ro_err = begin_checked_read(&ro, DatabaseRole::Capture)
                .expect_err("multimap-only db must still have no META_SCHEMA ledger");
            assert_eq!(ro_err.stable_code_str(), "LBR-IO-001");
        }

        // AC3 & AC4: Migration tombstone fence (`min_reader_version == u32::MAX`)
        // physically aborts subsequent read and write transactions on already-open
        // handles, even after another process commits the fence and unlinks the file.
        let fenced_path = dir.path().join("git.gen-00000001.redb");
        {
            let seed_db = create_redb_writable(
                &fenced_path,
                DatabaseRole::Repository,
                RedbRepairContext::WriteCommand,
            )
            .expect("create repo db for tombstone test");
            let wtx = begin_write_bounded_blocking(
                &seed_db,
                DatabaseRole::Repository,
                Duration::from_secs(2),
            )
            .expect("begin seed write");
            init_meta_schema(&wtx, DatabaseRole::Repository).expect("init repo META_SCHEMA");
            {
                let mut kv = wtx.open_table(DATA_KV).expect("open kv table");
                kv.insert("head", "v1").expect("seed head=v1");
            }
            wtx.commit().expect("commit initial repo state");
        }

        // Keep both a ReadOnlyDatabase handle and a writable Database handle open
        // BEFORE the external migrator process fences and unlinks the database.
        let stale_ro = open_redb_read_only(&fenced_path, DatabaseRole::Repository)
            .expect("open stale read-only handle before migration");
        let stale_rw = Arc::new(
            open_redb_writable(
                &fenced_path,
                DatabaseRole::Repository,
                RedbRepairContext::WriteCommand,
            )
            .expect("open stale writable handle before migration"),
        );

        // Sanity check: both handles read and write normally prior to fencing.
        {
            let rtx = begin_checked_read(&stale_ro, DatabaseRole::Repository)
                .expect("pre-fence checked read succeeds");
            let kv = rtx.open_table(DATA_KV).expect("open kv");
            assert_eq!(kv.get("head").unwrap().unwrap().value(), "v1");
        }
        rt.block_on(async {
            let wtx = begin_checked_write(
                Arc::clone(&stale_rw),
                DatabaseRole::Repository,
                Duration::from_secs(2),
            )
            .await
            .expect("pre-fence async checked write succeeds");
            {
                let mut kv = wtx.open_table(DATA_KV).expect("open kv");
                kv.insert("head", "v2").expect("update head=v2");
            }
            commit_write_bounded(wtx)
                .await
                .expect("commit pre-fence write");
        });

        // External migrator OS process writes `min_reader_version = u32::MAX`
        // and unlinks the obsolete file path (on Unix).
        let exe = env::current_exe().expect("locate current test executable");
        let migrator_status = Command::new(&exe)
            .arg("--exact")
            .arg("storage::meta::tests::meta_schema_role_future_version_and_migration_tombstone_fence")
            .arg("--nocapture")
            .env("LIBRA_RB02B_CHILD_MODE", "migrator_fence")
            .env("LIBRA_RB02B_DB_PATH", &fenced_path)
            .status()
            .expect("spawn migrator_fence child process");
        assert!(
            migrator_status.success(),
            "migrator_fence child failed with {migrator_status}"
        );

        // AC3: Already-open read handles (both ReadOnlyDatabase and Database)
        // immediately abort with `StorageBackendError::RepositoryMigrated`.
        let ro_fenced_err = begin_checked_read(&stale_ro, DatabaseRole::Repository)
            .expect_err("stale ReadOnlyDatabase handle must abort after tombstone fence");
        assert!(matches!(
            ro_fenced_err,
            StorageBackendError::RepositoryMigrated {
                role: DatabaseRole::Repository
            }
        ));
        assert_eq!(ro_fenced_err.stable_code_str(), "LBR-IO-001");

        let rw_read_fenced_err = begin_checked_read(&*stale_rw, DatabaseRole::Repository)
            .expect_err("stale Database read entry must abort after tombstone fence");
        assert!(matches!(
            rw_read_fenced_err,
            StorageBackendError::RepositoryMigrated {
                role: DatabaseRole::Repository
            }
        ));

        // AC4: Already-open writable handle immediately aborts on both
        // synchronous and async write transaction entry with `RepositoryMigrated`.
        let sync_write_err = begin_checked_write_blocking(
            &stale_rw,
            DatabaseRole::Repository,
            Duration::from_secs(2),
        )
        .err()
        .expect("stale blocking write entry must abort after tombstone fence");
        assert!(matches!(
            sync_write_err,
            StorageBackendError::RepositoryMigrated {
                role: DatabaseRole::Repository
            }
        ));
        assert_eq!(sync_write_err.stable_code_str(), "LBR-IO-001");

        let async_write_err = rt
            .block_on(begin_checked_write(
                Arc::clone(&stale_rw),
                DatabaseRole::Repository,
                Duration::from_secs(2),
            ))
            .err()
            .expect("stale async write entry must abort after tombstone fence");
        assert!(matches!(
            async_write_err,
            StorageBackendError::RepositoryMigrated {
                role: DatabaseRole::Repository
            }
        ));
        assert_eq!(async_write_err.stable_code_str(), "LBR-IO-001");

        // Even an explicit raw write calling `init_meta_schema` must fail closed
        // with `RepositoryMigrated` and never un-fence the tombstoned database.
        let raw_wtx = begin_write_bounded_blocking(
            &stale_rw,
            DatabaseRole::Repository,
            Duration::from_secs(2),
        )
        .expect("acquire raw write txn on stale handle for negative init test");
        let reinit_err = init_meta_schema(&raw_wtx, DatabaseRole::Repository)
            .expect_err("init_meta_schema must never overwrite a tombstoned DB");
        assert!(matches!(
            reinit_err,
            StorageBackendError::RepositoryMigrated {
                role: DatabaseRole::Repository
            }
        ));
        drop(raw_wtx);
    }
}
