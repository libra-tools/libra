//! SeaORM entity for the per-repository/per-scope operation-head set.

use sea_orm::entity::prelude::*;

#[derive(Clone, Debug, PartialEq, Eq, DeriveEntityModel)]
#[sea_orm(table_name = "operation_head")]
pub struct Model {
    #[sea_orm(primary_key, auto_increment = false)]
    pub repo_id: String,
    #[sea_orm(primary_key, auto_increment = false)]
    pub scope_key: String,
    #[sea_orm(primary_key, auto_increment = false)]
    pub op_id: String,
    pub generation: i64,
}

#[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
pub enum Relation {}

impl ActiveModelBehavior for ActiveModel {}
