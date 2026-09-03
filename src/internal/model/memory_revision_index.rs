//! SeaORM entity for rebuildable Memory revision provenance.

use sea_orm::entity::prelude::*;

#[derive(Clone, Debug, Eq, PartialEq, DeriveEntityModel)]
#[sea_orm(table_name = "memory_revision_index")]
pub struct Model {
    #[sea_orm(primary_key, auto_increment = false)]
    pub revision_oid: String,
    pub note_id: String,
    pub scope_key: String,
    pub namespace: String,
    pub origin: String,
    pub producer: String,
    pub rules_version: i64,
    pub prompt_version: Option<String>,
    pub model_id: Option<String>,
    pub policy_version: String,
    pub input_fingerprints_json: String,
    pub created_at: String,
}

#[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
pub enum Relation {}

impl ActiveModelBehavior for ActiveModel {}
