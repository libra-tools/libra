//! Shared checkpoint role closure and typed, bounded object reads.
//!
//! Fresh command readers and persisted-input revalidation use this owner.
//! This module has no command/error-code dependency and never reads reasoning
//! artifact bodies while establishing an ordinary role.

use std::{
    cell::RefCell,
    marker::PhantomData,
    path::{Path, PathBuf},
    rc::Rc,
    sync::Arc,
    time::{Duration, Instant},
};

use git_internal::{
    hash::{HashKind, ObjectHash},
    internal::object::{
        ObjectTrait,
        tree::{Tree, TreeItem, TreeItemMode},
        types::ObjectType,
    },
};
use thiserror::Error;

use crate::internal::ai::observed_agents::reasoning::{
    ReasoningProvider, ReasoningSourceKind, artifact_locator_is_valid,
};

type ReaderResult<T> = Result<T, CheckpointReaderError>;

#[derive(Debug, Error)]
pub(crate) enum CheckpointReaderError {
    #[error(
        "checkpoint catalog row is inconsistent; run `libra agent doctor` to inspect the store"
    )]
    RoleProof,
    #[error("{0}")]
    InvalidSavedSpec(String),
    #[error(
        "checkpoint catalog is unavailable; retry after the repository writer finishes, or run `libra agent doctor`"
    )]
    CatalogUnavailable,
    #[error(
        "checkpoint layout cannot be proved; run `libra agent doctor` and select a captured checkpoint"
    )]
    UnknownLayout,
    #[error(
        "checkpoint input exceeds its work budget or was cancelled; retry with a smaller checkpoint"
    )]
    Budget,
    #[error("checkpoint object identity or type is inconsistent; run `libra agent doctor`")]
    ObjectIntegrity,
}

impl CheckpointReaderError {
    fn fatal(message: impl Into<String>) -> Self {
        Self::InvalidSavedSpec(message.into())
    }
}

fn role_proof_error() -> CheckpointReaderError {
    CheckpointReaderError::RoleProof
}

/// An absolute caller deadline and the caller's actual cancellation check.
/// Cloning this budget never starts a new timeout.
#[derive(Clone)]
pub(crate) struct CheckpointReadBudget {
    deadline: Option<Instant>,
    cancelled: Arc<dyn Fn() -> bool + Send + Sync>,
    #[cfg(test)]
    catalog_observer: Option<Arc<dyn Fn(CatalogTestPhase) + Send + Sync>>,
}

impl CheckpointReadBudget {
    #[allow(
        dead_code,
        reason = "absolute caller budget is handed to the dependent scoped consumer card"
    )]
    pub(crate) fn new(deadline: Instant, cancelled: Arc<dyn Fn() -> bool + Send + Sync>) -> Self {
        Self {
            deadline: Some(deadline),
            cancelled,
            #[cfg(test)]
            catalog_observer: None,
        }
    }

    fn fresh() -> Self {
        Self {
            deadline: None,
            cancelled: Arc::new(|| false),
            #[cfg(test)]
            catalog_observer: None,
        }
    }

    #[cfg(test)]
    fn observe_catalog(&self, phase: CatalogTestPhase) {
        if let Some(observer) = &self.catalog_observer {
            observer(phase);
        }
    }

    pub(crate) fn check(&self) -> ReaderResult<()> {
        if (self.cancelled)() || self.deadline.is_some_and(|end| Instant::now() >= end) {
            Err(CheckpointReaderError::Budget)
        } else {
            Ok(())
        }
    }
}

#[cfg(test)]
#[derive(Clone, Copy)]
enum CatalogTestPhase {
    Progress,
    ConnectionCloseFinished,
    PoolCloseFinished,
}

thread_local! {
    // Only the synchronous shared reader installs this scope. It never spans
    // an await; the !Send guard restores both thread-local values on every exit.
    static READ_SCOPE: RefCell<Option<CheckpointReadBudget>> = const { RefCell::new(None) };
}

struct SynchronousReadScope {
    previous_budget: Option<CheckpointReadBudget>,
    previous_kind: HashKind,
    _same_thread: PhantomData<Rc<()>>,
}

impl SynchronousReadScope {
    fn enter(kind: HashKind, budget: &CheckpointReadBudget) -> ReaderResult<Self> {
        budget.check()?;
        let previous_budget = READ_SCOPE.with(|scope| scope.replace(Some(budget.clone())));
        let previous_kind = git_internal::hash::get_hash_kind();
        git_internal::hash::set_hash_kind(kind);
        Ok(Self {
            previous_budget,
            previous_kind,
            _same_thread: PhantomData,
        })
    }
}

impl Drop for SynchronousReadScope {
    fn drop(&mut self) {
        git_internal::hash::set_hash_kind(self.previous_kind);
        READ_SCOPE.with(|scope| {
            scope.replace(self.previous_budget.take());
        });
    }
}

fn check_read_budget() -> ReaderResult<()> {
    // A caller's cancellation callback may itself inspect reader state. Do
    // not hold the RefCell borrow while invoking that callback.
    let budget = READ_SCOPE.with(|scope| scope.borrow().clone());
    match budget {
        Some(budget) => budget.check(),
        None => Ok(()),
    }
}

/// The artifact manifest is a metadata-only contract shared by every reader.
/// A missing size supports the original unversioned format; readers still
/// check the actual object size and digest before exporting or publishing it.
#[derive(serde::Deserialize, serde::Serialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct ReasoningArtifactMetadata {
    pub(crate) path: String,
    pub(crate) oid: String,
    pub(crate) sha256: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) byte_len: Option<u64>,
    pub(crate) locator: String,
    pub(crate) provider: ReasoningProvider,
    pub(crate) source_kind: Option<ReasoningSourceKind>,
    pub(crate) availability: ArtifactAvailability,
    pub(crate) decrypt_capability: String,
}

#[derive(serde::Deserialize, serde::Serialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum ArtifactAvailability {
    EncryptedUnavailable,
    OpaqueArchived,
}

/// Errors deliberately contain no untrusted field value or parser detail.
fn parse_reasoning_artifact_metadata(
    manifest: &serde_json::Value,
) -> Result<Vec<ReasoningArtifactMetadata>, &'static str> {
    use crate::internal::ai::traces::{
        REASONING_ARTIFACT_MANIFEST_MAX_BYTES, REASONING_ARTIFACT_MAX_ENTRIES,
        REASONING_ARTIFACT_TOTAL_MAX_BYTES,
    };
    const INVALID: &str = "reasoning artifact metadata is invalid";
    let Some(value) = manifest.get("reasoning_artifacts") else {
        return Ok(Vec::new());
    };
    let entries = value.as_array().ok_or(INVALID)?;
    if entries.len() > REASONING_ARTIFACT_MAX_ENTRIES
        || serde_json::to_vec_pretty(&serde_json::json!({"reasoning_artifacts": value}))
            .map_err(|_| INVALID)?
            .len()
            > REASONING_ARTIFACT_MANIFEST_MAX_BYTES
    {
        return Err(INVALID);
    }
    let metadata: Vec<ReasoningArtifactMetadata> =
        serde_json::from_value(value.clone()).map_err(|_| INVALID)?;
    let mut locators = std::collections::BTreeSet::new();
    let mut digests = std::collections::BTreeMap::new();
    let mut objects = std::collections::BTreeMap::new();
    let mut declared_total = 0u64;
    for (entry, original) in metadata.iter().zip(entries) {
        if entry.sha256.len() != 64
            || !entry
                .sha256
                .bytes()
                .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
            || entry.path != format!("reasoning/encrypted/{}", entry.sha256)
            || crate::internal::object_format::parse_repo_oid(&entry.oid).is_err()
            || !artifact_locator_is_valid(entry.provider, entry.source_kind, &entry.locator)
            || entry.decrypt_capability != "none"
            || !locators.insert(&entry.locator)
            || (original.get("byte_len").is_some() && entry.byte_len.is_none())
        {
            return Err(INVALID);
        }
        if digests
            .insert(&entry.sha256, (&entry.oid, entry.byte_len))
            .is_some_and(|previous| previous != (&entry.oid, entry.byte_len))
            || objects
                .insert(&entry.oid, (&entry.sha256, entry.byte_len))
                .is_some_and(|previous| previous != (&entry.sha256, entry.byte_len))
        {
            return Err(INVALID);
        }
        if let Some(len) = entry.byte_len {
            declared_total = declared_total.checked_add(len).ok_or(INVALID)?;
            if declared_total > REASONING_ARTIFACT_TOTAL_MAX_BYTES {
                return Err(INVALID);
            }
        }
    }
    Ok(metadata)
}

pub(crate) fn reasoning_artifact_metadata(
    manifest: &serde_json::Value,
) -> ReaderResult<Vec<ReasoningArtifactMetadata>> {
    parse_reasoning_artifact_metadata(manifest).map_err(|_| role_proof_error())
}

/// Establish metadata/tree closure without reading ciphertext bodies.
pub(crate) fn validate_reasoning_artifact_tree(
    storage: &Path,
    inner: &Tree,
    metadata: &[ReasoningArtifactMetadata],
) -> Result<(), CheckpointReaderError> {
    let has_reasoning = tree_entry(inner, "reasoning").is_some();
    if metadata.is_empty() {
        return if has_reasoning {
            Err(role_proof_error())
        } else {
            Ok(())
        };
    }
    let reasoning = subtree(storage, inner, "reasoning").map_err(|_| role_proof_error())?;
    if reasoning.tree_items.len() != 1 {
        return Err(role_proof_error());
    }
    let encrypted = subtree(storage, &reasoning, "encrypted").map_err(|_| role_proof_error())?;
    let declared = metadata
        .iter()
        .map(|entry| (entry.sha256.as_str(), entry.oid.as_str()))
        .collect::<std::collections::BTreeMap<_, _>>();
    let mut names = std::collections::BTreeSet::new();
    if encrypted.tree_items.len() != declared.len()
        || encrypted.tree_items.iter().any(|item| {
            item.mode != TreeItemMode::Blob
                || !names.insert(item.name.as_str())
                || declared.get(item.name.as_str()).copied() != Some(item.id.to_string().as_str())
        })
    {
        return Err(role_proof_error());
    }
    Ok(())
}

/// Discover ciphertext object identities from the closed tree layout before
/// reading manifest.json. An unknown reasoning layout is never ordinary input.
pub(crate) fn reasoning_artifact_tree_oids(
    storage: &Path,
    inner: &Tree,
) -> ReaderResult<std::collections::BTreeSet<String>> {
    let Some(item) = tree_entry(inner, "reasoning") else {
        return Ok(std::collections::BTreeSet::new());
    };
    if item.mode != TreeItemMode::Tree {
        return Err(role_proof_error());
    }
    let reasoning = subtree(storage, inner, "reasoning").map_err(|_| role_proof_error())?;
    if reasoning.tree_items.len() != 1 {
        return Err(role_proof_error());
    }
    let encrypted = subtree(storage, &reasoning, "encrypted").map_err(|_| role_proof_error())?;
    if encrypted.tree_items.is_empty()
        || encrypted.tree_items.len() > crate::internal::ai::traces::REASONING_ARTIFACT_MAX_ENTRIES
    {
        return Err(role_proof_error());
    }
    let mut oids = std::collections::BTreeSet::new();
    for item in &encrypted.tree_items {
        if item.mode != TreeItemMode::Blob
            || item.name.len() != 64
            || !item
                .name
                .bytes()
                .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
        {
            return Err(role_proof_error());
        }
        oids.insert(item.id.to_string());
    }
    Ok(oids)
}

pub(crate) fn reject_artifact_alias(
    oid: &ObjectHash,
    artifacts: &std::collections::BTreeSet<String>,
) -> ReaderResult<()> {
    if artifacts.contains(&oid.to_string()) {
        Err(role_proof_error())
    } else {
        Ok(())
    }
}

// Ordinary preflight budgets apply to expanded paths, including OID aliases.
pub(crate) const CHECKPOINT_ORDINARY_MAX_ENTRIES: usize = 8192;
pub(crate) const CHECKPOINT_ORDINARY_MAX_FILES: usize = 4096;
pub(crate) const CHECKPOINT_ORDINARY_MAX_TREES: usize = 4096;
pub(crate) const CHECKPOINT_ORDINARY_MAX_DEPTH: usize = 64;
pub(crate) const CHECKPOINT_ORDINARY_MAX_PATH_BYTES: usize = 4096;
pub(crate) const CHECKPOINT_ORDINARY_MAX_TOTAL_PATH_BYTES: usize = 8 * 1024 * 1024;
const CANONICAL_REASONING_ARTIFACT_PATH_BYTES: usize = "reasoning/encrypted/".len() + 64;
// The already decoded selected leaf is <=16 MiB; all additional ordinary
// tree decodes together are <=16 MiB, including repeated OIDs. Thus the
// ordinary preflight's decoded tree bytes are bounded by 32 MiB in total.
pub(crate) const CHECKPOINT_ORDINARY_MAX_CHILD_TREE_BYTES: u64 = 16 * 1024 * 1024;

#[derive(Clone, Copy)]
pub(crate) struct CheckpointTraversalLimits {
    pub(crate) entries: usize,
    pub(crate) files: usize,
    pub(crate) trees: usize,
    pub(crate) depth: usize,
    pub(crate) path_bytes: usize,
    pub(crate) total_path_bytes: usize,
    pub(crate) child_tree_bytes: u64,
}

impl Default for CheckpointTraversalLimits {
    fn default() -> Self {
        Self {
            entries: CHECKPOINT_ORDINARY_MAX_ENTRIES,
            files: CHECKPOINT_ORDINARY_MAX_FILES,
            trees: CHECKPOINT_ORDINARY_MAX_TREES,
            depth: CHECKPOINT_ORDINARY_MAX_DEPTH,
            path_bytes: CHECKPOINT_ORDINARY_MAX_PATH_BYTES,
            total_path_bytes: CHECKPOINT_ORDINARY_MAX_TOTAL_PATH_BYTES,
            child_tree_bytes: CHECKPOINT_ORDINARY_MAX_CHILD_TREE_BYTES,
        }
    }
}

pub(crate) struct CheckpointOrdinaryTraversal<'a> {
    artifacts: &'a std::collections::BTreeSet<String>,
    limits: CheckpointTraversalLimits,
    entries: usize,
    trees: usize,
    path_bytes: usize,
    child_tree_bytes: u64,
    pending: Vec<(String, ObjectHash, usize)>,
    files: Vec<crate::internal::ai::checkpoint_input::CheckpointInputFile>,
}

impl CheckpointOrdinaryTraversal<'_> {
    fn visit_tree(&mut self, prefix: &str, tree: &Tree, depth: usize) -> ReaderResult<()> {
        use crate::internal::ai::checkpoint_input::CheckpointInputFile;

        check_read_budget()?;

        // Charge every expanded entry, not unique OIDs. A shared-child DAG
        // still creates distinct paths and must not multiply work unchecked.
        self.entries = self
            .entries
            .checked_add(tree.tree_items.len())
            .filter(|entries| *entries <= self.limits.entries)
            .ok_or(CheckpointReaderError::Budget)?;
        for item in &tree.tree_items {
            check_read_budget()?;
            if prefix.is_empty() && item.name == "reasoning" {
                continue;
            }
            reject_artifact_alias(&item.id, self.artifacts)?;
            let next_depth = depth
                .checked_add(1)
                .filter(|depth| *depth <= self.limits.depth)
                .ok_or(CheckpointReaderError::Budget)?;
            let path_len = prefix
                .len()
                .checked_add(usize::from(!prefix.is_empty()))
                .and_then(|len| len.checked_add(item.name.len()))
                .filter(|len| *len <= self.limits.path_bytes)
                .ok_or(CheckpointReaderError::Budget)?;
            self.path_bytes = self
                .path_bytes
                .checked_add(path_len)
                .filter(|bytes| *bytes <= self.limits.total_path_bytes)
                .ok_or(CheckpointReaderError::Budget)?;
            // All allocation/work reservations precede path formatting,
            // queue/result growth and the eventual child payload decode.
            match item.mode {
                TreeItemMode::Tree => {
                    self.trees = self
                        .trees
                        .checked_add(1)
                        .filter(|trees| *trees <= self.limits.trees)
                        .ok_or(CheckpointReaderError::Budget)?;
                }
                TreeItemMode::Commit => return Err(role_proof_error()),
                _ if self.files.len() >= self.limits.files => {
                    return Err(CheckpointReaderError::Budget);
                }
                _ => {}
            }
            let rel_path = if prefix.is_empty() {
                item.name.clone()
            } else {
                format!("{prefix}/{}", item.name)
            };
            crate::internal::ai::checkpoint_input::sanitize_rel_path(&rel_path)
                .map_err(|_| role_proof_error())?;
            if item.mode == TreeItemMode::Tree {
                self.pending.push((rel_path, item.id, next_depth));
            } else {
                self.files.push(CheckpointInputFile {
                    rel_path,
                    oid: item.id.to_string(),
                });
            }
        }
        Ok(())
    }
}

/// Enumerate ordinary roles with hard cumulative work and path budgets.
/// Artifact OIDs remain disjoint and the reasoning subtree remains excluded.
pub(crate) fn checkpoint_plain_files(
    storage: &Path,
    inner: &Tree,
    artifacts: &std::collections::BTreeSet<String>,
) -> ReaderResult<Vec<crate::internal::ai::checkpoint_input::CheckpointInputFile>> {
    checkpoint_plain_files_with_limits(
        storage,
        inner,
        artifacts,
        CheckpointTraversalLimits::default(),
    )
}

pub(crate) fn checkpoint_plain_files_with_limits(
    storage: &Path,
    inner: &Tree,
    artifacts: &std::collections::BTreeSet<String>,
    limits: CheckpointTraversalLimits,
) -> ReaderResult<Vec<crate::internal::ai::checkpoint_input::CheckpointInputFile>> {
    let mut traversal = CheckpointOrdinaryTraversal {
        artifacts,
        limits,
        entries: 0,
        trees: 1,
        path_bytes: 0,
        child_tree_bytes: 0,
        pending: Vec::new(),
        files: Vec::new(),
    };
    if limits.trees == 0 {
        return Err(CheckpointReaderError::Budget);
    }
    // Borrow the selected leaf; do not clone all its entries before bounds.
    traversal.visit_tree("", inner, 0)?;
    while let Some((prefix, oid, depth)) = traversal.pending.pop() {
        check_read_budget()?;
        let remaining = traversal
            .limits
            .child_tree_bytes
            .checked_sub(traversal.child_tree_bytes)
            .filter(|remaining| *remaining != 0)
            .ok_or(CheckpointReaderError::Budget)?;
        let (child, bytes) = read_tree_object_with_cap(
            storage,
            &oid.to_string(),
            remaining.min(CHECKPOINT_METADATA_READ_MAX_BYTES),
        )
        .map_err(|_| role_proof_error())?;
        traversal.child_tree_bytes = traversal
            .child_tree_bytes
            .checked_add(bytes)
            .filter(|bytes| *bytes <= traversal.limits.child_tree_bytes)
            .ok_or(CheckpointReaderError::Budget)?;
        traversal.visit_tree(&prefix, &child, depth)?;
    }
    Ok(traversal.files)
}

pub(crate) fn reject_manifest_role_artifact_aliases(
    value: &serde_json::Value,
    artifacts: &std::collections::BTreeSet<String>,
) -> ReaderResult<()> {
    check_read_budget()?;
    match value {
        serde_json::Value::Object(object) => {
            if object
                .get("oid")
                .and_then(serde_json::Value::as_str)
                .is_some_and(|oid| artifacts.contains(oid))
            {
                return Err(role_proof_error());
            }
            for child in object.values() {
                reject_manifest_role_artifact_aliases(child, artifacts)?;
            }
        }
        serde_json::Value::Array(values) => {
            for child in values {
                reject_manifest_role_artifact_aliases(child, artifacts)?;
            }
        }
        _ => {}
    }
    Ok(())
}

pub(crate) fn checkpoint_plain_manifest(
    storage: &Path,
    inner: &Tree,
    artifacts: &std::collections::BTreeSet<String>,
    files: &[crate::internal::ai::checkpoint_input::CheckpointInputFile],
) -> ReaderResult<Option<serde_json::Value>> {
    let Some(item) = tree_entry(inner, "manifest.json") else {
        validate_reasoning_artifact_tree(storage, inner, &[])?;
        return Ok(None);
    };
    reject_artifact_alias(&item.id, artifacts)?;
    if item.mode != TreeItemMode::Blob {
        return Err(role_proof_error());
    }
    let (bytes, truncated) =
        read_checkpoint_object_bounded(storage, &item.id, CHECKPOINT_METADATA_READ_MAX_BYTES)
            .map_err(|_| role_proof_error())?;
    if truncated {
        return Err(role_proof_error());
    }
    let manifest: serde_json::Value =
        serde_json::from_slice(&bytes).map_err(|_| role_proof_error())?;
    let metadata = reasoning_artifact_metadata(&manifest)?;
    validate_reasoning_artifact_tree(storage, inner, &metadata)?;
    if let Some(entries) = manifest.get("entries") {
        reject_manifest_role_artifact_aliases(entries, artifacts)?;
        let ordinary = files
            .iter()
            .map(|file| (file.rel_path.as_str(), file.oid.as_str()))
            .collect::<std::collections::BTreeMap<_, _>>();
        validate_declared_ordinary_roles(entries, &ordinary)?;
        let metadata = entries.get("metadata").ok_or_else(role_proof_error)?;
        if metadata.get("path").and_then(serde_json::Value::as_str) != Some("metadata.json") {
            return Err(role_proof_error());
        }
    }
    Ok(Some(manifest))
}

fn validate_declared_ordinary_roles(
    value: &serde_json::Value,
    ordinary: &std::collections::BTreeMap<&str, &str>,
) -> ReaderResult<()> {
    check_read_budget()?;
    match value {
        serde_json::Value::Object(object) => {
            if let Some(oid) = object.get("oid") {
                let oid = oid.as_str().ok_or_else(role_proof_error)?;
                let path = object
                    .get("path")
                    .and_then(serde_json::Value::as_str)
                    .ok_or_else(role_proof_error)?;
                if ordinary.get(path).copied() != Some(oid) {
                    return Err(role_proof_error());
                }
            }
            for child in object.values() {
                validate_declared_ordinary_roles(child, ordinary)?;
            }
        }
        serde_json::Value::Array(values) => {
            for child in values {
                validate_declared_ordinary_roles(child, ordinary)?;
            }
        }
        _ => {}
    }
    Ok(())
}

/// Bind every declared transcript part to its actual path and blob OID before
/// any content read. A manifest selector alone is never object authority.
pub(crate) fn checkpoint_transcript_oids(
    storage: &Path,
    inner: &Tree,
    manifest: &serde_json::Value,
    artifacts: &std::collections::BTreeSet<String>,
) -> ReaderResult<Vec<ObjectHash>> {
    let transcript = manifest
        .get("entries")
        .and_then(|entries| entries.get("transcript"))
        .ok_or_else(role_proof_error)?;
    let transcript_item = tree_entry(inner, "transcript").ok_or_else(role_proof_error)?;
    reject_artifact_alias(&transcript_item.id, artifacts)?;
    let tree = subtree(storage, inner, "transcript").map_err(|_| role_proof_error())?;
    let chunked = match transcript.get("chunked") {
        None => false,
        Some(value) => value.as_bool().ok_or_else(role_proof_error)?,
    };
    let declarations = if chunked {
        if transcript.get("oid").is_some() {
            return Err(role_proof_error());
        }
        transcript
            .get("parts")
            .and_then(serde_json::Value::as_array)
            .ok_or_else(role_proof_error)?
            .iter()
            .collect::<Vec<_>>()
    } else {
        if transcript.get("parts").is_some() {
            return Err(role_proof_error());
        }
        vec![transcript]
    };
    if declarations.is_empty() || declarations.len() != tree.tree_items.len() {
        return Err(role_proof_error());
    }
    let mut names = std::collections::BTreeSet::new();
    let mut oids = Vec::with_capacity(declarations.len());
    for entry in declarations {
        let path = entry
            .get("path")
            .and_then(serde_json::Value::as_str)
            .ok_or_else(role_proof_error)?;
        let name = path
            .strip_prefix("transcript/")
            .filter(|name| !name.is_empty() && !name.contains('/'))
            .ok_or_else(role_proof_error)?;
        crate::internal::ai::checkpoint_input::sanitize_rel_path(path)
            .map_err(|_| role_proof_error())?;
        if !names.insert(name) {
            return Err(role_proof_error());
        }
        let oid = entry
            .get("oid")
            .and_then(serde_json::Value::as_str)
            .ok_or_else(role_proof_error)?;
        let oid =
            crate::internal::object_format::parse_repo_oid(oid).map_err(|_| role_proof_error())?;
        reject_artifact_alias(&oid, artifacts)?;
        if !tree_entry(&tree, name)
            .is_some_and(|item| item.mode == TreeItemMode::Blob && item.id == oid)
        {
            return Err(role_proof_error());
        }
        oids.push(oid);
    }
    Ok(oids)
}

pub(crate) const CHECKPOINT_METADATA_READ_MAX_BYTES: u64 = 16 * 1024 * 1024;

#[cfg(test)]
thread_local! {
    pub(crate) static CHECKPOINT_BODY_READS: std::cell::RefCell<Vec<String>> = const {
        std::cell::RefCell::new(Vec::new())
    };
    static CHECKPOINT_ORDINARY_READS: RefCell<Vec<String>> = const { RefCell::new(Vec::new()) };
    static CHECKPOINT_PROOF_READS: RefCell<Vec<String>> = const { RefCell::new(Vec::new()) };
    static ORDINARY_READ_PHASE: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}

#[cfg(test)]
struct OrdinaryReadPhase(bool);

#[cfg(test)]
impl OrdinaryReadPhase {
    fn enter() -> Self {
        Self(ORDINARY_READ_PHASE.with(|phase| phase.replace(true)))
    }
}

#[cfg(test)]
impl Drop for OrdinaryReadPhase {
    fn drop(&mut self) {
        ORDINARY_READ_PHASE.with(|phase| phase.set(self.0));
    }
}

/// A checkpoint currently reads the same loose Git/zlib objects as the shared
/// bounded reader. Check the header on the held stream before touching payload:
/// a blob cannot impersonate a tree merely by using a tree mode in its parent.
pub(crate) fn read_checkpoint_object_bounded(
    storage: &Path,
    hash: &ObjectHash,
    cap: u64,
) -> Result<(Vec<u8>, bool), git_internal::errors::GitError> {
    read_checkpoint_object_bounded_as(storage, hash, cap, "blob")
}

/// A truncated blob is a header-checked prefix, not a verified object identity.
/// Callers requiring an integrity or role proof must refuse `truncated = true`.
/// Full reads validate both the declared size and the content-addressed OID.
pub(crate) fn read_checkpoint_object_bounded_as(
    storage: &Path,
    hash: &ObjectHash,
    cap: u64,
    expected_type: &str,
) -> Result<(Vec<u8>, bool), git_internal::errors::GitError> {
    use std::io::Read as _;

    use git_internal::errors::GitError;

    const HEADER_MAX: usize = 64;
    let oid = hash.to_string();
    // INVARIANT: ObjectHash formats as ASCII hex with at least 40 bytes.
    let file = std::fs::File::open(storage.join("objects").join(&oid[..2]).join(&oid[2..]))?;
    let mut decoder = flate2::read::ZlibDecoder::new(file);
    let mut header = Vec::with_capacity(HEADER_MAX);
    let mut byte = [0u8; 1];
    loop {
        check_read_budget().map_err(|error| GitError::InvalidObjectInfo(error.to_string()))?;
        if decoder.read(&mut byte)? == 0 {
            return Err(GitError::InvalidObjectInfo(
                "checkpoint object ended before its header terminator".to_string(),
            ));
        }
        if byte[0] == 0 {
            break;
        }
        if header.len() == HEADER_MAX {
            return Err(GitError::InvalidObjectInfo(
                "checkpoint object header exceeds its size cap".to_string(),
            ));
        }
        header.push(byte[0]);
    }
    let header = std::str::from_utf8(&header).map_err(|_| {
        GitError::InvalidObjectInfo("checkpoint object header is not UTF-8".to_string())
    })?;
    let (kind, size) = header.split_once(' ').ok_or_else(|| {
        GitError::InvalidObjectInfo("checkpoint object header has no type/size".to_string())
    })?;
    if kind != expected_type {
        return Err(GitError::InvalidObjectInfo(format!(
            "checkpoint object is not the required {expected_type} type"
        )));
    }
    let size = size.parse::<u64>().map_err(|_| {
        GitError::InvalidObjectInfo("checkpoint object header has an invalid size".to_string())
    })?;
    if kind == "tree" && size > cap {
        return Err(GitError::InvalidObjectInfo(
            "checkpoint tree exceeds its remaining decoded-byte budget".to_string(),
        ));
    }
    // This test hook records payload-read attempts, after header validation.
    // It observes actual accepted object reads as well as rejected alias tests.
    #[cfg(test)]
    CHECKPOINT_BODY_READS.with(|reads| reads.borrow_mut().push(hash.to_string()));
    #[cfg(test)]
    ORDINARY_READ_PHASE.with(|phase| {
        if phase.get() {
            CHECKPOINT_ORDINARY_READS.with(|reads| reads.borrow_mut().push(hash.to_string()));
        } else {
            CHECKPOINT_PROOF_READS.with(|reads| reads.borrow_mut().push(hash.to_string()));
        }
    });
    let mut content = Vec::new();
    let mut limited = decoder.take(cap.saturating_add(1));
    let mut chunk = [0u8; 16 * 1024];
    loop {
        check_read_budget().map_err(|error| GitError::InvalidObjectInfo(error.to_string()))?;
        let count = limited.read(&mut chunk)?;
        if count == 0 {
            break;
        }
        content.extend_from_slice(&chunk[..count]);
    }
    let truncated = content.len() as u64 > cap;
    if truncated {
        let cap = usize::try_from(cap).map_err(|_| {
            GitError::InvalidObjectInfo("checkpoint read cap exceeds this platform".to_string())
        })?;
        content.truncate(cap);
    } else if content.len() as u64 != size
        || ObjectHash::from_type_and_data_for_kind(
            hash.kind(),
            match kind {
                "blob" => ObjectType::Blob,
                "tree" => ObjectType::Tree,
                _ => {
                    return Err(GitError::InvalidObjectInfo(
                        "checkpoint object type is unsupported".to_string(),
                    ));
                }
            },
            &content,
        )
        .map_err(|error| {
            GitError::InvalidObjectInfo(format!(
                "checkpoint object hash could not be verified: {error}"
            ))
        })? != *hash
    {
        return Err(GitError::InvalidObjectInfo(
            "checkpoint object size or content-addressed identity is inconsistent".to_string(),
        ));
    }
    Ok((content, truncated))
}

pub(crate) fn read_tree_object(storage: &Path, oid_str: &str) -> Result<Tree, String> {
    read_tree_object_with_cap(storage, oid_str, CHECKPOINT_METADATA_READ_MAX_BYTES)
        .map(|(tree, _)| tree)
}

pub(crate) fn read_tree_object_with_cap(
    storage: &Path,
    oid_str: &str,
    cap: u64,
) -> Result<(Tree, u64), String> {
    let oid = crate::internal::object_format::parse_repo_oid(oid_str).map_err(|_| {
        "checkpoint catalog tree oid is invalid; run `libra agent doctor`".to_string()
    })?;
    let (body, truncated) =
        read_checkpoint_object_bounded_as(storage, &oid, cap, "tree").map_err(|e| {
            format!(
                "checkpoint tree {oid_str} is not readable from the local object \
                     store ({e}); layout unknown — metadata-first summary only"
            )
        })?;
    if truncated {
        return Err(format!(
            "checkpoint tree {oid_str} exceeds the {cap}-byte \
             metadata cap; refusing to load (corrupt or hostile object)"
        ));
    }
    let tree = Tree::from_bytes(&body, oid).map_err(|_| {
        format!("object {oid_str} did not parse as a checkpoint tree; run `libra agent doctor`")
    })?;
    let mut names = std::collections::BTreeSet::new();
    if tree
        .tree_items
        .iter()
        .any(|item| !names.insert(item.name.as_str()))
    {
        return Err("checkpoint tree contains duplicate entry names".to_string());
    }
    Ok((tree, body.len() as u64))
}

pub(crate) fn tree_entry<'t>(tree: &'t Tree, name: &str) -> Option<&'t TreeItem> {
    tree.tree_items.iter().find(|item| item.name == name)
}

fn resolve_input_spec_from_catalog(
    catalog: CheckpointCatalog,
    budget: &CheckpointReadBudget,
) -> ReaderResult<crate::internal::ai::checkpoint_input::CheckpointInputSpec> {
    use crate::internal::ai::checkpoint_input::{
        CHECKPOINT_INPUT_MAX_FILE_BYTES, CHECKPOINT_INPUT_MAX_TOTAL_BYTES, CheckpointInputSpec,
    };
    let _scope = SynchronousReadScope::enter(catalog.kind, budget)?;
    // Fresh proof traversal limits retain the original role-refusal mapping;
    // subsequent payload limits retain the scoped materialization context.
    let (mut files, _) = prove_catalog_roles(&catalog).map_err(|error| match error {
        CheckpointReaderError::Budget => role_proof_error(),
        error => error,
    })?;
    if files.is_empty() {
        return Err(CheckpointReaderError::fatal(format!(
            "checkpoint '{}' cannot be materialized as a scoped input: the checkpoint tree carries no files",
            catalog.checkpoint_id
        )));
    }
    files.sort_by(|a, b| a.rel_path.cmp(&b.rel_path));
    let validated = ValidatedCheckpointInput { catalog, files };
    let mut total = 0u64;
    for index in 0..validated.files().len() {
        let remaining = CHECKPOINT_INPUT_MAX_TOTAL_BYTES
            .checked_sub(total)
            .ok_or(CheckpointReaderError::Budget)?;
        let bytes = validated.read_ordinary_file(
            index,
            remaining.min(CHECKPOINT_INPUT_MAX_FILE_BYTES),
            budget,
        )?;
        total = total
            .checked_add(bytes.len() as u64)
            .ok_or(CheckpointReaderError::Budget)?;
    }
    Ok(CheckpointInputSpec {
        checkpoint_id: validated.catalog.checkpoint_id.clone(),
        files: validated.files,
    })
}

// Only preserved command fixtures may construct a synthetic catalog from an
// OID. Production callers must obtain its private identity from the database.
#[cfg(test)]
pub(crate) fn resolve_checkpoint_input_spec_from_storage(
    storage: &Path,
    checkpoint_id: &str,
    tree_oid: &str,
) -> ReaderResult<crate::internal::ai::checkpoint_input::CheckpointInputSpec> {
    validate_checkpoint_id(checkpoint_id)?;
    let kind = git_internal::hash::get_hash_kind();
    let tree_oid = crate::internal::object_format::parse_hex_for_kind(kind, tree_oid)
        .map_err(|_| CheckpointReaderError::ObjectIntegrity)?;
    let budget = READ_SCOPE
        .with(|scope| scope.borrow().clone())
        .unwrap_or_else(CheckpointReadBudget::fresh);
    resolve_input_spec_from_catalog(
        CheckpointCatalog {
            storage: storage.to_path_buf(),
            checkpoint_id: checkpoint_id.to_string(),
            tree_oid,
            kind,
        },
        &budget,
    )
}

pub(crate) fn subtree(storage: &Path, tree: &Tree, name: &str) -> Result<Tree, String> {
    let item = tree_entry(tree, name).ok_or_else(|| {
        format!("tree entry '{name}' missing while resolving the checkpoint tree")
    })?;
    if item.mode != TreeItemMode::Tree {
        return Err(format!("checkpoint tree entry '{name}' is not a tree"));
    }
    read_tree_object(storage, &item.id.to_string())
}

/// An identity obtained only through the explicit repository's readonly
/// catalog. Fields are private so a persisted spec cannot supply authority.
pub(crate) struct CheckpointCatalog {
    storage: PathBuf,
    checkpoint_id: String,
    tree_oid: ObjectHash,
    kind: HashKind,
}

/// Load a catalog row using one connection and one absolute query deadline.
/// The dedicated connection is closed before this function returns authority
/// to a filesystem worker. It never initializes a missing database or schema.
pub(crate) async fn load_checkpoint_catalog(
    storage: &Path,
    checkpoint_id: &str,
    budget: &CheckpointReadBudget,
) -> ReaderResult<CheckpointCatalog> {
    use sea_orm::sqlx::{
        Row,
        sqlite::{SqliteConnectOptions, SqlitePoolOptions},
    };

    validate_checkpoint_id(checkpoint_id)?;
    budget.check()?;
    let phase_end = Instant::now() + Duration::from_millis(200);
    let end = budget
        .deadline
        .map_or(phase_end, |caller| caller.min(phase_end));
    let timeout_at = tokio::time::Instant::from_std(end);
    let options = SqliteConnectOptions::new()
        .filename(storage.join(crate::utils::util::DATABASE))
        .read_only(true)
        .create_if_missing(false)
        .busy_timeout(Duration::ZERO);
    let progress_budget = budget.clone();
    // Preserve db.rs's SQLx 0.9 reaper safeguard for this readonly owner too.
    let pool = SqlitePoolOptions::new()
        .idle_timeout(None)
        .max_lifetime(None)
        .max_connections(1)
        .min_connections(0)
        .acquire_timeout(end.saturating_duration_since(Instant::now()))
        .test_before_acquire(false)
        .after_connect(move |connection, _| {
            let progress_budget = progress_budget.clone();
            Box::pin(async move {
                #[cfg(test)]
                let instructions = if progress_budget.catalog_observer.is_some() {
                    1 // Observe real SQLite VM progress even for tiny fixture queries.
                } else {
                    1000
                };
                #[cfg(not(test))]
                let instructions = 1000;
                connection
                    .lock_handle()
                    .await?
                    .set_progress_handler(instructions, move || {
                        #[cfg(test)]
                        progress_budget.observe_catalog(CatalogTestPhase::Progress);
                        Instant::now() < end && progress_budget.check().is_ok()
                    });
                Ok(())
            })
        })
        .connect_lazy_with(options);

    let acquired = tokio::time::timeout_at(timeout_at, pool.acquire()).await;
    let result = match acquired {
        Ok(Ok(mut connection)) => {
            // Failure paths close this connection, rather than returning a live
            // lease to a pool whose query may still be winding down.
            connection.close_on_drop();
            let query = async {
                budget.check()?;
                let tables = sea_orm::sqlx::query(
                "SELECT name FROM sqlite_schema WHERE type = 'table' AND name IN ('agent_checkpoint', 'config_kv')",
            ).fetch_all(&mut *connection).await
                .map_err(|_| CheckpointReaderError::CatalogUnavailable)?;
                let names = tables
                    .iter()
                    .map(|row| row.try_get::<String, _>("name"))
                    .collect::<Result<Vec<_>, _>>()
                    .map_err(|_| CheckpointReaderError::UnknownLayout)?;
                if !names.iter().any(|name| name == "agent_checkpoint") {
                    return Err(CheckpointReaderError::fatal(format!(
                        "no checkpoint matches '{checkpoint_id}': agent_checkpoint table not yet present \
                         (run `libra init`?)"
                    )));
                }
                if !names.iter().any(|name| name == "config_kv") {
                    return Err(CheckpointReaderError::UnknownLayout);
                }
                // One statement observes the named row and its format in the same
                // SQLite read snapshot. Bounded substrings avoid allocating an
                // unbounded corrupt catalog value; overlength values fail below.
                let rows = sea_orm::sqlx::query(
                "SELECT substr(checkpoint_id, 1, 129) AS checkpoint_id, substr(tree_oid, 1, 66) AS tree_oid, \
                 (SELECT substr(value, 1, 17) FROM config_kv WHERE key = 'core.objectformat' ORDER BY id DESC LIMIT 1) AS format, \
                 (SELECT encrypted FROM config_kv WHERE key = 'core.objectformat' ORDER BY id DESC LIMIT 1) AS encrypted \
                 FROM agent_checkpoint WHERE checkpoint_id = ? LIMIT 2",
            ).bind(checkpoint_id).fetch_all(&mut *connection).await
                .map_err(|_| CheckpointReaderError::CatalogUnavailable)?;
                budget.check()?;
                if rows.is_empty() {
                    return Err(CheckpointReaderError::fatal(format!(
                        "no checkpoint matches id '{checkpoint_id}'; list captured checkpoints with \
                         `libra agent checkpoint list`"
                    )));
                }
                if rows.len() != 1 {
                    return Err(CheckpointReaderError::RoleProof);
                }
                // INVARIANT: empty and non-singleton results were refused above.
                let row = &rows[0];
                let actual_id: String = row
                    .try_get("checkpoint_id")
                    .map_err(|_| CheckpointReaderError::RoleProof)?;
                if actual_id != checkpoint_id {
                    return Err(CheckpointReaderError::RoleProof);
                }
                let format: Option<String> = row
                    .try_get("format")
                    .map_err(|_| CheckpointReaderError::UnknownLayout)?;
                let encrypted: Option<i64> = row
                    .try_get("encrypted")
                    .map_err(|_| CheckpointReaderError::UnknownLayout)?;
                if encrypted.is_some_and(|value| value != 0) {
                    return Err(CheckpointReaderError::UnknownLayout);
                }
                let kind = match format {
                    Some(format) => crate::internal::object_format::parse_config_value(&format)
                        .map_err(|_| CheckpointReaderError::UnknownLayout)?,
                    None => HashKind::Sha1,
                };
                let tree_oid: String = row
                    .try_get("tree_oid")
                    .map_err(|_| CheckpointReaderError::RoleProof)?;
                let tree_oid = crate::internal::object_format::parse_hex_for_kind(kind, &tree_oid)
                    .map_err(|_| CheckpointReaderError::ObjectIntegrity)?;
                Ok(CheckpointCatalog {
                    storage: storage.to_path_buf(),
                    checkpoint_id: actual_id,
                    tree_oid,
                    kind,
                })
            };
            let query_result = match tokio::time::timeout_at(timeout_at, query).await {
                Ok(result) => result,
                Err(_) => Err(CheckpointReaderError::CatalogUnavailable),
            };
            // Keep ownership outside the timed query future. A deadline can
            // cancel a query, but cannot turn dropping close() into a joined
            // SQLite worker. Always finish closure, then refuse late authority.
            let close_result = connection.close().await;
            #[cfg(test)]
            budget.observe_catalog(CatalogTestPhase::ConnectionCloseFinished);
            match close_result {
                Ok(()) => query_result,
                Err(_) => Err(CheckpointReaderError::CatalogUnavailable),
            }
        }
        Ok(Err(_)) | Err(_) => Err(CheckpointReaderError::CatalogUnavailable),
    };
    pool.close().await;
    #[cfg(test)]
    budget.observe_catalog(CatalogTestPhase::PoolCloseFinished);
    budget.check()?;
    if Instant::now() >= end {
        return Err(CheckpointReaderError::CatalogUnavailable);
    }
    result
}

fn validate_checkpoint_id(id: &str) -> ReaderResult<()> {
    if id.len() < 3
        || id.len() > 128
        || !id
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || byte == b'-' || byte == b'_')
    {
        Err(CheckpointReaderError::InvalidSavedSpec(
            "checkpoint id is invalid; choose an id from `libra agent checkpoint list`".into(),
        ))
    } else {
        Ok(())
    }
}

fn validate_saved_bounds(
    spec: &crate::internal::ai::checkpoint_input::CheckpointInputSpec,
    kind: HashKind,
    budget: &CheckpointReadBudget,
) -> ReaderResult<()> {
    use crate::internal::ai::traces::REASONING_ARTIFACT_MAX_ENTRIES;
    validate_checkpoint_id(&spec.checkpoint_id)?;
    if spec.files.len() > CHECKPOINT_ORDINARY_MAX_FILES + REASONING_ARTIFACT_MAX_ENTRIES {
        return Err(CheckpointReaderError::Budget);
    }
    let mut total = 0usize;
    // Complete cardinality and byte reservations before allocating a map or
    // normalizing paths. Proof cannot make an initially oversized spec cheap.
    for file in &spec.files {
        budget.check()?;
        total = total
            .checked_add(file.rel_path.len())
            .ok_or(CheckpointReaderError::Budget)?;
        if file.rel_path.len() > CHECKPOINT_ORDINARY_MAX_PATH_BYTES
            || total
                > CHECKPOINT_ORDINARY_MAX_TOTAL_PATH_BYTES
                    + REASONING_ARTIFACT_MAX_ENTRIES * CANONICAL_REASONING_ARTIFACT_PATH_BYTES
        {
            return Err(CheckpointReaderError::Budget);
        }
    }
    let mut paths = std::collections::BTreeSet::new();
    for file in &spec.files {
        budget.check()?;
        crate::internal::ai::checkpoint_input::sanitize_rel_path(&file.rel_path)
            .map_err(|_| role_proof_error())?;
        crate::internal::object_format::parse_hex_for_kind(kind, &file.oid)
            .map_err(|_| CheckpointReaderError::ObjectIntegrity)?;
        if !paths.insert(file.rel_path.as_str()) {
            return Err(role_proof_error());
        }
    }
    Ok(())
}

/// Only the shared role proof constructs this input. Consumers can inspect
/// ordinary entries and request a typed, verified body; artifact entries are
/// never exposed as materialization authority.
pub(crate) struct ValidatedCheckpointInput {
    catalog: CheckpointCatalog,
    files: Vec<crate::internal::ai::checkpoint_input::CheckpointInputFile>,
}

impl ValidatedCheckpointInput {
    pub(crate) fn files(&self) -> &[crate::internal::ai::checkpoint_input::CheckpointInputFile] {
        &self.files
    }

    pub(crate) fn read_ordinary_file(
        &self,
        index: usize,
        cap: u64,
        budget: &CheckpointReadBudget,
    ) -> ReaderResult<Vec<u8>> {
        use crate::internal::ai::checkpoint_input::CHECKPOINT_INPUT_MAX_FILE_BYTES;
        let _scope = SynchronousReadScope::enter(self.catalog.kind, budget)?;
        let file = self.files.get(index).ok_or_else(role_proof_error)?;
        let oid = crate::internal::object_format::parse_hex_for_kind(self.catalog.kind, &file.oid)
            .map_err(|_| CheckpointReaderError::ObjectIntegrity)?;
        #[cfg(test)]
        let _phase = OrdinaryReadPhase::enter();
        let result = read_checkpoint_object_bounded(
            &self.catalog.storage,
            &oid,
            cap.min(CHECKPOINT_INPUT_MAX_FILE_BYTES),
        )
        .map_err(|_| CheckpointReaderError::ObjectIntegrity);
        budget.check()?;
        let (bytes, truncated) = result?;
        if truncated {
            return Err(CheckpointReaderError::Budget);
        }
        Ok(bytes)
    }
}

fn prove_catalog_roles(
    catalog: &CheckpointCatalog,
) -> ReaderResult<(
    Vec<crate::internal::ai::checkpoint_input::CheckpointInputFile>,
    std::collections::BTreeMap<String, String>,
)> {
    let root = read_tree_object(&catalog.storage, &catalog.tree_oid.to_string())
        .map_err(|_| CheckpointReaderError::ObjectIntegrity)?;
    let wrapped = subtree(&catalog.storage, &root, "checkpoint")
        .map_err(|_| CheckpointReaderError::UnknownLayout)?;
    // INVARIANT: catalog constructors validate ASCII checkpoint ids of at least three bytes.
    let prefix = subtree(&catalog.storage, &wrapped, &catalog.checkpoint_id[..2])
        .map_err(|_| CheckpointReaderError::UnknownLayout)?;
    let inner = subtree(&catalog.storage, &prefix, &catalog.checkpoint_id[2..])
        .map_err(|_| CheckpointReaderError::UnknownLayout)?;
    let artifacts = reasoning_artifact_tree_oids(&catalog.storage, &inner)?;
    let ordinary = checkpoint_plain_files(&catalog.storage, &inner, &artifacts)?;
    let manifest = checkpoint_plain_manifest(&catalog.storage, &inner, &artifacts, &ordinary)?;
    let metadata = match manifest.as_ref() {
        Some(manifest) => {
            checkpoint_transcript_oids(&catalog.storage, &inner, manifest, &artifacts)?;
            reasoning_artifact_metadata(manifest)?
        }
        None => Vec::new(),
    };
    let proven_artifacts = metadata
        .into_iter()
        .map(|entry| (entry.path, entry.oid))
        .collect::<std::collections::BTreeMap<_, _>>();
    Ok((ordinary, proven_artifacts))
}

/// Prove the entire ordinary path→OID map in the named wrapped checkpoint,
/// then remove only exact, manifest-and-tree-proven legacy artifact entries.
/// This intentionally does not read ordinary bodies before comparing maps.
#[allow(
    dead_code,
    reason = "the dependent scoped consumer card receives this private-constructor proof API"
)]
pub(crate) fn validate_saved_checkpoint_input(
    catalog: CheckpointCatalog,
    spec: &crate::internal::ai::checkpoint_input::CheckpointInputSpec,
    budget: &CheckpointReadBudget,
) -> ReaderResult<ValidatedCheckpointInput> {
    validate_saved_bounds(spec, catalog.kind, budget)?;
    if spec.checkpoint_id != catalog.checkpoint_id {
        return Err(role_proof_error());
    }
    let _scope = SynchronousReadScope::enter(catalog.kind, budget)?;
    let result = (|| {
        let (mut ordinary, proven_artifacts) = prove_catalog_roles(&catalog)?;
        if ordinary.is_empty() {
            return Err(role_proof_error());
        }
        let expected = ordinary
            .iter()
            .map(|file| (file.rel_path.as_str(), file.oid.as_str()))
            .collect::<std::collections::BTreeMap<_, _>>();
        if expected.len() != ordinary.len() {
            return Err(role_proof_error());
        }
        let mut actual = std::collections::BTreeMap::new();
        let mut path_bytes = 0usize;
        let mut excluded = 0usize;
        for file in &spec.files {
            budget.check()?;
            if proven_artifacts
                .get(file.rel_path.as_str())
                .map(String::as_str)
                == Some(file.oid.as_str())
            {
                if file.rel_path.len() != CANONICAL_REASONING_ARTIFACT_PATH_BYTES {
                    return Err(role_proof_error());
                }
                excluded += 1;
                if excluded > crate::internal::ai::traces::REASONING_ARTIFACT_MAX_ENTRIES {
                    return Err(CheckpointReaderError::Budget);
                }
                continue;
            }
            path_bytes = path_bytes
                .checked_add(file.rel_path.len())
                .ok_or(CheckpointReaderError::Budget)?;
            if actual.len() >= CHECKPOINT_ORDINARY_MAX_FILES
                || path_bytes > CHECKPOINT_ORDINARY_MAX_TOTAL_PATH_BYTES
            {
                return Err(CheckpointReaderError::Budget);
            }
            actual.insert(file.rel_path.as_str(), file.oid.as_str());
        }
        if actual != expected {
            return Err(role_proof_error());
        }
        ordinary.sort_by(|a, b| a.rel_path.cmp(&b.rel_path));
        Ok(ordinary)
    })();
    budget.check()?;
    let files = result?;
    Ok(ValidatedCheckpointInput { catalog, files })
}

pub(crate) async fn resolve_fresh_checkpoint_input(
    storage: &Path,
    checkpoint_id: &str,
) -> ReaderResult<crate::internal::ai::checkpoint_input::CheckpointInputSpec> {
    let budget = CheckpointReadBudget::fresh();
    let catalog = load_checkpoint_catalog(storage, checkpoint_id, &budget).await?;
    // The catalog connection has been closed. Join this bounded synchronous
    // read owner rather than blocking a runtime worker while holding a lease.
    tokio::task::spawn_blocking(move || resolve_input_spec_from_catalog(catalog, &budget))
        .await
        .map_err(|_| CheckpointReaderError::ObjectIntegrity)?
}

#[cfg(test)]
pub(crate) mod test_helpers {
    use git_internal::internal::object::ObjectTrait;

    use super::*;

    pub(crate) const RG03_CHECKPOINT_ID: &str = "aabbccdd-eeff-0011-2233-445566778899";
    pub(crate) const RG03_OPAQUE_BODY: &[u8] = b"rg03-opaque-body-read-canary";

    pub(crate) fn rg03_tree(storage: &Path, mut items: Vec<TreeItem>) -> ObjectHash {
        items.sort_by(|a, b| a.name.cmp(&b.name));
        let tree = Tree::from_tree_items(items).expect("fixture tree");
        crate::utils::object::write_git_object(storage, "tree", &tree.to_data().unwrap())
            .expect("write fixture tree")
    }

    pub(crate) fn rg03_blob(storage: &Path, bytes: &[u8]) -> ObjectHash {
        crate::utils::object::write_git_object(storage, "blob", bytes).expect("fixture blob")
    }

    pub(crate) fn rg03_read_fixture(
        storage: &Path,
        shape: &str,
        chunked: bool,
    ) -> (String, ObjectHash) {
        use sha2::{Digest, Sha256};

        let artifact = rg03_blob(storage, RG03_OPAQUE_BODY);
        let sha = format!("{:x}", Sha256::digest(RG03_OPAQUE_BODY));
        let transcript = rg03_blob(storage, b"ordinary transcript");
        let metadata = rg03_blob(storage, b"{}");
        let name = if chunked {
            "claude_code.jsonl.001"
        } else {
            "claude_code.jsonl"
        };
        let tree_transcript = if shape == "tree_alias" {
            artifact
        } else {
            transcript
        };
        let transcript_tree = rg03_tree(
            storage,
            vec![TreeItem::new(
                TreeItemMode::Blob,
                tree_transcript,
                name.into(),
            )],
        );
        let encrypted = rg03_tree(
            storage,
            vec![TreeItem::new(TreeItemMode::Blob, artifact, sha.clone())],
        );
        let reasoning = rg03_tree(
            storage,
            vec![TreeItem::new(
                TreeItemMode::Tree,
                if shape == "encrypted_root_alias" {
                    artifact
                } else {
                    encrypted
                },
                "encrypted".into(),
            )],
        );
        let manifest_transcript = if matches!(shape, "manifest_alias" | "tree_alias") {
            artifact
        } else {
            transcript
        };
        let transcript_entry = if chunked {
            serde_json::json!({"path":"transcript/claude_code.jsonl", "chunked":true,
                "parts":[{"path":format!("transcript/{name}"), "oid":manifest_transcript.to_string()}]})
        } else {
            serde_json::json!({"path":format!("transcript/{name}"), "oid":manifest_transcript.to_string()})
        };
        let artifact_entry = serde_json::json!({
            "path":format!("reasoning/encrypted/{sha}"), "oid":artifact.to_string(), "sha256":sha,
            "locator":"claude_code:msg=0/part=0/metadata=signature", "provider":"claude_code",
            "source_kind":"signature", "availability":"encrypted_unavailable", "decrypt_capability":"none",
            "byte_len":RG03_OPAQUE_BODY.len()
        });
        let mut manifest = serde_json::json!({"schema_version":1,
            "entries":{"metadata":{"path":"metadata.json","oid":metadata.to_string()},"transcript":transcript_entry},
            "reasoning_artifacts":[artifact_entry]});
        if shape == "manifest_metadata_alias" {
            manifest["entries"]["metadata"]["oid"] = serde_json::json!(artifact.to_string());
        }
        if shape == "manifest_metadata_other" {
            manifest["entries"]["metadata"]["oid"] = serde_json::json!(transcript.to_string());
        }
        if shape == "manifest_metadata_path" {
            manifest["entries"]["metadata"]["path"] = serde_json::json!("other.json");
        }
        let manifest_blob = rg03_blob(storage, &serde_json::to_vec(&manifest).unwrap());
        let mut inner = vec![
            TreeItem::new(
                TreeItemMode::Blob,
                if shape == "manifest_body_alias" {
                    artifact
                } else {
                    manifest_blob
                },
                "manifest.json".into(),
            ),
            TreeItem::new(
                TreeItemMode::Blob,
                if shape == "metadata_body_alias" {
                    artifact
                } else {
                    metadata
                },
                "metadata.json".into(),
            ),
            TreeItem::new(
                TreeItemMode::Tree,
                if shape == "transcript_root_alias" {
                    artifact
                } else {
                    transcript_tree
                },
                "transcript".into(),
            ),
            TreeItem::new(
                TreeItemMode::Tree,
                if shape == "reasoning_root_alias" {
                    artifact
                } else {
                    reasoning
                },
                "reasoning".into(),
            ),
        ];
        if shape == "nested_tree_alias" {
            inner.push(TreeItem::new(TreeItemMode::Tree, artifact, "events".into()));
        }
        if shape == "unknown_reasoning" {
            let other = rg03_tree(
                storage,
                vec![TreeItem::new(TreeItemMode::Blob, artifact, "other".into())],
            );
            inner
                .iter_mut()
                .find(|item| item.name == "reasoning")
                .unwrap()
                .id = other;
        }
        let inner = rg03_tree(storage, inner);
        let prefix = rg03_tree(
            storage,
            vec![TreeItem::new(
                TreeItemMode::Tree,
                inner,
                RG03_CHECKPOINT_ID[2..].into(),
            )],
        );
        let checkpoint = rg03_tree(
            storage,
            vec![TreeItem::new(
                TreeItemMode::Tree,
                prefix,
                RG03_CHECKPOINT_ID[..2].into(),
            )],
        );
        let root = rg03_tree(
            storage,
            vec![TreeItem::new(
                TreeItemMode::Tree,
                checkpoint,
                "checkpoint".into(),
            )],
        );
        (root.to_string(), artifact)
    }

    pub(crate) fn rg03_clear_body_reads() {
        CHECKPOINT_BODY_READS.with(|reads| reads.borrow_mut().clear());
    }

    pub(crate) fn rg03_assert_no_artifact_body_reads(oid: ObjectHash) {
        CHECKPOINT_BODY_READS.with(|reads| {
            assert!(
                !reads.borrow().contains(&oid.to_string()),
                "artifact body read observed: {:?}",
                reads.borrow()
            );
        });
    }

    pub(crate) fn rg04_root_with_extra_tree(
        storage: &Path,
        extra: ObjectHash,
    ) -> (String, ObjectHash) {
        let (root, artifact) = rg03_read_fixture(storage, "normal", false);
        let root = read_tree_object(storage, &root).unwrap();
        let checkpoints = subtree(storage, &root, "checkpoint").unwrap();
        let prefix = subtree(storage, &checkpoints, &RG03_CHECKPOINT_ID[..2]).unwrap();
        let mut inner = subtree(storage, &prefix, &RG03_CHECKPOINT_ID[2..])
            .unwrap()
            .tree_items;
        inner.push(TreeItem::new(TreeItemMode::Tree, extra, "fanout".into()));
        let inner = rg03_tree(storage, inner);
        let prefix = rg03_tree(
            storage,
            vec![TreeItem::new(
                TreeItemMode::Tree,
                inner,
                RG03_CHECKPOINT_ID[2..].into(),
            )],
        );
        let checkpoints = rg03_tree(
            storage,
            vec![TreeItem::new(
                TreeItemMode::Tree,
                prefix,
                RG03_CHECKPOINT_ID[..2].into(),
            )],
        );
        let root = rg03_tree(
            storage,
            vec![TreeItem::new(
                TreeItemMode::Tree,
                checkpoints,
                "checkpoint".into(),
            )],
        );
        (root.to_string(), artifact)
    }

    pub(crate) fn rg04_shared_dag(storage: &Path, depth: usize) -> ObjectHash {
        let ordinary = rg03_blob(storage, b"ordinary DAG payload");
        let mut child = rg03_tree(
            storage,
            vec![TreeItem::new(
                TreeItemMode::Blob,
                ordinary,
                "ordinary.json".into(),
            )],
        );
        for _ in 0..depth {
            child = rg03_tree(
                storage,
                vec![
                    TreeItem::new(TreeItemMode::Tree, child, "a".into()),
                    TreeItem::new(TreeItemMode::Tree, child, "b".into()),
                ],
            );
        }
        child
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn checkpoint_reader_error_display_is_payload_free() {
        for (error, expected) in [
            (
                CheckpointReaderError::RoleProof,
                "checkpoint catalog row is inconsistent; run `libra agent doctor` to inspect the store",
            ),
            (
                CheckpointReaderError::CatalogUnavailable,
                "checkpoint catalog is unavailable; retry after the repository writer finishes, or run `libra agent doctor`",
            ),
            (
                CheckpointReaderError::UnknownLayout,
                "checkpoint layout cannot be proved; run `libra agent doctor` and select a captured checkpoint",
            ),
            (
                CheckpointReaderError::Budget,
                "checkpoint input exceeds its work budget or was cancelled; retry with a smaller checkpoint",
            ),
            (
                CheckpointReaderError::ObjectIntegrity,
                "checkpoint object identity or type is inconsistent; run `libra agent doctor`",
            ),
            (
                CheckpointReaderError::InvalidSavedSpec(
                    "invalid saved checkpoint specification".into(),
                ),
                "invalid saved checkpoint specification",
            ),
        ] {
            assert_eq!(error.to_string(), expected);
        }
    }

    #[test]
    fn checkpoint_reader_corrupt_tree_and_path_errors_do_not_reflect_payload() {
        const CANARY: &str = "private-provider-payload-canary";
        let dir = TempDir::new().unwrap();
        let blob = rg03_blob(dir.path(), b"ordinary");
        let mut body = format!("{CANARY} name\0").into_bytes();
        body.extend_from_slice(&hex::decode(blob.to_string()).unwrap());
        let bad = crate::utils::object::write_git_object(dir.path(), "tree", &body).unwrap();
        let error = read_tree_object(dir.path(), &bad.to_string()).unwrap_err();
        assert!(!error.contains(CANARY));
        assert!(error.contains("libra agent doctor"));
        let error = read_tree_object(dir.path(), CANARY).unwrap_err();
        assert!(!error.contains(CANARY));
        let unsafe_tree = Tree::from_tree_items(vec![TreeItem::new(
            TreeItemMode::Blob,
            blob,
            format!("../{CANARY}"),
        )])
        .unwrap();
        let error =
            checkpoint_plain_files(dir.path(), &unsafe_tree, &Default::default()).unwrap_err();
        assert!(!error.to_string().contains(CANARY));
        assert!(matches!(error, CheckpointReaderError::RoleProof));
    }

    #[test]
    fn artifact_metadata_contract_rejects_unsafe_shapes_and_logical_budgets() {
        let entry = serde_json::json!({"path": format!("reasoning/encrypted/{}", "a".repeat(64)),
            "oid": "a".repeat(crate::utils::object::git_object_hash("blob", b"").to_string().len()), "sha256": "a".repeat(64), "byte_len": 7,
            "locator": "claude_code:msg=0/part=0/metadata=signature", "provider": "claude_code", "source_kind": "signature",
            "availability": "encrypted_unavailable", "decrypt_capability": "none"});
        let valid = serde_json::json!({"reasoning_artifacts": [entry]});
        assert!(reasoning_artifact_metadata(&valid).is_ok());
        let mut legacy = valid.clone();
        legacy["reasoning_artifacts"][0]
            .as_object_mut()
            .unwrap()
            .remove("byte_len");
        assert!(reasoning_artifact_metadata(&legacy).is_ok());
        for key in [
            "path",
            "oid",
            "sha256",
            "locator",
            "provider",
            "source_kind",
            "availability",
            "decrypt_capability",
            "body",
        ] {
            let mut invalid = valid.clone();
            invalid["reasoning_artifacts"][0][key] = serde_json::json!("artifact metadata canary");
            assert_eq!(
                parse_reasoning_artifact_metadata(&invalid).err(),
                Some("reasoning artifact metadata is invalid")
            );
        }
        let mut duplicate = valid.clone();
        duplicate["reasoning_artifacts"]
            .as_array_mut()
            .unwrap()
            .push(valid["reasoning_artifacts"][0].clone());
        assert!(reasoning_artifact_metadata(&duplicate).is_err());
        let mut shared = duplicate.clone();
        shared["reasoning_artifacts"][1]["locator"] =
            serde_json::json!("claude_code:msg=1/part=0/metadata=signature");
        assert!(reasoning_artifact_metadata(&shared).is_ok());
        for (key, value) in [
            (
                "oid",
                serde_json::json!(
                    "2".repeat(
                        valid["reasoning_artifacts"][0]["oid"]
                            .as_str()
                            .unwrap()
                            .len()
                    )
                ),
            ),
            ("byte_len", serde_json::json!(2)),
        ] {
            let mut contradictory = shared.clone();
            contradictory["reasoning_artifacts"][1][key] = value;
            assert!(reasoning_artifact_metadata(&contradictory).is_err());
        }
        let mut total = duplicate;
        total["reasoning_artifacts"][1]["locator"] =
            serde_json::json!("claude_code:msg=1/part=0/metadata=signature");
        total["reasoning_artifacts"][0]["byte_len"] =
            serde_json::json!(crate::internal::ai::traces::REASONING_ARTIFACT_TOTAL_MAX_BYTES);
        assert!(reasoning_artifact_metadata(&total).is_err());
        let mut null_size = valid.clone();
        null_size["reasoning_artifacts"][0]["byte_len"] = serde_json::Value::Null;
        assert!(reasoning_artifact_metadata(&null_size).is_err());
        let too_many = serde_json::json!({"reasoning_artifacts": vec![valid["reasoning_artifacts"][0].clone(); 513]});
        assert!(reasoning_artifact_metadata(&too_many).is_err());
    }

    use std::{
        fs,
        io::Write as _,
        sync::atomic::{AtomicBool, Ordering},
    };

    use sea_orm::{ConnectionTrait, Statement};
    use tempfile::TempDir;

    use super::{test_helpers::*, *};
    use crate::internal::ai::checkpoint_input::{CheckpointInputFile, CheckpointInputSpec};

    fn budget() -> CheckpointReadBudget {
        CheckpointReadBudget::new(Instant::now() + Duration::from_secs(30), Arc::new(|| false))
    }

    fn catalog(storage: &Path, root: &str) -> CheckpointCatalog {
        let kind = git_internal::hash::get_hash_kind();
        CheckpointCatalog {
            storage: storage.to_path_buf(),
            checkpoint_id: RG03_CHECKPOINT_ID.into(),
            tree_oid: crate::internal::object_format::parse_hex_for_kind(kind, root).unwrap(),
            kind,
        }
    }

    fn reset_reads() {
        rg03_clear_body_reads();
        CHECKPOINT_ORDINARY_READS.with(|reads| reads.borrow_mut().clear());
        CHECKPOINT_PROOF_READS.with(|reads| reads.borrow_mut().clear());
    }

    fn assert_only_proof_reads(artifact: ObjectHash) {
        rg03_assert_no_artifact_body_reads(artifact);
        CHECKPOINT_ORDINARY_READS.with(|reads| assert!(reads.borrow().is_empty()));
    }

    fn wrap(storage: &Path, inner: ObjectHash) -> String {
        let prefix = rg03_tree(
            storage,
            vec![TreeItem::new(
                TreeItemMode::Tree,
                inner,
                RG03_CHECKPOINT_ID[2..].into(),
            )],
        );
        let checkpoints = rg03_tree(
            storage,
            vec![TreeItem::new(
                TreeItemMode::Tree,
                prefix,
                RG03_CHECKPOINT_ID[..2].into(),
            )],
        );
        rg03_tree(
            storage,
            vec![TreeItem::new(
                TreeItemMode::Tree,
                checkpoints,
                "checkpoint".into(),
            )],
        )
        .to_string()
    }

    fn leaf(storage: &Path, root: &str) -> Tree {
        let root = read_tree_object(storage, root).unwrap();
        let checkpoints = subtree(storage, &root, "checkpoint").unwrap();
        let prefix = subtree(storage, &checkpoints, &RG03_CHECKPOINT_ID[..2]).unwrap();
        subtree(storage, &prefix, &RG03_CHECKPOINT_ID[2..]).unwrap()
    }

    fn ordinary_spec(storage: &Path, root: &str) -> CheckpointInputSpec {
        let (files, _) = prove_catalog_roles(&catalog(storage, root)).unwrap();
        CheckpointInputSpec {
            checkpoint_id: RG03_CHECKPOINT_ID.into(),
            files,
        }
    }

    #[test]
    fn fix_rg_scoped_01_role_closure() {
        // Pin actionable diagnostics without including any saved file body.
        for (error, expected) in [
            (
                CheckpointReaderError::RoleProof,
                "checkpoint catalog row is inconsistent; run `libra agent doctor` to inspect the store",
            ),
            (
                CheckpointReaderError::InvalidSavedSpec("invalid checkpoint id".into()),
                "invalid checkpoint id",
            ),
            (
                CheckpointReaderError::CatalogUnavailable,
                "checkpoint catalog is unavailable; retry after the repository writer finishes, or run `libra agent doctor`",
            ),
            (
                CheckpointReaderError::UnknownLayout,
                "checkpoint layout cannot be proved; run `libra agent doctor` and select a captured checkpoint",
            ),
            (
                CheckpointReaderError::Budget,
                "checkpoint input exceeds its work budget or was cancelled; retry with a smaller checkpoint",
            ),
            (
                CheckpointReaderError::ObjectIntegrity,
                "checkpoint object identity or type is inconsistent; run `libra agent doctor`",
            ),
        ] {
            assert_eq!(error.to_string(), expected);
        }
        let dir = TempDir::new().unwrap();
        let (root, artifact) = rg03_read_fixture(dir.path(), "normal", false);
        let spec = ordinary_spec(dir.path(), &root);
        reset_reads();
        let input =
            validate_saved_checkpoint_input(catalog(dir.path(), &root), &spec, &budget()).unwrap();
        assert_eq!(input.files().len(), 3);
        assert_only_proof_reads(artifact);
        CHECKPOINT_PROOF_READS.with(|reads| assert!(!reads.borrow().is_empty()));
        for index in 0..input.files().len() {
            assert!(
                !input
                    .read_ordinary_file(index, 1024 * 1024, &budget())
                    .unwrap()
                    .is_empty()
            );
        }
        CHECKPOINT_ORDINARY_READS.with(|reads| assert_eq!(reads.borrow().len(), 3));
        rg03_assert_no_artifact_body_reads(artifact);

        let inner = leaf(dir.path(), &root);
        let artifacts = reasoning_artifact_tree_oids(dir.path(), &inner).unwrap();
        let manifest = checkpoint_plain_manifest(dir.path(), &inner, &artifacts, &spec.files)
            .unwrap()
            .unwrap();
        let metadata = reasoning_artifact_metadata(&manifest).unwrap();
        let mut full = spec.clone();
        full.files.push(CheckpointInputFile {
            rel_path: metadata[0].path.clone(),
            oid: metadata[0].oid.clone(),
        });
        full.files.reverse();
        reset_reads();
        let input =
            validate_saved_checkpoint_input(catalog(dir.path(), &root), &full, &budget()).unwrap();
        assert_eq!(input.files().len(), spec.files.len());
        assert_only_proof_reads(artifact);

        let mut broken = Vec::new();
        let mut omitted = spec.clone();
        omitted.files.pop();
        broken.push(omitted);
        let mut changed = spec.clone();
        changed.files[0].oid = rg03_blob(dir.path(), b"foreign ordinary").to_string();
        broken.push(changed);
        let mut alias = spec.clone();
        alias.files[0].oid = artifact.to_string();
        broken.push(alias);
        let mut fake_prefix = full.clone();
        fake_prefix.files[0].rel_path.push_str("/other");
        broken.push(fake_prefix);
        let mut fake_artifact = full.clone();
        fake_artifact.files[0].oid = spec.files[0].oid.clone();
        broken.push(fake_artifact);
        for spec in broken {
            reset_reads();
            assert!(
                validate_saved_checkpoint_input(catalog(dir.path(), &root), &spec, &budget())
                    .is_err()
            );
            assert_only_proof_reads(artifact);
        }
        for shape in [
            "manifest_metadata_other",
            "manifest_metadata_path",
            "tree_alias",
            "unknown_reasoning",
        ] {
            let (root, artifact) = rg03_read_fixture(dir.path(), shape, false);
            reset_reads();
            assert!(
                validate_saved_checkpoint_input(catalog(dir.path(), &root), &spec, &budget())
                    .is_err()
            );
            assert_only_proof_reads(artifact);
        }
        // Original wrapped-v1 checkpoints carried no manifest or artifacts.
        // Different ordinary paths are allowed to reference the same blob.
        let shared = rg03_blob(dir.path(), b"legacy ordinary");
        let inner = rg03_tree(
            dir.path(),
            vec![
                TreeItem::new(TreeItemMode::Blob, shared, "metadata.json".into()),
                TreeItem::new(TreeItemMode::Blob, shared, "legacy.txt".into()),
            ],
        );
        let legacy = wrap(dir.path(), inner);
        let spec = ordinary_spec(dir.path(), &legacy);
        reset_reads();
        assert_eq!(
            validate_saved_checkpoint_input(catalog(dir.path(), &legacy), &spec, &budget())
                .unwrap()
                .files()
                .len(),
            2
        );
        CHECKPOINT_ORDINARY_READS.with(|reads| assert!(reads.borrow().is_empty()));
        assert!(matches!(
            validate_saved_checkpoint_input(
                catalog(dir.path(), &inner.to_string()),
                &spec,
                &budget()
            ),
            Err(CheckpointReaderError::UnknownLayout)
        ));
    }

    async fn seed_catalog(storage: &Path, root: &str) {
        let db = storage.join(crate::utils::util::DATABASE);
        let connection = if db.exists() {
            crate::internal::db::establish_connection(db.to_str().unwrap())
                .await
                .unwrap()
        } else {
            crate::internal::db::create_database(db.to_str().unwrap())
                .await
                .unwrap()
        };
        let backend = connection.get_database_backend();
        connection.execute_raw(Statement::from_string(backend,
            "INSERT INTO agent_session (session_id, agent_kind, provider_session_id, state, working_dir, metadata_json, redaction_report, started_at, last_event_at) VALUES ('core-fixture', 'claude_code', 'core-fixture', 'stopped', 'synthetic', '{}', '{}', 0, 0)"
        )).await.unwrap();
        connection.execute_raw(Statement::from_sql_and_values(backend,
            "INSERT INTO agent_checkpoint (checkpoint_id, session_id, scope, parent_commit, tree_oid, metadata_blob_oid, traces_commit, created_at) VALUES (?, 'core-fixture', 'committed', NULL, ?, ?, ?, 0)",
            [RG03_CHECKPOINT_ID.into(), root.into(), root.into(), root.into()],
        )).await.unwrap();
        connection.close().await.unwrap();
    }

    #[tokio::test]
    #[serial_test::serial(cwd, env)]
    async fn fix_rg_scoped_01_catalog_identity() {
        use sea_orm::sqlx::{
            Connection,
            sqlite::{SqliteConnectOptions, SqliteConnection},
        };
        let a = TempDir::new().unwrap();
        let b = TempDir::new().unwrap();
        let a_storage = a.path().join(".libra");
        let b_storage = b.path().join(".libra");
        fs::create_dir(&a_storage).unwrap();
        fs::create_dir(&b_storage).unwrap();
        let (root_a, _) = rg03_read_fixture(&a_storage, "normal", false);
        let (root_b, artifact) = rg03_read_fixture(&b_storage, "normal", true);
        seed_catalog(&a_storage, &root_a).await;
        seed_catalog(&b_storage, &root_b).await;
        let db = b_storage.join(crate::utils::util::DATABASE);
        let bytes = fs::read(&db).unwrap();
        let modified = fs::metadata(&db).unwrap().modified().unwrap();
        let _cwd = crate::utils::test::ChangeDirGuard::new(a.path());
        let identity = load_checkpoint_catalog(&b_storage, RG03_CHECKPOINT_ID, &budget())
            .await
            .unwrap();
        assert_eq!(identity.tree_oid.to_string(), root_b);
        assert_ne!(identity.tree_oid.to_string(), root_a);
        let spec = ordinary_spec(&b_storage, &root_b);
        reset_reads();
        assert!(validate_saved_checkpoint_input(identity, &spec, &budget()).is_ok());
        assert_only_proof_reads(artifact);
        assert_eq!(fs::read(&db).unwrap(), bytes);
        assert_eq!(fs::metadata(&db).unwrap().modified().unwrap(), modified);
        assert!(!b_storage.join("libra.db-wal").exists());
        assert!(!b_storage.join("libra.db-journal").exists());
        let missing = TempDir::new().unwrap();
        assert!(matches!(
            load_checkpoint_catalog(missing.path(), RG03_CHECKPOINT_ID, &budget()).await,
            Err(CheckpointReaderError::CatalogUnavailable)
        ));
        assert!(!missing.path().join(crate::utils::util::DATABASE).exists());
        assert!(matches!(
            load_checkpoint_catalog(&b_storage, "unknown-checkpoint", &budget()).await,
            Err(CheckpointReaderError::InvalidSavedSpec(_))
        ));

        let mut connection = SqliteConnection::connect_with(
            &SqliteConnectOptions::new()
                .filename(&db)
                .busy_timeout(Duration::ZERO),
        )
        .await
        .unwrap();
        // An independent exclusive writer prevents a reader from acquiring
        // SQLite's read lock. No sleep or mocked busy result is involved.
        sea_orm::sqlx::query("BEGIN EXCLUSIVE")
            .execute(&mut connection)
            .await
            .unwrap();
        let started = Instant::now();
        assert!(matches!(
            load_checkpoint_catalog(&b_storage, RG03_CHECKPOINT_ID, &budget()).await,
            Err(CheckpointReaderError::CatalogUnavailable)
        ));
        assert!(started.elapsed() < Duration::from_secs(2));
        sea_orm::sqlx::query("ROLLBACK")
            .execute(&mut connection)
            .await
            .unwrap();
        assert!(
            load_checkpoint_catalog(&b_storage, RG03_CHECKPOINT_ID, &budget())
                .await
                .is_ok()
        );
        assert_eq!(fs::read(&db).unwrap(), bytes);
        sea_orm::sqlx::query("UPDATE agent_checkpoint SET tree_oid = ? WHERE checkpoint_id = ?")
            .bind(&root_a)
            .bind(RG03_CHECKPOINT_ID)
            .execute(&mut connection)
            .await
            .unwrap();
        let changed = load_checkpoint_catalog(&b_storage, RG03_CHECKPOINT_ID, &budget())
            .await
            .unwrap();
        reset_reads();
        assert!(validate_saved_checkpoint_input(changed, &spec, &budget()).is_err());
        assert_only_proof_reads(artifact);
        sea_orm::sqlx::query("DELETE FROM agent_checkpoint WHERE checkpoint_id = ?")
            .bind(RG03_CHECKPOINT_ID)
            .execute(&mut connection)
            .await
            .unwrap();
        assert!(matches!(
            load_checkpoint_catalog(&b_storage, RG03_CHECKPOINT_ID, &budget()).await,
            Err(CheckpointReaderError::InvalidSavedSpec(_))
        ));
        sea_orm::sqlx::query("DROP TABLE agent_checkpoint")
            .execute(&mut connection)
            .await
            .unwrap();
        assert!(matches!(
            load_checkpoint_catalog(&b_storage, RG03_CHECKPOINT_ID, &budget()).await,
            Err(CheckpointReaderError::InvalidSavedSpec(_))
        ));
        connection.close().await.unwrap();
        let elapsed = CheckpointReadBudget::new(Instant::now(), Arc::new(|| false));
        assert!(matches!(
            load_checkpoint_catalog(&a_storage, RG03_CHECKPOINT_ID, &elapsed).await,
            Err(CheckpointReaderError::Budget)
        ));
        let cancelled =
            CheckpointReadBudget::new(Instant::now() + Duration::from_secs(1), Arc::new(|| true));
        assert!(matches!(
            load_checkpoint_catalog(&a_storage, RG03_CHECKPOINT_ID, &cancelled).await,
            Err(CheckpointReaderError::Budget)
        ));
        // Observe genuine SQLite VM progress after acquisition. Cancellation
        // and a late query completion must join the actual dedicated owner.
        for cancel_during_query in [true, false] {
            use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
            let progress = Arc::new(AtomicUsize::new(0));
            let callback_finished = Arc::new(AtomicBool::new(false));
            let lease_finished = Arc::new(AtomicBool::new(false));
            let pool_finished = Arc::new(AtomicBool::new(false));
            let cancelled = Arc::new(AtomicBool::new(false));
            let observed = (
                progress.clone(),
                callback_finished.clone(),
                lease_finished.clone(),
                pool_finished.clone(),
                cancelled.clone(),
            );
            let actual_cancel = cancelled.clone();
            let mut pending_budget = CheckpointReadBudget::new(
                Instant::now() + Duration::from_secs(2),
                Arc::new(move || actual_cancel.load(Ordering::SeqCst)),
            );
            pending_budget.catalog_observer = Some(Arc::new(move |phase| match phase {
                CatalogTestPhase::Progress => {
                    if observed.0.fetch_add(1, Ordering::SeqCst) == 0 {
                        if cancel_during_query {
                            observed.4.store(true, Ordering::SeqCst);
                        } else {
                            // SQLite is genuinely executing on its worker while
                            // the async phase's unchanged 200ms deadline expires.
                            std::thread::sleep(Duration::from_millis(250));
                        }
                        observed.1.store(true, Ordering::SeqCst);
                    }
                }
                CatalogTestPhase::ConnectionCloseFinished => {
                    observed.2.store(true, Ordering::SeqCst);
                }
                CatalogTestPhase::PoolCloseFinished => {
                    observed.3.store(true, Ordering::SeqCst);
                }
            }));
            let original_bytes = fs::read(a_storage.join(crate::utils::util::DATABASE)).unwrap();
            let original_mtime = fs::metadata(a_storage.join(crate::utils::util::DATABASE))
                .unwrap()
                .modified()
                .unwrap();
            reset_reads();
            let pending =
                load_checkpoint_catalog(&a_storage, RG03_CHECKPOINT_ID, &pending_budget).await;
            if cancel_during_query {
                assert!(matches!(pending, Err(CheckpointReaderError::Budget)));
            } else {
                assert!(matches!(
                    pending,
                    Err(CheckpointReaderError::CatalogUnavailable)
                ));
            }
            assert!(
                progress.load(Ordering::SeqCst) > 0,
                "real acquired SQLite progress"
            );
            assert!(
                callback_finished.load(Ordering::SeqCst),
                "worker callback actually finished"
            );
            assert!(
                lease_finished.load(Ordering::SeqCst),
                "close future actually completed"
            );
            assert!(
                pool_finished.load(Ordering::SeqCst),
                "pool owner actually joined"
            );
            CHECKPOINT_BODY_READS.with(|reads| assert!(reads.borrow().is_empty()));
            CHECKPOINT_ORDINARY_READS.with(|reads| assert!(reads.borrow().is_empty()));
            assert_eq!(
                fs::read(a_storage.join(crate::utils::util::DATABASE)).unwrap(),
                original_bytes
            );
            assert_eq!(
                fs::metadata(a_storage.join(crate::utils::util::DATABASE))
                    .unwrap()
                    .modified()
                    .unwrap(),
                original_mtime
            );
            let mut independent = SqliteConnection::connect_with(
                &SqliteConnectOptions::new()
                    .filename(a_storage.join(crate::utils::util::DATABASE))
                    .read_only(true)
                    .create_if_missing(false),
            )
            .await
            .unwrap();
            sea_orm::sqlx::query(
                "SELECT checkpoint_id FROM agent_checkpoint WHERE checkpoint_id = ?",
            )
            .bind(RG03_CHECKPOINT_ID)
            .fetch_one(&mut independent)
            .await
            .unwrap();
            independent.close().await.unwrap();
            assert!(
                load_checkpoint_catalog(&a_storage, RG03_CHECKPOINT_ID, &budget())
                    .await
                    .is_ok()
            );
        }
        // Explicit storage format, rather than cwd A or the async runtime
        // thread's ambient format, determines every catalog and object OID.
        for kind in [HashKind::Sha256, HashKind::Blake3] {
            use clap::Parser as _;
            let formatted = TempDir::new().unwrap();
            let args = crate::command::init::InitArgs::parse_from([
                "init",
                "--vault",
                "false",
                "--quiet",
                "--object-format",
                kind.as_str(),
                formatted.path().to_str().unwrap(),
            ]);
            // Exercise the production init writer instead of seeding its config
            // key by hand. The reader must discover the persisted format.
            crate::command::init::init(args).await.unwrap();
            // Init installs its format in this thread. Restore an unrelated
            // ambient format so a reader using it instead of storage must fail.
            let _ambient = git_internal::hash::set_hash_kind_for_test(HashKind::Sha1);
            let storage = formatted.path().join(".libra");
            let (root, saved) = {
                let _scope = SynchronousReadScope::enter(kind, &budget()).unwrap();
                let (root, _) = rg03_read_fixture(&storage, "normal", false);
                let saved = ordinary_spec(&storage, &root);
                (root, saved)
            };
            seed_catalog(&storage, &root).await;
            let identity = load_checkpoint_catalog(&storage, RG03_CHECKPOINT_ID, &budget())
                .await
                .unwrap();
            assert_eq!(identity.kind, kind);
            let validated = validate_saved_checkpoint_input(identity, &saved, &budget()).unwrap();
            assert_eq!(validated.files().len(), 3);
            assert!(
                validated
                    .read_ordinary_file(
                        0,
                        crate::internal::ai::checkpoint_input::CHECKPOINT_INPUT_MAX_FILE_BYTES,
                        &budget(),
                    )
                    .is_ok()
            );
        }
    }

    #[test]
    fn fix_rg_scoped_01_alias_and_budget() {
        use sha2::{Digest, Sha256};
        let dir = TempDir::new().unwrap();
        let (root, canary) = rg03_read_fixture(dir.path(), "normal", false);
        let spec = ordinary_spec(dir.path(), &root);
        let mut duplicate = spec.clone();
        duplicate.files.push(duplicate.files[0].clone());
        let mut traversal = spec.clone();
        traversal.files[0].rel_path = "../outside".into();
        let mut large_path = spec.clone();
        large_path.files[0].rel_path = "x".repeat(CHECKPOINT_ORDINARY_MAX_PATH_BYTES + 1);
        for saved in [duplicate, traversal, large_path] {
            reset_reads();
            assert!(
                validate_saved_checkpoint_input(catalog(dir.path(), &root), &saved, &budget())
                    .is_err()
            );
            CHECKPOINT_BODY_READS.with(|reads| assert!(reads.borrow().is_empty()));
        }
        const CHECKPOINT_SAVED_MAX_TOTAL_PATH_BYTES: usize =
            CHECKPOINT_ORDINARY_MAX_TOTAL_PATH_BYTES
                + crate::internal::ai::traces::REASONING_ARTIFACT_MAX_ENTRIES
                    * CANONICAL_REASONING_ARTIFACT_PATH_BYTES;
        // Saved total-path cap + one is refused before deduplication,
        // sanitization, role-map allocation or even a proof metadata read.
        let mut total_path_over = spec.clone();
        total_path_over.files = vec![
            CheckpointInputFile {
                rel_path: "x".repeat(CHECKPOINT_ORDINARY_MAX_PATH_BYTES),
                oid: spec.files[0].oid.clone(),
            };
            CHECKPOINT_SAVED_MAX_TOTAL_PATH_BYTES
                / CHECKPOINT_ORDINARY_MAX_PATH_BYTES
        ];
        total_path_over.files.push(CheckpointInputFile {
            rel_path: "x".repeat(
                CHECKPOINT_SAVED_MAX_TOTAL_PATH_BYTES % CHECKPOINT_ORDINARY_MAX_PATH_BYTES + 1,
            ),
            oid: spec.files[0].oid.clone(),
        });
        assert_eq!(
            total_path_over
                .files
                .iter()
                .map(|file| file.rel_path.len())
                .sum::<usize>(),
            CHECKPOINT_SAVED_MAX_TOTAL_PATH_BYTES + 1
        );
        reset_reads();
        assert!(matches!(
            validate_saved_checkpoint_input(
                catalog(dir.path(), &root),
                &total_path_over,
                &budget()
            ),
            Err(CheckpointReaderError::Budget)
        ));
        CHECKPOINT_BODY_READS.with(|reads| assert!(reads.borrow().is_empty()));
        let dag = rg04_shared_dag(dir.path(), 13);
        let (dag_root, artifact) = rg04_root_with_extra_tree(dir.path(), dag);
        reset_reads();
        assert!(
            validate_saved_checkpoint_input(catalog(dir.path(), &dag_root), &spec, &budget())
                .is_err()
        );
        assert_only_proof_reads(artifact);

        // A real, exact 4096 ordinary + 512 artifact fixture: unique opaque
        // objects and locators; ordinary OID aliases remain valid.
        let mut inner = leaf(dir.path(), &root).tree_items;
        let reasoning_index = inner
            .iter()
            .position(|item| item.name == "reasoning")
            .unwrap();
        let manifest_index = inner
            .iter()
            .position(|item| item.name == "manifest.json")
            .unwrap();
        let old_inner = leaf(dir.path(), &root);
        let old_artifacts = reasoning_artifact_tree_oids(dir.path(), &old_inner).unwrap();
        let mut manifest =
            checkpoint_plain_manifest(dir.path(), &old_inner, &old_artifacts, &spec.files)
                .unwrap()
                .unwrap();
        let mut encrypted = Vec::new();
        let mut metadata = Vec::new();
        for index in 0..crate::internal::ai::traces::REASONING_ARTIFACT_MAX_ENTRIES {
            let body = format!("opaque bounded artifact {index}").into_bytes();
            let oid = rg03_blob(dir.path(), &body);
            let sha = format!("{:x}", Sha256::digest(&body));
            encrypted.push(TreeItem::new(TreeItemMode::Blob, oid, sha.clone()));
            metadata.push(serde_json::json!({
                "path":format!("reasoning/encrypted/{sha}"), "oid":oid.to_string(), "sha256":sha,
                "locator":format!("claude_code:msg={index}/part=0/metadata=signature"), "provider":"claude_code",
                "source_kind":"signature", "availability":"encrypted_unavailable", "decrypt_capability":"none", "byte_len":body.len(),
            }));
        }
        manifest["reasoning_artifacts"] = serde_json::json!(metadata);
        let encrypted_tree = rg03_tree(dir.path(), encrypted);
        let reasoning = rg03_tree(
            dir.path(),
            vec![TreeItem::new(
                TreeItemMode::Tree,
                encrypted_tree,
                "encrypted".into(),
            )],
        );
        inner[reasoning_index].id = reasoning;
        inner[manifest_index].id = rg03_blob(dir.path(), &serde_json::to_vec(&manifest).unwrap());
        let ordinary_oid = rg03_blob(dir.path(), b"ordinary boundary payload");
        for index in 0..CHECKPOINT_ORDINARY_MAX_FILES - 3 {
            inner.push(TreeItem::new(
                TreeItemMode::Blob,
                ordinary_oid,
                format!("f{index:04}"),
            ));
        }
        let bound_root = wrap(dir.path(), rg03_tree(dir.path(), inner.clone()));
        let mut full = ordinary_spec(dir.path(), &bound_root);
        assert_eq!(full.files.len(), CHECKPOINT_ORDINARY_MAX_FILES);
        for item in reasoning_artifact_metadata(&manifest).unwrap() {
            full.files.push(CheckpointInputFile {
                rel_path: item.path,
                oid: item.oid,
            });
        }
        assert_eq!(full.files.len(), 4608);
        reset_reads();
        let input =
            validate_saved_checkpoint_input(catalog(dir.path(), &bound_root), &full, &budget())
                .unwrap();
        assert_eq!(input.files().len(), 4096);
        CHECKPOINT_ORDINARY_READS.with(|reads| assert!(reads.borrow().is_empty()));
        for item in reasoning_artifact_metadata(&manifest).unwrap() {
            CHECKPOINT_BODY_READS.with(|reads| assert!(!reads.borrow().contains(&item.oid)));
        }
        let mut plus_one = full.clone();
        plus_one.files.push(CheckpointInputFile {
            rel_path: "additional".into(),
            oid: ordinary_oid.to_string(),
        });
        reset_reads();
        assert!(matches!(
            validate_saved_checkpoint_input(catalog(dir.path(), &bound_root), &plus_one, &budget()),
            Err(CheckpointReaderError::Budget)
        ));
        CHECKPOINT_BODY_READS.with(|reads| assert!(reads.borrow().is_empty()));
        inner.push(TreeItem::new(
            TreeItemMode::Blob,
            ordinary_oid,
            "ordinary-plus-one".into(),
        ));
        let over_root = wrap(dir.path(), rg03_tree(dir.path(), inner));
        reset_reads();
        assert!(matches!(
            validate_saved_checkpoint_input(catalog(dir.path(), &over_root), &spec, &budget()),
            Err(CheckpointReaderError::Budget)
        ));
        assert_only_proof_reads(canary);

        let mut artifact_inner = leaf(dir.path(), &bound_root).tree_items;
        let mut encrypted_items = read_tree_object(dir.path(), &encrypted_tree.to_string())
            .unwrap()
            .tree_items;
        let extra_body = b"opaque artifact beyond entry budget";
        let extra_oid = rg03_blob(dir.path(), extra_body);
        let extra_sha = format!("{:x}", Sha256::digest(extra_body));
        encrypted_items.push(TreeItem::new(
            TreeItemMode::Blob,
            extra_oid,
            extra_sha.clone(),
        ));
        let extra_encrypted = rg03_tree(dir.path(), encrypted_items);
        let sorted_reasoning_index = artifact_inner
            .iter()
            .position(|item| item.name == "reasoning")
            .unwrap();
        assert_eq!(
            read_tree_object(dir.path(), &extra_encrypted.to_string())
                .unwrap()
                .tree_items
                .len(),
            513
        );
        artifact_inner[sorted_reasoning_index].id = rg03_tree(
            dir.path(),
            vec![TreeItem::new(
                TreeItemMode::Tree,
                extra_encrypted,
                "encrypted".into(),
            )],
        );
        let artifact_over_root = wrap(dir.path(), rg03_tree(dir.path(), artifact_inner));
        let mut artifact_over_spec = full.clone();
        artifact_over_spec.files.remove(0);
        artifact_over_spec.files.push(CheckpointInputFile {
            rel_path: format!("reasoning/encrypted/{extra_sha}"),
            oid: extra_oid.to_string(),
        });
        assert_eq!(artifact_over_spec.files.len(), 4608);
        reset_reads();
        assert!(matches!(
            validate_saved_checkpoint_input(
                catalog(dir.path(), &artifact_over_root),
                &artifact_over_spec,
                &budget()
            ),
            Err(CheckpointReaderError::RoleProof)
        ));
        assert_only_proof_reads(extra_oid);

        // Actual expanded path bytes at 8 MiB, with no artifact allowance
        // available to ordinary roles. The next byte must be refused.
        let mut path_items = Vec::new();
        for index in 0..CHECKPOINT_ORDINARY_MAX_FILES {
            path_items.push(TreeItem::new(
                TreeItemMode::Blob,
                ordinary_oid,
                format!("f{index:04}{}", "x".repeat(2043)),
            ));
        }
        let path_root = wrap(dir.path(), rg03_tree(dir.path(), path_items.clone()));
        let path_spec = ordinary_spec(dir.path(), &path_root);
        assert_eq!(
            path_spec
                .files
                .iter()
                .map(|file| file.rel_path.len())
                .sum::<usize>(),
            CHECKPOINT_ORDINARY_MAX_TOTAL_PATH_BYTES
        );
        reset_reads();
        assert!(
            validate_saved_checkpoint_input(catalog(dir.path(), &path_root), &path_spec, &budget())
                .is_ok()
        );
        CHECKPOINT_ORDINARY_READS.with(|reads| assert!(reads.borrow().is_empty()));
        path_items[0].name.push('x');
        let path_over_root = wrap(dir.path(), rg03_tree(dir.path(), path_items));
        reset_reads();
        assert!(matches!(
            validate_saved_checkpoint_input(
                catalog(dir.path(), &path_over_root),
                &path_spec,
                &budget()
            ),
            Err(CheckpointReaderError::Budget)
        ));
        CHECKPOINT_ORDINARY_READS.with(|reads| assert!(reads.borrow().is_empty()));

        let path_inner = rg03_tree(
            dir.path(),
            vec![TreeItem::new(
                TreeItemMode::Blob,
                ordinary_oid,
                "p".repeat(4096),
            )],
        );
        let path_exact_root = wrap(dir.path(), path_inner);
        let path_exact_spec = ordinary_spec(dir.path(), &path_exact_root);
        assert!(
            validate_saved_checkpoint_input(
                catalog(dir.path(), &path_exact_root),
                &path_exact_spec,
                &budget()
            )
            .is_ok()
        );
        let path_plus_inner = rg03_tree(
            dir.path(),
            vec![TreeItem::new(
                TreeItemMode::Blob,
                ordinary_oid,
                "p".repeat(4097),
            )],
        );
        reset_reads();
        assert!(matches!(
            validate_saved_checkpoint_input(
                catalog(dir.path(), &wrap(dir.path(), path_plus_inner)),
                &path_exact_spec,
                &budget()
            ),
            Err(CheckpointReaderError::Budget)
        ));
        CHECKPOINT_ORDINARY_READS.with(|reads| assert!(reads.borrow().is_empty()));

        // Proof authorizes an OID, while each body read still verifies its
        // actual blob header and content-addressed identity.
        let input =
            validate_saved_checkpoint_input(catalog(dir.path(), &root), &spec, &budget()).unwrap();
        let oid = input.files()[0].oid.clone();
        let object = dir.path().join("objects").join(&oid[..2]).join(&oid[2..]);
        let mut encoder =
            flate2::write::ZlibEncoder::new(Vec::new(), flate2::Compression::default());
        encoder.write_all(b"tree 0\0").unwrap();
        fs::write(&object, encoder.finish().unwrap()).unwrap();
        reset_reads();
        assert!(matches!(
            input.read_ordinary_file(0, 1024, &budget()),
            Err(CheckpointReaderError::ObjectIntegrity)
        ));
        CHECKPOINT_BODY_READS.with(|reads| assert!(reads.borrow().is_empty()));
        let cancellation = Arc::new(AtomicBool::new(true));
        let flag = cancellation.clone();
        let cancelled = CheckpointReadBudget::new(
            Instant::now() + Duration::from_secs(1),
            Arc::new(move || flag.load(Ordering::SeqCst)),
        );
        assert!(matches!(
            input.read_ordinary_file(0, 1024, &cancelled),
            Err(CheckpointReaderError::Budget)
        ));
    }
}
