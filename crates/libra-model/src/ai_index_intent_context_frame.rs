//! SeaORM entity for the intent -> context frame reverse index.

use sea_orm::entity::prelude::*;

#[derive(Clone, Debug, PartialEq, DeriveEntityModel, Eq)]
#[sea_orm(table_name = "ai_index_intent_context_frame")]
pub struct Model {
    #[sea_orm(primary_key, auto_increment = false)]
    pub intent_id: String,
    #[sea_orm(primary_key, auto_increment = false)]
    pub context_frame_id: String,
    #[sea_orm(primary_key, auto_increment = false)]
    pub relation_kind: String,
    pub created_at: i64,
}

#[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
pub enum Relation {}

impl ActiveModelBehavior for ActiveModel {}
