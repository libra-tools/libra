//! Stable logical Change IDs and their rewrite genealogy.

mod builder;
mod genealogy;
mod identity;
mod resolve;
mod store;
mod workflows;
pub use builder::{
    ChangeRevisionBuildError, ChangeRevisionBuilder, record_current_repo_commit_revision,
    record_current_repo_commit_revision_with_predecessors, record_duplicate_revision,
    record_split_revisions,
};
pub use genealogy::{
    AiOperationLink, GenealogyError, GenealogyRevision, PredecessorEdge, RelationKind,
    ai_links_for_change, ai_links_for_intent, attach_pending_ai_operation_links,
    evolution_for_commit, insert_predecessor, link_ai_operation, record_pending_ai_operation_link,
};
pub use identity::{ChangeId, ChangeIdError};
pub use resolve::{ChangeIdResolution, ResolveError, resolve_change_id_prefix};
pub use store::{ChangeRevision, ChangeStore, ChangeStoreError, RevisionVisibility};
pub use workflows::{ChangeWorkflow, record_change_workflow};
