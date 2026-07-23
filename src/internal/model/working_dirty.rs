//! SeaORM entity for the `working_dirty` dirty-set cache rows (lore.md 1.1).
//! All access goes through `internal::dirty::DirtyCache` (single owner API).

use sea_orm::entity::prelude::*;

#[derive(Clone, Debug, PartialEq, DeriveEntityModel, Eq)]
#[sea_orm(table_name = "working_dirty")]
pub struct Model {
    #[sea_orm(primary_key, auto_increment = true)]
    pub id: i64,
    /// Worktree scope key (`""` = main worktree); rows are unique per
    /// (worktree_id, path, kind).
    pub worktree_id: String,
    /// Repo-relative path, '/'-separated on every platform.
    pub path: String,
    /// `new`/`modified`/`deleted` (unstaged), `staged_new`/`staged_modified`/
    /// `staged_deleted` (staged snapshot), or `unknown` (manual mark).
    pub kind: String,
    /// `scan` / `manual` / `check`.
    pub source: String,
    pub marked_at: String,
    pub verified_at: Option<String>,
}

#[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
pub enum Relation {}

impl ActiveModelBehavior for ActiveModel {}
