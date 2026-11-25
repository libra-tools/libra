//! SeaORM entity for `sparse_view` — the ordered include-pattern list of the
//! read-only sparse view (lore.md 2.2). Owner: `internal::sparse::SparseViewStore`.

use sea_orm::entity::prelude::*;

#[derive(Clone, Debug, PartialEq, DeriveEntityModel, Eq)]
#[sea_orm(table_name = "sparse_view")]
pub struct Model {
    #[sea_orm(primary_key, auto_increment = true)]
    pub id: i64,
    /// A gitignore-syntax include pattern.
    pub pattern: String,
    /// Position in the ordered list (later patterns win, `!pat` re-excludes).
    pub ordinal: i64,
}

#[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
pub enum Relation {}

impl ActiveModelBehavior for ActiveModel {}
