//! SeaORM entity for the task -> run reverse index.

use sea_orm::entity::prelude::*;

#[derive(Clone, Debug, PartialEq, DeriveEntityModel, Eq)]
#[sea_orm(table_name = "ai_index_task_run")]
pub struct Model {
    #[sea_orm(primary_key, auto_increment = false)]
    pub task_id: String,
    #[sea_orm(primary_key, auto_increment = false)]
    pub run_id: String,
    pub is_latest: bool,
    pub created_at: i64,
    /// Tagged `repo-commit:<kind>:<hex>` when known; NULL keeps the legacy
    /// bare-anchor read path for B3-10 consumers.
    pub base_commit_ref: Option<String>,
}

#[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
pub enum Relation {}

impl ActiveModelBehavior for ActiveModel {}
