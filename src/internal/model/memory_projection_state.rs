//! SeaORM entity for per-scope Memory projection watermarks.

use sea_orm::entity::prelude::*;

#[derive(Clone, Debug, Eq, PartialEq, DeriveEntityModel)]
#[sea_orm(table_name = "memory_projection_state")]
pub struct Model {
    #[sea_orm(primary_key, auto_increment = false)]
    pub scope_key: String,
    pub projected_ref_oid: String,
    pub last_event_seq: i64,
    pub schema_version: i64,
    pub policy_version: String,
    pub rebuilt_at: i64,
}

#[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
pub enum Relation {}

impl ActiveModelBehavior for ActiveModel {}
