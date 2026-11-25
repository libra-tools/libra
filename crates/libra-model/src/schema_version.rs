//! SeaORM entity for the `schema_versions` table managed by the root crate's
//! `MigrationRunner`.
//!
//! One row per applied migration. Rows are inserted by
//! `MigrationRunner::run_pending` and removed by
//! `MigrationRunner::rollback_to`. Application code should treat this table
//! as **runner-owned** and never insert / delete rows directly — doing so
//! desyncs the runner's view of "current version".

use sea_orm::entity::prelude::*;

#[derive(Clone, Debug, PartialEq, DeriveEntityModel, Eq)]
#[sea_orm(table_name = "schema_versions")]
pub struct Model {
    /// Monotonic version. Matches `Migration::version`.
    #[sea_orm(primary_key, auto_increment = false)]
    pub version: i64,
    /// Human-readable name from `Migration::name`.
    pub name: String,
    /// RFC3339 timestamp of when the migration's `up` DDL ran.
    pub applied_at: String,
}

#[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
pub enum Relation {}

impl ActiveModelBehavior for ActiveModel {}
