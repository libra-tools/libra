//! SeaORM entity for the per-ref `revision_ordinal_meta` freshness record
//! (lore.md 1.16). All access goes through
//! `internal::revision_ordinal::RevisionOrdinalIndex`.

use sea_orm::entity::prelude::*;

#[derive(Clone, Debug, PartialEq, DeriveEntityModel, Eq)]
#[sea_orm(table_name = "revision_ordinal_meta")]
pub struct Model {
    #[sea_orm(primary_key, auto_increment = false)]
    pub ref_name: String,
    /// Freshness fingerprint: the ref tip at (re)build time.
    pub tip_oid: String,
    /// Digest of the `refs/replace` set at (re)build time — a replace
    /// mutation changes the EFFECTIVE chain without moving the tip.
    pub replace_sig: String,
    pub max_ordinal: i64,
    pub built_at: String,
}

#[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
pub enum Relation {}

impl ActiveModelBehavior for ActiveModel {}
