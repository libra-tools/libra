//! Storage backend abstraction, dual-store implementations, and capture queue.
//!
//! Track A modules live exclusively under `src/storage/` so storage work stays
//! isolated from business commands until Track B atomic integration.

#[cfg(not(any(feature = "storage-sqlite", feature = "storage-redb")))]
compile_error!(
    "At least one storage backend feature (`storage-sqlite` or `storage-redb`) must be enabled."
);

#[cfg(feature = "storage-redb")]
const _: () = assert!(
    cfg!(feature = "storage-queue"),
    "Enabling `storage-redb` must implicitly enable `storage-queue`."
);

pub mod backend;
#[cfg(any(feature = "storage-redb", feature = "storage-queue"))]
pub mod codec;
pub mod config_store;
pub mod error;
pub mod git_store;
pub mod manifest;
#[cfg(feature = "storage-redb")]
pub mod meta;
#[cfg(feature = "storage-redb")]
pub mod opener;
#[cfg(feature = "storage-redb")]
pub mod redb;
#[cfg(feature = "storage-sqlite")]
pub mod sqlite;

pub use backend::{
    LIBRA_STORAGE_BACKEND_ENV, REDB_RUNTIME_UNLOCKED, StorageBackendKind,
    assert_redb_runtime_unlocked,
};
pub use config_store::{
    CONFIG_FORMAT_MANIFEST_FILE, CONFIG_FORMAT_MANIFEST_VERSION, CONFIG_MIGRATION_LOCK_FILE,
    CONFIG_REDB_FILE_NAME, CONFIG_SQLITE_FILE_NAME, CONFIG_SQLITE_TOMBSTONE_SENTINEL, ConfigEntry,
    ConfigFormatManifest, ConfigInitPhase, ConfigScopeAuthority, ConfigScopeLock, ConfigStore,
    DEFAULT_CONFIG_SCOPE_LOCK_TIMEOUT, LIBRA_CONFIG_GLOBAL_DB_ENV, LIBRA_CONFIG_SYSTEM_DB_ENV,
    LegacyConfigEntry, LockedConfigScope, MetadataCasOutcome, MetadataEntryRecord, MetadataStore,
    SQLITE_MIGRATED_FENCE_SQL, SQLITE_MIGRATED_FENCE_TABLE, SchemaVersionReceipt,
    TombstoneReserveOutcome, config_format_manifest_path, config_migration_lock_path,
    config_redb_path_in_scope_dir, config_scope_dir_from_db_path, config_sqlite_path_in_scope_dir,
    inspect_config_scope_authority, is_config_sqlite_fenced_tombstone,
    is_config_sqlite_tombstone_sentinel, key_in_section, open_config_scope_for_read,
    open_config_scope_for_read_with_deadline, open_config_scope_read_only,
    open_config_scope_read_only_with_deadline, open_config_scope_read_write,
    open_config_scope_read_write_with_deadline, open_locked_config_scope_for_read,
    open_locked_config_scope_read_only, open_locked_config_scope_read_write,
    open_or_create_locked_config_scope_for_write, publish_config_format_manifest,
    read_config_format_manifest, reserve_config_sqlite_tombstone_hard_link,
};
#[cfg(unix)]
pub use config_store::{
    GLOBAL_CONFIG_FILE_MODE, GLOBAL_CONFIG_TOMBSTONE_MODE, REPOSITORY_CONFIG_FILE_MODE,
    SYSTEM_CONFIG_FILE_MODE, SYSTEM_CONFIG_TOMBSTONE_MODE, expected_config_file_mode,
    expected_config_tombstone_mode,
};
pub use error::{StorageBackendError, StorageResult};
#[cfg(feature = "storage-redb")]
pub use git_store::RedbGitStore;
#[cfg(feature = "storage-sqlite")]
pub use git_store::SqliteGitStore;
pub use git_store::{
    BridgePublicationPhase, BridgePublicationRecord, DEFAULT_NOTES_REF_PREFIX, GitStore,
    MAX_BRIDGE_OPERATION_ID_LEN, NoteBatchOutcome, NoteCasOp, NoteRecord, NoteWriteOutcome,
    OBLITERATION_FENCE_ERROR_PREFIX, ObjectHashKind, ObjectIndexRecord, ObjectObliterationRecord,
    ObliterationRequest, ObliterationState, REFLOG_HEAD_REF_NAME, RefCasOutcome, RefKey, RefRecord,
    RefTarget, ReflogEntry, ReflogWrite, RevisionOrdinalAppendOutcome, RevisionOrdinalMeta,
    is_obliteration_fenced_error, is_valid_bridge_operation_id,
};
pub use manifest::{
    INITIAL_STORAGE_GENERATION, LEGACY_SQLITE_DB_FILE, ManifestOrigin, PRIVATE_SUBDIR,
    Phase2DbState, Phase2Progress, REDB_SUBDIR, RedbGenerationPaths, ResolvedStorageLayout,
    STORAGE_FORMAT_MANIFEST_FILE, STORAGE_FORMAT_MANIFEST_VERSION, SourceDigests, StorageAuthority,
    StorageFormatManifest, canonical_capture_redb_rel_path, canonical_git_redb_rel_path,
    detect_storage_authority, format_generation_id, publish_storage_manifest,
    read_storage_manifest, resolve_redb_generation_paths, resolve_storage_layout,
    storage_manifest_path,
};
#[cfg(feature = "storage-redb")]
pub use meta::{
    CURRENT_REDB_FORMAT_VERSION, MAX_SUPPORTED_READER_VERSION, META_SCHEMA_CODEC_VERSION,
    META_SCHEMA_SINGLETON_KEY, META_SCHEMA_TABLE, MIGRATION_TOMBSTONE_MIN_READER_VERSION,
    MetaSchemaRecord, MetaSchemaRole, begin_checked_read, begin_checked_write,
    begin_checked_write_blocking, init_meta_schema, validate_meta_schema, verify_meta_schema_read,
    verify_meta_schema_write, write_migration_tombstone,
};
#[cfg(feature = "storage-redb")]
pub use opener::{
    CACHE_SIZE_LONG_LIVED, CACHE_SIZE_READ_ONLY, CACHE_SIZE_STANDARD_WRITE, RedbOpenProfile,
    RedbRepairContext, RedbRepairReport, RedbWriteProfile, begin_write_bounded,
    begin_write_bounded_blocking, commit_write_bounded, create_redb_writable, open_redb_read_only,
    open_redb_writable, open_redb_writable_for_repair, repair_redb_explicit,
};
#[cfg(feature = "storage-redb")]
pub use redb::RedbConfigStore;
#[cfg(feature = "storage-sqlite")]
pub use sqlite::SqliteConfigStore;

/// Returns whether the SQLite storage backend was compiled into this binary.
#[must_use]
pub const fn has_sqlite_backend() -> bool {
    cfg!(feature = "storage-sqlite")
}

/// Returns whether the redb storage backend was compiled into this binary.
#[must_use]
pub const fn has_redb_backend() -> bool {
    cfg!(feature = "storage-redb")
}

/// Returns whether the capture frame queue was compiled into this binary.
#[must_use]
pub const fn has_storage_queue() -> bool {
    cfg!(feature = "storage-queue")
}

#[cfg(test)]
mod tests {
    use tempfile::tempdir;

    use super::*;

    #[cfg(feature = "storage-sqlite")]
    #[test]
    fn sqlite_only_rejects_redb_override() {
        #[cfg(not(feature = "storage-redb"))]
        if let Some(dir) = std::env::var_os("LIBRA_RB01C_SQLITE_ONLY_CHILD_DIR") {
            let real_err = StorageBackendKind::resolve(std::path::Path::new(&dir)).expect_err(
                "sqlite-only build must reject LIBRA_STORAGE_BACKEND=redb via resolve()",
            );
            assert!(
                matches!(real_err, StorageBackendError::UnsupportedBackend { .. }),
                "expected UnsupportedBackend in sqlite-only child process, got {real_err:?}"
            );
            assert_eq!(real_err.stable_code_str(), "LBR-CLI-003");
            println!("RB01C_SQLITE_ONLY_CHILD_OK");
            return;
        }

        let fresh = tempdir().expect("create fresh storage dir");
        let err =
            StorageBackendKind::resolve_with_capabilities(fresh.path(), Some("redb"), true, false)
                .expect_err("sqlite-only capability must reject redb override");
        assert!(
            matches!(err, StorageBackendError::UnsupportedBackend { .. }),
            "expected UnsupportedBackend, got {err:?}"
        );
        assert_eq!(err.stable_code_str(), "LBR-CLI-003");

        #[cfg(not(feature = "storage-redb"))]
        {
            let exe = std::env::current_exe().expect("locate current test executable");
            let output = std::process::Command::new(exe)
                .arg("--exact")
                .arg("storage::tests::sqlite_only_rejects_redb_override")
                .arg("--nocapture")
                .env("LIBRA_RB01C_SQLITE_ONLY_CHILD_DIR", fresh.path())
                .env(LIBRA_STORAGE_BACKEND_ENV, "redb")
                .env("LIBRA_TEST", "1")
                .output()
                .expect("spawn isolated sqlite-only resolve child");
            let stdout = String::from_utf8_lossy(&output.stdout);
            let stderr = String::from_utf8_lossy(&output.stderr);
            assert!(
                output.status.success()
                    && stdout.contains("RB01C_SQLITE_ONLY_CHILD_OK")
                    && stdout.contains("1 passed; 0 failed"),
                "isolated sqlite-only resolve child did not execute expected test (status={:?}):\nstdout:\n{stdout}\nstderr:\n{stderr}",
                output.status
            );

            let resolved = StorageBackendKind::resolve_from_override(fresh.path(), Some("sqlite"))
                .expect("sqlite-only build must accept sqlite override");
            assert_eq!(resolved, StorageBackendKind::Sqlite);

            let default_resolved = StorageBackendKind::resolve_from_override(fresh.path(), None)
                .expect("sqlite-only build must default to sqlite");
            assert_eq!(default_resolved, StorageBackendKind::Sqlite);
        }
    }

    #[cfg(feature = "storage-redb")]
    #[test]
    fn redb_only_rejects_sqlite_override() {
        #[cfg(not(feature = "storage-sqlite"))]
        if let Some(dir) = std::env::var_os("LIBRA_RB01C_REDB_ONLY_CHILD_DIR") {
            let real_err = StorageBackendKind::resolve(std::path::Path::new(&dir)).expect_err(
                "redb-only build must reject LIBRA_STORAGE_BACKEND=sqlite via resolve()",
            );
            assert!(
                matches!(real_err, StorageBackendError::UnsupportedBackend { .. }),
                "expected UnsupportedBackend in redb-only child process, got {real_err:?}"
            );
            assert_eq!(real_err.stable_code_str(), "LBR-CLI-003");
            println!("RB01C_REDB_ONLY_CHILD_OK");
            return;
        }

        let fresh = tempdir().expect("create fresh storage dir");
        let err = StorageBackendKind::resolve_with_capabilities(
            fresh.path(),
            Some("sqlite"),
            false,
            true,
        )
        .expect_err("redb-only capability must reject sqlite override");
        assert!(
            matches!(err, StorageBackendError::UnsupportedBackend { .. }),
            "expected UnsupportedBackend, got {err:?}"
        );
        assert_eq!(err.stable_code_str(), "LBR-CLI-003");

        #[cfg(not(feature = "storage-sqlite"))]
        {
            let exe = std::env::current_exe().expect("locate current test executable");
            let output = std::process::Command::new(exe)
                .arg("--exact")
                .arg("storage::tests::redb_only_rejects_sqlite_override")
                .arg("--nocapture")
                .env("LIBRA_RB01C_REDB_ONLY_CHILD_DIR", fresh.path())
                .env(LIBRA_STORAGE_BACKEND_ENV, "sqlite")
                .env("LIBRA_TEST", "1")
                .output()
                .expect("spawn isolated redb-only resolve child");
            let stdout = String::from_utf8_lossy(&output.stdout);
            let stderr = String::from_utf8_lossy(&output.stderr);
            assert!(
                output.status.success()
                    && stdout.contains("RB01C_REDB_ONLY_CHILD_OK")
                    && stdout.contains("1 passed; 0 failed"),
                "isolated redb-only resolve child did not execute expected test (status={:?}):\nstdout:\n{stdout}\nstderr:\n{stderr}",
                output.status
            );
        }
    }
}
