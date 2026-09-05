//! SeaORM entity for pre-OL-02 operation parent edges.

use sea_orm::entity::prelude::*;

#[derive(Clone, Debug, PartialEq, Eq, DeriveEntityModel)]
#[sea_orm(table_name = "legacy_operation_parent")]
pub struct Model {
    #[sea_orm(primary_key, auto_increment = false)]
    pub op_id: String,
    #[sea_orm(primary_key, auto_increment = false)]
    pub parent_op_id: String,
}

#[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
pub enum Relation {}

impl ActiveModelBehavior for ActiveModel {}
