//! SeaORM entity for the `layer` overlay registry (lore.md 2.4). All access
//! goes through `internal::layer::LayerStore` (single-owner API). NEVER
//! serialized into a commit — a layer is a purely-local materialized overlay.

use sea_orm::entity::prelude::*;

#[derive(Clone, Debug, PartialEq, DeriveEntityModel, Eq)]
#[sea_orm(table_name = "layer")]
pub struct Model {
    #[sea_orm(primary_key, auto_increment = true)]
    pub id: i64,
    /// Unique layer name.
    pub name: String,
    /// Local source directory the overlay materializes from.
    pub source: String,
    /// Stack priority — higher wins on a same-destination collision.
    pub priority: i64,
    /// Whether the layer participates in `apply` (1 = enabled).
    pub enabled: i32,
    pub created_at: String,
    pub updated_at: String,
}

#[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
pub enum Relation {}

impl ActiveModelBehavior for ActiveModel {}
