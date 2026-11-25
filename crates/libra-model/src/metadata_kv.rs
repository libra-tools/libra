//! SeaORM entity for the unified scoped `metadata_kv` table — branch (and
//! future scoped) metadata KV, lore.md 1.5. Repo-scope metadata intentionally
//! lives in `config_kv` under the `metadata.*` namespace instead. All reads
//! and writes go through `internal::metadata::MetadataKv` (the single owner
//! API); other code must not touch this entity directly.

use sea_orm::entity::prelude::*;

#[derive(Clone, Debug, PartialEq, DeriveEntityModel, Eq)]
#[sea_orm(table_name = "metadata_kv")]
pub struct Model {
    #[sea_orm(primary_key, auto_increment = true)]
    pub id: i64,
    /// Metadata scope; `'branch'` in v1 (app-validated, no DB CHECK so future
    /// scopes need no table rebuild).
    pub scope: String,
    /// Scope target; for `'branch'` the LOCAL branch short name (matches
    /// `reference.name` where `remote IS NULL`), treated as an opaque exact
    /// string (no parsing).
    pub target: String,
    /// Metadata key, e.g. `protect`, `archive`, `lineage.parent`, user keys.
    pub key: String,
    /// Metadata value (text in v1; 1.10 adds typed values via `value_type`).
    pub value: String,
    /// Value type tag; always `'text'` in v1 (reserved for 1.10).
    pub value_type: String,
    /// ISO-8601 UTC creation timestamp.
    pub created_at: String,
    /// ISO-8601 UTC last-update timestamp.
    pub updated_at: String,
}

#[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
pub enum Relation {}

impl ActiveModelBehavior for ActiveModel {}
