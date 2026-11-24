//! SeaORM entity for thread -> intent membership.

use sea_orm::entity::prelude::*;

#[derive(Clone, Debug, PartialEq, DeriveEntityModel, Eq)]
#[sea_orm(table_name = "ai_thread_intent")]
pub struct Model {
    #[sea_orm(primary_key, auto_increment = false)]
    pub thread_id: String,
    #[sea_orm(primary_key, auto_increment = false)]
    pub intent_id: String,
    pub ordinal: i64,
    pub is_head: bool,
    pub linked_at: i64,
    pub link_reason: String,
}

#[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
pub enum Relation {}

impl ActiveModelBehavior for ActiveModel {}
