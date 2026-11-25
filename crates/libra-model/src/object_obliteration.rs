//! SeaORM entity for `object_obliteration` — the intentional-absence tombstone
//! registry (lore.md 2.5). Owner: `internal::obliteration::ObliterationStore`
//! (single-owner). A row is a permanently-retained compliance tombstone; its
//! absence means the object is Live.

use sea_orm::entity::prelude::*;

#[derive(Clone, Debug, PartialEq, DeriveEntityModel, Eq)]
#[sea_orm(table_name = "object_obliteration")]
pub struct Model {
    #[sea_orm(primary_key, auto_increment = true)]
    pub id: i64,
    /// Content OID whose PAYLOAD is being / has been removed (address kept).
    pub oid: String,
    /// `sha1` / `sha256`.
    pub hash_kind: String,
    /// `obliterating` (tombstone written, payload possibly present) or
    /// `obliterated` (payload physically removed).
    pub state: String,
    pub reason: Option<String>,
    pub actor: Option<String>,
    pub approval_source: Option<String>,
    pub requested_at: String,
    pub tombstone_written_at: String,
    pub payload_deleted_at: Option<String>,
    pub updated_at: String,
}

#[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
pub enum Relation {}

impl ActiveModelBehavior for ActiveModel {}
