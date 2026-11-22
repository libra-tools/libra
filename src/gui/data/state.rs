//! Source-level and item-level states for window data reads.
//!
//! The window never guesses a data source's condition: [`probe_path`] preserves
//! the filesystem error kind, [`classify_open_error`] maps a failed database
//! open, and [`classify_schema`] maps the schema compatibility report. The
//! desktop renders exactly the state returned here (ADR-UID-06).

use std::{fs::File, io::ErrorKind, path::Path};

use crate::internal::db::SchemaCompatibility;

/// State of a data source as the window may display it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SourceState {
    /// Readable and schema-compatible.
    Available,
    /// Not created yet (rendered as an empty state, not an error).
    Absent,
    /// Exists but cannot be read; the filesystem error kind is preserved.
    Unreadable { kind: ErrorKind },
    /// Readable but the database structure/content is broken.
    Corrupt,
    /// A writer holds the lock beyond the busy timeout.
    Busy,
    /// The on-disk schema is older than this binary.
    SchemaOld,
    /// The on-disk schema is newer than this binary.
    SchemaNew,
    /// Not checked yet (placeholder before a source verdict).
    Unchecked,
    /// A projection is stale (zero rows are shown).
    Stale,
    /// The window's data worker could not start.
    Worker,
}

impl SourceState {
    /// Short user-facing label.
    pub fn label(&self) -> &'static str {
        match self {
            Self::Available => "available",
            Self::Absent => "not created",
            Self::Unreadable { .. } => "unreadable",
            Self::Corrupt => "corrupt",
            Self::Busy => "busy",
            Self::SchemaOld => "schema upgrade required",
            Self::SchemaNew => "schema too new",
            Self::Unchecked => "未检查",
            Self::Stale => "stale",
            Self::Worker => "worker failed",
        }
    }

    /// Human-readable guidance for a failed source.
    pub fn hint(&self) -> Option<&'static str> {
        match self {
            Self::Unreadable { .. } => Some("check the file and its permissions"),
            Self::Corrupt => Some("run `libra fsck`, or restore the database"),
            Self::Busy => Some("another process is writing; reopen the window or retry"),
            Self::SchemaOld => {
                Some("run a command that opens the repository (e.g. `libra status`) to upgrade")
            }
            Self::SchemaNew => Some("use a newer `libra` binary"),
            Self::Stale => Some("run `libra memory rebuild`"),
            Self::Unchecked => None,
            Self::Worker => Some("reopen the window"),
            Self::Available | Self::Absent => None,
        }
    }

    /// True when the source is usable.
    pub fn is_available(&self) -> bool {
        matches!(self, Self::Available)
    }
}

/// Result of a filesystem probe that keeps the original error kind.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ProbeOutcome {
    /// The path exists and can be opened for reading.
    Present,
    /// The path does not exist.
    Absent,
    /// The path exists but the read-open probe failed.
    Unreadable { kind: ErrorKind },
}

/// Probe a path without opening the database: metadata first (to keep
/// `NotFound`/`PermissionDenied` distinct), then a read-open probe so a
/// permission failure on the file itself is not mistaken for absence.
pub fn probe_path(path: &Path) -> ProbeOutcome {
    match std::fs::metadata(path) {
        Ok(_) => match File::open(path) {
            Ok(_) => ProbeOutcome::Present,
            Err(error) => ProbeOutcome::Unreadable { kind: error.kind() },
        },
        Err(error) if error.kind() == ErrorKind::NotFound => ProbeOutcome::Absent,
        Err(error) => ProbeOutcome::Unreadable { kind: error.kind() },
    }
}

/// State for a probe outcome, given that the caller treats absence as an empty
/// state rather than an error.
pub fn state_for_probe(outcome: &ProbeOutcome) -> SourceState {
    match outcome {
        ProbeOutcome::Present => SourceState::Available,
        ProbeOutcome::Absent => SourceState::Absent,
        ProbeOutcome::Unreadable { kind } => SourceState::Unreadable { kind: *kind },
    }
}

/// Map a failed database open. A successful read-open probe followed by a
/// failed database open means the file is readable but its structure or lock
/// state is bad.
pub fn classify_open_error(error: &std::io::Error) -> SourceState {
    let text = error.to_string().to_ascii_lowercase();
    if text.contains("locked")
        || text.contains("busy")
        || text.contains("timed out")
        || text.contains("timedout")
        || text.contains("pool")
    {
        return SourceState::Busy;
    }
    match error.kind() {
        ErrorKind::PermissionDenied => SourceState::Unreadable {
            kind: ErrorKind::PermissionDenied,
        },
        ErrorKind::NotFound => SourceState::Absent,
        ErrorKind::WouldBlock | ErrorKind::TimedOut => SourceState::Busy,
        _ => SourceState::Corrupt,
    }
}

/// Classify a typed branch-store error: a decoded-row corruption is always
/// `Corrupt`; only a query failure may be lock contention.
pub fn classify_branch_store_error(
    error: &crate::internal::branch::BranchStoreError,
) -> SourceState {
    use crate::internal::branch::BranchStoreError;
    match error {
        BranchStoreError::Corrupt { .. } => SourceState::Corrupt,
        BranchStoreError::Query(text) => classify_store_message(text),
        BranchStoreError::NotFound(_) => SourceState::Corrupt,
        _ => SourceState::Corrupt,
    }
}

/// Classify an error chain: lock/pool timeouts anywhere in the chain mean
/// `Busy`; anything else is `Corrupt`.
pub fn classify_error_chain(error: &anyhow::Error) -> SourceState {
    let mut busy = false;
    for cause in error.chain() {
        let text = cause.to_string().to_ascii_lowercase();
        if text.contains("locked")
            || text.contains("busy")
            || text.contains("timed out")
            || text.contains("timedout")
            || text.contains("timeout")
            || text.contains("pool")
        {
            busy = true;
        }
    }
    if busy {
        SourceState::Busy
    } else {
        SourceState::Corrupt
    }
}

/// Classify a store error message (branch/HEAD queries wrap SQLite errors as
/// text): lock contention is `Busy`, everything else is `Corrupt`.
pub fn classify_store_message(text: &str) -> SourceState {
    let lower = text.to_ascii_lowercase();
    if lower.contains("locked")
        || lower.contains("busy")
        || lower.contains("timed out")
        || lower.contains("timedout")
        || lower.contains("pool")
    {
        SourceState::Busy
    } else {
        SourceState::Corrupt
    }
}

/// Map the schema compatibility report.
pub fn classify_schema(compatibility: &SchemaCompatibility) -> SourceState {
    match compatibility {
        SchemaCompatibility::Compatible { .. } => SourceState::Available,
        SchemaCompatibility::UpgradeRequired { .. } => SourceState::SchemaOld,
        SchemaCompatibility::UnsupportedFuture { .. } => SourceState::SchemaNew,
    }
}
