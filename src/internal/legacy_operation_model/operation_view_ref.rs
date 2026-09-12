//! SeaORM entity for pre-OL-02 operation view reference snapshots.

use sea_orm::entity::prelude::*;

#[derive(Clone, Debug, PartialEq, Eq, DeriveEntityModel)]
#[sea_orm(table_name = "legacy_operation_view_ref")]
pub struct Model {
    #[sea_orm(primary_key, auto_increment = false)]
    pub view_id: String,
    #[sea_orm(primary_key, auto_increment = false)]
    pub ref_kind: String,
    #[sea_orm(primary_key, auto_increment = false)]
    pub ref_name: String,
    #[sea_orm(primary_key, auto_increment = false)]
    pub ref_remote: String,
    pub target_oid: String,
}

#[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
pub enum Relation {}

impl ActiveModelBehavior for ActiveModel {}
