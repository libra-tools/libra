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
    let ops = operation::Entity::find()
        .filter(operation::Column::OpId.is_in(op_ids))
        .order_by_asc(operation::Column::OpId)
        .all(conn)
        .await?;
    Ok(ops
        .into_iter()
        .map(|op| (op.op_id, op.status, op.end_ts))
        .collect())
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
