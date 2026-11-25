//! SeaORM entity for the `revision_ordinal` index rows (lore.md 1.16).
//! All access goes through `internal::revision_ordinal::RevisionOrdinalIndex`.

use sea_orm::entity::prelude::*;

#[derive(Clone, Debug, PartialEq, DeriveEntityModel, Eq)]
#[sea_orm(table_name = "revision_ordinal")]
pub struct Model {
    #[sea_orm(primary_key, auto_increment = true)]
    pub id: i64,
    /// Full ref name (e.g. `refs/heads/main`) — per-ref ordinal spaces.
    pub ref_name: String,
    /// 1-based position on the root→tip first-parent chain (1 = root).
    pub ordinal: i64,
    /// Hex OID as TEXT (sha1/sha256 agnostic).
    pub oid: String,
}

#[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
pub enum Relation {}

impl ActiveModelBehavior for ActiveModel {}
