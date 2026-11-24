//! SeaORM entity for the runtime-owned formal final decision.
//!
//! The terminal artifact in the ValidationReport -> RiskScoreBreakdown ->
//! DecisionProposal -> Decision chain (Implementation Phase 4). A row records
//! the resolved final-decision verdict string (the Code-era
//! `runtime::contracts::FinalDecisionVerdict` type was deleted with RC-23)
//! for a thread once a DecisionProposal has been finalised. Shape mirrors
//! `ai_decision_proposal` so the same latest-pointer persistence pattern
//! applies.

use sea_orm::entity::prelude::*;

#[derive(Clone, Debug, PartialEq, DeriveEntityModel, Eq)]
#[sea_orm(table_name = "ai_final_decision")]
pub struct Model {
    #[sea_orm(primary_key, auto_increment = false)]
    pub decision_id: String,
    pub thread_id: String,
    pub decision_proposal_id: Option<String>,
    pub validation_report_id: Option<String>,
    pub policy_version: String,
    pub verdict: String,
    pub stale: i64,
    pub is_latest: i64,
    pub summary_json: String,
    pub created_at: i64,
    pub updated_at: i64,
}

#[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
pub enum Relation {}

impl ActiveModelBehavior for ActiveModel {}
