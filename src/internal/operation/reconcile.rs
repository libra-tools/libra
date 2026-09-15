//! Multi-head reconciliation for the operation log (OL-13).
//!
//! Concurrent worktree publications can leave the operation head set with
//! more than one head.  A reconcile is an explicit, append-only operation
//! that converges the head set to a single node when the concurrent states
//! are provably unambiguous: every shared reference must agree across the
//! concurrent views.  Conflicting references are reported and the head set
//! is left untouched — reconciliation never guesses a winner.

use std::collections::BTreeMap;

use git_internal::{hash::ObjectHash, internal::object::types::ObjectType};
use serde::Serialize;
use thiserror::Error;

use super::{
    OperationKind, OperationMetaV2, OperationStatusV2, OperationStoreV2, OperationV2, RepoViewV2,
    StoreError, view::REPO_VIEW_SCHEMA_VERSION, working_copy::PinnedRequestScope,
};

/// Machine-readable result of one reconcile attempt.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[serde(tag = "outcome", rename_all = "snake_case")]
pub enum ReconcileOutcome {
    /// The head set already had exactly one head.
    NothingToReconcile,
    /// The concurrent heads were provably unambiguous and were converged.
    Converged {
        reconcile_op_id: String,
        parents: Vec<String>,
        generation: u64,
    },
    /// Concurrent heads disagree on at least one reference. The head set is
    /// preserved and no operation is written.
    Conflicted { conflicts: Vec<RefConflict> },
    /// The heads were unambiguous but the dry-run only reported the merge.
    DryRunConverged { parents: Vec<String> },
}

/// One reference whose target differs between concurrent operation heads.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct RefConflict {
    pub kind: String,
    pub name: String,
    pub remote: Option<String>,
    pub worktree_id: Option<String>,
    /// Observed target per head operation id; at least two distinct values.
    pub targets: BTreeMap<String, String>,
}

#[derive(Debug, Error)]
pub enum ReconcileError {
    #[error("operation storage failed: {0}")]
    Storage(String),
    #[error("reconcile CAS failed: {0}")]
    Cas(String),
    #[error("invalid reference facet on operation '{0}': {1}")]
    InvalidRefsFacet(String, String),
    #[error("cannot load view for operation '{0}': {1}")]
    View(String, String),
}

impl From<StoreError> for ReconcileError {
    fn from(error: StoreError) -> Self {
        match error {
            StoreError::CasConflict { .. } => ReconcileError::Cas(error.to_string()),
            other => ReconcileError::Storage(other.to_string()),
        }
    }
}

/// Reference identity: kind + name + remote + owning worktree.  Two rows are
/// the same reference when this tuple agrees.
fn json_plain(value: Option<&serde_json::Value>) -> String {
    match value {
        None | Some(serde_json::Value::Null) => String::new(),
        Some(serde_json::Value::String(text)) => text.clone(),
        Some(other) => other.to_string(),
    }
}

fn reference_key(
    entry: &serde_json::Value,
) -> Result<(String, String, String, String), ReconcileError> {
    let kind = json_plain(entry.get("kind"));
    let name = json_plain(entry.get("name"));
    let remote = json_plain(entry.get("remote"));
    let worktree_id = json_plain(entry.get("worktree_id"));
    Ok((kind, name, remote, worktree_id))
}

fn reference_target(entry: &serde_json::Value) -> String {
    json_plain(entry.get("commit"))
}

/// Extract the references array from either refs-facet shape: the restore
/// writer emits `{"schema_version": 1, "references": [...]}` while the
/// middleware writer also carries the published `heads`.
fn references_from_facet(
    op_id: &str,
    bytes: &[u8],
) -> Result<Vec<serde_json::Value>, ReconcileError> {
    let value: serde_json::Value = serde_json::from_slice(bytes)
        .map_err(|error| ReconcileError::InvalidRefsFacet(op_id.to_string(), error.to_string()))?;
    value
        .get("references")
        .and_then(serde_json::Value::as_array)
        .cloned()
        .ok_or_else(|| {
            ReconcileError::InvalidRefsFacet(
                op_id.to_string(),
                "facet has no references array".to_string(),
            )
        })
}

pub struct ReconcileEngine {
    scope: PinnedRequestScope,
    repo_id: String,
    store: OperationStoreV2,
}

impl ReconcileEngine {
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

    pub fn store(&self) -> &OperationStoreV2 {
        &self.store
    }

    /// Attempt to converge the current multi-head set.  `dry_run` only
    /// reports what would happen and never writes.
    pub async fn reconcile(&self, dry_run: bool) -> Result<ReconcileOutcome, ReconcileError> {
        let scope_key = self.scope.scope.storage_key().to_string();
        let heads = self
            .store
            .read_heads_view(&self.repo_id, &scope_key)
            .await?;
        let head_ids = heads.head_ids();
        if head_ids.len() <= 1 {
            return Ok(ReconcileOutcome::NothingToReconcile);
        }

        // Load every head's view and references facet.
        let mut views = BTreeMap::new();
        let mut facets = BTreeMap::new();
        for head in &head_ids {
            let operation = self.store.load_operation(head).await?.ok_or_else(|| {
                ReconcileError::View(head.clone(), "missing operation row".into())
            })?;
            let view: RepoViewV2 = self
                .store
                .load_view(&operation.post_view_oid)
                .map_err(|error| ReconcileError::View(head.clone(), error.to_string()))?;
            let bytes = self
                .store
                .load_object(&view.refs_facet_oid)
                .map_err(|error| ReconcileError::View(head.clone(), error.to_string()))?;
            let references = references_from_facet(head, &bytes)?;
            views.insert(head.clone(), view);
            facets.insert(head.clone(), references);
        }

        // Merge reference rows: a shared reference must agree on its target,
        // otherwise the concurrent states conflict and reconciliation
        // refuses to guess a winner.
        let mut targets: BTreeMap<(String, String, String, String), BTreeMap<String, String>> =
            BTreeMap::new();
        for (head, references) in &facets {
            for entry in references {
                let key = reference_key(entry)?;
                targets
                    .entry(key)
                    .or_default()
                    .entry(head.clone())
                    .or_insert_with(|| reference_target(entry));
            }
        }
        let mut conflicts: Vec<RefConflict> = Vec::new();
        for ((kind, name, remote, worktree_id), per_head) in &targets {
            let distinct: std::collections::BTreeSet<&String> = per_head.values().collect();
            if distinct.len() > 1 {
                conflicts.push(RefConflict {
                    kind: kind.clone(),
                    name: name.clone(),
                    remote: if remote.is_empty() {
                        None
                    } else {
                        Some(remote.clone())
                    },
                    worktree_id: if worktree_id.is_empty() {
                        None
                    } else {
                        Some(worktree_id.clone())
                    },
                    targets: per_head.clone(),
                });
            }
        }
        if !conflicts.is_empty() {
            return Ok(ReconcileOutcome::Conflicted { conflicts });
        }
        // Provably unambiguous: merge the workspaces and reference rows into
        // one converged view. Every reference agrees across heads, so keep
        // one row per identity (deterministically ordered).
        let mut workspaces = BTreeMap::new();
        for (head, view) in &views {
            for (workspace, snapshot) in &view.workspaces {
                if workspaces
                    .insert(workspace.clone(), *snapshot)
                    .is_some_and(|previous| previous != *snapshot)
                {
                    return Err(ReconcileError::View(
                        head.clone(),
                        format!("workspace '{workspace}' has diverging snapshots across heads"),
                    ));
                }
            }
        }
        let mut merged_references: Vec<serde_json::Value> = Vec::new();
        for key in targets.keys() {
            let (head, entry) = facets
                .iter()
                .find_map(|(head, references)| {
                    references
                        .iter()
                        .find(|entry| reference_key(entry).is_ok_and(|parsed| parsed == *key))
                        .map(|entry| (head.clone(), entry.clone()))
                })
                .expect("a reference row recorded in targets must exist in a facet");
            let _ = head;
            merged_references.push(entry);
        }

        let refs_bytes = serde_json::to_vec(&serde_json::json!({
            "schema_version": 1,
            "references": merged_references,
        }))
        .map_err(|error| ReconcileError::Storage(error.to_string()))?;
        let refs_oid = ObjectHash::from_type_and_data(ObjectType::Blob, &refs_bytes);
        let converged_view = RepoViewV2 {
            schema_version: REPO_VIEW_SCHEMA_VERSION,
            repo_id: self.repo_id.clone(),
            refs_facet_oid: refs_oid,
            workspaces,
            change_roots: Vec::new(),
            extension_facets: Default::default(),
        };

        // Dry-run must not write to the object store or the operation DAG; it
        // only reports the convergence it would have produced.
        if dry_run {
            return Ok(ReconcileOutcome::DryRunConverged {
                parents: head_ids.clone(),
            });
        }

        self.store
            .write_blob(&refs_oid, &refs_bytes, ObjectType::Blob)
            .map_err(ReconcileError::from)?;
        let converged_view_oid = self
            .store
            .write_view_manifest(&converged_view)
            .map_err(ReconcileError::from)?;

        // Publish the reconcile operation with every concurrent head as an
        // explicit parent, then converge the head set to it.
        let reconcile_id = uuid::Uuid::now_v7().to_string();
        let operation = OperationV2 {
            op_id: reconcile_id.clone(),
            parent_op_ids: head_ids.clone(),
            pre_view_oid: converged_view_oid,
            post_view_oid: converged_view_oid,
            kind: OperationKind::Reconcile,
            status: OperationStatusV2::Running,
            metadata: OperationMetaV2 {
                command_name: Some("op reconcile".to_string()),
                description: Some(format!(
                    "reconcile {} concurrent operation heads",
                    head_ids.len()
                )),
                actor: Some("libra-user".to_string()),
                args_digest: None,
                ..Default::default()
            },
            restores_op_id: None,
            reverts_op_id: None,
            predecessor_map_oid: None,
        };
        self.store.write_operation(&operation).await?;
        let generation = match self
            .store
            .cas_update_op_heads(
                &self.repo_id,
                &scope_key,
                &head_ids,
                std::slice::from_ref(&reconcile_id),
            )
            .await
        {
            Ok(generation) => generation,
            Err(error) => {
                let _ = self
                    .store
                    .update_operation_status(&reconcile_id, OperationStatusV2::Failed)
                    .await;
                return Err(ReconcileError::Cas(error.to_string()));
            }
        };
        self.store
            .update_operation_status(&reconcile_id, OperationStatusV2::Success)
            .await?;
        Ok(ReconcileOutcome::Converged {
            reconcile_op_id: reconcile_id,
            parents: head_ids,
            generation,
        })
    }
}
