//! SeaORM entity for v2 operation parent edges.

use sea_orm::entity::prelude::*;

#[derive(Clone, Debug, PartialEq, Eq, DeriveEntityModel)]
#[sea_orm(table_name = "operation_parent")]
pub struct Model {
    #[sea_orm(primary_key, auto_increment = false)]
    pub op_id: String,
    #[sea_orm(primary_key, auto_increment = false)]
    pub parent_op_id: String,
    pub ordinal: i32,
}

#[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
pub enum Relation {}

impl ActiveModelBehavior for ActiveModel {}
