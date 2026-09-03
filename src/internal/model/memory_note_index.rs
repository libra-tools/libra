//! SeaORM entity for the rebuildable Memory note reverse index.

use sea_orm::entity::prelude::*;

#[derive(Clone, Debug, Eq, PartialEq, DeriveEntityModel)]
#[sea_orm(table_name = "memory_note_index")]
pub struct Model {
    #[sea_orm(primary_key, auto_increment = false)]
    pub note_id: String,
    pub scope_key: String,
    pub namespace: String,
    pub path: String,
    pub kind: String,
    pub lifecycle: String,
    pub review_state: String,
    pub confidence: String,
    pub trust: String,
    pub sensitivity: String,
    pub visibility: String,
    pub acl_policy_id: String,
    pub origin: String,
    pub idempotency_key: String,
    pub idempotency_scope: String,
    pub created_at: String,
}

#[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
pub enum Relation {}

impl ActiveModelBehavior for ActiveModel {}
