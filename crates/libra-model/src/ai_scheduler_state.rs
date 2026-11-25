//! SeaORM entity for per-thread scheduler state.

use sea_orm::entity::prelude::*;

#[derive(Clone, Debug, PartialEq, DeriveEntityModel, Eq)]
#[sea_orm(table_name = "ai_scheduler_state")]
pub struct Model {
    #[sea_orm(primary_key, auto_increment = false)]
    pub thread_id: String,
    pub selected_plan_id: Option<String>,
    pub active_task_id: Option<String>,
    pub active_run_id: Option<String>,
    pub metadata_json: Option<String>,
    pub version: i64,
    pub updated_at: i64,
}

#[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
pub enum Relation {}

impl ActiveModelBehavior for ActiveModel {}
