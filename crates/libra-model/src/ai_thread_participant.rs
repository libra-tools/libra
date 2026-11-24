//! SeaORM entity for thread participants.

use sea_orm::entity::prelude::*;

#[derive(Clone, Debug, PartialEq, DeriveEntityModel, Eq)]
#[sea_orm(table_name = "ai_thread_participant")]
pub struct Model {
    #[sea_orm(primary_key, auto_increment = false)]
    pub thread_id: String,
    #[sea_orm(primary_key, auto_increment = false)]
    pub actor_kind: String,
    #[sea_orm(primary_key, auto_increment = false)]
    pub actor_id: String,
    pub actor_display_name: Option<String>,
    pub role: String,
    pub joined_at: i64,
}

#[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
pub enum Relation {}

impl ActiveModelBehavior for ActiveModel {}
