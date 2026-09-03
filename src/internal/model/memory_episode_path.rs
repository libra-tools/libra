//! SeaORM entity for rebuildable Episode revision-to-code-path links.

use sea_orm::entity::prelude::*;

#[derive(Clone, Debug, Eq, PartialEq, DeriveEntityModel)]
#[sea_orm(table_name = "memory_episode_path")]
pub struct Model {
    #[sea_orm(primary_key, auto_increment = false)]
    pub note_id: String,
    #[sea_orm(primary_key, auto_increment = false)]
    pub revision_oid: String,
    #[sea_orm(primary_key, auto_increment = false)]
    pub code_path: String,
}

#[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
pub enum Relation {}

impl ActiveModelBehavior for ActiveModel {}
