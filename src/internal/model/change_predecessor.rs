//! SeaORM entity for rewrite genealogy edges.

use sea_orm::entity::prelude::*;

#[derive(Clone, Debug, PartialEq, Eq, DeriveEntityModel)]
#[sea_orm(table_name = "change_predecessor")]
pub struct Model {
    #[sea_orm(primary_key, auto_increment = false)]
    pub successor_oid: String,
    #[sea_orm(primary_key, auto_increment = false)]
    pub predecessor_oid: String,
    #[sea_orm(primary_key, auto_increment = false)]
    pub op_id: String,
    pub relation_kind: String,
    pub ordinal: i32,
}

#[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
pub enum Relation {}

impl ActiveModelBehavior for ActiveModel {}
