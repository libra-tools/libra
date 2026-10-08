//! Persist projected `Episode` rows into the zero-authority projection tables.
//!
//! All writes go through the `memory_episode` / `memory_episode_evidence` /
//! `memory_episode_path` tables created by DM-01 / DM-10. Each insert is an
//! idempotent upsert, so `rebuild` and incremental convergence never duplicate
//! rows (GC-DM-01).

use sea_orm::{ConnectionTrait, DatabaseConnection, Statement};

use super::{episode::Episode, error::MemoryError};

pub async fn project_episodes(
    conn: &DatabaseConnection,
    episodes: &[Episode],
) -> Result<(), MemoryError> {
    for episode in episodes {
        project_episode(conn, episode).await?;
    }
    Ok(())
}

async fn project_episode(conn: &DatabaseConnection, episode: &Episode) -> Result<(), MemoryError> {
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
#[derive(Clone, Debug, Eq, PartialEq)]
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
pub async fn rebuild(
    conn: &DatabaseConnection,
    repo_id: &str,
    horizon: usize,
) -> Result<RebuildReport, MemoryError> {
    use sea_orm::{Statement, Value};

    // Clear the repo's project rows so rebuild converges on the horizon window.
    for (table, has_episode_id) in [
        ("memory_episode_path", true),
        ("memory_episode_evidence", true),
    ] {
        if has_episode_id {
            conn.execute_raw(Statement::from_sql_and_values(
                conn.get_database_backend(),
                format!(
                    "DELETE FROM `{table}` WHERE `{table}`.episode_id IN \
                     (SELECT episode_id FROM `memory_episode` WHERE repo_id = ?)"
                ),
                [repo_id.to_string().into()],
            ))
            .await?;
        }
    }
    conn.execute_raw(Statement::from_sql_and_values(
        conn.get_database_backend(),
        "DELETE FROM `memory_episode` WHERE `repo_id` = ?",
        [repo_id.to_string().into()],
    ))
    .await?;

    let episodes = derive_commit_episodes(conn, repo_id, horizon).await?;
    let projected = episodes.len();
    for episode in &episodes {
        project_episode(conn, episode).await?;
    }

    let operations = commit_operations(conn, repo_id).await?;
    let fingerprint = commit_fingerprint(&operations);
    let horizon_truncated = projected >= horizon;
    let report = RebuildReport {
        repo_id: repo_id.to_string(),
        projected,
        horizon_truncated,
        revoked_count: 0,
        aged_out_count: 0,
    };

    conn.execute_raw(Statement::from_sql_and_values(
        conn.get_database_backend(),
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

    let _: Value = Value::Int(Some(0));
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
