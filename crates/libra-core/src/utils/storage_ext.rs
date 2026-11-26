//! Extension helpers for storage backends that need higher-level object operations.
//!
//! Boundary: these helpers keep backend-neutral behavior out of individual storage
//! implementations. Storage-flow and cloud-backup tests cover missing objects,
//! compression boundaries, and local/remote fallback behavior.

use anyhow::{Result, anyhow};
use async_trait::async_trait;
use git_internal::{
    hash::ObjectHash,
    internal::object::{
        context::ContextSnapshot,
        context_frame::ContextFrame,
        decision::Decision,
        evidence::Evidence,
        intent_event::IntentEvent,
        patchset::PatchSet,
        plan::Plan,
        plan_step_event::PlanStepEvent,
        provenance::Provenance,
        run::Run,
        run_event::RunEvent,
        run_usage::RunUsage,
        task::Task,
        task_event::TaskEvent,
        tool::ToolInvocation,
        types::{ArtifactRef, ObjectType},
    },
};
use serde::{Serialize, de::DeserializeOwned};

use crate::utils::storage::Storage;

/// Trait for objects that have a unique ID and Type, used for Ref creation.
pub trait Identifiable {
    fn object_id(&self) -> String;
    fn object_type(&self) -> String;
}

impl Identifiable for Task {
    fn object_id(&self) -> String {
        self.header().object_id().to_string()
    }
    fn object_type(&self) -> String {
        self.header().object_type().to_string()
    }
}

impl Identifiable for IntentEvent {
    fn object_id(&self) -> String {
        self.header().object_id().to_string()
    }
    fn object_type(&self) -> String {
        self.header().object_type().to_string()
    }
}

impl Identifiable for TaskEvent {
    fn object_id(&self) -> String {
        self.header().object_id().to_string()
    }
    fn object_type(&self) -> String {
        self.header().object_type().to_string()
    }
}

impl Identifiable for RunEvent {
    fn object_id(&self) -> String {
        self.header().object_id().to_string()
    }
    fn object_type(&self) -> String {
        self.header().object_type().to_string()
    }
}

impl Identifiable for Run {
    fn object_id(&self) -> String {
        self.header().object_id().to_string()
    }
    fn object_type(&self) -> String {
        self.header().object_type().to_string()
    }
}

impl Identifiable for Plan {
    fn object_id(&self) -> String {
        self.header().object_id().to_string()
    }
    fn object_type(&self) -> String {
        self.header().object_type().to_string()
    }
}

impl Identifiable for ContextSnapshot {
    fn object_id(&self) -> String {
        self.header().object_id().to_string()
    }
    fn object_type(&self) -> String {
        self.header().object_type().to_string()
    }
}

impl Identifiable for PatchSet {
    fn object_id(&self) -> String {
        self.header().object_id().to_string()
    }
    fn object_type(&self) -> String {
        self.header().object_type().to_string()
    }
}

impl Identifiable for Evidence {
    fn object_id(&self) -> String {
        self.header().object_id().to_string()
    }
    fn object_type(&self) -> String {
        self.header().object_type().to_string()
    }
}

impl Identifiable for ToolInvocation {
    fn object_id(&self) -> String {
        self.header().object_id().to_string()
    }
    fn object_type(&self) -> String {
        self.header().object_type().to_string()
    }
}

impl Identifiable for Provenance {
    fn object_id(&self) -> String {
        self.header().object_id().to_string()
    }
    fn object_type(&self) -> String {
        self.header().object_type().to_string()
    }
}

impl Identifiable for Decision {
    fn object_id(&self) -> String {
        self.header().object_id().to_string()
    }
    fn object_type(&self) -> String {
        self.header().object_type().to_string()
    }
}

impl Identifiable for ContextFrame {
    fn object_id(&self) -> String {
        self.header().object_id().to_string()
    }
    fn object_type(&self) -> String {
        self.header().object_type().to_string()
    }
}

impl Identifiable for PlanStepEvent {
    fn object_id(&self) -> String {
        self.header().object_id().to_string()
    }
    fn object_type(&self) -> String {
        self.header().object_type().to_string()
    }
}

impl Identifiable for RunUsage {
    fn object_id(&self) -> String {
        self.header().object_id().to_string()
    }
    fn object_type(&self) -> String {
        self.header().object_type().to_string()
    }
}

/// Extension trait for Storage to support Structured Objects (JSON) and Artifacts
#[async_trait]
pub trait StorageExt: Storage + Send + Sync {
    /// Store a serializable object (Task, Run, etc.) as a Git Blob.
    /// Returns the Git Object Hash.
    async fn put_json<T: Serialize + Send + Sync>(&self, object: &T) -> Result<ObjectHash>;

    /// Retrieve and deserialize an object from a Git Blob hash.
    async fn get_json<T: DeserializeOwned + Send + Sync>(&self, hash: &ObjectHash) -> Result<T>;

    /// Store raw content as an Artifact.
    /// Returns an ArtifactRef pointing to the stored content.
    async fn put_artifact(&self, data: &[u8]) -> Result<ArtifactRef>;
}

#[async_trait]
impl<S: Storage + Send + Sync + ?Sized> StorageExt for S {
    async fn put_json<T: Serialize + Send + Sync>(&self, object: &T) -> Result<ObjectHash> {
        // Serialize to JSON
        let data =
            serde_json::to_vec(object).map_err(|e| anyhow!("Failed to serialize object: {}", e))?;

        // Compute hash for Blob type
        let hash = ObjectHash::from_type_and_data(ObjectType::Blob, &data);

        // Store in backend
        self.put(&hash, &data, ObjectType::Blob)
            .await
            .map_err(|e| anyhow!(e))?;

        Ok(hash)
    }

    async fn get_json<T: DeserializeOwned + Send + Sync>(&self, hash: &ObjectHash) -> Result<T> {
        let (data, obj_type) = self.get(hash).await.map_err(|e| anyhow!(e))?;

        if obj_type != ObjectType::Blob {
            return Err(anyhow!("Expected Blob for object, found {}", obj_type));
        }

        let object = serde_json::from_slice(&data)
            .map_err(|e| anyhow!("Failed to deserialize object: {}", e))?;

        Ok(object)
    }

    async fn put_artifact(&self, data: &[u8]) -> Result<ArtifactRef> {
        // Compute Git Object Hash (SHA1/SHA256) for storage addressing
        let object_hash = ObjectHash::from_type_and_data(ObjectType::Blob, data);

        // Store as Blob
        self.put(&object_hash, data, ObjectType::Blob)
            .await
            .map_err(|e| anyhow!(e))?;

        // Create ArtifactRef
        // Key is the Git Object Hash string
        // Store is unified as "libra"
        let artifact =
            ArtifactRef::new("libra", object_hash.to_string()).map_err(|e| anyhow!(e))?;

        Ok(artifact)
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use git_internal::internal::object::{
        task::{GoalType, Task},
        types::ActorRef,
    };
    use tempfile::tempdir;

    use super::*;
    use crate::utils::storage::local::LocalStorage;

    #[tokio::test]
    async fn test_storage_ext() {
        let dir = tempdir().unwrap();
        let storage = Arc::new(LocalStorage::try_new(dir.path().to_path_buf()).expect("storage"));

        // 1. Test Task Storage
        let actor = ActorRef::human("tester").unwrap();
        let task = Task::new(actor, "Test Task", Some(GoalType::Feature)).unwrap();

        let hash = storage.put_json(&task).await.unwrap();
        let loaded_task: Task = storage.get_json(&hash).await.unwrap();

        assert_eq!(task.header().object_id(), loaded_task.header().object_id());
        assert_eq!(task.title(), loaded_task.title());

        // 2. Test Artifact Storage
        let content = b"Hello Libra Artifact";
        let artifact = storage.put_artifact(content).await.unwrap();

        assert_eq!(artifact.store(), "libra");
        assert!(!artifact.key().is_empty());

        // Verify retrieval using standard storage get (simulating Artifact resolution)
        let key_hash = crate::internal::object_format::parse_repo_oid(artifact.key()).unwrap();
        let (data, _) = storage.get(&key_hash).await.unwrap();
        assert_eq!(data, content);
    }
}
