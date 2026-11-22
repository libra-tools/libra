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
pub mod error;
#[cfg(feature = "storage-redb")]
pub mod opener;

pub use backend::{
    LIBRA_STORAGE_BACKEND_ENV, REDB_RUNTIME_UNLOCKED, StorageBackendKind,
    assert_redb_runtime_unlocked,
};
pub use error::{StorageBackendError, StorageResult};
#[cfg(feature = "storage-redb")]
pub use opener::{
    CACHE_SIZE_LONG_LIVED, CACHE_SIZE_READ_ONLY, CACHE_SIZE_STANDARD_WRITE, RedbOpenProfile,
    RedbRepairContext, RedbRepairReport, RedbWriteProfile, begin_write_bounded,
    begin_write_bounded_blocking, commit_write_bounded, create_redb_writable, open_redb_read_only,
    open_redb_writable, open_redb_writable_for_repair, repair_redb_explicit,
};

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
    use super::*;

    #[cfg(feature = "storage-sqlite")]
    #[test]
    fn sqlite_only_rejects_redb_override() {
        #[cfg(not(feature = "storage-redb"))]
        if std::env::var_os("LIBRA_RB01C_SQLITE_ONLY_CHILD").is_some() {
            let real_err = StorageBackendKind::resolve().expect_err(
                "sqlite-only build must reject LIBRA_STORAGE_BACKEND=redb via resolve()",
            );
            assert!(
                matches!(real_err, StorageBackendError::UnsupportedBackend { .. }),
                "expected UnsupportedBackend in sqlite-only child process, got {real_err:?}"
            );
            assert_eq!(real_err.stable_code_str(), "LBR-CLI-003");
            return;
        }

        let err = StorageBackendKind::resolve_with_capabilities(Some("redb"), true, false)
            .expect_err("sqlite-only capability must reject redb override");
        assert!(
            matches!(err, StorageBackendError::UnsupportedBackend { .. }),
            "expected UnsupportedBackend, got {err:?}"
        );
        assert_eq!(err.stable_code_str(), "LBR-CLI-003");

        #[cfg(not(feature = "storage-redb"))]
        {
            let exe = std::env::current_exe().expect("locate current test executable");
            let status = std::process::Command::new(exe)
                .arg("--exact")
                .arg("storage::tests::sqlite_only_rejects_redb_override")
                .arg("--nocapture")
                .env("LIBRA_RB01C_SQLITE_ONLY_CHILD", "1")
                .env(LIBRA_STORAGE_BACKEND_ENV, "redb")
                .env("LIBRA_TEST", "1")
                .status()
                .expect("spawn isolated sqlite-only resolve child");
            assert!(
                status.success(),
                "isolated sqlite-only resolve child failed with {status}"
            );

            let resolved = StorageBackendKind::resolve_from_override(Some("sqlite"))
                .expect("sqlite-only build must accept sqlite override");
            assert_eq!(resolved, StorageBackendKind::Sqlite);

            let default_resolved = StorageBackendKind::resolve_from_override(None)
                .expect("sqlite-only build must default to sqlite");
            assert_eq!(default_resolved, StorageBackendKind::Sqlite);
        }
    }

    #[cfg(feature = "storage-redb")]
    #[test]
    fn redb_only_rejects_sqlite_override() {
        #[cfg(not(feature = "storage-sqlite"))]
        if std::env::var_os("LIBRA_RB01C_REDB_ONLY_CHILD").is_some() {
            let real_err = StorageBackendKind::resolve().expect_err(
                "redb-only build must reject LIBRA_STORAGE_BACKEND=sqlite via resolve()",
            );
            assert!(
                matches!(real_err, StorageBackendError::UnsupportedBackend { .. }),
                "expected UnsupportedBackend in redb-only child process, got {real_err:?}"
            );
            assert_eq!(real_err.stable_code_str(), "LBR-CLI-003");
            return;
        }

        let err = StorageBackendKind::resolve_with_capabilities(Some("sqlite"), false, true)
            .expect_err("redb-only capability must reject sqlite override");
        assert!(
            matches!(err, StorageBackendError::UnsupportedBackend { .. }),
            "expected UnsupportedBackend, got {err:?}"
        );
        assert_eq!(err.stable_code_str(), "LBR-CLI-003");

        #[cfg(not(feature = "storage-sqlite"))]
        {
            let exe = std::env::current_exe().expect("locate current test executable");
            let status = std::process::Command::new(exe)
                .arg("--exact")
                .arg("storage::tests::redb_only_rejects_sqlite_override")
                .arg("--nocapture")
                .env("LIBRA_RB01C_REDB_ONLY_CHILD", "1")
                .env(LIBRA_STORAGE_BACKEND_ENV, "sqlite")
                .env("LIBRA_TEST", "1")
                .status()
                .expect("spawn isolated redb-only resolve child");
            assert!(
                status.success(),
                "isolated redb-only resolve child failed with {status}"
            );
        }
    }
}
