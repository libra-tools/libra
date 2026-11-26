//! Unified storage backend error types and stable error-code mappings.
//!
//! All storage errors carry user-friendly, actionable descriptions and avoid
//! exposing raw private absolute paths, credentials, or session payloads
//! (`GC-RB-03` / `ER-11`).

use std::io;

use thiserror::Error;

use crate::{
    internal::db::schema::DatabaseRole,
    utils::error::{CliError, StableErrorCode},
};

/// Result alias for storage backend operations.
pub type StorageResult<T> = Result<T, StorageBackendError>;

/// Format sanitized I/O metadata using only structured kind and optional OS
/// error code so arbitrary `io::Error` payloads can never leak private paths
/// or credentials through `Display` or `Error::source` chains.
fn format_sanitized_io_fields(kind: io::ErrorKind, raw_os_error: Option<i32>) -> String {
    match raw_os_error {
        Some(code) => format!("{kind:?} (os error {code})"),
        None => format!("{kind:?}"),
    }
}

/// Unified error type returned by `crate::storage` backends, codecs, openers,
/// manifests, queues, and migrators.
#[derive(Debug, Error)]
pub enum StorageBackendError {
    /// Failed to serialize a value into the versioned binary envelope.
    #[error("failed to encode storage value ({context}): {detail}")]
    CodecEncode {
        context: &'static str,
        detail: String,
    },

    /// Encoded value was shorter than the 2-byte `u16_le` schema version header.
    #[error(
        "storage value is truncated: expected at least 2-byte schema version header, got {actual_len} byte(s)"
    )]
    TruncatedHeader { actual_len: usize },

    /// Encoded value had a valid 2-byte version header but its payload ended unexpectedly.
    #[error(
        "storage value payload is truncated for schema version {schema_ver} ({actual_payload_len} payload byte(s)): {detail}"
    )]
    TruncatedPayload {
        schema_ver: u16,
        actual_payload_len: usize,
        detail: String,
    },

    /// Encoded value payload bytes were malformed or contained trailing unconsumed bytes.
    #[error("storage value payload is corrupt for schema version {schema_ver}: {detail}")]
    CorruptPayload { schema_ver: u16, detail: String },

    /// Encoded value carried a schema version newer than this binary supports.
    #[error(
        "unsupported storage value schema version {found} (max supported: {max_supported}); upgrade Libra to read this database"
    )]
    UnsupportedSchemaVersion { found: u16, max_supported: u16 },

    /// Requested backend is not compiled into this binary or incompatible with the target.
    #[error("storage backend `{requested}` is not supported here: {detail}")]
    UnsupportedBackend { requested: String, detail: String },

    /// The `redb` runtime path is still locked by `REDB_RUNTIME_UNLOCKED = false`.
    #[error("redb storage runtime is locked prior to integration cutover: {detail}")]
    PreReleaseGateLocked { detail: String },

    /// Storage or configuration format manifest is incomplete (`finalized == false`).
    #[error(
        "storage migration is incomplete (manifest not finalized): {detail}; re-run `libra-migrate` to finish recovery"
    )]
    ManifestNotFinalized { detail: String },

    /// Environment or caller backend selection conflicts with repository/config authority.
    #[error("storage backend mismatch: {detail}")]
    BackendMismatch { detail: String },

    /// Database handle observed a `META_SCHEMA` migration tombstone (`min_reader_version == u32::MAX`).
    #[error(
        "{role} database has been migrated to a newer generation or backend; reopen the repository to use the active store"
    )]
    RepositoryMigrated { role: DatabaseRole },

    /// `open_read_only` encountered an unrecovered redb file (`DatabaseError::RepairAborted`).
    #[error(
        "{role} redb database requires crash recovery before read-only access ({detail}); run `libra fsck` or `libra agent doctor --repair`"
    )]
    RepairAborted { role: DatabaseRole, detail: String },

    /// Opened database role did not match the expected `GC-13` database role.
    #[error("database role mismatch: expected {expected}, found {found}")]
    RoleMismatch {
        expected: DatabaseRole,
        found: DatabaseRole,
    },

    /// Database file requires a newer Libra binary than the current one.
    #[error(
        "{role} database format requires reader version {found} (max supported: {max_supported}); install a newer Libra binary"
    )]
    SchemaTooNew {
        role: DatabaseRole,
        found: u32,
        max_supported: u32,
    },

    /// Bounded write-lock acquisition timed out under contention.
    #[error(
        "{role} database write lock remained busy after {waited_ms}ms; retry once concurrent writers finish"
    )]
    WriteLockBusy { role: DatabaseRole, waited_ms: u64 },

    /// Committed queue frame, prefix CRC32C, sealed segment boundary, or dual tail metadata failed verification.
    #[error(
        "committed capture queue data failed integrity verification: {detail}; run `libra agent doctor --repair` to inspect before modifying"
    )]
    CorruptCommittedFrame { detail: String },

    /// Capture queue capacity or sealed-segment cap rejected an incoming frame (backpressure).
    #[error("capture queue capacity limit reached: {detail}")]
    QueueFull { detail: String },

    /// Capture queue lock remained contended for the caller's wait budget.
    #[error("capture queue lock remained busy: {detail}")]
    QueueBusy { detail: String },

    /// Requested configuration scope or resource does not exist on disk.
    #[error("storage resource not found: {resource}")]
    NotFound { resource: String },

    /// Underlying filesystem or I/O error retaining only sanitized kind and
    /// OS errno (no raw `io::Error` source payload that could leak private
    /// paths or credentials via `Display` or `anyhow` `{error:#}` chains).
    #[error("storage I/O failure ({context}): {}", format_sanitized_io_fields(*.kind, *.raw_os_error))]
    Io {
        context: &'static str,
        kind: io::ErrorKind,
        raw_os_error: Option<i32>,
    },

    /// Generic backend or invariant error with actionable context.
    #[error("storage backend error: {detail}")]
    BackendInternal { detail: String },
}

impl StorageBackendError {
    /// Map this storage error to the authoritative Libra [`StableErrorCode`].
    #[must_use]
    pub const fn stable_error_code(&self) -> StableErrorCode {
        match self {
            Self::UnsupportedBackend { .. }
            | Self::PreReleaseGateLocked { .. }
            | Self::BackendMismatch { .. } => StableErrorCode::CliInvalidTarget,
            Self::SchemaTooNew { role, .. } => match role {
                DatabaseRole::Capture => StableErrorCode::CaptureSchemaTooNew,
                DatabaseRole::GlobalConfig | DatabaseRole::SystemConfig => {
                    StableErrorCode::ConfigSchemaFuture
                }
                DatabaseRole::Repository | DatabaseRole::Derived => StableErrorCode::IoReadFailed,
            },
            Self::WriteLockBusy {
                role: DatabaseRole::Capture,
                ..
            } => StableErrorCode::CaptureDatabaseBusy,
            Self::QueueFull { .. } => StableErrorCode::CaptureQueueFull,
            Self::QueueBusy { .. } => StableErrorCode::CaptureQueueBusy,
            Self::CodecEncode { .. }
            | Self::TruncatedHeader { .. }
            | Self::TruncatedPayload { .. }
            | Self::CorruptPayload { .. }
            | Self::UnsupportedSchemaVersion { .. }
            | Self::ManifestNotFinalized { .. }
            | Self::RepositoryMigrated { .. }
            | Self::RepairAborted { .. }
            | Self::RoleMismatch { .. }
            | Self::WriteLockBusy { .. }
            | Self::CorruptCommittedFrame { .. }
            | Self::NotFound { .. }
            | Self::Io { .. }
            | Self::BackendInternal { .. } => StableErrorCode::IoReadFailed,
        }
    }

    /// Return the stable `LBR-*` identifier string for diagnostics and tests.
    #[must_use]
    pub const fn stable_code_str(&self) -> &'static str {
        self.stable_error_code().as_str()
    }

    /// Convert this storage error into a user-facing [`CliError`] with its mapped stable code.
    #[must_use]
    pub fn into_cli_error(self) -> CliError {
        let code = self.stable_error_code();
        CliError::fatal(self.to_string()).with_stable_code(code)
    }
}

impl From<StorageBackendError> for CliError {
    fn from(err: StorageBackendError) -> Self {
        err.into_cli_error()
    }
}

impl StorageBackendError {
    /// Construct a sanitized [`StorageBackendError::Io`] from an `io::Error`,
    /// discarding any arbitrary custom error string payload.
    #[must_use]
    pub fn io(context: &'static str, source: io::Error) -> Self {
        Self::Io {
            context,
            kind: source.kind(),
            raw_os_error: source.raw_os_error(),
        }
    }

    /// Return the underlying [`io::ErrorKind`] when this is an `Io` error.
    #[must_use]
    pub const fn io_kind(&self) -> Option<io::ErrorKind> {
        match self {
            Self::Io { kind, .. } => Some(*kind),
            _ => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use std::io;

    use super::*;

    #[test]
    fn io_error_display_and_cli_error_do_not_leak_sensitive_source_text() {
        let secret_path = "/Users/private-home/.libra/private/secret.db";
        let secret_token = "sk-live-secret-9999";
        let source = io::Error::new(
            io::ErrorKind::PermissionDenied,
            format!("cannot open {secret_path} with bearer {secret_token}"),
        );
        let err = StorageBackendError::io("open capture store", source);
        assert_eq!(err.io_kind(), Some(io::ErrorKind::PermissionDenied));

        let displayed = err.to_string();
        assert!(displayed.contains("open capture store"));
        assert!(displayed.contains("PermissionDenied"));
        assert!(
            !displayed.contains(secret_path),
            "Display must not leak private path: {displayed}"
        );
        assert!(
            !displayed.contains(secret_token),
            "Display must not leak credential token: {displayed}"
        );

        let anyhow_err = anyhow::Error::new(err);
        let chained = format!("{anyhow_err:#}");
        assert!(
            !chained.contains(secret_path),
            "anyhow alternate chain must not leak private path: {chained}"
        );
        assert!(
            !chained.contains(secret_token),
            "anyhow alternate chain must not leak credential token: {chained}"
        );

        let cli_rendered = StorageBackendError::io(
            "open capture store",
            io::Error::new(
                io::ErrorKind::PermissionDenied,
                format!("cannot open {secret_path} with bearer {secret_token}"),
            ),
        )
        .into_cli_error()
        .render();
        assert!(
            !cli_rendered.contains(secret_path),
            "CliError must not leak private path: {cli_rendered}"
        );
        assert!(
            !cli_rendered.contains(secret_token),
            "CliError must not leak credential token: {cli_rendered}"
        );
    }
}
