//! Snapshot and context types delivered from the data worker to the window.

use super::state::SourceState;

/// Repository context used by the status bar and repository views.
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
    /// Pinned HEAD commit (used to keep History on the same revision).
    pub head_oid: Option<String>,
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
    /// History rows (empty for non-History readers).
    pub history: Vec<super::history::HistoryRow>,
    /// Captured sessions for the Sessions surface.
    pub sessions: Vec<super::sessions::SessionRow>,
    /// Operation snapshot for the Operations surface.
    pub operations: super::operations::OperationsRead,
    /// Operations-specific failure while the context itself is available.
    pub operations_error: Option<String>,
    /// Memory projection for the Memory surface.
    pub memory: super::memory::MemoryRead,
    /// Memory-specific failure while the context itself is available.
    pub memory_error: Option<String>,
    /// Run records for Reviews/Investigate.
    pub runs: super::runs::RunsRead,
    /// Runs-specific failure while the context itself is available.
    pub runs_error: Option<String>,
    /// Sessions-specific failure while the context itself is available.
    pub sessions_error: Option<String>,
    /// The typed Sessions source state.
    pub sessions_state: SourceState,
    /// History-specific failure while the context itself is available.
    pub history_error: Option<String>,
    /// The typed History source state.
    pub history_state: SourceState,
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
