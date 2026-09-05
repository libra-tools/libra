//! SeaORM entity for crash-recovery phases of an operation publication.

use sea_orm::entity::prelude::*;

#[derive(Clone, Debug, PartialEq, Eq, DeriveEntityModel)]
#[sea_orm(table_name = "operation_journal")]
pub struct Model {
    #[sea_orm(primary_key, auto_increment = false)]
    pub journal_id: String,
    pub op_id: String,
    pub phase: String,
    pub pre_view_oid: Option<String>,
    pub target_view_oid: Option<String>,
    pub owner: String,
    pub updated_at: i64,
    pub recovery_payload: Option<String>,
}

#[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
pub enum Relation {}

impl ActiveModelBehavior for ActiveModel {}
