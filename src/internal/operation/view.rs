//! Canonical, content-addressed v2 repository and workspace manifests.

use std::collections::{BTreeMap, BTreeSet};

use git_internal::hash::ObjectHash;
use serde::{Deserialize, Serialize};
use thiserror::Error;

use super::facet::{FacetName, RestorePolicy};

pub const REPO_VIEW_SCHEMA_VERSION: u32 = 2;
pub const WORKSPACE_SNAPSHOT_SCHEMA_VERSION: u32 = 2;

pub type WorkspaceId = String;

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
pub enum HeadState {
    Symbolic { reference: String },
    Detached { oid: ObjectHash },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CapturePolicy {
    Tracked,
    TrackedAndUntracked,
    FailClosed,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Completeness {
    Full,
    Partial,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
pub struct RepoViewV2 {
    pub schema_version: u32,
    pub repo_id: String,
    pub refs_facet_oid: ObjectHash,
    pub workspaces: BTreeMap<WorkspaceId, ObjectHash>,
    pub change_roots: Vec<ObjectHash>,
    pub extension_facets: BTreeMap<FacetName, ObjectHash>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
pub struct WorkspaceSnapshotV2 {
    pub schema_version: u32,
    pub workspace_id: WorkspaceId,
    pub head: HeadState,
    pub index_tree_oid: ObjectHash,
    pub raw_index_blob_oid: ObjectHash,
    pub working_copy_tree_oid: ObjectHash,
    pub untracked_manifest_oid: ObjectHash,
    pub sparse_facet_oid: Option<ObjectHash>,
    pub sequencer_facet_oid: Option<ObjectHash>,
    pub worktree_generation: u64,
    pub capture_policy: CapturePolicy,
    pub completeness: Completeness,
    pub facet_restore_policies: BTreeMap<FacetName, RestorePolicy>,
}

#[derive(Debug, Error)]
pub enum ViewError {
    #[error("unsupported repository view schema version {0}; expected {REPO_VIEW_SCHEMA_VERSION}")]
    UnknownRepoSchema(u32),
    #[error(
        "unsupported workspace snapshot schema version {0}; expected {WORKSPACE_SNAPSHOT_SCHEMA_VERSION}"
    )]
    UnknownWorkspaceSchema(u32),
    #[error("repository view has an empty repo_id")]
    EmptyRepoId,
    #[error("workspace snapshot has an empty workspace_id")]
    EmptyWorkspaceId,
    #[error("canonical manifest JSON is invalid: {0}")]
    Json(#[from] serde_json::Error),
    #[error("manifest is not in canonical JSON form")]
    NonCanonical,
    #[error("manifest object closure is missing {0}")]
    MissingObject(ObjectHash),
    #[error("workspace manifest {oid} is not canonical: {source}")]
    InvalidWorkspaceManifest {
        oid: ObjectHash,
        source: Box<ViewError>,
    },
}

impl RepoViewV2 {
    pub fn validate(&self) -> Result<(), ViewError> {
        if self.schema_version != REPO_VIEW_SCHEMA_VERSION {
            return Err(ViewError::UnknownRepoSchema(self.schema_version));
        }
        if self.repo_id.trim().is_empty() {
            return Err(ViewError::EmptyRepoId);
        }
        Ok(())
    }

    pub fn to_canonical_bytes(&self) -> Result<Vec<u8>, ViewError> {
        self.validate()?;
        let mut canonical = self.clone();
        canonical.change_roots.sort();
        Ok(serde_json::to_vec(&canonical)?)
    }

    pub fn from_canonical_bytes(bytes: &[u8]) -> Result<Self, ViewError> {
        let view: Self = serde_json::from_slice(bytes)?;
        view.validate()?;
        if view.to_canonical_bytes()? != bytes {
            return Err(ViewError::NonCanonical);
        }
        Ok(view)
    }

    pub fn from_bytes(bytes: &[u8]) -> Result<Self, ViewError> {
        Self::from_canonical_bytes(bytes)
    }

    pub fn roots(&self) -> Vec<ObjectHash> {
        let mut roots = std::collections::BTreeSet::new();
        roots.insert(self.refs_facet_oid);
        roots.extend(self.workspaces.values().copied());
        roots.extend(self.change_roots.iter().copied());
        roots.extend(self.extension_facets.values().copied());
        roots.into_iter().collect()
    }

    pub fn validate_closed<F>(&self, mut has_object: F) -> Result<(), ViewError>
    where
        F: FnMut(&ObjectHash) -> bool,
    {
        self.validate()?;
        for root in self.roots() {
            if !has_object(&root) {
                return Err(ViewError::MissingObject(root));
            }
        }
        Ok(())
    }

    pub fn validate_closure<F>(&self, has_object: F) -> Result<(), ViewError>
    where
        F: FnMut(&ObjectHash) -> bool,
    {
        self.validate_closed(has_object)
    }

    /// Validate the transitive repository-view closure using an object loader.
    ///
    /// The direct roots remain opaque content-addressed objects, but workspace
    /// roots are typed WorkspaceSnapshotV2 manifests and must themselves be
    /// decoded and checked. This keeps a valid repository view from anchoring
    /// only its top-level blob while losing the snapshot objects it names.
    pub fn validate_recursive_closure<F>(&self, mut load_object: F) -> Result<(), ViewError>
    where
        F: FnMut(&ObjectHash) -> Option<Vec<u8>>,
    {
        self.validate()?;
        for root in self.roots() {
            if load_object(&root).is_none() {
                return Err(ViewError::MissingObject(root));
            }
        }
        let mut expanded = BTreeSet::new();
        for oid in self.workspaces.values() {
            validate_workspace_closure(*oid, &mut load_object, &mut expanded)?;
        }
        Ok(())
    }
}

fn validate_workspace_closure<F>(
    oid: ObjectHash,
    load_object: &mut F,
    seen: &mut BTreeSet<ObjectHash>,
) -> Result<(), ViewError>
where
    F: FnMut(&ObjectHash) -> Option<Vec<u8>>,
{
    if !seen.insert(oid) {
        return Ok(());
    }
    let bytes = load_object(&oid).ok_or(ViewError::MissingObject(oid))?;
    let snapshot = WorkspaceSnapshotV2::from_canonical_bytes(&bytes).map_err(|source| {
        ViewError::InvalidWorkspaceManifest {
            oid,
            source: Box::new(source),
        }
    })?;
    for root in snapshot.roots() {
        if load_object(&root).is_none() {
            return Err(ViewError::MissingObject(root));
        }
        seen.insert(root);
    }
    Ok(())
}

impl WorkspaceSnapshotV2 {
    pub fn validate(&self) -> Result<(), ViewError> {
        if self.schema_version != WORKSPACE_SNAPSHOT_SCHEMA_VERSION {
            return Err(ViewError::UnknownWorkspaceSchema(self.schema_version));
        }
        if self.workspace_id.trim().is_empty() {
            return Err(ViewError::EmptyWorkspaceId);
        }
        if let HeadState::Symbolic { reference } = &self.head
            && reference.trim().is_empty()
        {
            return Err(ViewError::EmptyWorkspaceId);
        }
        Ok(())
    }

    pub fn to_canonical_bytes(&self) -> Result<Vec<u8>, ViewError> {
        self.validate()?;
        Ok(serde_json::to_vec(self)?)
    }

    pub fn from_canonical_bytes(bytes: &[u8]) -> Result<Self, ViewError> {
        let snapshot: Self = serde_json::from_slice(bytes)?;
        snapshot.validate()?;
        if snapshot.to_canonical_bytes()? != bytes {
            return Err(ViewError::NonCanonical);
        }
        Ok(snapshot)
    }

    pub fn from_bytes(bytes: &[u8]) -> Result<Self, ViewError> {
        Self::from_canonical_bytes(bytes)
    }

    pub fn roots(&self) -> Vec<ObjectHash> {
        let mut roots = std::collections::BTreeSet::new();
        roots.insert(self.index_tree_oid);
        roots.insert(self.raw_index_blob_oid);
        roots.insert(self.working_copy_tree_oid);
        roots.insert(self.untracked_manifest_oid);
        if let HeadState::Detached { oid } = self.head {
            roots.insert(oid);
        }
        if let Some(oid) = self.sparse_facet_oid {
            roots.insert(oid);
        }
        if let Some(oid) = self.sequencer_facet_oid {
            roots.insert(oid);
        }
        roots.into_iter().collect()
    }

    pub fn validate_closed<F>(&self, mut has_object: F) -> Result<(), ViewError>
    where
        F: FnMut(&ObjectHash) -> bool,
    {
        self.validate()?;
        for root in self.roots() {
            if !has_object(&root) {
                return Err(ViewError::MissingObject(root));
            }
        }
        Ok(())
    }

    pub fn validate_closure<F>(&self, has_object: F) -> Result<(), ViewError>
    where
        F: FnMut(&ObjectHash) -> bool,
    {
        self.validate_closed(has_object)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn oid(byte: u8) -> ObjectHash {
        ObjectHash::from_bytes(&[byte; 20]).expect("test SHA-1 object id")
    }

    fn snapshot() -> WorkspaceSnapshotV2 {
        WorkspaceSnapshotV2 {
            schema_version: WORKSPACE_SNAPSHOT_SCHEMA_VERSION,
            workspace_id: "workspace-a".to_string(),
            head: HeadState::Symbolic {
                reference: "refs/heads/main".to_string(),
            },
            index_tree_oid: oid(1),
            raw_index_blob_oid: oid(2),
            working_copy_tree_oid: oid(3),
            untracked_manifest_oid: oid(4),
            sparse_facet_oid: None,
            sequencer_facet_oid: Some(oid(5)),
            worktree_generation: 7,
            capture_policy: CapturePolicy::TrackedAndUntracked,
            completeness: Completeness::Full,
            facet_restore_policies: BTreeMap::from([(
                FacetName::from("refs"),
                RestorePolicy::AutoRestore,
            )]),
        }
    }

    #[test]
    fn workspace_snapshot_roundtrips_canonical_bytes() {
        let original = snapshot();
        let bytes = original.to_canonical_bytes().expect("serialize snapshot");
        assert_eq!(
            WorkspaceSnapshotV2::from_canonical_bytes(&bytes).expect("decode snapshot"),
            original
        );
    }

    #[test]
    fn repo_view_sorts_change_roots_before_hashing() {
        let view = RepoViewV2 {
            schema_version: REPO_VIEW_SCHEMA_VERSION,
            repo_id: "repo-a".to_string(),
            refs_facet_oid: oid(1),
            workspaces: BTreeMap::from([("workspace-a".to_string(), oid(2))]),
            change_roots: vec![oid(4), oid(3)],
            extension_facets: BTreeMap::new(),
        };
        let bytes = view.to_canonical_bytes().expect("serialize view");
        let decoded = RepoViewV2::from_canonical_bytes(&bytes).expect("decode view");
        assert_eq!(decoded.change_roots, vec![oid(3), oid(4)]);
    }

    #[test]
    fn unknown_schema_version_is_rejected_before_closure_check() {
        let mut value = serde_json::to_value(snapshot()).expect("serialize test snapshot");
        value["schema_version"] = serde_json::json!(99);
        let bytes = serde_json::to_vec(&value).expect("serialize unknown schema");
        assert!(matches!(
            WorkspaceSnapshotV2::from_canonical_bytes(&bytes),
            Err(ViewError::UnknownWorkspaceSchema(99))
        ));
    }

    #[test]
    fn missing_root_fails_closed() {
        let original = snapshot();
        let error = original
            .validate_closed(|candidate| *candidate != oid(3))
            .expect_err("missing object must fail closure validation");
        assert_eq!(
            error.to_string(),
            format!("manifest object closure is missing {}", oid(3))
        );
    }

    #[test]
    fn recursive_closure_checks_workspace_manifest_and_its_roots() {
        let mut view = RepoViewV2 {
            schema_version: REPO_VIEW_SCHEMA_VERSION,
            repo_id: "repo-a".to_string(),
            refs_facet_oid: oid(20),
            workspaces: BTreeMap::from([("workspace-a".to_string(), oid(21))]),
            change_roots: Vec::new(),
            extension_facets: BTreeMap::new(),
        };
        let snapshot = snapshot();
        let snapshot_bytes = snapshot.to_canonical_bytes().expect("serialize snapshot");
        let mut objects = BTreeMap::from([(oid(20), b"refs".to_vec()), (oid(21), snapshot_bytes)]);
        for root in snapshot.roots() {
            objects.insert(root, b"object".to_vec());
        }
        view.validate_recursive_closure(|object| objects.get(object).cloned())
            .expect("nested snapshot closure is complete");
        view.workspaces.insert("workspace-b".to_string(), oid(22));
        let error = view
            .validate_recursive_closure(|object| objects.get(object).cloned())
            .expect_err("missing nested workspace must fail closed");
        assert_eq!(
            error.to_string(),
            format!("manifest object closure is missing {}", oid(22))
        );
    }
}
