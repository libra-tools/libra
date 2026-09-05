//! SeaORM entity for the v2 append-only operation record.

use sea_orm::entity::prelude::*;

#[derive(Clone, Debug, PartialEq, Eq, DeriveEntityModel)]
#[sea_orm(table_name = "operation")]
pub struct Model {
    #[sea_orm(primary_key, auto_increment = false)]
    pub op_id: String,
    pub repo_id: String,
    pub format_version: i32,
    pub kind: String,
    pub status: String,
    pub command_name: Option<String>,
    pub description: Option<String>,
    pub args_digest: Option<String>,
    pub actor: Option<String>,
    pub worktree_id: Option<String>,
    pub scope_kind: String,
    pub pre_view_oid: String,
    pub post_view_oid: String,
    pub restores_op_id: Option<String>,
    pub reverts_op_id: Option<String>,
    pub predecessor_map_oid: Option<String>,
    pub causal_context_id: Option<String>,
    pub start_ts: i64,
    pub end_ts: Option<i64>,
}

#[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
pub enum Relation {}

impl ActiveModelBehavior for ActiveModel {}
