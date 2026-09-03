//! SeaORM entity for the rebuildable Episode external-content search source.

use sea_orm::entity::prelude::*;

#[derive(Clone, Debug, Eq, PartialEq, DeriveEntityModel)]
#[sea_orm(table_name = "memory_episode_search_doc")]
pub struct Model {
    #[sea_orm(primary_key, auto_increment = true, column_name = "rowid")]
    pub rowid: i64,
    pub note_id: String,
    pub revision_oid: String,
    pub root_kind: String,
    pub root_id: String,
    pub completion_status: String,
    pub code_change_status: String,
    pub ended_at: Option<String>,
    pub goal: String,
    pub summary: String,
    pub decisions: String,
    pub failed_attempts: String,
    pub unresolved: String,
}

#[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
pub enum Relation {}

impl ActiveModelBehavior for ActiveModel {}
