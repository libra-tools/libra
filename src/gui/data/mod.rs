//! Read-only data seam for the desktop window.
//!
//! The GPUI main thread never reads a repository: it sends numbered requests to
//! one background worker and renders the snapshots it receives. Every database
//! connection is injected and opened without schema management, so a window
//! read cannot migrate the repository (ADR-UID-01/02).

mod history;
mod repo;
mod sessions;
mod snapshot;
mod state;
mod worker;

pub use history::{AttributionFilter, HistoryRow, age_label};
pub use repo::READ_BUSY_TIMEOUT;
pub use sessions::{SESSION_ROW_CAP, SessionRow, clock_from_epoch};
pub use snapshot::{DataError, RepoContext, Snapshot};
pub use state::{ProbeOutcome, SourceState};
pub use worker::{CLOSE_BUDGET, DataHandle, QUEUE_CAP, ReadOutcome};

#[cfg(test)]
mod tests;
