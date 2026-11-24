//! SeaORM entity for Libra thread projections.

use sea_orm::entity::prelude::*;

#[derive(Clone, Debug, PartialEq, DeriveEntityModel, Eq)]
#[sea_orm(table_name = "ai_thread")]
pub struct Model {
    #[sea_orm(primary_key, auto_increment = false)]
    pub thread_id: String,
    pub title: Option<String>,
    pub owner_kind: String,
    pub owner_id: String,
    pub owner_display_name: Option<String>,
    pub current_intent_id: Option<String>,
    pub latest_intent_id: Option<String>,
    pub metadata_json: Option<String>,
    pub archived: bool,
    pub version: i64,
    pub created_at: i64,
    pub updated_at: i64,
}

#[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
pub enum Relation {}

impl ActiveModelBehavior for ActiveModel {}
