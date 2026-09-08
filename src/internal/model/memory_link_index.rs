//! SeaORM entity for rebuildable fixed-revision Memory links.

use sea_orm::entity::prelude::*;

#[derive(Clone, Debug, Eq, PartialEq, DeriveEntityModel)]
#[sea_orm(table_name = "memory_link_index")]
pub struct Model {
    pub source_scope_key: String,
    pub source_namespace: String,
    pub source_note_id: String,
    #[sea_orm(primary_key, auto_increment = false)]
    pub source_revision_oid: String,
    #[sea_orm(primary_key, auto_increment = false)]
    pub target_note_id: String,
    pub target_revision_oid: Option<String>,
    #[sea_orm(primary_key, auto_increment = false)]
    pub link_kind: String,
    pub source_path: String,
    pub target_path: String,
    pub evidence_refs_json: String,
    pub valid_from: Option<String>,
    pub valid_until: Option<String>,
}

#[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
pub enum Relation {}

impl ActiveModelBehavior for ActiveModel {}
