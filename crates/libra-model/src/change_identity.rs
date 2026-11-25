//! SeaORM entity for stable logical change identities.

use sea_orm::entity::prelude::*;

#[derive(Clone, Debug, PartialEq, Eq, DeriveEntityModel)]
#[sea_orm(table_name = "change_identity")]
pub struct Model {
    #[sea_orm(primary_key, auto_increment = false)]
    pub change_id: String,
    pub repo_id: String,
    pub origin: String,
    pub created_op_id: String,
    pub created_at: i64,
}

#[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
pub enum Relation {}

impl ActiveModelBehavior for ActiveModel {}
