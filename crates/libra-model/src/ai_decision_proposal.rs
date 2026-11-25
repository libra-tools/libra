//! SeaORM entity for runtime-owned decision proposals.

use sea_orm::entity::prelude::*;

#[derive(Clone, Debug, PartialEq, DeriveEntityModel, Eq)]
#[sea_orm(table_name = "ai_decision_proposal")]
pub struct Model {
    #[sea_orm(primary_key, auto_increment = false)]
    pub proposal_id: String,
    pub thread_id: String,
    pub validation_report_id: Option<String>,
    pub risk_score_breakdown_id: Option<String>,
    pub policy_version: String,
    pub stale: i64,
    pub is_latest: i64,
    pub summary_json: String,
    pub created_at: i64,
    pub updated_at: i64,
}

#[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
pub enum Relation {}

impl ActiveModelBehavior for ActiveModel {}
