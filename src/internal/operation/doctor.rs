//! Read-only diagnosis and explicit repair for v2 operation state.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};
use thiserror::Error;

use super::middleware::ScopeLease;
use super::{OperationStoreV2, PinnedRequestScope, PointerError, WorkspaceStatePointer};

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct DoctorIssue {
    pub code: String,
    pub message: String,
    pub repairable: bool,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct DoctorReport {
    pub dry_run: bool,
    pub fixed: Vec<String>,
    pub issues: Vec<DoctorIssue>,
    pub operations_checked: usize,
    pub journal_entries_checked: usize,
}

#[derive(Debug, Error)]
pub enum DoctorError {
    #[error("doctor storage failed: {0}")]
    Storage(String),
    #[error("doctor pointer repair failed: {0}")]
    Pointer(String),
}

#[derive(Clone)]
pub struct DoctorEngine {
    scope: PinnedRequestScope,
    repo_id: String,
    store: OperationStoreV2,
}

impl DoctorEngine {
    pub fn new(
        scope: PinnedRequestScope,
        repo_id: impl Into<String>,
        store: OperationStoreV2,
    ) -> Self {
        Self {
            scope,
            repo_id: repo_id.into(),
            store,
        }
    }

    pub async fn inspect(&self, dry_run: bool, fix: bool) -> Result<DoctorReport, DoctorError> {
        let _repo_lease = if fix && !dry_run {
            Some(
                ScopeLease::acquire_repository(&self.scope, &self.repo_id)
                    .await
                    .map_err(|error| DoctorError::Storage(error.to_string()))?,
            )
        } else {
            None
        };
        let _worktree_lease = if fix && !dry_run {
            Some(
                ScopeLease::acquire(&self.scope, &self.repo_id)
                    .await
                    .map_err(|error| DoctorError::Storage(error.to_string()))?,
            )
        } else {
            None
        };
        let operations = self
            .store
            .list_operations()
            .await
            .map_err(|error| DoctorError::Storage(error.to_string()))?;
        let journals = self
            .store
            .read_all_journal()
            .await
            .map_err(|error| DoctorError::Storage(error.to_string()))?;
        let mut issues = Vec::new();
        for operation in &operations {
            for (label, oid) in [
                ("pre", operation.pre_view_oid),
                ("post", operation.post_view_oid),
            ] {
                match self.store.load_view(&oid) {
                    Ok(view) => {
                        if view.repo_id != self.repo_id {
                            issues.push(DoctorIssue {
                                code: "repo-mismatch".to_string(),
                                message: format!(
                                    "operation {} {label} view points at another repository",
                                    operation.op_id
                                ),
                                repairable: false,
                            });
                        }
                        if let Err(error) = view.validate_recursive_closure(|object| {
                            self.store.load_object(object).ok()
                        }) {
                            issues.push(DoctorIssue {
                                code: "missing-object".to_string(),
                                message: format!(
                                    "operation {} {label} view: {error}",
                                    operation.op_id
                                ),
                                repairable: false,
                            });
                        }
                    }
                    Err(error) => issues.push(DoctorIssue {
                        code: "missing-view".to_string(),
                        message: format!("operation {} {label} view: {error}", operation.op_id),
                        repairable: false,
                    }),
                }
            }
        }

        let scope_key = self.scope.scope.storage_key().to_string();
        let mut heads = self
            .store
            .read_heads(&self.repo_id, &scope_key)
            .await
            .map_err(|error| DoctorError::Storage(error.to_string()))?;
        for head in &heads {
            match operations.iter().find(|operation| operation.op_id == *head) {
                None => issues.push(DoctorIssue {
                    code: "orphan-head".to_string(),
                    message: format!("head {head} has no operation row"),
                    repairable: false,
                }),
                Some(operation) if operation.status != super::OperationStatusV2::Success => {
                    issues.push(DoctorIssue {
                        code: "invalid-head-status".to_string(),
                        message: format!(
                            "head {head} points to operation with status {}",
                            operation.status
                        ),
                        repairable: false,
                    });
                }
                Some(_) => {}
            }
        }
        if heads.len() > 1 {
            issues.push(DoctorIssue {
                code: "ambiguous-heads".to_string(),
                message: format!("operation scope has {} current heads", heads.len()),
                repairable: false,
            });
        }

        let mut latest_journals = BTreeMap::new();
        for journal in journals.iter().cloned() {
            let replace = latest_journals
                .get(&journal.op_id)
                .map_or(true, |current: &super::JournalEntry| {
                    current.updated_at <= journal.updated_at
                });
            if replace {
                latest_journals.insert(journal.op_id.clone(), journal);
            }
        }
        let current_workspace_id = self.scope.scope.worktree_id().unwrap_or("main");
        let journal_belongs_to_scope = |journal: &super::JournalEntry| {
            journal
                .recovery_payload
                .as_deref()
                .and_then(|payload| serde_json::from_str::<serde_json::Value>(payload).ok())
                .and_then(|payload| {
                    payload
                        .get("workspace_id")
                        .and_then(serde_json::Value::as_str)
                        .map(|workspace| workspace == current_workspace_id)
                })
                .unwrap_or(true)
        };
        for journal in latest_journals.values() {
            let incomplete = journal.phase != super::JournalPhase::Publish;
            if incomplete && journal_belongs_to_scope(journal) {
                issues.push(DoctorIssue {
                    code: "unfinished-journal".to_string(),
                    message: format!("operation {} stopped at {}", journal.op_id, journal.phase),
                    repairable: true,
                });
            }
        }

        let mut fixed = Vec::new();
        let pointer = WorkspaceStatePointer::load(&self.scope).await;
        if let Err(PointerError::Missing(_)) = &pointer {
            issues.push(DoctorIssue {
                code: "missing-pointer".to_string(),
                message: "workspace operation pointer is missing".to_string(),
                repairable: true,
            });
        } else if let Ok(pointer) = &pointer {
            let heads_view = self
                .store
                .read_heads_view(&self.repo_id, &scope_key)
                .await
                .map_err(|error| DoctorError::Storage(error.to_string()))?;
            if !heads.is_empty()
                && !matches!(pointer.staleness(&heads_view), super::Staleness::Fresh)
            {
                issues.push(DoctorIssue {
                    code: "stale-pointer".to_string(),
                    message: format!("pointer {} is not at a current head", pointer.last_op_id),
                    repairable: true,
                });
            }
        } else if let Err(error) = pointer {
            issues.push(DoctorIssue {
                code: "invalid-pointer".to_string(),
                message: error.to_string(),
                repairable: false,
            });
        }

        if fix && !dry_run {
            if latest_journals.values().any(|journal| {
                journal.phase != super::JournalPhase::Publish && journal_belongs_to_scope(journal)
            }) {
                let engine = super::RestoreEngine::new(
                    self.scope.clone(),
                    self.repo_id.clone(),
                    self.store.db().clone(),
                    self.store.storage(),
                );
                match engine.recover_interrupted_operations(&scope_key).await {
                    Ok(()) => {}
                    Err(error) if error.to_string().contains("recovered interrupted") => {}
                    Err(error) => return Err(DoctorError::Storage(error.to_string())),
                }
                fixed.push("unfinished-journals".to_string());
                heads = self
                    .store
                    .read_heads(&self.repo_id, &scope_key)
                    .await
                    .map_err(|error| DoctorError::Storage(error.to_string()))?;
            }
            if heads.len() == 1
                && let Some(head) = heads.first()
                && let Some(operation) =
                    operations.iter().find(|operation| operation.op_id == *head)
                && operation.status == super::OperationStatusV2::Success
            {
                let view = self
                    .store
                    .load_view(&operation.post_view_oid)
                    .map_err(|error| DoctorError::Storage(error.to_string()))?;
                if let Some(snapshot_oid) = view
                    .workspaces
                    .get(&self.scope.scope.worktree_id().unwrap_or("main").to_string())
                {
                    let generation = self
                        .store
                        .read_head_generation(&self.repo_id, &scope_key)
                        .await
                        .map_err(|error| DoctorError::Storage(error.to_string()))?;
                    let mut pointer = WorkspaceStatePointer::new(head, *snapshot_oid, generation);
                    pointer.last_content_oid = Some(*snapshot_oid);
                    pointer
                        .save(&self.scope)
                        .await
                        .map_err(|error| DoctorError::Pointer(error.to_string()))?;
                    fixed.push("workspace-pointer".to_string());
                }
            }
        }
        Ok(DoctorReport {
            dry_run,
            fixed,
            issues,
            operations_checked: operations.len(),
            journal_entries_checked: journals.len(),
        })
    }
}
