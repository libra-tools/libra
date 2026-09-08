//! SeaORM entity for rebuildable Memory path summaries.

use sea_orm::entity::prelude::*;

#[derive(Clone, Debug, Eq, PartialEq, DeriveEntityModel)]
#[sea_orm(table_name = "memory_path_summary")]
pub struct Model {
    #[sea_orm(primary_key, auto_increment = false)]
    pub scope_key: String,
    #[sea_orm(primary_key, auto_increment = false)]
    pub namespace: String,
    #[sea_orm(primary_key, auto_increment = false)]
    pub path: String,
    pub confirmed_count: i64,
    pub quarantined_count: i64,
    pub child_count: i64,
    pub prefix_count: i64,
    pub preview: String,
    pub last_changed_at: String,
}

#[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
pub enum Relation {}

impl ActiveModelBehavior for ActiveModel {}
