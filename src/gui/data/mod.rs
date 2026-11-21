//! Read-only data seam for the desktop window.
//!
//! The GPUI main thread never reads a repository: it sends numbered requests to
//! one background worker and renders the snapshots it receives. Every database
//! connection is injected and opened without schema management, so a window
//! read cannot migrate the repository (ADR-UID-01/02).

mod repo;
mod snapshot;
mod state;
mod worker;

pub use repo::READ_BUSY_TIMEOUT;
pub use snapshot::{DataError, RepoContext, Snapshot};
pub use state::{ProbeOutcome, SourceState};
pub use worker::{CLOSE_BUDGET, DataHandle, QUEUE_CAP};

#[cfg(test)]
mod tests;
