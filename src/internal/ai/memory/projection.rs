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
