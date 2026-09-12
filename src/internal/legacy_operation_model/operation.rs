//! SeaORM entity for the pre-OL-02 operation table.

use sea_orm::entity::prelude::*;

#[derive(Clone, Debug, PartialEq, Eq, DeriveEntityModel)]
#[sea_orm(table_name = "legacy_operation")]
pub struct Model {
    #[sea_orm(primary_key, auto_increment = false)]
    pub op_id: String,
    pub repo_id: String,
    pub view_id: String,
    pub command_name: String,
    pub description: String,
    pub actor: String,
    pub args_digest: Option<String>,
    pub start_ts: i64,
    pub end_ts: Option<i64>,
    pub status: String,
    pub worktree_id: String,
    pub scope_provenance: String,
    pub restorable: i32,
    pub control_slot: Option<String>,
    pub claim_owner: Option<String>,
    pub scope_kind: String,
}

#[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
pub enum Relation {}

impl ActiveModelBehavior for ActiveModel {}
