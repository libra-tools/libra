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
    AiOperationLink, GenealogyError, GenealogyRevision, PredecessorEdge, RelationKind,
    ai_links_for_change, ai_links_for_intent, evolution_for_commit, insert_predecessor,
    link_ai_operation,
};
pub use identity::{ChangeId, ChangeIdError};
pub use resolve::{ChangeIdResolution, ResolveError, resolve_change_id_prefix};
pub use store::{ChangeRevision, ChangeStore, ChangeStoreError, RevisionVisibility};
