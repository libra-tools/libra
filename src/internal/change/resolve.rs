//! Prefix resolution with explicit ambiguity instead of guessing.

use sea_orm::{ConnectionTrait, DatabaseConnection, DbBackend, Statement};
use serde::{Deserialize, Serialize};
use thiserror::Error;

use super::{ChangeId, ChangeStoreError};

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub enum ChangeIdResolution {
    Exact(ChangeId),
    Ambiguous(Vec<ChangeId>),
    NotFound,
}

#[derive(Debug, Error)]
pub enum ResolveError {
    #[error("change id prefix is invalid: {0}")]
    Invalid(#[from] super::ChangeIdError),
    #[error("change projection query failed: {0}")]
    Storage(#[from] ChangeStoreError),
}

/// Resolve a human prefix through the indexed projection.  The returned
/// canonical IDs are always full 128-bit values, including in JSON output.
pub async fn resolve_change_id_prefix(
    db: &DatabaseConnection,
    repo_id: &str,
    prefix: &str,
) -> Result<ChangeIdResolution, ResolveError> {
    let prefix = prefix.trim().to_ascii_lowercase();
    if prefix.is_empty() || prefix.len() > 32 || !prefix.chars().all(|c| c.is_ascii_hexdigit()) {
        return Err(ResolveError::Invalid(super::ChangeIdError::InvalidHex));
    }
    let rows = db
        .query_all_raw(Statement::from_sql_and_values(
            DbBackend::Sqlite,
            "SELECT DISTINCT change_id FROM change_identity \
             WHERE repo_id = ? AND change_id GLOB ? ORDER BY change_id",
            [repo_id.to_string().into(), format!("{prefix}*").into()],
        ))
        .await
        .map_err(|error| ResolveError::Storage(ChangeStoreError::Database(error)))?;
    let mut ids = rows
        .into_iter()
        .map(|row| {
            row.try_get::<String>("", "change_id")
                .map_err(|error| ResolveError::Storage(ChangeStoreError::Database(error)))
                .and_then(|id| id.parse().map_err(ResolveError::Invalid))
        })
        .collect::<Result<Vec<_>, _>>()?;
    ids.sort();
    Ok(match ids.len() {
        0 => ChangeIdResolution::NotFound,
        1 => ChangeIdResolution::Exact(ids.remove(0)),
        _ => ChangeIdResolution::Ambiguous(ids),
    })
}
