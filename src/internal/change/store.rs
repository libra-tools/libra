//! SQLite projection for stable changes and their visible revisions.

use std::fmt;

use chrono::Utc;
use sea_orm::{ConnectionTrait, DatabaseConnection, DbBackend, DbErr, Statement};
use thiserror::Error;

use super::ChangeId;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum RevisionVisibility {
    Visible,
    Hidden,
}

impl fmt::Display for RevisionVisibility {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::Visible => "visible",
            Self::Hidden => "hidden",
        })
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ChangeRevision {
    pub change_id: ChangeId,
    pub commit_oid: String,
    pub created_op_id: String,
    pub visibility: RevisionVisibility,
    pub revision_ordinal: i64,
}

#[derive(Debug, Error)]
pub enum ChangeStoreError {
    #[error("database error: {0}")]
    Database(#[from] DbErr),
    #[error("change id collision: {0}")]
    Collision(String),
    #[error("invalid projected change id: {0}")]
    InvalidChangeId(String),
}

#[derive(Clone)]
pub struct ChangeStore {
    db: DatabaseConnection,
}

impl ChangeStore {
    pub fn new(db: DatabaseConnection) -> Self {
        Self { db }
    }

    pub async fn insert_identity(
        &self,
        repo_id: &str,
        change_id: ChangeId,
        origin: &str,
        created_op_id: &str,
    ) -> Result<(), ChangeStoreError> {
        let result = self
            .db
            .execute_raw(Statement::from_sql_and_values(
                DbBackend::Sqlite,
                "INSERT INTO change_identity \
                 (change_id, repo_id, origin, created_op_id, created_at) VALUES (?, ?, ?, ?, ?)",
                [
                    change_id.to_string().into(),
                    repo_id.to_string().into(),
                    origin.to_string().into(),
                    created_op_id.to_string().into(),
                    Utc::now().timestamp_millis().into(),
                ],
            ))
            .await;
        result.map(|_| ()).map_err(|error| {
            if error
                .to_string()
                .to_ascii_lowercase()
                .contains("constraint")
            {
                ChangeStoreError::Collision(change_id.to_string())
            } else {
                ChangeStoreError::Database(error)
            }
        })
    }

    pub async fn ensure_identity(
        &self,
        repo_id: &str,
        change_id: ChangeId,
        origin: &str,
        created_op_id: &str,
    ) -> Result<(), ChangeStoreError> {
        match self
            .insert_identity(repo_id, change_id, origin, created_op_id)
            .await
        {
            Ok(()) => Ok(()),
            Err(ChangeStoreError::Collision(_)) => {
                let row = self
                    .db
                    .query_one_raw(Statement::from_sql_and_values(
                        DbBackend::Sqlite,
                        "SELECT repo_id FROM change_identity WHERE change_id = ?",
                        [change_id.to_string().into()],
                    ))
                    .await?;
                if row
                    .and_then(|row| row.try_get::<String>("", "repo_id").ok())
                    .as_deref()
                    == Some(repo_id)
                {
                    Ok(())
                } else {
                    Err(ChangeStoreError::Collision(change_id.to_string()))
                }
            }
            Err(error) => Err(error),
        }
    }

    pub async fn insert_revision(&self, revision: &ChangeRevision) -> Result<(), ChangeStoreError> {
        self.db
            .execute_raw(Statement::from_sql_and_values(
                DbBackend::Sqlite,
                "INSERT INTO change_revision \
                 (change_id, commit_oid, created_op_id, visibility, revision_ordinal) \
                 VALUES (?, ?, ?, ?, ?)",
                [
                    revision.change_id.to_string().into(),
                    revision.commit_oid.clone().into(),
                    revision.created_op_id.clone().into(),
                    revision.visibility.to_string().into(),
                    revision.revision_ordinal.into(),
                ],
            ))
            .await
            .map(|_| ())
            .map_err(ChangeStoreError::Database)
    }

    pub async fn revisions_for_change(
        &self,
        repo_id: &str,
        change_id: ChangeId,
    ) -> Result<Vec<ChangeRevision>, ChangeStoreError> {
        let rows = self
            .db
            .query_all_raw(Statement::from_sql_and_values(
                DbBackend::Sqlite,
                "SELECT r.change_id, r.commit_oid, r.created_op_id, r.visibility, \
                 r.revision_ordinal FROM change_revision r \
                 JOIN change_identity i ON i.change_id = r.change_id \
                 WHERE i.repo_id = ? AND r.change_id = ? ORDER BY r.revision_ordinal",
                [repo_id.to_string().into(), change_id.to_string().into()],
            ))
            .await?;
        rows.into_iter()
            .map(|row| {
                let id = row.try_get::<String>("", "change_id")?;
                Ok(ChangeRevision {
                    change_id: id
                        .parse()
                        .map_err(|_| ChangeStoreError::InvalidChangeId(id.clone()))?,
                    commit_oid: row.try_get("", "commit_oid")?,
                    created_op_id: row.try_get("", "created_op_id")?,
                    visibility: match row.try_get::<String>("", "visibility")?.as_str() {
                        "visible" => RevisionVisibility::Visible,
                        "hidden" => RevisionVisibility::Hidden,
                        value => return Err(ChangeStoreError::InvalidChangeId(value.to_string())),
                    },
                    revision_ordinal: row.try_get("", "revision_ordinal")?,
                })
            })
            .collect()
    }
}
