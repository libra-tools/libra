//! Persist projected `Episode` rows into the zero-authority projection tables.
//!
//! All writes go through the `memory_episode` / `memory_episode_evidence` /
//! `memory_episode_path` tables created by DM-01 / DM-10. Each insert is an
//! idempotent upsert, so `rebuild` and incremental convergence never duplicate
//! rows (GC-DM-01).

use sea_orm::{ConnectionTrait, DatabaseConnection, Statement, TransactionTrait};

use super::{episode::Episode, error::MemoryError};

/// Publish the complete batch atomically, including every evidence and path row.
/// A failed write leaves the previously committed projection unchanged.
pub async fn project_episodes(
    conn: &DatabaseConnection,
    episodes: &[Episode],
) -> Result<(), MemoryError> {
    let transaction = conn.begin().await?;
    for episode in episodes {
        project_episode(&transaction, episode).await?;
    }
    transaction.commit().await?;
    Ok(())
}

async fn project_episode<C: ConnectionTrait>(
    conn: &C,
    episode: &Episode,
) -> Result<(), MemoryError> {
    let backend = conn.get_database_backend();
    conn.execute_raw(Statement::from_sql_and_values(
        backend,
        "INSERT OR REPLACE INTO `memory_episode` \
         (`episode_id`, `repo_id`, `source_kind`, `source_key`, `outcome`, `actor`, \
          `started_at`, `ended_at`, `anchor_commit`, `change_id`, `title`, `body`, \
          `content_digest`, `producer`, `rules_version`) \
         VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)",
        [
            episode.episode_id.clone().into(),
            episode.repo_id.clone().into(),
            episode.source_kind.as_str().into(),
            episode.source_key.clone().into(),
            episode.outcome.as_str().into(),
            episode.actor.clone().into(),
            episode.started_at.into(),
            episode.ended_at.into(),
            episode.anchor_commit.clone().into(),
            episode.change_id.clone().into(),
            episode.title.clone().into(),
            episode.body.clone().into(),
            episode.content_digest.clone().into(),
            episode.producer.into(),
            episode.rules_version.into(),
        ],
    ))
    .await?;

    for (ordinal, evidence) in episode.evidence.iter().enumerate() {
        conn.execute_raw(Statement::from_sql_and_values(
            backend,
            "INSERT OR REPLACE INTO `memory_episode_evidence` \
             (`episode_id`, `ordinal`, `kind`, `ref_id`, `link_confidence`, `resolution_status`) \
             VALUES (?, ?, ?, ?, ?, ?)",
            [
                episode.episode_id.clone().into(),
                (ordinal as i64).into(),
                evidence.kind.clone().into(),
                evidence.ref_id.clone().into(),
                evidence.link_confidence.clone().into(),
                evidence.resolution_status.clone().into(),
            ],
        ))
        .await?;
    }

    for path in &episode.paths {
        conn.execute_raw(Statement::from_sql_and_values(
            backend,
            "INSERT OR REPLACE INTO `memory_episode_path` \
             (`episode_id`, `code_path`, `change_kind`, `blob_oid_at_end`, `ended_at`) \
             VALUES (?, ?, ?, ?, ?)",
            [
                episode.episode_id.clone().into(),
                path.code_path.clone().into(),
                path.change_kind.clone().into(),
                path.blob_oid_at_end.clone().into(),
                path.ended_at.into(),
            ],
        ))
        .await?;
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// plan-20260926 DM-13: freshness, horizon window and rebuild equivalence.
// ---------------------------------------------------------------------------

use crate::internal::ai::memory::derive_commit::derive_commit_episodes;

/// A deterministic fingerprint over the horizon-window operation set, NOT over
/// `max(end_ts)`. Terminalizations land in-place; a same-ms replacement or a
/// late terminalization below an existing `max(end_ts)` changes the set, so a
/// `max`-only fingerprint would miss it (AC `freshness_detects_*`).
pub fn commit_fingerprint(operations: &[(String, String, Option<i64>)]) -> String {
    use sha2::{Digest, Sha256};
    let mut entries: Vec<String> = operations
        .iter()
        .map(|(op_id, status, end_ts)| {
            format!(
                "{op_id}\x1f{status}\x1f{}",
                end_ts.map(|v| v.to_string()).unwrap_or_default()
            )
        })
        .collect();
    entries.sort();
    let mut hasher = Sha256::new();
    for entry in entries {
        hasher.update(entry.as_bytes());
        hasher.update(b"\n");
    }
    format!("{:x}", hasher.finalize())
}

/// Report returned by a commit rebuild.
#[derive(Clone, Debug, Eq, PartialEq, serde::Serialize)]
pub struct RebuildReport {
    pub repo_id: String,
    pub projected: usize,
    pub horizon_truncated: bool,
    pub revoked_count: i64,
    pub aged_out_count: i64,
}

/// Rebuild the commit projection for `repo_id` from scratch and persist the
/// `memory_projection_state` row (GC-DM-01). The projection domain equals the
/// horizon window; rows whose change is no longer in the window are aged out,
/// and rows that no longer satisfy the terminal predicate are revoked.
/// Source-read or publication errors preserve the previous committed projection
/// and its state; no partially rebuilt generation is left in storage.
pub async fn rebuild(
    conn: &DatabaseConnection,
    repo_id: &str,
    horizon: usize,
) -> Result<RebuildReport, MemoryError> {
    // Finish source reads before replacing any committed projection. Publishing
    // rows and their state together also rolls back a late persistence failure.
    let episodes = derive_commit_episodes(conn, repo_id, horizon).await?;
    let operations = commit_operations(conn, repo_id).await?;
    let transaction = conn.begin().await?;

    // Clear the repo's projected rows so rebuild converges on the horizon window.
    for (table, has_episode_id) in [
        ("memory_episode_path", true),
        ("memory_episode_evidence", true),
    ] {
        if has_episode_id {
            transaction
                .execute_raw(Statement::from_sql_and_values(
                    transaction.get_database_backend(),
                    format!(
                        "DELETE FROM `{table}` WHERE `{table}`.episode_id IN \
                     (SELECT episode_id FROM `memory_episode` WHERE repo_id = ?)"
                    ),
                    [repo_id.to_string().into()],
                ))
                .await?;
        }
    }
    transaction
        .execute_raw(Statement::from_sql_and_values(
            transaction.get_database_backend(),
            "DELETE FROM `memory_episode` WHERE `repo_id` = ?",
            [repo_id.to_string().into()],
        ))
        .await?;

    let projected = episodes.len();
    for episode in &episodes {
        project_episode(&transaction, episode).await?;
    }

    let fingerprint = commit_fingerprint(&operations);
    let horizon_truncated = projected >= horizon;
    let report = RebuildReport {
        repo_id: repo_id.to_string(),
        projected,
        horizon_truncated,
        revoked_count: 0,
        aged_out_count: 0,
    };

    transaction
        .execute_raw(Statement::from_sql_and_values(
            transaction.get_database_backend(),
            "INSERT OR REPLACE INTO `memory_projection_state` \
         (`repo_id`, `source_kind`, `cursor_json`, `fingerprint`, `rules_version`, \
          `schema_version`, `horizon_truncated`, `revoked_count`, `aged_out_count`, \
          `rebuilt_at`) VALUES (?, 'commit', '{}', ?, 1, 1, ?, ?, ?, \
          CAST(strftime('%s','now') AS INTEGER))",
            [
                repo_id.to_string().into(),
                fingerprint.into(),
                i64::from(horizon_truncated).into(),
                report.revoked_count.into(),
                report.aged_out_count.into(),
            ],
        ))
        .await?;

    transaction.commit().await?;
    Ok(report)
}

/// Read the persisted commit projection state (`meta`).
pub async fn meta(
    conn: &DatabaseConnection,
    repo_id: &str,
) -> Result<Option<(String, bool, i64, i64)>, MemoryError> {
    read_meta_with_tag(conn, repo_id).await
}

async fn read_meta_with_tag(
    conn: &DatabaseConnection,
    repo_id: &str,
) -> Result<Option<(String, bool, i64, i64)>, MemoryError> {
    use sea_orm::{ConnectionTrait, Statement};
    Ok(conn
        .query_one_raw(Statement::from_sql_and_values(
            conn.get_database_backend(),
            "SELECT fingerprint, horizon_truncated, revoked_count, aged_out_count \
             FROM `memory_projection_state` WHERE repo_id = ? AND source_kind = 'commit'",
            [repo_id.to_string().into()],
        ))
        .await?
        .map(|row| {
            let fingerprint: String = row.try_get("", "fingerprint").unwrap_or_default();
            let horizon_truncated: i64 = row.try_get("", "horizon_truncated").unwrap_or(0);
            let revoked_count: i64 = row.try_get("", "revoked_count").unwrap_or(0);
            let aged_out_count: i64 = row.try_get("", "aged_out_count").unwrap_or(0);
            (
                fingerprint,
                horizon_truncated == 1,
                revoked_count,
                aged_out_count,
            )
        }))
}

/// Read the horizon-window operation set (op_id, status, end_ts).
async fn commit_operations<C: sea_orm::ConnectionTrait>(
    conn: &C,
    repo_id: &str,
) -> Result<Vec<(String, String, Option<i64>)>, MemoryError> {
    use sea_orm::{ColumnTrait, EntityTrait, QueryFilter, QueryOrder};

    use crate::internal::model::{ai_operation_link, operation};

    let op_ids: Vec<String> = ai_operation_link::Entity::find()
        .filter(ai_operation_link::Column::RepoId.eq(repo_id))
        .all(conn)
        .await?
        .into_iter()
        .map(|link| link.operation_id)
        .collect();
    if op_ids.is_empty() {
        return Ok(Vec::new());
    }
    // Batch the IN clauses to stay under SQLite's bind-variable limit while
    // preserving the canonical op-id ordering.
    let mut ops: Vec<(String, String, Option<i64>)> = Vec::with_capacity(op_ids.len());
    for batch in op_ids.chunks(500) {
        let mut rows = operation::Entity::find()
            .filter(operation::Column::OpId.is_in(batch.iter().cloned()))
            .order_by_asc(operation::Column::OpId)
            .all(conn)
            .await?
            .into_iter()
            .map(|op| (op.op_id, op.status, op.end_ts))
            .collect::<Vec<_>>();
        ops.append(&mut rows);
    }
    ops.sort_by(|a, b| a.0.cmp(&b.0));
    Ok(ops)
}

// ---------------------------------------------------------------------------
// Backend-agnostic `GitStateStore` projection path (RB-23).
// ---------------------------------------------------------------------------

#[allow(dead_code)]
fn to_storage_source_kind(kind: super::episode::SourceKind) -> crate::storage::MemorySourceKind {
    match kind {
        super::episode::SourceKind::Commit => crate::storage::MemorySourceKind::Commit,
        super::episode::SourceKind::AgentSession => crate::storage::MemorySourceKind::AgentSession,
        super::episode::SourceKind::AgentRun => crate::storage::MemorySourceKind::AgentRun,
        super::episode::SourceKind::BridgeOperation => {
            crate::storage::MemorySourceKind::BridgeOperation
        }
    }
}

#[allow(dead_code)]
fn to_storage_outcome(outcome: super::episode::Outcome) -> crate::storage::MemoryOutcome {
    match outcome {
        super::episode::Outcome::Succeeded => crate::storage::MemoryOutcome::Succeeded,
        super::episode::Outcome::Failed => crate::storage::MemoryOutcome::Failed,
        super::episode::Outcome::Aborted => crate::storage::MemoryOutcome::Aborted,
        super::episode::Outcome::Partial => crate::storage::MemoryOutcome::Partial,
        super::episode::Outcome::Unknown => crate::storage::MemoryOutcome::Unknown,
    }
}

#[allow(dead_code)]
struct PreparedMemoryEpisode {
    episode: crate::storage::MemoryEpisodeRecord,
    evidence: Vec<crate::storage::MemoryEpisodeEvidenceRecord>,
    paths: Vec<crate::storage::MemoryEpisodePathRecord>,
    paths_text: String,
}

#[allow(dead_code)]
fn prepare_episode_records(episode: &Episode) -> Result<PreparedMemoryEpisode, MemoryError> {
    let ep_rec = crate::storage::MemoryEpisodeRecord {
        episode_id: episode.episode_id.clone(),
        repo_id: episode.repo_id.clone(),
        source_kind: to_storage_source_kind(episode.source_kind),
        source_key: episode.source_key.clone(),
        outcome: to_storage_outcome(episode.outcome),
        actor: episode.actor.clone(),
        started_at: episode.started_at,
        ended_at: episode.ended_at,
        anchor_commit: episode.anchor_commit.clone(),
        change_id: episode.change_id.clone(),
        title: episode.title.clone(),
        body: episode.body.clone(),
        content_digest: episode.content_digest.clone(),
        producer: episode.producer.to_string(),
        rules_version: episode.rules_version,
    };

    let mut ev_recs = Vec::with_capacity(episode.evidence.len());
    for (ordinal, edge) in episode.evidence.iter().enumerate() {
        ev_recs.push(crate::storage::MemoryEpisodeEvidenceRecord {
            episode_id: episode.episode_id.clone(),
            ordinal: i64::try_from(ordinal).unwrap_or(i64::MAX),
            kind: crate::storage::MemoryEvidenceKind::parse(&edge.kind)?,
            ref_id: edge.ref_id.clone(),
            link_confidence: crate::storage::MemoryLinkConfidence::parse(&edge.link_confidence)?,
            resolution_status: crate::storage::MemoryResolutionStatus::parse(
                &edge.resolution_status,
            )?,
        });
    }

    let mut path_recs = Vec::with_capacity(episode.paths.len());
    let mut rendered_paths = Vec::with_capacity(episode.paths.len());
    for path in &episode.paths {
        let change_kind = crate::storage::MemoryPathChangeKind::parse(&path.change_kind)?;
        rendered_paths.push(super::reader::render_code_path(&path.code_path));
        path_recs.push(crate::storage::MemoryEpisodePathRecord {
            episode_id: episode.episode_id.clone(),
            code_path: path.code_path.clone(),
            change_kind,
            blob_oid_at_end: path.blob_oid_at_end.clone(),
            ended_at: path.ended_at,
        });
    }

    Ok(PreparedMemoryEpisode {
        episode: ep_rec,
        evidence: ev_recs,
        paths: path_recs,
        paths_text: rendered_paths.join(" "),
    })
}

/// Persist projected `Episode` rows (including evidence, path index, and search
/// documents) into a backend-agnostic [`crate::storage::GitStateStore`].
#[allow(dead_code)]
pub async fn project_episodes_with_store(
    state_store: &dyn crate::storage::GitStateStore,
    episodes: &[Episode],
) -> Result<(), MemoryError> {
    let mut prepared = Vec::with_capacity(episodes.len());
    for episode in episodes {
        prepared.push(prepare_episode_records(episode)?);
    }

    for item in &prepared {
        // Clear any prior episode + cascaded evidence/path/search_doc rows so
        // replacements never leave orphaned children behind.
        let _ = state_store
            .delete_memory_episode(&item.episode.episode_id)
            .await?;
        state_store.upsert_memory_episode(&item.episode).await?;
        state_store
            .replace_memory_episode_evidence(&item.episode.episode_id, &item.evidence)
            .await?;
        for path in &item.paths {
            state_store.upsert_memory_episode_path(path).await?;
        }
        let _ = state_store
            .upsert_memory_episode_search_doc(
                &item.episode.episode_id,
                &item.episode.title,
                &item.episode.body,
                &item.paths_text,
            )
            .await?;
    }
    Ok(())
}

/// Rebuild the commit memory projection for `repo_id` from scratch on a
/// backend-agnostic [`crate::storage::GitStateStore`].
#[allow(dead_code)]
pub async fn rebuild_with_store(
    state_store: &dyn crate::storage::GitStateStore,
    repo_id: &str,
    horizon: usize,
) -> Result<RebuildReport, MemoryError> {
    // Finish source reads before replacing any committed projection.
    let episodes =
        super::derive_commit::derive_commit_episodes_with_store(state_store, repo_id, horizon)
            .await?;
    let operations = commit_operations_with_store(state_store, repo_id).await?;

    let existing = state_store.list_memory_episodes(repo_id).await?;
    for old in existing {
        let _ = state_store.delete_memory_episode(&old.episode_id).await?;
    }

    project_episodes_with_store(state_store, &episodes).await?;

    let projected = episodes.len();
    let fingerprint = commit_fingerprint(&operations);
    let horizon_truncated = projected >= horizon;
    let report = RebuildReport {
        repo_id: repo_id.to_string(),
        projected,
        horizon_truncated,
        revoked_count: 0,
        aged_out_count: 0,
    };

    let state = crate::storage::MemoryProjectionStateRecord {
        repo_id: repo_id.to_string(),
        source_kind: crate::storage::MemorySourceKind::Commit,
        cursor_json: "{}".to_string(),
        fingerprint,
        rules_version: 1,
        schema_version: 1,
        horizon_truncated,
        revoked_count: report.revoked_count,
        aged_out_count: report.aged_out_count,
        rebuilt_at: chrono::Utc::now().timestamp(),
    };
    state_store.upsert_memory_projection_state(&state).await?;

    Ok(report)
}

/// Read the persisted commit projection state metadata from a
/// [`crate::storage::GitStateStore`].
#[allow(dead_code)]
pub async fn meta_with_store(
    state_store: &dyn crate::storage::GitStateStore,
    repo_id: &str,
) -> Result<Option<(String, bool, i64, i64)>, MemoryError> {
    let state = state_store
        .get_memory_projection_state(repo_id, crate::storage::MemorySourceKind::Commit)
        .await?;
    Ok(state.map(|row| {
        (
            row.fingerprint,
            row.horizon_truncated,
            row.revoked_count,
            row.aged_out_count,
        )
    }))
}

/// Read the horizon-window operation set `(op_id, status, end_ts)` for `repo_id`
/// via [`crate::storage::GitStateStore`].
pub(super) async fn commit_operations_with_store(
    state_store: &dyn crate::storage::GitStateStore,
    repo_id: &str,
) -> Result<Vec<(String, String, Option<i64>)>, MemoryError> {
    let operations = state_store.list_operations(repo_id).await?;
    let mut ops: Vec<(String, String, Option<i64>)> = Vec::new();
    for (op, _) in operations {
        if let Some(link) = state_store.get_ai_operation_link(&op.op_id).await?
            && link.repo_id == repo_id
        {
            ops.push((op.op_id, op.status, op.end_ts));
        }
    }
    ops.sort_by(|a, b| a.0.cmp(&b.0));
    Ok(ops)
}

#[cfg(test)]
mod atomicity_tests {
    use sea_orm::{Database, DatabaseConnection, DbBackend};

    use super::*;
    use crate::internal::ai::memory::{EvidenceEdge, Outcome, PathRow, SourceKind, content_digest};

    async fn database() -> DatabaseConnection {
        let db = Database::connect("sqlite::memory:").await.unwrap();
        db.execute_unprepared("PRAGMA foreign_keys = ON")
            .await
            .unwrap();
        db.execute_unprepared(include_str!(
            "../../../../sql/migrations/2026092901_memory_core.sql"
        ))
        .await
        .unwrap();
        db.execute_unprepared(include_str!(
            "../../../../sql/migrations/2026092902_memory_path_search.sql"
        ))
        .await
        .unwrap();
        db
    }

    fn episode(key: &str) -> Episode {
        let mut episode = Episode {
            episode_id: Episode::compute_episode_id("repo", SourceKind::Commit, key),
            repo_id: "repo".to_string(),
            source_kind: SourceKind::Commit,
            source_key: key.to_string(),
            outcome: Outcome::Succeeded,
            actor: None,
            started_at: 1,
            ended_at: 2,
            anchor_commit: None,
            change_id: None,
            title: format!("old {key}"),
            body: "original".to_string(),
            content_digest: String::new(),
            producer: "derived-v1",
            rules_version: 1,
            evidence: vec![EvidenceEdge {
                kind: "commit".to_string(),
                ref_id: "source".to_string(),
                link_confidence: "identity".to_string(),
                resolution_status: "resolved".to_string(),
            }],
            paths: vec![PathRow {
                code_path: b"src/lib.rs".to_vec(),
                change_kind: "modified".to_string(),
                blob_oid_at_end: Some("original-blob".to_string()),
                ended_at: 2,
            }],
        };
        episode.content_digest = content_digest(&episode);
        episode
    }

    async fn snapshot(db: &DatabaseConnection) -> Vec<String> {
        let mut rows = Vec::new();
        for sql in [
            "SELECT episode_id || ':' || title || ':' || body || ':' || content_digest AS value FROM memory_episode ORDER BY episode_id",
            "SELECT episode_id || ':' || ordinal || ':' || ref_id AS value FROM memory_episode_evidence ORDER BY episode_id, ordinal",
            "SELECT episode_id || ':' || hex(code_path) || ':' || change_kind || ':' || COALESCE(blob_oid_at_end, '') AS value FROM memory_episode_path ORDER BY episode_id, code_path",
            "SELECT episode_id || ':' || title || ':' || body || ':' || paths_text AS value FROM memory_episode_search_doc ORDER BY episode_id",
            "SELECT repo_id || ':' || source_kind || ':' || fingerprint || ':' || rebuilt_at AS value FROM memory_projection_state ORDER BY repo_id, source_kind",
        ] {
            rows.push(sql.to_string());
            for row in db
                .query_all_raw(Statement::from_string(DbBackend::Sqlite, sql))
                .await
                .unwrap()
            {
                rows.push(row.try_get("", "value").unwrap());
            }
        }
        rows
    }

    async fn seed(db: &DatabaseConnection) {
        project_episodes(db, &[episode("saved")]).await.unwrap();
        db.execute_unprepared("INSERT INTO memory_episode_search_doc (episode_id, title, body, paths_text) SELECT episode_id, title, body, 'src/lib.rs' FROM memory_episode").await.unwrap();
        db.execute_unprepared("INSERT INTO memory_projection_state VALUES ('repo', 'commit', '{}', 'previous-generation', 1, 1, 0, 0, 0, 123)").await.unwrap();
    }

    #[tokio::test]
    async fn failed_batch_restores_replaced_episode_and_children() {
        let db = database().await;
        seed(&db).await;
        let before = snapshot(&db).await;
        let mut replacement = episode("saved");
        replacement.title = "replacement".to_string();
        replacement.evidence[0].ref_id = "new-source".to_string();
        replacement.paths[0].blob_oid_at_end = Some("new-blob".to_string());
        replacement.content_digest = content_digest(&replacement);
        let mut invalid = episode("invalid");
        invalid.paths[0].change_kind = "invalid-kind".to_string();
        let error = project_episodes(&db, &[replacement, invalid])
            .await
            .unwrap_err();
        assert!(
            error.to_string().contains("CHECK constraint failed"),
            "must reach the invalid path write, got {error}"
        );
        assert_eq!(
            snapshot(&db).await,
            before,
            "a rejected batch must leave the complete prior projection intact"
        );
    }

    #[tokio::test]
    async fn successful_batch_replaces_children_without_leaving_old_edges() {
        let db = database().await;
        seed(&db).await;
        let mut replacement = episode("saved");
        replacement.evidence.clear();
        replacement.paths.clear();
        replacement.title = "replacement".to_string();
        replacement.content_digest = content_digest(&replacement);
        project_episodes(&db, &[replacement]).await.unwrap();
        let current = super::super::reader::list_episodes(&db, "repo")
            .await
            .unwrap();
        assert_eq!(current.len(), 1);
        assert_eq!(current[0].title, "replacement");
        assert!(current[0].evidence.is_empty());
        assert!(current[0].paths.is_empty());
    }

    #[tokio::test]
    async fn failed_source_read_keeps_previous_generation() {
        let db = database().await;
        seed(&db).await;
        let before = snapshot(&db).await;
        // This fixture intentionally lacks change_revision: a source-read error
        // must not delete a previously successful projection.
        let error = rebuild(&db, "repo", 100).await.unwrap_err();
        assert!(
            matches!(&error, MemoryError::Change(_)),
            "must reach the source read, got {error}"
        );
        assert_eq!(snapshot(&db).await, before);
    }

    #[tokio::test]
    async fn failed_state_publication_rolls_back_projection_deletes() {
        let db = database().await;
        seed(&db).await;
        // A valid empty source window needs no Git object reads.
        db.execute_unprepared("CREATE TABLE change_identity (change_id TEXT, repo_id TEXT); CREATE TABLE change_revision (change_id TEXT, commit_oid TEXT, created_op_id TEXT, visibility TEXT, revision_ordinal INTEGER); CREATE TABLE ai_operation_link (operation_id TEXT PRIMARY KEY, change_id TEXT, session_id TEXT, run_id TEXT, tool_invocation_id TEXT, intent_id TEXT, repo_id TEXT, worktree_id TEXT, workspace_id TEXT, lease_generation INTEGER, config_provenance_digest TEXT, redaction_version TEXT)").await.unwrap();
        db.execute_unprepared("CREATE TRIGGER reject_state BEFORE INSERT ON memory_projection_state BEGIN SELECT RAISE(ABORT, 'injected state publication failure'); END").await.unwrap();
        let before = snapshot(&db).await;
        let error = rebuild(&db, "repo", 100).await.unwrap_err();
        assert!(
            error
                .to_string()
                .contains("injected state publication failure"),
            "must reach the publication failure, got {error}"
        );
        assert_eq!(snapshot(&db).await, before);
    }
}

#[cfg(test)]
mod tests {
    #[cfg(feature = "storage-redb")]
    #[tokio::test]
    #[serial_test::serial(cwd, env)]
    async fn redb_memory_automation_permission_and_projection_stores() {
        use chrono::Utc;
        use git_internal::{
            hash::{HashKind, get_hash_kind},
            internal::object::{
                commit::Commit,
                signature::{Signature, SignatureType},
            },
        };
        use serde_json::json;

        use super::{
            super::{
                derive_commit::derive_commit_episodes_with_store,
                episode::{Episode, EvidenceEdge, Outcome, PathRow, SourceKind, content_digest},
                reader::{
                    list_episodes_bounded_with_store, list_episodes_with_store,
                    projection_is_stale_with_store, read_episode_search_doc_with_store,
                    read_episode_with_store, read_status_with_store,
                    search_episodes_by_path_with_store, search_episodes_text_with_store,
                },
            },
            meta_with_store, project_episodes_with_store, rebuild_with_store,
        };
        use crate::{
            internal::{
                ai::{
                    automation::{
                        AutomationHistory, AutomationRunResult, AutomationRunStatus,
                        dispatch_repo_hook_lifecycle_event_to_history,
                        dispatch_repo_vcs_event_to_history,
                    },
                    hooks::LifecycleEventKind,
                    permission::{
                        ApprovalProvenance, ApprovedRulesetStore, PermissionAction,
                        resolve_approval_runtime_cache,
                    },
                    projection::{
                        TaskRunIndexRow,
                        rebuild::{
                            insert_task_run_row_with_store, rebuild_task_run_index_with_store,
                            snapshot_task_run_base_commit_refs_with_store,
                        },
                        resolver::{
                            list_task_runs_with_store, resolve_latest_task_run_with_store,
                            resolve_task_run_with_store,
                        },
                    },
                },
                config::ConfigKv,
            },
            storage::{
                AiOperationLinkRecord, ChangeIdentityRecord, ChangeRevisionRecord, OperationRecord,
                OperationScopeKind, OperationScopeProvenance,
                runtime::{
                    init_redb_repository_storage, open_git_state_store_read_only,
                    open_git_state_store_read_write, open_repo_config_store_read_write,
                },
            },
        };

        let tmp = tempfile::tempdir().expect("tempdir");
        let repo_dir = tmp.path().join("repo");
        let dot_libra = repo_dir.join(".libra");
        std::fs::create_dir_all(dot_libra.join("objects")).expect("create .libra/objects");

        let _gen = init_redb_repository_storage(&dot_libra)
            .await
            .expect("init redb repository storage");
        assert!(
            !dot_libra.join("libra.db").exists(),
            "fresh redb repo must not create libra.db"
        );

        let repo_id = "repo-rb23-redb-e2e";
        let config_store = open_repo_config_store_read_write(&dot_libra)
            .await
            .expect("open redb config store");
        ConfigKv::set_with_store(config_store.as_ref(), "libra.repoid", repo_id, false)
            .await
            .expect("seed libra.repoid in redb");

        let state_store = open_git_state_store_read_write(&dot_libra)
            .await
            .expect("open redb git state store rw");

        let _cwd_guard = crate::utils::test::ChangeDirGuard::new(&repo_dir);

        // =====================================================================
        // AC1: Memory derive, rebuild, reader, path index & search doc on Redb
        // =====================================================================
        let current_kind = get_hash_kind();
        let zero_oid = "0".repeat(current_kind.size() * 2);
        let tree_id = crate::internal::object_format::digest(current_kind, b"rb23-tree");
        let author = Signature::new(
            SignatureType::Author,
            "RB23 Tester".to_string(),
            "rb23@libra.test".to_string(),
        );
        let committer = Signature::new(
            SignatureType::Committer,
            "RB23 Tester".to_string(),
            "rb23@libra.test".to_string(),
        );
        let commit = Commit::new(
            author,
            committer,
            tree_id,
            vec![],
            "feat: redb memory derivation\n\nDetailed commit body for redb search doc.",
        );
        let objects_storage = crate::utils::client_storage::ClientStorage::init_local_existing(
            dot_libra.join("objects"),
        );
        crate::command::save_object_to_storage(&objects_storage, &commit, &commit.id)
            .expect("save commit object");
        let commit_oid = commit.id.to_string();

        let op_id = "op-rb23-commit-1";
        let change_id = "chg-rb23-0001";
        let op = OperationRecord {
            op_id: op_id.to_string(),
            repo_id: repo_id.to_string(),
            format_version: 2,
            kind: "command".to_string(),
            status: "success".to_string(),
            command_name: Some("commit".to_string()),
            description: Some("seed commit".to_string()),
            args_digest: None,
            actor: Some("rb23-agent".to_string()),
            worktree_id: None,
            scope_kind: OperationScopeKind::Main,
            scope_provenance: OperationScopeProvenance::Declared,
            pre_view_oid: zero_oid.clone(),
            post_view_oid: commit_oid.clone(),
            restores_op_id: None,
            reverts_op_id: None,
            predecessor_map_oid: None,
            causal_context_id: None,
            restorable: true,
            control_slot: None,
            claim_owner: None,
            start_ts: 1_700_000_010,
            end_ts: Some(1_700_000_020),
        };
        state_store
            .append_operation_atomic(&op, &[], None)
            .await
            .expect("append commit operation");
        state_store
            .upsert_change_identity(&ChangeIdentityRecord {
                change_id: change_id.to_string(),
                repo_id: repo_id.to_string(),
                origin: "commit".to_string(),
                created_op_id: op_id.to_string(),
                created_at: 1_700_000_010,
            })
            .await
            .expect("upsert change_identity");
        state_store
            .upsert_change_revision(&ChangeRevisionRecord {
                change_id: change_id.to_string(),
                commit_oid: commit_oid.clone(),
                created_op_id: op_id.to_string(),
                visibility: "visible".to_string(),
                revision_ordinal: 0,
            })
            .await
            .expect("upsert change_revision");
        state_store
            .upsert_ai_operation_link(&AiOperationLinkRecord {
                operation_id: op_id.to_string(),
                session_id: Some("sess-rb23".to_string()),
                run_id: None,
                tool_invocation_id: None,
                intent_id: None,
                repo_id: repo_id.to_string(),
                worktree_id: None,
                workspace_id: None,
                lease_generation: None,
                config_provenance_digest: None,
                redaction_version: "v1".to_string(),
                change_id: Some(change_id.to_string()),
            })
            .await
            .expect("upsert ai_operation_link");

        // Derive & rebuild on Redb.
        let derived = derive_commit_episodes_with_store(state_store.as_ref(), repo_id, 50)
            .await
            .expect("derive commit episodes on redb");
        assert_eq!(derived.len(), 1);
        assert_eq!(derived[0].title, "feat: redb memory derivation");
        assert_eq!(derived[0].body, "Detailed commit body for redb search doc.");
        assert_eq!(derived[0].outcome, Outcome::Succeeded);
        assert_eq!(derived[0].actor.as_deref(), Some("rb23-agent"));
        assert_eq!(
            derived[0].anchor_commit.as_deref(),
            Some(commit_oid.as_str())
        );

        let report = rebuild_with_store(state_store.as_ref(), repo_id, 50)
            .await
            .expect("rebuild memory projection on redb");
        assert_eq!(report.projected, 1);
        assert!(!report.horizon_truncated);

        let ro_state = open_git_state_store_read_only(&dot_libra)
            .await
            .expect("open redb git state store ro");
        let meta_row = meta_with_store(ro_state.as_ref(), repo_id)
            .await
            .expect("read memory meta")
            .expect("meta present");
        assert!(!meta_row.0.is_empty());

        let status = read_status_with_store(ro_state.as_ref(), repo_id)
            .await
            .expect("read memory status")
            .expect("status present");
        assert!(status.is_fresh, "freshly rebuilt projection must be fresh");
        assert_eq!(
            projection_is_stale_with_store(ro_state.as_ref(), repo_id)
                .await
                .expect("stale check"),
            Some(false)
        );

        let listed = list_episodes_with_store(ro_state.as_ref(), repo_id)
            .await
            .expect("list episodes");
        assert_eq!(listed.len(), 1);
        let derived_id = listed[0].episode_id.clone();
        let search_doc = read_episode_search_doc_with_store(ro_state.as_ref(), &derived_id)
            .await
            .expect("read search doc")
            .expect("search doc populated");
        assert_eq!(search_doc.title, "feat: redb memory derivation");

        // Project two episodes sharing a code path to test MEMORY_BY_PATH index + search doc.
        let mut ep_old = Episode {
            episode_id: Episode::compute_episode_id(repo_id, SourceKind::AgentRun, "run-older"),
            repo_id: repo_id.to_string(),
            source_kind: SourceKind::AgentRun,
            source_key: "run-older".to_string(),
            outcome: Outcome::Succeeded,
            actor: Some("agent-a".to_string()),
            started_at: 100,
            ended_at: 200,
            anchor_commit: Some(commit_oid.clone()),
            change_id: None,
            title: "older path touch".to_string(),
            body: "touched storage layer".to_string(),
            content_digest: String::new(),
            producer: "derived-v1",
            rules_version: 1,
            evidence: vec![EvidenceEdge {
                kind: "operation".to_string(),
                ref_id: op_id.to_string(),
                link_confidence: "operation".to_string(),
                resolution_status: "resolved".to_string(),
            }],
            paths: vec![PathRow {
                code_path: b"src/storage/mod.rs".to_vec(),
                change_kind: "modified".to_string(),
                blob_oid_at_end: Some(commit_oid.clone()),
                ended_at: 200,
            }],
        };
        ep_old.content_digest = content_digest(&ep_old);

        let mut ep_new = Episode {
            episode_id: Episode::compute_episode_id(repo_id, SourceKind::AgentRun, "run-newer"),
            repo_id: repo_id.to_string(),
            source_kind: SourceKind::AgentRun,
            source_key: "run-newer".to_string(),
            outcome: Outcome::Partial,
            actor: Some("agent-b".to_string()),
            started_at: 300,
            ended_at: 500,
            anchor_commit: Some(commit_oid.clone()),
            change_id: None,
            title: "newer path touch".to_string(),
            body: "updated redb projection index".to_string(),
            content_digest: String::new(),
            producer: "derived-v1",
            rules_version: 1,
            evidence: vec![EvidenceEdge {
                kind: "commit".to_string(),
                ref_id: commit_oid.clone(),
                link_confidence: "identity".to_string(),
                resolution_status: "resolved".to_string(),
            }],
            paths: vec![PathRow {
                code_path: b"src/storage/mod.rs".to_vec(),
                change_kind: "modified".to_string(),
                blob_oid_at_end: Some(commit_oid.clone()),
                ended_at: 500,
            }],
        };
        ep_new.content_digest = content_digest(&ep_new);

        project_episodes_with_store(state_store.as_ref(), &[ep_old.clone(), ep_new.clone()])
            .await
            .expect("project episodes with paths");

        let ro_state = open_git_state_store_read_only(&dot_libra)
            .await
            .expect("reopen redb git state store ro after path projection");
        let by_path = search_episodes_by_path_with_store(
            ro_state.as_ref(),
            repo_id,
            b"src/storage/mod.rs",
            None,
            None,
            None,
        )
        .await
        .expect("path search via MEMORY_BY_PATH");
        assert_eq!(by_path.len(), 2);
        assert_eq!(
            by_path[0].episode_id, ep_new.episode_id,
            "path search must order by ended_at DESC"
        );
        assert_eq!(by_path[1].episode_id, ep_old.episode_id);

        let bounded_path = search_episodes_by_path_with_store(
            ro_state.as_ref(),
            repo_id,
            b"src/storage/mod.rs",
            Some(250),
            Some(600),
            Some(1),
        )
        .await
        .expect("bounded path search");
        assert_eq!(bounded_path.len(), 1);
        assert_eq!(bounded_path[0].episode_id, ep_new.episode_id);

        let text_hits =
            search_episodes_text_with_store(ro_state.as_ref(), repo_id, "redb projection", Some(5))
                .await
                .expect("text search");
        assert_eq!(text_hits.len(), 1);
        assert_eq!(text_hits[0].episode_id, ep_new.episode_id);

        let bounded_list = list_episodes_bounded_with_store(ro_state.as_ref(), repo_id, 2)
            .await
            .expect("bounded list");
        assert_eq!(bounded_list.len(), 2);

        let single = read_episode_with_store(ro_state.as_ref(), repo_id, &ep_new.episode_id)
            .await
            .expect("read single episode")
            .expect("episode exists");
        assert_eq!(single.paths.len(), 1);
        assert_eq!(single.evidence.len(), 1);

        // Staleness detection: mutating linked op end_ts must flip freshness to stale.
        assert!(
            state_store
                .update_operation_status(
                    repo_id,
                    op_id,
                    "partial",
                    Some(&commit_oid),
                    Some(1_700_000_099),
                )
                .await
                .expect("mutate op status")
        );
        let ro_state = open_git_state_store_read_only(&dot_libra)
            .await
            .expect("reopen redb git state store ro after op mutation");
        assert_eq!(
            projection_is_stale_with_store(ro_state.as_ref(), repo_id)
                .await
                .expect("stale check after mutation"),
            Some(true)
        );

        // =====================================================================
        // AC2: Automation runtime + history log on Redb without libra.db
        // =====================================================================
        std::fs::write(
            dot_libra.join("automations.toml"),
            br#"
[[rules]]
id = "rb23_post_commit"
trigger = { kind = "vcs", event = "post_commit" }
action = { kind = "prompt", prompt = "summarize commit" }

[[rules]]
id = "rb23_session_start"
trigger = { kind = "hook", event = "session_start" }
action = { kind = "prompt", prompt = "greet session" }
"#,
        )
        .expect("write automations.toml");

        let vcs_runs = dispatch_repo_vcs_event_to_history(&repo_dir, &dot_libra, "post_commit")
            .await
            .expect("dispatch repo vcs event on redb");
        assert_eq!(vcs_runs.len(), 1);
        assert_eq!(vcs_runs[0].rule_id, "rb23_post_commit");

        let hook_runs = dispatch_repo_hook_lifecycle_event_to_history(
            &repo_dir,
            &dot_libra,
            LifecycleEventKind::SessionStart,
        )
        .await
        .expect("dispatch repo hook event on redb");
        assert_eq!(hook_runs.len(), 1);
        assert_eq!(hook_runs[0].rule_id, "rb23_session_start");

        let manual = AutomationRunResult::new(
            "rb23_manual",
            "manual",
            "prompt",
            AutomationRunStatus::Succeeded,
            "manual redb history append",
            json!({"source": "rb23"}),
            Utc::now(),
        );
        AutomationHistory::append_with_store(state_store.as_ref(), &manual)
            .await
            .expect("append manual automation log");

        let ro_state = open_git_state_store_read_only(&dot_libra)
            .await
            .expect("reopen redb git state store ro after automation logs");
        let recent = AutomationHistory::list_recent_with_store(ro_state.as_ref(), 10)
            .await
            .expect("list recent automation logs");
        assert_eq!(recent.len(), 3);
        assert!(recent.iter().any(|r| r.rule_id == "rb23_post_commit"));
        assert!(recent.iter().any(|r| r.rule_id == "rb23_session_start"));
        assert!(recent.iter().any(|r| r.rule_id == "rb23_manual"));

        // =====================================================================
        // AC3: Permission approved store + runtime_cache on Redb
        // =====================================================================
        let prov = ApprovalProvenance {
            source_worktree_id: "wt-main".to_string(),
            source_session_id: "sess-rb23".to_string(),
            source_workspace_id: "ws-rb23".to_string(),
        };
        assert_eq!(
            ApprovedRulesetStore::append_with_store(
                config_store.as_ref(),
                state_store.as_ref(),
                "edit",
                "src/**",
                &prov,
            )
            .await
            .expect("append fresh approval"),
            1
        );
        assert_eq!(
            ApprovedRulesetStore::append_with_store(
                config_store.as_ref(),
                state_store.as_ref(),
                "edit",
                "src/**",
                &prov,
            )
            .await
            .expect("duplicate approval is idempotent"),
            0
        );
        assert_eq!(
            ApprovedRulesetStore::append_for_project_id_with_store(
                state_store.as_ref(),
                "legacy-rb23",
                "shell",
                "git status",
                &ApprovalProvenance::empty(),
            )
            .await
            .expect("append legacy approval"),
            1
        );
        assert_eq!(
            ApprovedRulesetStore::adopt_legacy_project_id_with_store(
                config_store.as_ref(),
                state_store.as_ref(),
                "legacy-rb23",
            )
            .await
            .expect("adopt legacy approval"),
            1
        );

        let ro_state = open_git_state_store_read_only(&dot_libra)
            .await
            .expect("reopen redb git state store ro after approvals");
        let with_prov = ApprovedRulesetStore::list_with_provenance_with_store(
            config_store.as_ref(),
            ro_state.as_ref(),
        )
        .await
        .expect("list approved with provenance");
        assert_eq!(with_prov.len(), 2);
        assert!(
            with_prov
                .iter()
                .any(|r| r.permission == "edit" && r.provenance.source_session_id == "sess-rb23")
        );

        let cache = resolve_approval_runtime_cache(&repo_dir)
            .await
            .expect("resolve approval runtime cache on redb");
        assert_eq!(cache.repo_id, repo_id);
        assert_eq!(cache.scope_key, format!("repo:{repo_id}"));
        assert_eq!(cache.approved_ruleset.rules.len(), 2);
        assert!(
            cache
                .approved_ruleset
                .rules
                .iter()
                .all(|r| r.action == PermissionAction::Allow)
        );

        assert_eq!(
            ApprovedRulesetStore::remove_with_store(
                config_store.as_ref(),
                state_store.as_ref(),
                "shell",
                "git status",
            )
            .await
            .expect("remove approval"),
            1
        );
        assert_eq!(
            ApprovedRulesetStore::clear_with_store(config_store.as_ref(), state_store.as_ref())
                .await
                .expect("clear approvals"),
            1
        );
        let ro_state = open_git_state_store_read_only(&dot_libra)
            .await
            .expect("reopen redb git state store ro after clear");
        assert!(
            ApprovedRulesetStore::load_with_store(config_store.as_ref(), ro_state.as_ref())
                .await
                .expect("load empty after clear")
                .is_empty()
        );

        // =====================================================================
        // AC4: AI projection rebuild + resolver on Redb (GC-08 tagged backfill)
        // =====================================================================
        let tagged_blake3 =
            "repo-commit:blake3:0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef";
        insert_task_run_row_with_store(
            state_store.as_ref(),
            &TaskRunIndexRow::new(
                "task-rb23",
                "run-rb23-1",
                true,
                100,
                Some(tagged_blake3.to_string()),
            ),
        )
        .await
        .expect("insert tagged task run row");

        let ro_state = open_git_state_store_read_only(&dot_libra)
            .await
            .expect("reopen redb git state store ro after task run insert");
        let snap = snapshot_task_run_base_commit_refs_with_store(
            ro_state.as_ref(),
            &["task-rb23".to_string()],
        )
        .await
        .expect("snapshot base_commit_ref");
        assert_eq!(snap.len(), 1);
        assert_eq!(
            snap.get("task-rb23", "run-rb23-1")
                .cloned()
                .flatten()
                .as_deref(),
            Some(tagged_blake3)
        );

        rebuild_task_run_index_with_store(
            state_store.as_ref(),
            &[
                TaskRunIndexRow::new("task-rb23", "run-rb23-1", false, 100, None),
                TaskRunIndexRow::new("task-rb23", "run-rb23-2", true, 200, None),
            ],
        )
        .await
        .expect("rebuild task run index on redb");

        let ro_state = open_git_state_store_read_only(&dot_libra)
            .await
            .expect("reopen redb git state store ro after task run rebuild");
        let resolved_1 = resolve_task_run_with_store(ro_state.as_ref(), "task-rb23", "run-rb23-1")
            .await
            .expect("resolve run-1")
            .expect("run-1 present");
        assert_eq!(
            resolved_1.base_commit_ref.as_deref(),
            Some(tagged_blake3),
            "rebuild must preserve tagged base_commit_ref via snapshot backfill"
        );
        assert!(
            resolved_1
                .typed_base_commit(HashKind::Blake3)
                .expect("typed blake3 parse")
                .is_some()
        );

        let latest = resolve_latest_task_run_with_store(ro_state.as_ref(), "task-rb23")
            .await
            .expect("resolve latest")
            .expect("latest present");
        assert_eq!(latest.run_id, "run-rb23-2");
        assert!(latest.is_latest);

        let all_runs = list_task_runs_with_store(ro_state.as_ref(), "task-rb23")
            .await
            .expect("list task runs");
        assert_eq!(all_runs.len(), 2);

        // Final strict pure-Redb invariant: `.libra/libra.db` must NEVER be created.
        assert!(
            !dot_libra.join("libra.db").exists(),
            "pure-Redb memory/automation/permission/projection workflow must never create .libra/libra.db"
        );
    }
}
