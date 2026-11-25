//! SeaORM entity for `layer_path`: the exact working-tree paths a `layer
//! apply` materialized (lore.md 2.4). Owner: `internal::layer::LayerStore`.
//! The `content_hash` lets unapply/remove skip user-modified overlay files
//! (never clobber edits). NEVER serialized into a commit.

use sea_orm::entity::prelude::*;

#[derive(Clone, Debug, PartialEq, DeriveEntityModel, Eq)]
#[sea_orm(table_name = "layer_path")]
pub struct Model {
    #[sea_orm(primary_key, auto_increment = true)]
    pub id: i64,
    /// Which layer materialized this path.
    pub layer_name: String,
    /// Repo-relative, '/'-separated destination path (unique — one owner).
    pub path: String,
    /// Content hash at materialization time (edit detection for unapply).
    pub content_hash: String,
    pub materialized_at: String,
}

#[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
pub enum Relation {}

impl ActiveModelBehavior for ActiveModel {}
