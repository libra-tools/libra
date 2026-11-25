//! SeaORM entity for provider-specific thread diagnostics metadata.

use sea_orm::entity::prelude::*;

#[derive(Clone, Debug, PartialEq, DeriveEntityModel, Eq)]
#[sea_orm(table_name = "ai_thread_provider_metadata")]
pub struct Model {
    #[sea_orm(primary_key, auto_increment = false)]
    pub thread_id: String,
    pub legacy_session_id: Option<String>,
    pub provider_thread_id: Option<String>,
    pub provider_kind: Option<String>,
    pub metadata_json: Option<String>,
    pub updated_at: i64,
}

#[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
pub enum Relation {}

impl ActiveModelBehavior for ActiveModel {}
