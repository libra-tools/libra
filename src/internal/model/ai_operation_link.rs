//! SeaORM entity for redacted AI-to-operation causal links.

use sea_orm::entity::prelude::*;

#[derive(Clone, Debug, PartialEq, Eq, DeriveEntityModel)]
#[sea_orm(table_name = "ai_operation_link")]
pub struct Model {
    #[sea_orm(primary_key, auto_increment = false)]
    pub operation_id: String,
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

#[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
pub enum Relation {}

impl ActiveModelBehavior for ActiveModel {}
