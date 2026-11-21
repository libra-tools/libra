//! Snapshot and context types delivered from the data worker to the window.

use super::state::SourceState;

/// Repository context shown in the window header.
///
/// Paths are display labels relative to the repository; absolute host paths
/// never reach the window (ADR-UID-04).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RepoContext {
    /// Directory name of the working-tree root.
    pub worktree_label: String,
    /// Storage label (`".libra"`), not an absolute path.
    pub storage_label: String,
    /// Current branch name, or `"detached"`.
    pub branch: String,
    /// Abbreviated HEAD commit, or `"no commits"`.
    pub head: String,
    /// Object format reported by the repository configuration.
    pub object_format: String,
}

/// One immutable snapshot of a data source.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Snapshot {
    /// Request sequence number this snapshot answers.
    pub seq: u64,
    /// Source-level state the window must render.
    pub state: SourceState,
    /// Context when the source is available.
    pub context: Option<RepoContext>,
    /// Extra explanation for a failed source.
    pub message: Option<String>,
}

/// A typed read failure that the window renders as a source state.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DataError {
    /// State to render.
    pub state: SourceState,
    /// Explanation (already sanitized: no absolute host paths).
    pub message: String,
}

impl DataError {
    /// Build a failure for a state.
    pub fn new(state: SourceState, message: impl Into<String>) -> Self {
        Self {
            state,
            message: message.into(),
        }
    }
}
