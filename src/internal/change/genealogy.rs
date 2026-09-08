//! Typed predecessor edges and bounded genealogy queries.

use sea_orm::{ConnectionTrait, DatabaseConnection, DbBackend, QueryResult, Statement};
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

/// Redacted causal metadata connecting an AI operation to a stable logical
/// change. This intentionally contains no commit OID, prompt, transcript, or
/// secret; commit revisions are queried through `ChangeId` projections.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct AiOperationLink {
    pub operation_id: String,
    pub change_id: ChangeId,
    pub session_id: Option<String>,
    pub run_id: Option<String>,
    pub tool_invocation_id: Option<String>,
    pub intent_id: Option<String>,
    pub repo_id: String,
    pub worktree_id: Option<String>,
    pub workspace_id: Option<String>,
    pub lease_generation: Option<i64>,
    pub config_provenance_digest: Option<String>,
    pub redaction_version: String,
}

#[derive(Debug, Error)]
pub enum GenealogyError {
    #[error("database error: {0}")]
    Database(#[from] sea_orm::DbErr),
    #[error("invalid change id: {0}")]
    InvalidChangeId(String),
    #[error("invalid relation kind: {0}")]
    InvalidRelation(String),
    #[error("invalid AI operation link: {0}")]
    InvalidAiLink(String),
}

pub async fn insert_predecessor(
    db: &DatabaseConnection,
    edge: &PredecessorEdge,
) -> Result<(), GenealogyError> {
    insert_predecessor_on(db, edge).await
}

/// Atomically upsert redacted AI causal metadata by stable operation
/// identity. The change association is a Change ID, never a commit OID.
pub async fn link_ai_operation(
    db: &DatabaseConnection,
    link: &AiOperationLink,
) -> Result<(), GenealogyError> {
    validate_ai_link(link)?;
    let change_exists = db
        .query_one_raw(Statement::from_sql_and_values(
            DbBackend::Sqlite,
            "SELECT 1 FROM change_identity WHERE repo_id = ? AND change_id = ? LIMIT 1",
            [
                link.repo_id.clone().into(),
                link.change_id.to_string().into(),
            ],
        ))
        .await?
        .is_some();
    if !change_exists {
        return Err(GenealogyError::InvalidAiLink(format!(
            "change {} is not registered in repository {}",
            link.change_id, link.repo_id
        )));
    }
    db.execute_raw(Statement::from_sql_and_values(
        DbBackend::Sqlite,
        "INSERT INTO ai_operation_link \
         (operation_id, change_id, session_id, run_id, tool_invocation_id, intent_id, \
          repo_id, worktree_id, workspace_id, lease_generation, config_provenance_digest, \
          redaction_version) VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?) \
         ON CONFLICT(operation_id) DO UPDATE SET \
          change_id = excluded.change_id, session_id = excluded.session_id, \
          run_id = excluded.run_id, tool_invocation_id = excluded.tool_invocation_id, \
          intent_id = excluded.intent_id, repo_id = excluded.repo_id, \
          worktree_id = excluded.worktree_id, workspace_id = excluded.workspace_id, \
          lease_generation = excluded.lease_generation, \
          config_provenance_digest = excluded.config_provenance_digest, \
          redaction_version = excluded.redaction_version",
        [
            link.operation_id.clone().into(),
            link.change_id.to_string().into(),
            link.session_id.clone().into(),
            link.run_id.clone().into(),
            link.tool_invocation_id.clone().into(),
            link.intent_id.clone().into(),
            link.repo_id.clone().into(),
            link.worktree_id.clone().into(),
            link.workspace_id.clone().into(),
            link.lease_generation.into(),
            link.config_provenance_digest.clone().into(),
            link.redaction_version.clone().into(),
        ],
    ))
    .await
    .map(|_| ())
    .map_err(GenealogyError::Database)
}

/// Return all AI links for a stable Change ID in one repository.
pub async fn ai_links_for_change(
    db: &DatabaseConnection,
    repo_id: &str,
    change_id: ChangeId,
) -> Result<Vec<AiOperationLink>, GenealogyError> {
    let rows = db
        .query_all_raw(Statement::from_sql_and_values(
            DbBackend::Sqlite,
            "SELECT operation_id, change_id, session_id, run_id, tool_invocation_id, intent_id, \
             repo_id, worktree_id, workspace_id, lease_generation, config_provenance_digest, \
             redaction_version FROM ai_operation_link \
             WHERE repo_id = ? AND change_id = ? ORDER BY operation_id",
            [repo_id.to_string().into(), change_id.to_string().into()],
        ))
        .await?;
    rows.into_iter().map(ai_link_from_row).collect()
}

/// Return all AI links for a redacted intent ID in one repository.
pub async fn ai_links_for_intent(
    db: &DatabaseConnection,
    repo_id: &str,
    intent_id: &str,
) -> Result<Vec<AiOperationLink>, GenealogyError> {
    if intent_id.trim().is_empty() {
        return Err(GenealogyError::InvalidAiLink(
            "intent id must not be empty".to_string(),
        ));
    }
    let rows = db
        .query_all_raw(Statement::from_sql_and_values(
            DbBackend::Sqlite,
            "SELECT operation_id, change_id, session_id, run_id, tool_invocation_id, intent_id, \
             repo_id, worktree_id, workspace_id, lease_generation, config_provenance_digest, \
             redaction_version FROM ai_operation_link \
             WHERE repo_id = ? AND intent_id = ? AND change_id IS NOT NULL ORDER BY operation_id",
            [repo_id.to_string().into(), intent_id.to_string().into()],
        ))
        .await?;
    rows.into_iter().map(ai_link_from_row).collect()
}

fn validate_ai_link(link: &AiOperationLink) -> Result<(), GenealogyError> {
    for (name, value) in [
        ("operation id", link.operation_id.as_str()),
        ("repository id", link.repo_id.as_str()),
        ("redaction version", link.redaction_version.as_str()),
    ] {
        validate_redacted_value(name, value, true)?;
    }
    for (name, value) in [
        ("session id", link.session_id.as_deref()),
        ("run id", link.run_id.as_deref()),
        ("tool invocation id", link.tool_invocation_id.as_deref()),
        ("intent id", link.intent_id.as_deref()),
        ("worktree id", link.worktree_id.as_deref()),
        ("workspace id", link.workspace_id.as_deref()),
        (
            "config provenance digest",
            link.config_provenance_digest.as_deref(),
        ),
    ] {
        if let Some(value) = value {
            validate_redacted_value(name, value, false)?;
        }
    }
    Ok(())
}

fn validate_redacted_value(name: &str, value: &str, required: bool) -> Result<(), GenealogyError> {
    if required && value.trim().is_empty() {
        return Err(GenealogyError::InvalidAiLink(format!(
            "{name} must not be empty"
        )));
    }
    if value.len() > 256 || value.chars().any(char::is_control) {
        return Err(GenealogyError::InvalidAiLink(format!(
            "{name} must be a bounded, control-free redacted identifier"
        )));
    }
    Ok(())
}

fn ai_link_from_row(row: QueryResult) -> Result<AiOperationLink, GenealogyError> {
    let raw_change_id = row.try_get::<String>("", "change_id")?;
    let change_id = raw_change_id
        .parse()
        .map_err(|_| GenealogyError::InvalidChangeId(raw_change_id.clone()))?;
    Ok(AiOperationLink {
        operation_id: row.try_get("", "operation_id")?,
        change_id,
        session_id: row.try_get::<Option<String>>("", "session_id")?,
        run_id: row.try_get::<Option<String>>("", "run_id")?,
        tool_invocation_id: row.try_get::<Option<String>>("", "tool_invocation_id")?,
        intent_id: row.try_get::<Option<String>>("", "intent_id")?,
        repo_id: row.try_get("", "repo_id")?,
        worktree_id: row.try_get::<Option<String>>("", "worktree_id")?,
        workspace_id: row.try_get::<Option<String>>("", "workspace_id")?,
        lease_generation: row.try_get::<Option<i64>>("", "lease_generation")?,
        config_provenance_digest: row.try_get::<Option<String>>("", "config_provenance_digest")?,
        redaction_version: row.try_get("", "redaction_version")?,
    })
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
