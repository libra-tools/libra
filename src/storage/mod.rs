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

#[cfg(any(feature = "storage-redb", feature = "storage-queue"))]
pub mod codec;
pub mod error;

pub use error::{StorageBackendError, StorageResult};

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
