//! SeaORM model for reference rows storing branch, tag, or HEAD names with target commits and optional remotes.

use sea_orm::entity::prelude::*;
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, PartialEq, DeriveEntityModel, Eq, Serialize, Deserialize)]
#[sea_orm(table_name = "reference")]
pub struct Model {
    #[sea_orm(primary_key, auto_increment = true)]
    pub id: i64,
    /// The name of the reference (e.g., branch name, tag name)
    pub name: Option<String>,
    /// The type of the reference (Branch, Tag, Head, Intent)
    /// Note: `type` is a reserved keyword in Rust, so we use `kind`
    pub kind: ConfigKind,
    /// The commit hash the reference points to
    pub commit: Option<String>,
    /// The remote name if this is a remote tracking branch
    /// None for local references; Some("origin") for remote references.
    /// Empty string is not valid.
    pub remote: Option<String>,
    /// lore.md 2.1: per-worktree scoping for HEAD rows (kind='Head', remote
    /// NULL). NULL = the main worktree (and all shared Branch/Tag rows);
    /// Some(id) = a linked worktree's private HEAD. Shared refs keep this NULL.
    pub worktree_id: Option<String>,
}

#[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
pub enum Relation {}

impl ActiveModelBehavior for ActiveModel {}

/// The kind of reference stored in the database.
/// Maps to Git reference types.
#[derive(Debug, Clone, PartialEq, Eq, EnumIter, DeriveActiveEnum, Serialize, Deserialize)]
#[sea_orm(rs_type = "String", db_type = "Enum", enum_name = "config_kind")]
pub enum ConfigKind {
    /// Represents a local branch (e.g., refs/heads/main)
    #[sea_orm(string_value = "Branch")]
    Branch,
    /// Represents a tag (e.g., refs/tags/v1.0)
    #[sea_orm(string_value = "Tag")]
    Tag,
    /// Represents the HEAD reference (current checkout)
    #[sea_orm(string_value = "Head")]
    Head,
}
