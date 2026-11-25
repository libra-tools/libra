//! SeaORM entity for object_index table storing Git object metadata for cloud backup synchronization.

use sea_orm::entity::prelude::*;

/// Object index model for tracking Git objects and their cloud sync status.
/// Each row represents a single Git object (blob, tree, commit, or tag) with its
/// hash, size, and synchronization state for D1/R2 backup.
#[derive(Clone, Debug, PartialEq, DeriveEntityModel, Eq)]
#[sea_orm(table_name = "object_index")]
pub struct Model {
    #[sea_orm(primary_key, auto_increment = true)]
    pub id: i64,
    /// Object hash (algorithm-agnostic hex; kind comes from `core.objectformat`)
    // Removed unique constraint to allow same object in different repos
    pub o_id: String,
    /// Object type: blob, tree, commit, tag
    pub o_type: String,
    /// Original object size in bytes (before compression)
    pub o_size: i64,
    /// Repository UUID for multi-tenant isolation
    pub repo_id: String,
    /// Unix timestamp when the object was created/indexed
    pub created_at: i64,
    /// Sync status: 0 = not synced to cloud, 1 = synced
    #[sea_orm(default_value = "0")]
    pub is_synced: i32,
}

#[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
pub enum Relation {}

impl ActiveModelBehavior for ActiveModel {}
