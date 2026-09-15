//! Append-only undo, redo, and revert transitions over RestoreEngine.

use thiserror::Error;

use super::{
    OperationKind, OperationStatusV2, RestoreEngine, RestoreError, RestoreReceipt, RestoreWhat,
};

#[derive(Debug, Error)]
pub enum UndoError {
    #[error(transparent)]
    Restore(#[from] RestoreError),
    #[error("operation '{0}' is not a successful undo operation")]
    NotUndo(String),
}

#[derive(Clone)]
pub struct UndoEngine {
    restore: RestoreEngine,
}

impl UndoEngine {
    pub fn new(restore: RestoreEngine) -> Self {
        Self { restore }
    }

    pub async fn undo(
        &self,
        target_op_id: impl Into<String>,
        dry_run: bool,
        confirm_repo_wide: bool,
    ) -> Result<RestoreReceipt, UndoError> {
        let target_op_id = target_op_id.into();
        let operation = self
            .restore
            .store()
            .load_operation(&target_op_id)
            .await
            .map_err(|error| RestoreError::Storage(error.to_string()))?
            .ok_or_else(|| {
                RestoreError::Storage(format!("operation '{target_op_id}' not found"))
            })?;
        if operation.status != OperationStatusV2::Success {
            return Err(RestoreError::Storage(format!(
                "operation '{target_op_id}' is not a completed success"
            ))
            .into());
        }
        let expected_head = self.require_current_head(&target_op_id).await?;
        self.restore
            .restore_with_expected_head(
                target_op_id,
                operation.pre_view_oid,
                OperationKind::Undo,
                RestoreWhat::All,
                dry_run,
                confirm_repo_wide,
                expected_head,
            )
            .await
            .map_err(UndoError::from)
    }

    pub async fn redo(
        &self,
        undo_op_id: impl Into<String>,
        dry_run: bool,
        confirm_repo_wide: bool,
    ) -> Result<RestoreReceipt, UndoError> {
        let undo_op_id = undo_op_id.into();
        let operation = self
            .restore
            .store()
            .load_operation(&undo_op_id)
            .await
            .map_err(|error| RestoreError::Storage(error.to_string()))?
            .ok_or_else(|| RestoreError::Storage(format!("operation '{undo_op_id}' not found")))?;
        if operation.kind != OperationKind::Undo {
            return Err(UndoError::NotUndo(undo_op_id));
        }
        if operation.status != OperationStatusV2::Success {
            return Err(RestoreError::Storage(format!(
                "undo operation '{undo_op_id}' is not a completed success"
            ))
            .into());
        }
        let expected_head = self.require_current_head(&undo_op_id).await?;
        let original_op_id = operation.restores_op_id.clone().ok_or_else(|| {
            UndoError::Restore(RestoreError::Storage(format!(
                "undo operation '{undo_op_id}' has no source operation"
            )))
        })?;
        let original = self
            .restore
            .store()
            .load_operation(&original_op_id)
            .await
            .map_err(|error| RestoreError::Storage(error.to_string()))?
            .ok_or_else(|| {
                UndoError::Restore(RestoreError::Storage(format!(
                    "source operation '{original_op_id}' not found"
                )))
            })?;
        if original.status != OperationStatusV2::Success {
            return Err(RestoreError::Storage(format!(
                "source operation '{original_op_id}' is not a completed success"
            ))
            .into());
        }
        self.restore
            .restore_with_expected_head(
                original_op_id,
                original.post_view_oid,
                OperationKind::Redo,
                RestoreWhat::All,
                dry_run,
                confirm_repo_wide,
                expected_head,
            )
            .await
            .map_err(UndoError::from)
    }

    pub async fn revert(
        &self,
        target_op_id: impl Into<String>,
        parent_op_id: impl Into<String>,
        dry_run: bool,
        confirm_repo_wide: bool,
    ) -> Result<RestoreReceipt, UndoError> {
        let target_op_id = target_op_id.into();
        let parent_op_id = parent_op_id.into();
        let operation = self
            .restore
            .store()
            .load_operation(&target_op_id)
            .await
            .map_err(|error| RestoreError::Storage(error.to_string()))?
            .ok_or_else(|| {
                RestoreError::Storage(format!("operation '{target_op_id}' not found"))
            })?;
        if !operation.parent_op_ids.iter().any(|id| id == &parent_op_id) {
            return Err(RestoreError::Storage(format!(
                "operation '{parent_op_id}' is not an explicit parent of '{target_op_id}'"
            ))
            .into());
        }
        let parent = self
            .restore
            .store()
            .load_operation(&parent_op_id)
            .await
            .map_err(|error| RestoreError::Storage(error.to_string()))?
            .ok_or_else(|| {
                RestoreError::Storage(format!("parent operation '{parent_op_id}' not found"))
            })?;
        if parent.status != OperationStatusV2::Success {
            return Err(RestoreError::Storage(format!(
                "parent operation '{parent_op_id}' is not a completed success"
            ))
            .into());
        }
        let current_heads = self
            .restore
            .store()
            .read_heads_view(self.restore.repo_id(), &self.restore.scope_key())
            .await
            .map_err(|error| RestoreError::Storage(error.to_string()))?;
        if current_heads.head_ids().len() != 1 {
            return Err(RestoreError::Storage(
                "revert requires a unique current operation head; refusing to guess".to_string(),
            )
            .into());
        }
        let current = self
            .restore
            .store()
            .load_operation(&current_heads.head_ids()[0])
            .await
            .map_err(|error| RestoreError::Storage(error.to_string()))?
            .ok_or_else(|| {
                RestoreError::Storage("current operation head has no operation row".to_string())
            })?;
        if current.post_view_oid != operation.post_view_oid {
            return Err(RestoreError::Storage(format!(
                "revert of '{target_op_id}' conflicts with current state; select a parent or resolve the current head first"
            ))
            .into());
        }
        let expected_head = current_heads.head_ids()[0].clone();
        self.restore
            .restore_with_relationship_and_expected_head(
                target_op_id,
                parent.post_view_oid,
                OperationKind::Revert,
                RestoreWhat::All,
                dry_run,
                confirm_repo_wide,
                Some(parent_op_id),
                Some(expected_head),
            )
            .await
            .map_err(UndoError::from)
    }

    async fn require_current_head(&self, op_id: &str) -> Result<String, UndoError> {
        let heads = self
            .restore
            .store()
            .read_heads_view(self.restore.repo_id(), &self.restore.scope_key())
            .await
            .map_err(|error| RestoreError::Storage(error.to_string()))?;
        let head_ids = heads.head_ids();
        if head_ids != [op_id.to_string()] {
            return Err(RestoreError::Storage(format!(
                "operation '{op_id}' is not the unique current operation head; refusing to guess"
            ))
            .into());
        }
        Ok(head_ids[0].clone())
    }
}
