//! Production adapters for multi-edge Change workflows.
//!
//! Command implementations that eventually expose split/duplicate operations
//! use this enum as their single hand-off into the revision builder. Keeping
//! the workflow dispatch here prevents a future command from recreating the
//! relation mapping or accidentally sharing Change IDs between split outputs.

use super::{
    ChangeRevision, ChangeRevisionBuildError, record_duplicate_revision, record_split_revisions,
};

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ChangeWorkflow {
    Split {
        revisions: Vec<(String, String, Vec<String>)>,
    },
    Duplicate {
        operation_id: String,
        commit_oid: String,
        predecessor_oid: String,
    },
}

/// Execute a supported multi-edge Change workflow through the canonical builder.
pub async fn record_change_workflow(
    workflow: ChangeWorkflow,
) -> Result<Vec<ChangeRevision>, ChangeRevisionBuildError> {
    match workflow {
        ChangeWorkflow::Split { revisions } => record_split_revisions(revisions).await,
        ChangeWorkflow::Duplicate {
            operation_id,
            commit_oid,
            predecessor_oid,
        } => Ok(vec![
            record_duplicate_revision(operation_id, commit_oid, predecessor_oid).await?,
        ]),
    }
}
