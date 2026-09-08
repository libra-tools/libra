//! State-facet adapters used by workspace snapshots.
//!
//! The adapters keep capture/restore ownership next to the state they know
//! how to serialize.  They do not implement a restore engine: OL-10 owns the
//! ordering, journal, and rollback policy.  In particular, the raw index
//! facet persists the original bytes rather than reconstructing an Index, so
//! intent-to-add, skip-worktree, assume-unchanged, and stat bits survive.

use std::io::Read;

use git_internal::{hash::ObjectHash, internal::object::types::ObjectType};
use serde_json::json;

use super::{
    FacetCapture, FacetCaptureCtx, FacetDiff, FacetError, FacetName, FacetRegistry,
    FacetRestoreCtx, RestorePolicy, facet::StateFacet,
};
use crate::{
    internal::{sequencer, sparse, worktree_scope::RequestScope},
    utils::{atomic_write::write_atomic, client_storage::ClientStorage},
};

const FACET_SCHEMA_VERSION: u32 = 1;
const MAX_RAW_INDEX_BYTES: u64 = 512 * 1024 * 1024;

#[derive(Clone)]
pub struct RawIndexFacet {
    scope: RequestScope,
    storage: ClientStorage,
}

impl RawIndexFacet {
    pub fn new(scope: RequestScope, storage: ClientStorage) -> Self {
        Self { scope, storage }
    }
}

impl StateFacet for RawIndexFacet {
    fn name(&self) -> FacetName {
        FacetName::from("index")
    }
    fn schema_version(&self) -> u32 {
        FACET_SCHEMA_VERSION
    }
    fn restore_policy(&self) -> RestorePolicy {
        RestorePolicy::AutoRestore
    }

    fn capture(&self, _ctx: &FacetCaptureCtx) -> Result<FacetCapture, FacetError> {
        let (present, bytes) = match std::fs::File::open(self.scope.gitdir.join("index")) {
            Ok(file) => {
                let mut bytes = Vec::new();
                file.take(MAX_RAW_INDEX_BYTES.saturating_add(1))
                    .read_to_end(&mut bytes)
                    .map_err(|error| FacetError::Capture(error.to_string()))?;
                if bytes.len() as u64 > MAX_RAW_INDEX_BYTES {
                    return Err(FacetError::Capture(
                        "raw index exceeds snapshot byte budget".to_string(),
                    ));
                }
                (true, bytes)
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => (false, Vec::new()),
            Err(error) => return Err(FacetError::Capture(error.to_string())),
        };
        let oid = ObjectHash::from_type_and_data(ObjectType::Blob, &bytes);
        self.storage
            .put(&oid, &bytes, ObjectType::Blob)
            .map_err(|error| FacetError::Capture(error.to_string()))?;
        Ok(FacetCapture {
            facet: self.name(),
            schema_version: FACET_SCHEMA_VERSION,
            payload_oid: Some(oid),
            meta: json!({"byte_exact": true, "length": bytes.len(), "present": present}),
        })
    }

    fn validate(&self, capture: &FacetCapture) -> Result<(), FacetError> {
        if capture.payload_oid.is_none() {
            return Err(FacetError::Validation(
                "index facet has no payload".to_string(),
            ));
        }
        Ok(())
    }

    fn restore(
        &self,
        capture: &FacetCapture,
        _ctx: &mut FacetRestoreCtx,
    ) -> Result<(), FacetError> {
        let oid = capture
            .payload_oid
            .ok_or_else(|| FacetError::Restore("index payload is missing".to_string()))?;
        let bytes = self
            .storage
            .get(&oid)
            .map_err(|error| FacetError::Restore(error.to_string()))?;
        if capture
            .meta
            .get("present")
            .and_then(serde_json::Value::as_bool)
            == Some(false)
        {
            match std::fs::remove_file(self.scope.gitdir.join("index")) {
                Ok(()) => {}
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
                Err(error) => return Err(FacetError::Restore(error.to_string())),
            }
            return Ok(());
        }
        write_atomic(&self.scope.gitdir.join("index"), &bytes, true)
            .map_err(|error| FacetError::Restore(error.to_string()))
    }

    fn diff(&self, from: &FacetCapture, to: &FacetCapture) -> Result<FacetDiff, FacetError> {
        Ok(FacetDiff {
            changes: json!({"from": from.payload_oid, "to": to.payload_oid}),
        })
    }

    fn roots(&self, capture: &FacetCapture) -> Vec<ObjectHash> {
        capture.payload_oid.into_iter().collect()
    }
}

#[derive(Clone)]
pub struct SequencerFacet {
    scope: RequestScope,
    storage: ClientStorage,
}

impl SequencerFacet {
    pub fn new(scope: RequestScope, storage: ClientStorage) -> Self {
        Self { scope, storage }
    }
}

impl StateFacet for SequencerFacet {
    fn name(&self) -> FacetName {
        FacetName::from("sequencer")
    }
    fn schema_version(&self) -> u32 {
        FACET_SCHEMA_VERSION
    }
    fn restore_policy(&self) -> RestorePolicy {
        RestorePolicy::AutoRestore
    }

    fn capture(&self, _ctx: &FacetCaptureCtx) -> Result<FacetCapture, FacetError> {
        let scope = self.scope.clone();
        let storage = scope.storage.clone();
        let state =
            run_async(
                async move { sequencer::load_snapshot_for_storage(&storage, &scope.scope).await },
            )
            .map_err(FacetError::Capture)?
            .map_err(FacetError::Capture)?;
        let value = state.unwrap_or_else(|| json!({"present": false}));
        let bytes =
            serde_json::to_vec(&value).map_err(|error| FacetError::Capture(error.to_string()))?;
        let oid = ObjectHash::from_type_and_data(ObjectType::Blob, &bytes);
        self.storage
            .put(&oid, &bytes, ObjectType::Blob)
            .map_err(|error| FacetError::Capture(error.to_string()))?;
        Ok(FacetCapture {
            facet: self.name(),
            schema_version: FACET_SCHEMA_VERSION,
            payload_oid: Some(oid),
            meta: json!({"present": value.get("present").is_none_or(|v| v != false)}),
        })
    }

    fn validate(&self, capture: &FacetCapture) -> Result<(), FacetError> {
        capture
            .payload_oid
            .ok_or_else(|| FacetError::Validation("sequencer payload is missing".to_string()))
            .map(|_| ())
    }

    fn restore(
        &self,
        capture: &FacetCapture,
        _ctx: &mut FacetRestoreCtx,
    ) -> Result<(), FacetError> {
        let oid = capture
            .payload_oid
            .ok_or_else(|| FacetError::Restore("sequencer payload is missing".to_string()))?;
        let bytes = self
            .storage
            .get(&oid)
            .map_err(|error| FacetError::Restore(error.to_string()))?;
        let value: serde_json::Value = serde_json::from_slice(&bytes)
            .map_err(|error| FacetError::Restore(error.to_string()))?;
        if value.get("present").and_then(serde_json::Value::as_bool) == Some(false) {
            let scope = self.scope.clone();
            run_async(async move {
                sequencer::clear_snapshot_for_storage(&scope.storage, &scope.scope).await
            })
            .map_err(FacetError::Restore)?
            .map_err(FacetError::Restore)?;
            return Ok(());
        }
        let scope = self.scope.clone();
        run_async(async move {
            sequencer::restore_snapshot_for_storage(&scope.storage, &scope.scope, &value).await
        })
        .map_err(FacetError::Restore)?
        .map_err(FacetError::Restore)?;
        Ok(())
    }

    fn diff(&self, from: &FacetCapture, to: &FacetCapture) -> Result<FacetDiff, FacetError> {
        Ok(FacetDiff {
            changes: json!({"from": from.payload_oid, "to": to.payload_oid}),
        })
    }
    fn roots(&self, capture: &FacetCapture) -> Vec<ObjectHash> {
        capture.payload_oid.into_iter().collect()
    }
}

#[derive(Clone)]
pub struct SparseFacet {
    scope: RequestScope,
    storage: ClientStorage,
}

impl SparseFacet {
    pub fn new(scope: RequestScope, storage: ClientStorage) -> Self {
        Self { scope, storage }
    }
}

impl StateFacet for SparseFacet {
    fn name(&self) -> FacetName {
        FacetName::from("sparse")
    }
    fn schema_version(&self) -> u32 {
        FACET_SCHEMA_VERSION
    }
    fn restore_policy(&self) -> RestorePolicy {
        RestorePolicy::Rebuild
    }

    fn capture(&self, _ctx: &FacetCaptureCtx) -> Result<FacetCapture, FacetError> {
        let scope = self.scope.clone();
        let storage = scope.storage.clone();
        let value = run_async(async move {
            let (patterns, enabled) = sparse::snapshot_for_storage(&storage, &scope.scope).await?;
            Ok::<_, String>(json!({"enabled": enabled, "patterns": patterns}))
        })
        .map_err(FacetError::Capture)?
        .map_err(FacetError::Capture)?;
        let bytes =
            serde_json::to_vec(&value).map_err(|error| FacetError::Capture(error.to_string()))?;
        let oid = ObjectHash::from_type_and_data(ObjectType::Blob, &bytes);
        self.storage
            .put(&oid, &bytes, ObjectType::Blob)
            .map_err(|error| FacetError::Capture(error.to_string()))?;
        Ok(FacetCapture {
            facet: self.name(),
            schema_version: FACET_SCHEMA_VERSION,
            payload_oid: Some(oid),
            meta: value,
        })
    }

    fn validate(&self, capture: &FacetCapture) -> Result<(), FacetError> {
        capture
            .payload_oid
            .ok_or_else(|| FacetError::Validation("sparse payload is missing".to_string()))
            .map(|_| ())
    }
    fn restore(
        &self,
        capture: &FacetCapture,
        _ctx: &mut FacetRestoreCtx,
    ) -> Result<(), FacetError> {
        let oid = capture
            .payload_oid
            .ok_or_else(|| FacetError::Restore("sparse payload is missing".to_string()))?;
        let value: serde_json::Value = serde_json::from_slice(
            &self
                .storage
                .get(&oid)
                .map_err(|error| FacetError::Restore(error.to_string()))?,
        )
        .map_err(|error| FacetError::Restore(error.to_string()))?;
        let patterns = value
            .get("patterns")
            .and_then(serde_json::Value::as_array)
            .ok_or_else(|| FacetError::Restore("sparse patterns are missing".to_string()))?
            .iter()
            .filter_map(serde_json::Value::as_str)
            .map(str::to_string)
            .collect::<Vec<_>>();
        let scope = self.scope.clone();
        run_async(async move {
            let enabled = value
                .get("enabled")
                .and_then(serde_json::Value::as_bool)
                .unwrap_or(false);
            sparse::restore_for_storage(&scope.storage, &scope.scope, &patterns, enabled).await
        })
        .map_err(FacetError::Restore)?
        .map_err(FacetError::Restore)?;
        Ok(())
    }
    fn diff(&self, from: &FacetCapture, to: &FacetCapture) -> Result<FacetDiff, FacetError> {
        Ok(FacetDiff {
            changes: json!({"from": from.payload_oid, "to": to.payload_oid}),
        })
    }
    fn roots(&self, capture: &FacetCapture) -> Vec<ObjectHash> {
        capture.payload_oid.into_iter().collect()
    }
}

/// Register exactly the three OL-07 mutable-state facets for a worktree.
pub fn registry_for_scope(
    scope: RequestScope,
    storage: ClientStorage,
) -> Result<FacetRegistry, FacetError> {
    let mut registry = FacetRegistry::new();
    registry.register(Box::new(RawIndexFacet::new(scope.clone(), storage.clone())))?;
    registry.register(Box::new(SequencerFacet::new(
        scope.clone(),
        storage.clone(),
    )))?;
    registry.register(Box::new(SparseFacet::new(scope, storage)))?;
    Ok(registry)
}

fn run_async<T: Send + 'static>(
    future: impl std::future::Future<Output = T> + Send + 'static,
) -> Result<T, String> {
    std::thread::spawn(move || {
        tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .map_err(|error| error.to_string())
            .map(|runtime| runtime.block_on(future))
    })
    .join()
    .map_err(|_| "facet worker panicked".to_string())?
}
