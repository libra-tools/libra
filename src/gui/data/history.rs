//! History projection for the window: real commits with capture attribution.

use std::collections::HashMap;

/// Maximum History rows kept for the window.
pub const HISTORY_ROW_CAP: usize = 200;
/// Maximum commit objects loaded during the walk (bounded memory).
const HISTORY_WALK_DEPTH: usize = 400;

use super::{
    snapshot::DataError,
    state::{self, SourceState},
};
use crate::{internal::db, utils::util};

/// One commit row shown by the History surface.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HistoryRow {
    /// Full commit id.
    pub oid: String,
    /// Abbreviated commit id.
    pub short: String,
    /// Author name.
    pub author: String,
    /// Commit subject (truncated).
    pub subject: String,
    /// Author timestamp (seconds since the epoch).
    pub timestamp: usize,
    /// Number of parents (2+ means a merge commit).
    pub parents: usize,
    /// Parent object ids, preserved so the graph can derive edges.
    pub parent_oids: Vec<String>,
    /// Captured sessions linked to this commit.
    pub sessions: i64,
    /// Captured checkpoints linked to this commit.
    pub checkpoints: i64,
}

/// Attribution filter selected in the History toolbar.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AttributionFilter {
    All,
    Human,
    AgentAuthored,
    HasCheckpoint,
}

impl HistoryRow {
    /// Whether the row satisfies an attribution filter.
    pub fn matches(&self, filter: AttributionFilter) -> bool {
        match filter {
            AttributionFilter::All => true,
            AttributionFilter::Human => self.sessions == 0,
            AttributionFilter::AgentAuthored => self.sessions > 0,
            AttributionFilter::HasCheckpoint => self.checkpoints > 0,
        }
    }
}

/// Read the newest commits reachable from HEAD with capture attribution.
///
/// Uses an injected non-migrating connection for the repository and a
/// read-only capture connection; a window read never writes either database.
pub async fn read_history(
    scope: &crate::internal::worktree_scope::RequestScope,
    head_oid: Option<String>,
) -> Result<Vec<HistoryRow>, DataError> {
    let Some(head_oid) = head_oid else {
        return Ok(Vec::new());
    };
    let db_path = scope.storage.join(util::DATABASE);
    let db_path_str = db_path.to_str().ok_or_else(|| {
        DataError::new(
            SourceState::Corrupt,
            "the repository database path is not UTF-8",
        )
    })?;
    let conn =
        db::open_connection_without_schema_management(db_path_str, super::repo::READ_BUSY_TIMEOUT)
            .await
            .map_err(|error| {
                DataError::new(
                    state::classify_open_error(&error),
                    "the repository database is unusable",
                )
            })?;

    let head_oid = crate::internal::object_format::parse_repo_oid(&head_oid)
        .map_err(|_| DataError::new(SourceState::Corrupt, "HEAD is not a valid object id"))?;

    // Bounded traversal: never load more than HISTORY_WALK_DEPTH commits, so a
    // merge-heavy history cannot monopolize the worker or its memory.
    let shallow = crate::internal::shallow::ShallowSet::load().map_err(|error| {
        DataError::new(
            SourceState::Corrupt,
            format!("shallow metadata is unreadable: {}", error.hint()),
        )
    })?;
    let mut commits = walk_bounded(&head_oid, HISTORY_WALK_DEPTH, &shallow)?;
    commits.sort_by_key(|commit| std::cmp::Reverse(commit.committer.timestamp));
    commits.truncate(HISTORY_ROW_CAP);

    let ids: Vec<String> = commits.iter().map(|commit| commit.id.to_string()).collect();
    let attribution = read_attribution(scope, &conn, &ids).await;

    let mut rows = Vec::with_capacity(commits.len());
    for commit in commits {
        let oid = commit.id.to_string();
        let attribution = attribution.get(&oid).copied().unwrap_or_default();
        rows.push(HistoryRow {
            short: oid.chars().take(7).collect(),
            oid,
            author: commit.author.name.clone(),
            subject: subject_line(&commit.message),
            timestamp: commit.author.timestamp,
            parents: commit.parent_commit_ids.len(),
            parent_oids: commit
                .parent_commit_ids
                .iter()
                .map(|oid| oid.to_string())
                .collect(),
            sessions: attribution.sessions,
            checkpoints: attribution.checkpoints,
        });
    }
    // Rows stay in the committer-time order established above; author time is
    // display-only.
    Ok(rows)
}

/// Attribution is best-effort: a missing, unreadable or busy capture catalog
/// leaves every commit Human instead of hiding the commit list.
async fn read_attribution(
    scope: &crate::internal::worktree_scope::RequestScope,
    repo_conn: &sea_orm::DatabaseConnection,
    commit_ids: &[String],
) -> HashMap<String, crate::command::agent::CommitAttribution> {
    let capture_path = crate::internal::ai::capture::store::capture_database_path(&scope.storage);
    match state::probe_path(&capture_path) {
        state::ProbeOutcome::Present => {}
        _ => return HashMap::new(),
    }
    let Ok(capture_conn) = crate::internal::ai::capture::store::open_readonly(
        &scope.storage,
        super::repo::READ_BUSY_TIMEOUT,
    )
    .await
    else {
        return HashMap::new();
    };
    // Resolve the scope with the injected repository connection (no hidden
    // second connection) and verify the workspace fence with a SELECT-only
    // check.
    let Ok(capture_scope) =
        crate::internal::ai::capture_scope::CaptureScope::resolve(repo_conn, &scope.worktree_root)
            .await
    else {
        return HashMap::new();
    };
    if capture_scope
        .assert_workspace_fence_live(repo_conn)
        .await
        .is_err()
    {
        return HashMap::new();
    }
    crate::command::agent::commit_attribution_batch(&capture_conn, &capture_scope, commit_ids)
        .await
        .unwrap_or_default()
}

fn subject_line(message: &str) -> String {
    // Commit bodies can start with a newline; the subject is the first
    // non-empty line.
    let line = message
        .lines()
        .map(str::trim)
        .find(|line| !line.is_empty())
        .unwrap_or("");
    line.chars().take(200).collect()
}

/// Format an age label from a commit timestamp (test seam).
pub fn age_label(timestamp: usize, now: usize) -> String {
    let seconds = now.saturating_sub(timestamp);
    if seconds < 60 {
        "now".to_string()
    } else if seconds < 3600 {
        format!("{}m", seconds / 60)
    } else if seconds < 86_400 {
        format!("{}h", seconds / 3600)
    } else {
        format!("{}d", seconds / 86_400)
    }
}

/// Walk parents from HEAD, loading at most `max_commits` commit objects.
pub(super) fn walk_bounded(
    head: &git_internal::hash::ObjectHash,
    max_commits: usize,
    shallow: &crate::internal::shallow::ShallowSet,
) -> Result<Vec<git_internal::internal::object::commit::Commit>, DataError> {
    use std::collections::HashSet;

    use git_internal::internal::object::commit::Commit;

    let mut queue = std::collections::VecDeque::from([*head]);
    let mut seen: HashSet<git_internal::hash::ObjectHash> = HashSet::new();
    let mut commits = Vec::new();
    while let Some(oid) = queue.pop_front() {
        if commits.len() >= max_commits {
            break;
        }
        if !seen.insert(oid) {
            continue;
        }
        let commit: Commit = crate::command::load_object(&oid).map_err(|_error| {
            DataError::new(SourceState::Corrupt, "a commit object could not be read")
        })?;
        // Respect shallow boundaries: a shallow commit's parents are absent
        // from the object store by design.
        queue.extend(
            shallow
                .parents_for_walk(&commit.id, &commit.parent_commit_ids)
                .iter()
                .copied(),
        );
        commits.push(commit);
    }
    Ok(commits)
}
