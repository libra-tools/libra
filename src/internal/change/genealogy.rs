//! Typed predecessor edges and bounded genealogy queries.

use sea_orm::{ConnectionTrait, DatabaseConnection, DbBackend, Statement};
use serde::{Deserialize, Serialize};
use thiserror::Error;

use super::ChangeId;

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RelationKind {
    Amend,
    Rebase,
    CherryPick,
    Squash,
    Split,
    Duplicate,
    Import,
    ExternalReconcile,
}

impl RelationKind {
    fn as_str(self) -> &'static str {
        match self {
            Self::Amend => "amend",
            Self::Rebase => "rebase",
            Self::CherryPick => "cherry_pick",
            Self::Squash => "squash",
            Self::Split => "split",
            Self::Duplicate => "duplicate",
            Self::Import => "import",
            Self::ExternalReconcile => "external_reconcile",
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct PredecessorEdge {
    pub successor_oid: String,
    pub predecessor_oid: String,
    pub op_id: String,
    pub relation_kind: RelationKind,
    pub ordinal: i32,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct GenealogyRevision {
    pub change_id: ChangeId,
    pub commit_oid: String,
    pub predecessors: Vec<PredecessorEdge>,
}

#[derive(Debug, Error)]
pub enum GenealogyError {
    #[error("database error: {0}")]
    Database(#[from] sea_orm::DbErr),
    #[error("invalid change id: {0}")]
    InvalidChangeId(String),
    #[error("invalid relation kind: {0}")]
    InvalidRelation(String),
}

pub async fn insert_predecessor(
    db: &DatabaseConnection,
    edge: &PredecessorEdge,
) -> Result<(), GenealogyError> {
    insert_predecessor_on(db, edge).await
}

pub(crate) async fn insert_predecessor_on<C: ConnectionTrait>(
    db: &C,
    edge: &PredecessorEdge,
) -> Result<(), GenealogyError> {
    db.execute_raw(Statement::from_sql_and_values(
        DbBackend::Sqlite,
        "INSERT INTO change_predecessor \
         (successor_oid, predecessor_oid, op_id, relation_kind, ordinal) VALUES (?, ?, ?, ?, ?)",
        [
            edge.successor_oid.clone().into(),
            edge.predecessor_oid.clone().into(),
            edge.op_id.clone().into(),
            edge.relation_kind.as_str().to_string().into(),
            edge.ordinal.into(),
        ],
    ))
    .await
    .map(|_| ())
    .map_err(GenealogyError::Database)
}

pub async fn evolution_for_commit(
    db: &DatabaseConnection,
    commit_oid: &str,
    limit: usize,
) -> Result<Vec<PredecessorEdge>, GenealogyError> {
    let rows = db
        .query_all_raw(Statement::from_sql_and_values(
            DbBackend::Sqlite,
            "SELECT successor_oid, predecessor_oid, op_id, relation_kind, ordinal \
             FROM change_predecessor WHERE successor_oid = ? ORDER BY ordinal LIMIT ?",
            [
                commit_oid.to_string().into(),
                (limit.clamp(1, 10_000) as i64).into(),
            ],
        ))
        .await?;
    rows.into_iter()
        .map(|row| {
            let relation = row.try_get::<String>("", "relation_kind")?;
            Ok(PredecessorEdge {
                successor_oid: row.try_get("", "successor_oid")?,
                predecessor_oid: row.try_get("", "predecessor_oid")?,
                op_id: row.try_get("", "op_id")?,
                relation_kind: relation_kind(&relation)?,
                ordinal: row.try_get("", "ordinal")?,
            })
        })
        .collect()
}

fn relation_kind(value: &str) -> Result<RelationKind, GenealogyError> {
    match value {
        "amend" => Ok(RelationKind::Amend),
        "rebase" => Ok(RelationKind::Rebase),
        "cherry_pick" => Ok(RelationKind::CherryPick),
        "squash" => Ok(RelationKind::Squash),
        "split" => Ok(RelationKind::Split),
        "duplicate" => Ok(RelationKind::Duplicate),
        "import" => Ok(RelationKind::Import),
        "external_reconcile" => Ok(RelationKind::ExternalReconcile),
        _ => Err(GenealogyError::InvalidRelation(value.to_string())),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn relation_kinds_are_stable_machine_values() {
        assert_eq!(RelationKind::CherryPick.as_str(), "cherry_pick");
        assert_eq!(
            RelationKind::ExternalReconcile.as_str(),
            "external_reconcile"
        );
    }
}
