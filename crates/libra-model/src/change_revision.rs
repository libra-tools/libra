//! SeaORM entity for change-to-commit revision projections.

use sea_orm::entity::prelude::*;

#[derive(Clone, Debug, PartialEq, Eq, DeriveEntityModel)]
#[sea_orm(table_name = "change_revision")]
pub struct Model {
    #[sea_orm(primary_key, auto_increment = false)]
    pub change_id: String,
    #[sea_orm(primary_key, auto_increment = false)]
    pub commit_oid: String,
    pub created_op_id: String,
    pub visibility: String,
    pub revision_ordinal: i64,
}

#[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
pub enum Relation {}

impl ActiveModelBehavior for ActiveModel {}
