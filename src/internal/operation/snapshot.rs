//! Bounded working-copy snapshots for operation-log v2.
//!
//! A workspace snapshot is a content-addressed manifest, not a Git commit.
//! The scanner records the index view and the visible working-copy files while
//! keeping the raw index bytes as a separate blob.  This deliberately leaves
//! publication and pointer advancement to the operation middleware.

use std::{
    collections::{BTreeMap, BTreeSet},
    fs,
    io,
    path::{Path, PathBuf},
    str::FromStr,
    sync::Arc,
    time::{Duration, Instant},
};

use git_internal::{
    hash::ObjectHash,
    internal::{
        index::Index,
        object::{
            tree::{Tree, TreeItem, TreeItemMode},
            ObjectTrait,
            types::ObjectType,
        },
    },
};
use serde::{Deserialize, Serialize};
use thiserror::Error;

use super::{
    facet::RestorePolicy,
    view::{CapturePolicy, Completeness, HeadState, WorkspaceSnapshotV2, WORKSPACE_SNAPSHOT_SCHEMA_VERSION},
    PinnedRequestScope,
};
use crate::{
    internal::worktree_io::{
        default_worktree_io,
        executor::WorktreeIo,
        protocol::{
            path_to_bytes, relative_worktree_path, IoEvent, IoRequest, unwrap_wire,
        },
    },
    utils::{
        client_storage::ClientStorage,
        ignore::{self, IgnorePolicy},
    },
};

const DEFAULT_TIMEOUT: Duration = Duration::from_secs(5);
const DEFAULT_MAX_FILES: usize = 100_000;
const DEFAULT_MAX_BYTES: u64 = 512 * 1024 * 1024;

#[derive(Debug, Error)]
pub enum ScanError {
    #[error("working-copy scan failed: {0}")]
    Io(#[from] io::Error),
    #[error("working-copy scan exceeded its budget: {0}")]
    Budget(String),
    #[error("working-copy entry is unstable while being read: {0}")]
    Unstable(PathBuf),
    #[error("working-copy I/O worker failed: {0}")]
    Worker(String),
    #[error("index could not be read: {0}")]
    Index(String),
}

#[derive(Debug, Error)]
pub enum SnapshotError {
    #[error(transparent)]
    Scan(#[from] ScanError),
    #[error("snapshot object write failed: {0}")]
    Object(String),
    #[error("snapshot manifest is invalid: {0}")]
    View(#[from] super::view::ViewError),
    #[error("HEAD could not be read: {0}")]
    Head(#[from] io::Error),
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ScanResult {
    pub tracked: BTreeMap<String, ObjectHash>,
    pub untracked: BTreeMap<String, ObjectHash>,
    pub completeness: Completeness,
    pub bytes: u64,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SnapshotOutcome {
    pub snapshot_oid: ObjectHash,
    pub snapshot: WorkspaceSnapshotV2,
    pub changed: bool,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct UntrackedManifest {
    schema_version: u32,
    files: BTreeMap<String, ObjectHash>,
}

/// Captures one pinned worktree.  `capture` never creates an Operation or a
/// commit OID; the caller that owns the operation transaction publishes the
/// manifest and advances the sidecar pointer after its CAS succeeds.
pub struct WorkspaceSnapshotter {
    pub scope: PinnedRequestScope,
    pub(crate) io: Arc<WorktreeIo>,
    pub pointer: super::WorkspaceStatePointer,
    pub capture_policy: CapturePolicy,
    timeout: Duration,
    max_files: usize,
    max_bytes: u64,
}

impl WorkspaceSnapshotter {
    pub fn new(
        scope: PinnedRequestScope,
        pointer: super::WorkspaceStatePointer,
    ) -> Self {
        Self {
            scope,
            io: Arc::new(default_worktree_io()),
            pointer,
            capture_policy: CapturePolicy::TrackedAndUntracked,
            timeout: DEFAULT_TIMEOUT,
            max_files: DEFAULT_MAX_FILES,
            max_bytes: DEFAULT_MAX_BYTES,
        }
    }

    #[allow(dead_code)]
    pub(crate) fn with_io(mut self, io: Arc<WorktreeIo>) -> Self {
        self.io = io;
        self
    }

    pub fn with_limits(mut self, timeout: Duration, max_files: usize, max_bytes: u64) -> Self {
        self.timeout = timeout;
        self.max_files = max_files;
        self.max_bytes = max_bytes;
        self
    }

    /// Scan tracked and visible untracked files through the bounded worker.
    pub async fn scan_working_copy(&self) -> Result<ScanResult, ScanError> {
        let started = Instant::now();
        let index_path = self.scope.gitdir.join("index");
        let index = Index::load(&index_path).map_err(|error| ScanError::Index(error.to_string()))?;
        let all_files = list_visible_files(&self.scope.worktree_root, &index)?;
        let mut tracked_names = BTreeSet::new();
        let mut tracked = BTreeMap::new();
        let mut untracked = BTreeMap::new();
        let mut bytes = 0u64;

        for entry in index.tracked_entries(0) {
            tracked_names.insert(entry.name.clone());
        }
        for relative in all_files {
            if started.elapsed() > self.timeout {
                return Err(ScanError::Budget("scan timeout".to_string()));
            }
            if tracked.len() + untracked.len() >= self.max_files {
                return Err(ScanError::Budget("file-count limit".to_string()));
            }
            let relative = relative_worktree_path(
                &path_to_bytes(&self.scope.worktree_root),
                &relative,
                false,
            )?;
            let key = relative.to_string_lossy().replace('\\', "/");
            let oid = self.hash_file(&relative)?;
            let content_len = fs::metadata(self.scope.worktree_root.join(&relative))?.len();
            bytes = bytes.saturating_add(content_len);
            if bytes > self.max_bytes {
                return Err(ScanError::Budget("byte limit".to_string()));
            }
            if tracked_names.contains(&key) {
                tracked.insert(key, oid);
            } else if matches!(self.capture_policy, CapturePolicy::TrackedAndUntracked) {
                untracked.insert(key, oid);
            }
        }

        let completeness = if tracked_names.iter().all(|name| tracked.contains_key(name)) {
            Completeness::Full
        } else {
            Completeness::Partial
        };
        Ok(ScanResult {
            tracked,
            untracked,
            completeness,
            bytes,
        })
    }

    /// Capture immutable blobs and a canonical `WorkspaceSnapshotV2` manifest.
    pub async fn capture(&mut self) -> Result<SnapshotOutcome, SnapshotError> {
        let scan = self.scan_working_copy().await?;
        let storage = ClientStorage::init_local(self.scope.storage.join("objects"));
        let index_bytes = fs::read(self.scope.gitdir.join("index"))?;
        let raw_index_oid = put_blob(&storage, &index_bytes)?;
        let index = Index::load(self.scope.gitdir.join("index"))
            .map_err(|error| SnapshotError::Object(error.to_string()))?;
        let index_tree_oid = put_tree(&storage, &tree_from_index(&index, &scan.tracked)?)?;
        let working_copy_tree_oid = index_tree_oid;
        let untracked_manifest = UntrackedManifest {
            schema_version: 1,
            files: scan.untracked.clone(),
        };
        let untracked_bytes = serde_json::to_vec(&untracked_manifest)
            .map_err(|error| SnapshotError::Object(error.to_string()))?;
        let untracked_oid = put_blob(&storage, &untracked_bytes)?;
        let snapshot = WorkspaceSnapshotV2 {
            schema_version: WORKSPACE_SNAPSHOT_SCHEMA_VERSION,
            workspace_id: workspace_id(&self.scope),
            head: read_head(&self.scope.gitdir.join("HEAD"))?,
            index_tree_oid,
            raw_index_blob_oid: raw_index_oid,
            working_copy_tree_oid,
            untracked_manifest_oid: untracked_oid,
            sparse_facet_oid: None,
            sequencer_facet_oid: None,
            worktree_generation: self.pointer.generation,
            capture_policy: self.capture_policy,
            completeness: scan.completeness,
            facet_restore_policies: BTreeMap::from([
                (super::FacetName::from("index"), RestorePolicy::AutoRestore),
                (super::FacetName::from("sequencer"), RestorePolicy::AutoRestore),
                (super::FacetName::from("sparse"), RestorePolicy::Rebuild),
            ]),
        };
        let manifest = snapshot.to_canonical_bytes()?;
        let snapshot_oid = put_blob(&storage, &manifest)?;
        Ok(SnapshotOutcome {
            changed: self.pointer.last_snapshot_oid != snapshot_oid,
            snapshot_oid,
            snapshot,
        })
    }

    fn hash_file(&self, relative: &Path) -> Result<ObjectHash, ScanError> {
        let request = IoRequest::FileBlobHash {
            path: path_to_bytes(relative),
            root: path_to_bytes(&self.scope.worktree_root),
            hash_kind: git_internal::hash::get_hash_kind().to_string(),
            root_session: 1,
        };
        let events = self
            .io
            .submit_absolute(request, relative.as_os_str().to_string_lossy().as_bytes().to_vec(), self.timeout)
            .map_err(|error| ScanError::Worker(error.to_string()))?;
        let hex = events.into_iter().find_map(|event| match event {
            IoEvent::DoneHash { hex } => Some(unwrap_wire(hex)),
            _ => None,
        }).ok_or_else(|| ScanError::Worker("hash worker returned no result".to_string()))??;
        let before = fs::metadata(self.scope.worktree_root.join(relative))?.len();
        let after = fs::metadata(self.scope.worktree_root.join(relative))?.len();
        if before != after {
            return Err(ScanError::Unstable(relative.to_path_buf()));
        }
        ObjectHash::from_str(&hex).map_err(|error| ScanError::Worker(error.to_string()))
    }
}

fn list_visible_files(root: &Path, index: &Index) -> Result<Vec<PathBuf>, io::Error> {
    let mut files = Vec::new();
    for item in walkdir::WalkDir::new(root).follow_links(false) {
        let item = item.map_err(|error| io::Error::other(error.to_string()))?;
        let relative = item
            .path()
            .strip_prefix(root)
            .map_err(|error| io::Error::other(error.to_string()))?;
        if relative.as_os_str().is_empty() {
            continue;
        }
        if relative.components().any(|component| {
            matches!(component, std::path::Component::Normal(name) if name == ".git" || name == ".libra")
        }) {
            continue;
        }
        if item.file_type().is_file() || item.file_type().is_symlink() {
            let ignored = ignore::should_ignore(relative, IgnorePolicy::Respect, index);
            if !ignored {
                files.push(relative.to_path_buf());
            }
        }
    }
    files.sort();
    Ok(files)
}

fn put_blob(storage: &ClientStorage, bytes: &[u8]) -> Result<ObjectHash, SnapshotError> {
    let oid = ObjectHash::from_type_and_data(ObjectType::Blob, bytes);
    storage
        .put(&oid, bytes, ObjectType::Blob)
        .map_err(|error| SnapshotError::Object(error.to_string()))?;
    Ok(oid)
}

fn put_tree(storage: &ClientStorage, tree: &Tree) -> Result<ObjectHash, SnapshotError> {
    let bytes = tree.to_data().map_err(|error| SnapshotError::Object(error.to_string()))?;
    storage
        .put(&tree.id, &bytes, ObjectType::Tree)
        .map_err(|error| SnapshotError::Object(error.to_string()))?;
    Ok(tree.id)
}

fn tree_from_index(index: &Index, current: &BTreeMap<String, ObjectHash>) -> Result<Tree, SnapshotError> {
    let mut entries = BTreeMap::new();
    for entry in index.tracked_entries(0) {
        let oid = current.get(&entry.name).copied().unwrap_or(entry.hash);
        let mode = match entry.mode & 0o170000 {
            0o120000 => TreeItemMode::Link,
            0o160000 => TreeItemMode::Commit,
            _ if entry.mode & 0o111 != 0 => TreeItemMode::BlobExecutable,
            _ => TreeItemMode::Blob,
        };
        entries.insert(entry.name.clone(), (mode, oid));
    }
    let mut items = Vec::new();
    for (name, (mode, oid)) in entries {
        items.push(TreeItem::new(mode, oid, name));
    }
    items.sort_by_key(|item| {
        let mut key = item.name.as_bytes().to_vec();
        if item.mode == TreeItemMode::Tree { key.push(b'/'); }
        key
    });
    let bytes = items.iter().flat_map(|item| item.to_data()).collect::<Vec<_>>();
    Ok(Tree { id: ObjectHash::from_type_and_data(ObjectType::Tree, &bytes), tree_items: items })
}

fn workspace_id(scope: &PinnedRequestScope) -> String {
    scope
        .scope
        .worktree_id()
        .map(str::to_string)
        .unwrap_or_else(|| "main".to_string())
}

fn read_head(path: &Path) -> Result<HeadState, io::Error> {
    let value = fs::read_to_string(path)?;
    let value = value.trim();
    if let Some(reference) = value.strip_prefix("ref: ") {
        return Ok(HeadState::Symbolic { reference: reference.to_string() });
    }
    let oid = ObjectHash::from_str(value).map_err(|error| io::Error::other(error.to_string()))?;
    Ok(HeadState::Detached { oid })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn untracked_manifest_is_deterministic() {
        let manifest = UntrackedManifest { schema_version: 1, files: BTreeMap::from([("a".into(), ObjectHash::new(&[1; 20]))]) };
        let bytes = serde_json::to_vec(&manifest).expect("manifest serializes");
        assert_eq!(bytes, br#"{"schema_version":1,"files":{"a":"0000000000000000000000000000000000000001"}}"#);
    }
}
