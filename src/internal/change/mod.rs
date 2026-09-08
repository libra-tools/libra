//! Stable logical Change IDs and their rewrite genealogy.

mod builder;
mod genealogy;
mod identity;
mod resolve;
mod store;
pub use builder::{
    ChangeRevisionBuildError, ChangeRevisionBuilder, record_current_repo_commit_revision,
};
pub use genealogy::{
    GenealogyError, GenealogyRevision, PredecessorEdge, RelationKind, evolution_for_commit,
    insert_predecessor,
};
pub use identity::{ChangeId, ChangeIdError};
pub use resolve::{ChangeIdResolution, ResolveError, resolve_change_id_prefix};
pub use store::{ChangeRevision, ChangeStore, ChangeStoreError, RevisionVisibility};
