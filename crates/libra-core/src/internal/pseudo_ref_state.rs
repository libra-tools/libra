//! Read-only state queries for pseudo-ref projection (`ORIG_HEAD`, `MERGE_HEAD`,
//! `MERGE_MSG`, `REVERT_HEAD`, `REBASE_HEAD`) sunk from `command::{merge,revert,rebase,bisect}`.

use std::{fs, path::Path};

use sea_orm::{ConnectionTrait, DbBackend, Statement};
use serde::Deserialize;

use crate::internal::worktree_scope::WorktreeScope;

/// Minimal read-only projection of `merge-state.json` needed by `pseudo_ref`.
#[doc(hidden)]
#[derive(Debug, Clone, Deserialize)]
pub struct PseudoRefMergeState {
    pub orig_head: String,
    pub target: String,
    #[serde(default)]
    pub targets: Vec<String>,
    #[serde(default)]
    pub message: Option<String>,
    #[serde(default)]
    pub auto_merge: Option<String>,
}

#[derive(Debug, Deserialize)]
struct PseudoRefRevertState {
    orig_head: String,
    reverted_commit: String,
}

#[doc(hidden)]
pub fn merge_state_for_pseudo_refs(gitdir: &Path) -> Result<Option<PseudoRefMergeState>, String> {
    let path = gitdir.join("merge-state.json");
    if !path.exists() {
        return Ok(None);
    }
    let data = fs::read_to_string(&path)
        .map_err(|error| format!("failed to read {}: {error}", path.display()))?;
    serde_json::from_str(&data)
        .map(Some)
        .map_err(|error| format!("failed to parse {}: {error}", path.display()))
}

#[doc(hidden)]
pub fn revert_state_for_pseudo_refs(gitdir: &Path) -> Result<Option<(String, String)>, String> {
    let path = gitdir.join("revert-state.json");
    if !path.exists() {
        return Ok(None);
    }
    let data = fs::read_to_string(&path)
        .map_err(|error| format!("failed to read {}: {error}", path.display()))?;
    let state: PseudoRefRevertState = serde_json::from_str(&data)
        .map_err(|error| format!("failed to parse {}: {error}", path.display()))?;
    Ok(Some((state.orig_head, state.reverted_commit)))
}

#[doc(hidden)]
pub async fn rebase_orig_head_for_scope(scope: &WorktreeScope) -> Result<Option<String>, String> {
    let db = crate::internal::sequencer::request_db_checked().await?;
    let stmt = Statement::from_sql_and_values(
        DbBackend::Sqlite,
        "SELECT orig_head FROM rebase_state WHERE worktree_id = ? LIMIT 1;",
        [scope.storage_key().into()],
    );
    let Some(row) = db
        .query_one_raw(stmt)
        .await
        .map_err(|e| format!("failed to load rebase_state: {e}"))?
    else {
        return Ok(None);
    };
    let orig_head: String = row
        .try_get_by_index(0)
        .map_err(|e| format!("failed to decode rebase_state.orig_head: {e}"))?;
    Ok(Some(orig_head))
}

#[doc(hidden)]
pub async fn rebase_stopped_sha_for_scope(scope: &WorktreeScope) -> Result<Option<String>, String> {
    let db = crate::internal::sequencer::request_db_checked().await?;
    let stmt = Statement::from_sql_and_values(
        DbBackend::Sqlite,
        "SELECT stopped_sha FROM rebase_state WHERE worktree_id = ? LIMIT 1;",
        [scope.storage_key().into()],
    );
    let Some(row) = db
        .query_one_raw(stmt)
        .await
        .map_err(|e| format!("failed to load rebase_state: {e}"))?
    else {
        return Ok(None);
    };
    let stopped_sha: Option<String> = row
        .try_get_by_index(0)
        .map_err(|e| format!("failed to decode rebase_state.stopped_sha: {e}"))?;
    Ok(stopped_sha.filter(|s| !s.is_empty()))
}

#[doc(hidden)]
pub async fn bisect_orig_head_for_scope(scope: &WorktreeScope) -> Result<Option<String>, String> {
    let db = crate::internal::sequencer::request_db_checked().await?;
    let stmt = Statement::from_sql_and_values(
        DbBackend::Sqlite,
        "SELECT orig_head FROM bisect_state WHERE worktree_id = ? LIMIT 1;",
        [scope.storage_key().into()],
    );
    let Some(row) = db
        .query_one_raw(stmt)
        .await
        .map_err(|e| format!("failed to load bisect state: {e}"))?
    else {
        return Ok(None);
    };
    let orig_head: String = row
        .try_get_by_index(0)
        .map_err(|e| format!("failed to decode bisect_state.orig_head: {e}"))?;
    Ok(Some(orig_head))
}
