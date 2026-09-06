//! State-facet adapters used by workspace snapshots.
//!
//! The adapters keep capture/restore ownership next to the state they know
//! how to serialize.  They do not implement a restore engine: OL-10 owns the
//! ordering, journal, and rollback policy.  In particular, the raw index
//! facet persists the original bytes rather than reconstructing an Index, so
//! intent-to-add, skip-worktree, assume-unchanged, and stat bits survive.

use git_internal::{
    hash::ObjectHash,
    internal::object::types::ObjectType,
};
use serde_json::json;

use super::{
    FacetCapture, FacetCaptureCtx, FacetDiff, FacetError, FacetName, FacetRegistry,
    FacetRestoreCtx, RestorePolicy,
};
use super::facet::StateFacet;
use crate::{
    internal::{
        sequencer::{self, SequenceKind, SequenceState},
        sparse::SparseViewStore,
        worktree_scope::{RequestScope, WorktreeScope},
    },
    utils::{
        atomic_write::write_atomic,
        client_storage::ClientStorage,
    },
};

const FACET_SCHEMA_VERSION: u32 = 1;

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
    fn name(&self) -> FacetName { FacetName::from("index") }
    fn schema_version(&self) -> u32 { FACET_SCHEMA_VERSION }
    fn restore_policy(&self) -> RestorePolicy { RestorePolicy::AutoRestore }

    fn capture(&self, _ctx: &FacetCaptureCtx) -> Result<FacetCapture, FacetError> {
        let bytes = std::fs::read(self.scope.gitdir.join("index"))
            .map_err(|error| FacetError::Capture(error.to_string()))?;
        let oid = ObjectHash::from_type_and_data(ObjectType::Blob, &bytes);
        self.storage
            .put(&oid, &bytes, ObjectType::Blob)
            .map_err(|error| FacetError::Capture(error.to_string()))?;
        Ok(FacetCapture {
            facet: self.name(),
            schema_version: FACET_SCHEMA_VERSION,
            payload_oid: Some(oid),
            meta: json!({"byte_exact": true, "length": bytes.len()}),
        })
    }

    fn validate(&self, capture: &FacetCapture) -> Result<(), FacetError> {
        if capture.payload_oid.is_none() {
            return Err(FacetError::Validation("index facet has no payload".to_string()));
        }
        Ok(())
    }

    fn restore(&self, capture: &FacetCapture, _ctx: &mut FacetRestoreCtx) -> Result<(), FacetError> {
        let oid = capture.payload_oid.ok_or_else(|| FacetError::Restore("index payload is missing".to_string()))?;
        let bytes = self.storage.get(&oid).map_err(|error| FacetError::Restore(error.to_string()))?;
        write_atomic(&self.scope.gitdir.join("index"), &bytes, true)
            .map_err(|error| FacetError::Restore(error.to_string()))
    }

    fn diff(&self, from: &FacetCapture, to: &FacetCapture) -> Result<FacetDiff, FacetError> {
        Ok(FacetDiff { changes: json!({"from": from.payload_oid, "to": to.payload_oid}) })
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
    pub fn new(scope: RequestScope, storage: ClientStorage) -> Self { Self { scope, storage } }
}

impl StateFacet for SequencerFacet {
    fn name(&self) -> FacetName { FacetName::from("sequencer") }
    fn schema_version(&self) -> u32 { FACET_SCHEMA_VERSION }
    fn restore_policy(&self) -> RestorePolicy { RestorePolicy::AutoRestore }

    fn capture(&self, _ctx: &FacetCaptureCtx) -> Result<FacetCapture, FacetError> {
        let scope = self.scope.clone();
        let state = run_async(async move { sequencer::load_for_scope(&scope.scope).await })
            .map_err(FacetError::Capture)?
            .map_err(FacetError::Capture)?;
        let value = state.map(sequence_to_json).unwrap_or_else(|| json!({"present": false}));
        let bytes = serde_json::to_vec(&value).map_err(|error| FacetError::Capture(error.to_string()))?;
        let oid = ObjectHash::from_type_and_data(ObjectType::Blob, &bytes);
        self.storage.put(&oid, &bytes, ObjectType::Blob).map_err(|error| FacetError::Capture(error.to_string()))?;
        Ok(FacetCapture { facet: self.name(), schema_version: FACET_SCHEMA_VERSION, payload_oid: Some(oid), meta: json!({"present": value.get("present").is_none_or(|v| v != false)}) })
    }

    fn validate(&self, capture: &FacetCapture) -> Result<(), FacetError> {
        capture.payload_oid.ok_or_else(|| FacetError::Validation("sequencer payload is missing".to_string())).map(|_| ())
    }

    fn restore(&self, capture: &FacetCapture, _ctx: &mut FacetRestoreCtx) -> Result<(), FacetError> {
        let oid = capture.payload_oid.ok_or_else(|| FacetError::Restore("sequencer payload is missing".to_string()))?;
        let bytes = self.storage.get(&oid).map_err(|error| FacetError::Restore(error.to_string()))?;
        let value: serde_json::Value = serde_json::from_slice(&bytes).map_err(|error| FacetError::Restore(error.to_string()))?;
        if value.get("present").and_then(serde_json::Value::as_bool) == Some(false) { return Ok(()); }
        let state = sequence_from_json(&value).map_err(FacetError::Restore)?;
        let scope = self.scope.clone();
        run_async(async move {
            let _pin = WorktreeScope::override_scope(scope.workdir.clone());
            sequencer::save(&state).await
        }).map_err(FacetError::Restore)?
            .map_err(FacetError::Restore)?;
        Ok(())
    }

    fn diff(&self, from: &FacetCapture, to: &FacetCapture) -> Result<FacetDiff, FacetError> {
        Ok(FacetDiff { changes: json!({"from": from.payload_oid, "to": to.payload_oid}) })
    }
    fn roots(&self, capture: &FacetCapture) -> Vec<ObjectHash> { capture.payload_oid.into_iter().collect() }
}

#[derive(Clone)]
pub struct SparseFacet {
    scope: RequestScope,
    storage: ClientStorage,
}

impl SparseFacet {
    pub fn new(scope: RequestScope, storage: ClientStorage) -> Self { Self { scope, storage } }
}

impl StateFacet for SparseFacet {
    fn name(&self) -> FacetName { FacetName::from("sparse") }
    fn schema_version(&self) -> u32 { FACET_SCHEMA_VERSION }
    fn restore_policy(&self) -> RestorePolicy { RestorePolicy::Rebuild }

    fn capture(&self, _ctx: &FacetCaptureCtx) -> Result<FacetCapture, FacetError> {
        let scope = self.scope.clone();
        let value = run_async(async move {
            let patterns = SparseViewStore::list(&scope.scope).await?;
            let enabled = SparseViewStore::is_enabled(&scope.scope).await;
            Ok::<_, String>(json!({"enabled": enabled, "patterns": patterns}))
        }).map_err(FacetError::Capture)?
            .map_err(FacetError::Capture)?;
        let bytes = serde_json::to_vec(&value).map_err(|error| FacetError::Capture(error.to_string()))?;
        let oid = ObjectHash::from_type_and_data(ObjectType::Blob, &bytes);
        self.storage.put(&oid, &bytes, ObjectType::Blob).map_err(|error| FacetError::Capture(error.to_string()))?;
        Ok(FacetCapture { facet: self.name(), schema_version: FACET_SCHEMA_VERSION, payload_oid: Some(oid), meta: value })
    }

    fn validate(&self, capture: &FacetCapture) -> Result<(), FacetError> { capture.payload_oid.ok_or_else(|| FacetError::Validation("sparse payload is missing".to_string())).map(|_| ()) }
    fn restore(&self, capture: &FacetCapture, _ctx: &mut FacetRestoreCtx) -> Result<(), FacetError> {
        let oid = capture.payload_oid.ok_or_else(|| FacetError::Restore("sparse payload is missing".to_string()))?;
        let value: serde_json::Value = serde_json::from_slice(&self.storage.get(&oid).map_err(|error| FacetError::Restore(error.to_string()))?).map_err(|error| FacetError::Restore(error.to_string()))?;
        let patterns = value.get("patterns").and_then(serde_json::Value::as_array).ok_or_else(|| FacetError::Restore("sparse patterns are missing".to_string()))?.iter().filter_map(serde_json::Value::as_str).map(str::to_string).collect::<Vec<_>>();
        let scope = self.scope.clone();
        run_async(async move {
            let _pin = WorktreeScope::override_scope(scope.workdir.clone());
            SparseViewStore::replace(&scope.scope, &patterns).await?;
            if value.get("enabled").and_then(serde_json::Value::as_bool) == Some(false) {
                SparseViewStore::disable(&scope.scope).await?;
            }
            Ok::<_, String>(())
        }).map_err(FacetError::Restore)?
            .map_err(FacetError::Restore)?;
        Ok(())
    }
    fn diff(&self, from: &FacetCapture, to: &FacetCapture) -> Result<FacetDiff, FacetError> { Ok(FacetDiff { changes: json!({"from": from.payload_oid, "to": to.payload_oid}) }) }
    fn roots(&self, capture: &FacetCapture) -> Vec<ObjectHash> { capture.payload_oid.into_iter().collect() }
}

/// Register exactly the three OL-07 mutable-state facets for a worktree.
pub fn registry_for_scope(scope: RequestScope, storage: ClientStorage) -> Result<FacetRegistry, FacetError> {
    let mut registry = FacetRegistry::new();
    registry.register(Box::new(RawIndexFacet::new(scope.clone(), storage.clone())))?;
    registry.register(Box::new(SequencerFacet::new(scope.clone(), storage.clone())))?;
    registry.register(Box::new(SparseFacet::new(scope, storage)))?;
    Ok(registry)
}

fn sequence_to_json(state: SequenceState) -> serde_json::Value {
    json!({"present": true, "kind": state.kind.as_str(), "head_name": state.head_name, "head_orig": state.head_orig, "current_oid": state.current_oid, "todo": state.todo, "payload": state.payload})
}

fn sequence_from_json(value: &serde_json::Value) -> Result<SequenceState, String> {
    let kind = match value.get("kind").and_then(serde_json::Value::as_str).ok_or("sequencer kind missing")? {
        "merge" => SequenceKind::Merge,
        "revert" => SequenceKind::Revert,
        "cherry_pick" => SequenceKind::CherryPick,
        "rebase" => SequenceKind::Rebase,
        other => return Err(format!("unknown sequencer kind '{other}'")),
    };
    Ok(SequenceState {
        kind,
        head_name: string_field(value, "head_name")?,
        head_orig: string_field(value, "head_orig")?,
        current_oid: string_field(value, "current_oid")?,
        todo: value.get("todo").and_then(serde_json::Value::as_array).ok_or("sequencer todo missing")?.iter().filter_map(serde_json::Value::as_str).map(str::to_string).collect(),
        payload: string_field(value, "payload")?,
    })
}

fn string_field(value: &serde_json::Value, key: &str) -> Result<String, String> { value.get(key).and_then(serde_json::Value::as_str).map(str::to_string).ok_or_else(|| format!("sequencer field '{key}' missing")) }

fn run_async<T: Send + 'static>(future: impl std::future::Future<Output = T> + Send + 'static) -> Result<T, String> {
    std::thread::spawn(move || {
        tokio::runtime::Builder::new_current_thread().enable_all().build().map_err(|error| error.to_string()).and_then(|runtime| Ok(runtime.block_on(future)))
    }).join().map_err(|_| "facet worker panicked".to_string())?
}
