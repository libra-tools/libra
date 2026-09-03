//! SeaORM entity for the rebuildable current Memory note heads.

use sea_orm::entity::prelude::*;

#[derive(Clone, Debug, Eq, PartialEq, DeriveEntityModel)]
#[sea_orm(table_name = "memory_head")]
pub struct Model {
    #[sea_orm(primary_key, auto_increment = false)]
    pub scope_key: String,
    #[sea_orm(primary_key, auto_increment = false)]
    pub namespace: String,
    #[sea_orm(primary_key, auto_increment = false)]
    pub path: String,
    #[sea_orm(primary_key, auto_increment = false)]
    pub note_id: String,
    pub latest_revision_oid: String,
    pub live_revision_oid: Option<String>,
    pub latest_action: String,
    pub latest_review_state: String,
    pub kind: String,
    pub lifecycle: String,
    pub confidence: String,
    pub trust: String,
    pub sensitivity: String,
    pub visibility: String,
    pub acl_policy_id: String,
    pub valid_from: Option<String>,
    pub valid_until: Option<String>,
    pub effective_from_commit: Option<String>,
    pub effective_until_commit: Option<String>,
    pub expires_at: Option<String>,
    pub rank_hint: i64,
    pub last_event_seq: i64,
    pub updated_at: String,
}

#[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
pub enum Relation {}

impl ActiveModelBehavior for ActiveModel {}
