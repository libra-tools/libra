//! Worktree-local operation pointer and freshness classification.
//!
//! The pointer is deliberately kept in the worktree's private gitdir rather
//! than SQLite: it follows the physical worktree, while operation heads are
//! repository-coordinated state. A pointer is only advisory until its
//! operation ancestry and generation have been checked against the heads.

use std::{fs, io, path::PathBuf};

use git_internal::hash::ObjectHash;
use serde::{Deserialize, Serialize};
use thiserror::Error;

use super::store::OpHeadsView;
use crate::{internal::worktree_scope::RequestScope, utils::atomic_write::write_atomic};

const POINTER_FILE: &str = "operation-pointer.json";
const POINTER_SCHEMA_VERSION: u32 = 1;

/// Alias used by the operation design document for the request-pinned scope.
pub type PinnedRequestScope = RequestScope;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Staleness {
    Fresh,
    Stale,
    Sibling,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct PersistedPointer {
    schema_version: u32,
    pointer: WorkspaceStatePointer,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct WorkspaceStatePointer {
    pub last_op_id: String,
    pub last_snapshot_oid: ObjectHash,
    #[serde(default)]
    pub last_content_oid: Option<ObjectHash>,
    pub generation: u64,
}

#[derive(Debug, Error)]
pub enum PointerError {
    #[error("workspace operation pointer is missing: {0}")]
    Missing(PathBuf),
    #[error("failed to read workspace operation pointer: {0}")]
    Io(#[from] io::Error),
    #[error("workspace operation pointer is invalid: {0}")]
    Json(#[from] serde_json::Error),
    #[error("unsupported workspace operation pointer schema version {0}")]
    Schema(u32),
    #[error("workspace operation pointer contains an empty operation id")]
    EmptyOperationId,
}

impl WorkspaceStatePointer {
    pub fn new(
        last_op_id: impl Into<String>,
        last_snapshot_oid: ObjectHash,
        generation: u64,
    ) -> Self {
        Self {
            last_op_id: last_op_id.into(),
            last_snapshot_oid,
            last_content_oid: None,
            generation,
        }
    }

    pub fn path(scope: &PinnedRequestScope) -> PathBuf {
        scope.gitdir.join("info").join(POINTER_FILE)
    }

    pub async fn load(scope: &PinnedRequestScope) -> Result<Self, PointerError> {
        let path = Self::path(scope);
        let bytes = fs::read(&path).map_err(|error| {
            if error.kind() == io::ErrorKind::NotFound {
                PointerError::Missing(path.clone())
            } else {
                PointerError::Io(error)
            }
        })?;
        let persisted: PersistedPointer = serde_json::from_slice(&bytes)?;
        if persisted.schema_version != POINTER_SCHEMA_VERSION {
            return Err(PointerError::Schema(persisted.schema_version));
        }
        persisted.pointer.validate()?;
        Ok(persisted.pointer)
    }

    pub async fn save(&self, scope: &PinnedRequestScope) -> Result<(), PointerError> {
        self.validate()?;
        let persisted = PersistedPointer {
            schema_version: POINTER_SCHEMA_VERSION,
            pointer: self.clone(),
        };
        let bytes = serde_json::to_vec(&persisted)?;
        write_atomic(&Self::path(scope), &bytes, true)?;
        Ok(())
    }

    fn validate(&self) -> Result<(), PointerError> {
        if self.last_op_id.trim().is_empty() {
            return Err(PointerError::EmptyOperationId);
        }
        Ok(())
    }

    /// Compare the pointer with the currently published operation heads.
    ///
    /// A pointer is fresh only when it is exactly a current head at the same
    /// generation. It is stale when a current head descends from it. A
    /// pointer that belongs to no current-head ancestry is a sibling.
    pub fn staleness(&self, heads: &OpHeadsView) -> Staleness {
        if let Some(current_generation) = heads.generation(&self.last_op_id) {
            if current_generation == self.generation {
                return Staleness::Fresh;
            }
            if current_generation > self.generation {
                return Staleness::Stale;
            }
        }
        if heads
            .head_ids()
            .iter()
            .any(|head| heads.is_ancestor(&self.last_op_id, head))
        {
            return Staleness::Stale;
        }
        Staleness::Sibling
    }
}

#[cfg(test)]
mod tests {
    use git_internal::hash::ObjectHash;

    use super::*;

    fn oid(byte: u8) -> ObjectHash {
        ObjectHash::from_bytes(&[byte; 20]).expect("test object id")
    }

    #[test]
    fn classifies_fresh_stale_and_sibling_pointers() {
        let mut heads = OpHeadsView::with_generations(vec![("child".into(), 2)]).unwrap();
        heads.add_ancestor("child", "root").unwrap();
        let fresh = WorkspaceStatePointer::new("child", oid(1), 2);
        let stale = WorkspaceStatePointer::new("root", oid(1), 1);
        let sibling = WorkspaceStatePointer::new("other", oid(1), 1);
        assert_eq!(fresh.staleness(&heads), Staleness::Fresh);
        assert_eq!(stale.staleness(&heads), Staleness::Stale);
        assert_eq!(sibling.staleness(&heads), Staleness::Sibling);
    }

    #[test]
    fn generation_lag_is_stale_when_the_operation_is_still_a_head() {
        let heads = OpHeadsView::with_generations(vec![("head".into(), 4)]).unwrap();
        let pointer = WorkspaceStatePointer::new("head", oid(2), 3);
        assert_eq!(pointer.staleness(&heads), Staleness::Stale);
    }

    #[tokio::test]
    async fn pointer_roundtrips_and_missing_pointer_is_explicit() {
        let root = tempfile::tempdir().unwrap();
        let gitdir = root.path().join(".libra");
        std::fs::create_dir_all(&gitdir).unwrap();
        let scope = RequestScope {
            scope: crate::internal::worktree_scope::WorktreeScope::Main,
            workdir: root.path().to_path_buf(),
            gitdir: gitdir.clone(),
            storage: root.path().to_path_buf(),
            worktree_root: root.path().to_path_buf(),
        };
        assert!(matches!(
            WorkspaceStatePointer::load(&scope).await,
            Err(PointerError::Missing(_))
        ));
        let pointer = WorkspaceStatePointer::new("op-1", oid(3), 7);
        pointer.save(&scope).await.unwrap();
        assert_eq!(WorkspaceStatePointer::load(&scope).await.unwrap(), pointer);
    }
}
