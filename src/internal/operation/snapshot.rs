//! Bounded working-copy snapshots for operation-log v2.
//!
//! A workspace snapshot is a content-addressed manifest, not a Git commit.
//! The scanner records the index view and the visible working-copy files while
//! keeping the raw index bytes as a separate blob.  This deliberately leaves
//! publication and pointer advancement to the operation middleware.

#[cfg(unix)]
use std::os::unix::fs::PermissionsExt;
use std::{
    collections::{BTreeMap, BTreeSet, VecDeque},
    fs,
    io::{self, Read},
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
            tree::{TreeItem, TreeItemMode},
            types::ObjectType,
        },
    },
};
use serde::{Deserialize, Serialize};
use thiserror::Error;

use super::{
    PinnedRequestScope,
    facet::{FacetCaptureCtx, FacetError},
    facets::registry_for_scope,
    view::{
        CapturePolicy, Completeness, HeadState, WORKSPACE_SNAPSHOT_SCHEMA_VERSION,
        WorkspaceSnapshotV2,
    },
};
use crate::{
    internal::worktree_io::{
        default_worktree_io,
        executor::WorktreeIo,
        protocol::{
            IoEvent, IoRequest, bytes_to_path, path_to_bytes, relative_worktree_path, unwrap_wire,
        },
    },
    utils::{
        client_storage::ClientStorage,
        ignore::{self, IgnorePolicy},
    },
};

mod gitlinks;
use gitlinks::BoundaryMatch;

// A repository with a large working tree must still get a bounded capture,
// but five seconds is too small for the 2k-file rename and compatibility
// fixtures on a busy CI worker.  The deadline remains shared by enumeration,
// hashing, and persistence.
const DEFAULT_TIMEOUT: Duration = Duration::from_secs(30);
const DEFAULT_MAX_FILES: usize = 100_000;
const DEFAULT_MAX_BYTES: u64 = 512 * 1024 * 1024;

fn in_process_test_host() -> bool {
    std::env::current_exe()
        .ok()
        .and_then(|path| path.parent().map(Path::to_path_buf))
        .and_then(|path| path.file_name().map(|name| name == "deps"))
        .unwrap_or(false)
}

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
    #[error("state facet capture failed: {0}")]
    Facet(#[from] FacetError),
    #[error("index metadata could not be read: {0}")]
    Index(String),
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
    pub content_oid: ObjectHash,
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
    storage: Option<ClientStorage>,
    timeout: Duration,
    max_files: usize,
    max_bytes: u64,
}

impl WorkspaceSnapshotter {
    pub fn new(scope: PinnedRequestScope, pointer: super::WorkspaceStatePointer) -> Self {
        Self {
            scope,
            io: Arc::new(default_worktree_io()),
            pointer,
            capture_policy: CapturePolicy::TrackedAndUntracked,
            storage: None,
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

    pub(crate) fn with_storage(mut self, storage: ClientStorage) -> Self {
        self.storage = Some(storage);
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
        self.scan_working_copy_until(Instant::now() + self.timeout)
            .await
    }

    async fn scan_working_copy_until(&self, deadline: Instant) -> Result<ScanResult, ScanError> {
        let index_path = self.scope.gitdir.join("index");
        let (index, index_valid) = match fs::symlink_metadata(&index_path) {
            // Only an actually absent entry is a valid unborn index. A
            // dangling link or inaccessible entry cannot establish boundaries.
            Err(error) if error.kind() == io::ErrorKind::NotFound => (Index::new(), true),
            Err(_) => (Index::new(), false),
            Ok(_) => match fs::metadata(&index_path) {
                Ok(metadata) if metadata.is_file() => {
                    if metadata.len() > self.max_bytes {
                        return Err(ScanError::Budget("raw index byte limit".to_string()));
                    }
                    match Index::from_file(&index_path) {
                        Ok(index) => (index, true),
                        // Raw bytes are captured separately; the wrapped
                        // command remains responsible for its typed error.
                        Err(_) => (Index::new(), false),
                    }
                }
                Ok(_) | Err(_) => (Index::new(), false),
            },
        };
        let mut tracked_names = BTreeSet::new();
        let (all_files, listing_complete) = if index_valid {
            self.list_visible_files(&index, deadline)?
        } else {
            // A corrupt index cannot identify opaque gitlink boundaries. Keep
            // its raw bytes in the partial snapshot without reading user files.
            (Vec::new(), false)
        };
        let mut tracked = BTreeMap::new();
        let mut untracked = BTreeMap::new();
        let mut bytes = 0u64;

        for entry in index.tracked_entries(0) {
            tracked_names.insert(entry.name.clone());
        }
        let mut complete = index_valid && listing_complete && Instant::now() <= deadline;
        for relative in all_files {
            if Instant::now() > deadline {
                complete = false;
                break;
            }
            if tracked.len() + untracked.len() >= self.max_files {
                complete = false;
                break;
            }
            let relative = relative_worktree_path(
                &path_to_bytes(&self.scope.worktree_root),
                &relative,
                false,
            )?;
            let key = relative.to_string_lossy().replace('\\', "/");
            let oid = match self.hash_file(&relative, deadline) {
                Ok(oid) => oid,
                Err(ScanError::Unstable(_)) => {
                    complete = false;
                    continue;
                }
                Err(ScanError::Io(error)) if error.kind() == io::ErrorKind::NotFound => {
                    // The directory listing and the hash are a bounded
                    // snapshot attempt, not a filesystem freeze.  A path
                    // disappearing between them makes the capture partial;
                    // it must not turn an otherwise valid command into an
                    // unrelated fatal I/O error.
                    complete = false;
                    continue;
                }
                Err(error) => return Err(error),
            };
            let content_len = match fs::metadata(self.scope.worktree_root.join(&relative)) {
                Ok(metadata) => metadata.len(),
                Err(error) if error.kind() == io::ErrorKind::NotFound => {
                    complete = false;
                    continue;
                }
                Err(error) => return Err(ScanError::Io(error)),
            };
            bytes = bytes.saturating_add(content_len);
            if bytes > self.max_bytes {
                complete = false;
                break;
            }
            if tracked_names.contains(&key) {
                tracked.insert(key, oid);
            } else if matches!(self.capture_policy, CapturePolicy::TrackedAndUntracked) {
                untracked.insert(key, oid);
            }
        }

        let completeness = if complete {
            // A missing tracked path is a coherent deletion in the working
            // copy, not an incomplete scan.  Listing errors and budget/time
            // exhaustion still set `complete = false` above.
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
        let deadline = Instant::now() + self.timeout;
        // Validate authoritative repository state before scanning or writing snapshot objects.
        let head = read_head(&self.scope).await?;
        let index_path = self.scope.gitdir.join("index");
        let index_before = match fs::metadata(&index_path) {
            Ok(metadata) => Some(metadata),
            Err(error) if error.kind() == io::ErrorKind::NotFound => None,
            Err(error) => return Err(SnapshotError::Index(error.to_string())),
        };
        let scan = self.scan_working_copy_until(deadline).await?;
        let storage = self
            .storage
            .clone()
            .unwrap_or_else(|| ClientStorage::init_local(self.scope.storage.join("objects")));
        let index_bytes = match fs::File::open(&index_path) {
            Ok(file) => {
                let mut bytes = Vec::new();
                file.take(self.max_bytes.saturating_add(1))
                    .read_to_end(&mut bytes)
                    .map_err(|error| SnapshotError::Index(error.to_string()))?;
                if bytes.len() as u64 > self.max_bytes {
                    return Err(SnapshotError::Index("raw index byte limit".to_string()));
                }
                bytes
            }
            Err(error) if error.kind() == io::ErrorKind::NotFound => Vec::new(),
            Err(error) => return Err(SnapshotError::Index(error.to_string())),
        };
        let index_after = match fs::metadata(&index_path) {
            Ok(metadata) => Some(metadata),
            Err(error) if error.kind() == io::ErrorKind::NotFound => None,
            Err(error) => return Err(SnapshotError::Index(error.to_string())),
        };
        let mut completeness = scan.completeness;
        if index_before.as_ref().map(|metadata| metadata.len())
            != index_after.as_ref().map(|metadata| metadata.len())
            || index_before
                .as_ref()
                .and_then(|metadata| metadata.modified().ok())
                != index_after
                    .as_ref()
                    .and_then(|metadata| metadata.modified().ok())
        {
            completeness = Completeness::Partial;
        }
        let raw_index_oid = put_blob(&storage, &index_bytes)?;
        let index = if index_path.exists() {
            match Index::load(&index_path) {
                Ok(index) => index,
                Err(_) => {
                    // Preserve the raw bytes and publish a partial snapshot;
                    // the wrapped command remains responsible for its typed
                    // index-corruption error.
                    completeness = Completeness::Partial;
                    Index::new()
                }
            }
        } else {
            Index::new()
        };
        let registry = registry_for_scope(self.scope.clone(), storage.clone())?;
        let facet_ctx = FacetCaptureCtx {
            repo_id: None,
            workspace_id: Some(workspace_id(&self.scope)),
        };
        let facet_names = [
            super::FacetName::from("index"),
            super::FacetName::from("sequencer"),
            super::FacetName::from("sparse"),
        ];
        let captures = facet_names
            .iter()
            .map(|name| registry.capture(name, &facet_ctx))
            .collect::<Result<Vec<_>, _>>()?;
        registry.validate_captures(&captures)?;
        let sparse_facet_oid = captures
            .iter()
            .find(|capture| capture.facet.as_str() == "sparse")
            .and_then(|capture| capture.payload_oid);
        let sequencer_facet_oid = captures
            .iter()
            .find(|capture| capture.facet.as_str() == "sequencer")
            .and_then(|capture| capture.payload_oid);
        if !registry.is_fully_restorable(&captures) {
            completeness = Completeness::Partial;
        }
        let (tracked, tracked_complete) = self.persist_files(&storage, &scan.tracked, deadline)?;
        let (untracked, untracked_complete) =
            self.persist_files(&storage, &scan.untracked, deadline)?;
        if !tracked_complete || !untracked_complete {
            completeness = Completeness::Partial;
        }
        let index_tree_oid = tree_from_index(&storage, &index)?;
        let working_copy_tree_oid = tree_from_working_copy(
            &storage,
            &index,
            &tracked,
            &untracked,
            &self.scope.worktree_root,
        )?;
        let untracked_manifest = UntrackedManifest {
            schema_version: 1,
            files: untracked,
        };
        let untracked_bytes = serde_json::to_vec(&untracked_manifest)
            .map_err(|error| SnapshotError::Object(error.to_string()))?;
        let untracked_oid = put_blob(&storage, &untracked_bytes)?;
        let snapshot = WorkspaceSnapshotV2 {
            schema_version: WORKSPACE_SNAPSHOT_SCHEMA_VERSION,
            workspace_id: workspace_id(&self.scope),
            head,
            index_tree_oid,
            raw_index_blob_oid: raw_index_oid,
            working_copy_tree_oid,
            untracked_manifest_oid: untracked_oid,
            sparse_facet_oid,
            sequencer_facet_oid,
            worktree_generation: self.pointer.generation,
            capture_policy: self.capture_policy,
            completeness,
            facet_restore_policies: registry.policies(&captures),
        };
        let manifest = snapshot.to_canonical_bytes()?;
        let snapshot_oid = put_blob(&storage, &manifest)?;
        let mut content_snapshot = snapshot.clone();
        content_snapshot.worktree_generation = 0;
        let content_manifest = content_snapshot.to_canonical_bytes()?;
        let content_oid = ObjectHash::from_type_and_data(ObjectType::Blob, &content_manifest);
        let previous_content_oid = self
            .pointer
            .last_content_oid
            .unwrap_or(self.pointer.last_snapshot_oid);
        Ok(SnapshotOutcome {
            changed: previous_content_oid != content_oid,
            content_oid,
            snapshot_oid,
            snapshot,
        })
    }

    fn persist_files(
        &self,
        storage: &ClientStorage,
        files: &BTreeMap<String, ObjectHash>,
        deadline: Instant,
    ) -> Result<(BTreeMap<String, ObjectHash>, bool), SnapshotError> {
        let mut persisted = BTreeMap::new();
        let mut complete = true;
        for (path, expected_oid) in files {
            if Instant::now() > deadline {
                complete = false;
                break;
            }
            match self.read_stable_file(Path::new(path), deadline) {
                Ok((oid, bytes)) if &oid == expected_oid => {
                    put_blob(storage, &bytes)?;
                    persisted.insert(path.clone(), oid);
                }
                Ok(_) | Err(ScanError::Unstable(_)) | Err(ScanError::Budget(_)) => {
                    complete = false;
                }
                Err(ScanError::Io(error)) if error.kind() == io::ErrorKind::NotFound => {
                    complete = false;
                }
                Err(error) => return Err(SnapshotError::Scan(error)),
            }
        }
        Ok((persisted, complete))
    }

    fn read_stable_file(
        &self,
        relative: &Path,
        deadline: Instant,
    ) -> Result<(ObjectHash, Vec<u8>), ScanError> {
        let path = self.scope.worktree_root.join(relative);
        let before = fs::symlink_metadata(&path)?;
        if before.len() > self.max_bytes {
            return Err(ScanError::Budget("file-size limit".to_string()));
        }
        let bytes = if before.file_type().is_symlink() {
            fs::read_link(&path).map(|target| {
                target
                    .as_os_str()
                    .to_string_lossy()
                    .into_owned()
                    .into_bytes()
            })?
        } else {
            fs::read(&path)?
        };
        if Instant::now() > deadline {
            return Err(ScanError::Budget(
                "snapshot persistence timeout".to_string(),
            ));
        }
        let after = fs::symlink_metadata(&path)?;
        if before.len() != after.len()
            || before.modified().ok() != after.modified().ok()
            || before.file_type().is_symlink() != after.file_type().is_symlink()
        {
            return Err(ScanError::Unstable(relative.to_path_buf()));
        }
        let oid = ObjectHash::from_type_and_data(ObjectType::Blob, &bytes);
        Ok((oid, bytes))
    }

    fn hash_file(&self, relative: &Path, deadline: Instant) -> Result<ObjectHash, ScanError> {
        let before = fs::metadata(self.scope.worktree_root.join(relative))?;
        let request = || IoRequest::FileBlobHash {
            path: path_to_bytes(relative),
            root: path_to_bytes(&self.scope.worktree_root),
            hash_kind: git_internal::hash::get_hash_kind().to_string(),
            root_session: 1,
        };
        let timeout = deadline.saturating_duration_since(Instant::now());
        if timeout.is_zero() {
            return Err(ScanError::Budget("snapshot scan timeout".to_string()));
        }
        let path_key = relative.as_os_str().to_string_lossy().as_bytes().to_vec();
        let events = if in_process_test_host() {
            // Library and test binaries intentionally cannot spawn the CLI
            // worker.  Keep the same WorktreeIo capability handler there;
            // the production CLI always uses the killable absolute-deadline
            // worker below.
            self.io
                .submit_in_process(request(), path_key, timeout)
                .map_err(|error| ScanError::Worker(format!("hash fallback: {error}")))?
        } else {
            self.io
                .submit_absolute(request(), path_key, timeout)
                .map_err(|error| ScanError::Worker(error.to_string()))?
        };
        let hex = events
            .into_iter()
            .find_map(|event| match event {
                IoEvent::DoneHash { hex } => Some(unwrap_wire(hex)),
                _ => None,
            })
            .ok_or_else(|| ScanError::Worker("hash worker returned no result".to_string()))??;
        let after = fs::metadata(self.scope.worktree_root.join(relative))?;
        if before.len() != after.len() || before.modified().ok() != after.modified().ok() {
            return Err(ScanError::Unstable(relative.to_path_buf()));
        }
        ObjectHash::from_str(&hex).map_err(|error| ScanError::Worker(error.to_string()))
    }
    fn list_visible_files(
        &self,
        index: &Index,
        deadline: Instant,
    ) -> Result<(Vec<PathBuf>, bool), ScanError> {
        let Some(gitlinks) = self.gitlink_boundaries(index, deadline) else {
            return Ok((Vec::new(), false));
        };
        if !self.gitlink_boundaries_unchanged(&gitlinks, deadline) {
            return Ok((Vec::new(), false));
        }
        let ignore_walk = ignore::BoundedIgnoreWalk::new(
            &self.scope.worktree_root,
            crate::internal::layer::ExclusionSnapshot::for_request(),
        );
        let mut files = Vec::new();
        let mut complete = true;
        let mut directories = VecDeque::from([PathBuf::new()]);
        while let Some(directory) = directories.pop_front() {
            if Instant::now() > deadline {
                return Ok((Vec::new(), false));
            }
            let timeout = deadline.saturating_duration_since(Instant::now());
            if timeout.is_zero() {
                return Ok((Vec::new(), false));
            }
            let path_key = path_to_bytes(&directory);
            let request = || IoRequest::ReadDir {
                path: path_key.clone(),
                root: path_to_bytes(&self.scope.worktree_root),
                remaining: self.max_files.saturating_sub(files.len()),
                checkpoint_every: 32,
            };
            let events = if in_process_test_host() {
                self.io
                    .submit_in_process(request(), path_key, timeout)
                    .map_err(|error| ScanError::Worker(format!("readdir fallback: {error}")))?
            } else {
                self.io
                    .submit_absolute(request(), path_key, timeout)
                    .map_err(|error| ScanError::Worker(error.to_string()))?
            };
            for event in events {
                if Instant::now() >= deadline {
                    return Ok((Vec::new(), false));
                }
                match event {
                    IoEvent::RecordDirent(dirent) => {
                        let relative = directory.join(bytes_to_path(&dirent.name));
                        if gitlinks.contains_literal(&relative) {
                            continue;
                        }
                        if !gitlinks.is_empty() {
                            let identity = match self.entry_identity(&relative, deadline) {
                                Ok(identity) => identity,
                                Err(_) => {
                                    complete = false;
                                    continue;
                                }
                            };
                            match gitlinks.classify(identity) {
                                BoundaryMatch::Visible => {}
                                BoundaryMatch::Opaque => continue,
                                BoundaryMatch::Ambiguous => {
                                    complete = false;
                                    continue;
                                }
                            }
                        }
                        if !dirent.type_ok {
                            complete = false;
                            continue;
                        }
                        if relative.components().any(|component| {
                        matches!(component, std::path::Component::Normal(name) if name == ".git" || name == ".libra")
                    }) {
                        continue;
                    }
                        if dirent.is_dir {
                            let Some(ignored) = ignore_walk.should_ignore(
                                &relative,
                                IgnorePolicy::Respect,
                                index,
                                true,
                                deadline,
                            ) else {
                                return Ok((Vec::new(), false));
                            };
                            if !ignored {
                                directories.push_back(relative);
                            }
                        } else if relative.to_str().is_none() {
                            // Manifest paths are UTF-8 strings.  Preserve the
                            // legacy command's ability to operate when an
                            // unrelated untracked entry has non-UTF-8 bytes;
                            // such a path is outside the representable v2
                            // manifest and is deliberately omitted from this
                            // snapshot rather than lossy-converted to U+FFFD.
                        } else if dirent.is_file || dirent.is_symlink {
                            let Some(ignored) = ignore_walk.should_ignore(
                                &relative,
                                IgnorePolicy::Respect,
                                index,
                                false,
                                deadline,
                            ) else {
                                return Ok((Vec::new(), false));
                            };
                            if !ignored {
                                files.push(relative);
                            }
                        }
                        if files.len() >= self.max_files {
                            complete = false;
                            break;
                        }
                    }
                    IoEvent::RecordError { .. } => complete = false,
                    IoEvent::DoneReadDir { listing }
                        if listing.hit_cap
                            || listing.timed_out
                            || !listing.error_kinds.is_empty() =>
                    {
                        complete = false;
                    }
                    _ => {}
                }
            }
            if !complete && files.len() >= self.max_files {
                break;
            }
        }
        if Instant::now() >= deadline || !self.gitlink_boundaries_unchanged(&gitlinks, deadline) {
            return Ok((Vec::new(), false));
        }
        files.sort();
        Ok((files, complete))
    }
}

fn put_blob(storage: &ClientStorage, bytes: &[u8]) -> Result<ObjectHash, SnapshotError> {
    let oid = ObjectHash::from_type_and_data(ObjectType::Blob, bytes);
    put_content_addressed_object(storage, &oid, bytes, ObjectType::Blob, "blob")?;
    Ok(oid)
}

/// Store an immutable content-addressed object without repairing or replacing
/// a pre-existing payload. A matching object is already complete; a mismatch
/// is repository corruption and must stay visible to the caller.
fn put_content_addressed_object(
    storage: &ClientStorage,
    oid: &ObjectHash,
    bytes: &[u8],
    object_type: ObjectType,
    kind: &str,
) -> Result<(), SnapshotError> {
    if storage.exist(oid) {
        let existing = storage.get(oid).map_err(|error| {
            SnapshotError::Object(format!("failed to load {kind} object {oid}: {error}"))
        })?;
        if existing != bytes {
            return Err(SnapshotError::Object(format!(
                "existing {kind} object {oid} has different content"
            )));
        }
        return Ok(());
    }
    storage
        .put(oid, bytes, object_type)
        .map_err(|error| SnapshotError::Object(error.to_string()))?;
    Ok(())
}

#[derive(Default)]
struct TreeNode {
    entries: BTreeMap<String, (TreeItemMode, ObjectHash)>,
    directories: BTreeMap<String, TreeNode>,
}

fn tree_from_index(storage: &ClientStorage, index: &Index) -> Result<ObjectHash, SnapshotError> {
    let mut root = TreeNode::default();
    for entry in index.tracked_entries(0) {
        insert_tree_path(&mut root, &entry.name, index_mode(entry.mode), entry.hash)?;
    }
    write_tree_node(storage, root)
}

fn tree_from_working_copy(
    storage: &ClientStorage,
    index: &Index,
    tracked: &BTreeMap<String, ObjectHash>,
    untracked: &BTreeMap<String, ObjectHash>,
    root_path: &Path,
) -> Result<ObjectHash, SnapshotError> {
    let mut root = TreeNode::default();
    for (name, oid) in tracked {
        let mode = index
            .tracked_entries(0)
            .into_iter()
            .find(|entry| entry.name == *name)
            .map(|entry| index_mode(entry.mode))
            .unwrap_or(TreeItemMode::Blob);
        insert_tree_path(&mut root, name, mode, *oid)?;
    }
    for (name, oid) in untracked {
        let mode = fs::symlink_metadata(root_path.join(name))
            .ok()
            .map(|metadata| {
                if metadata.file_type().is_symlink() {
                    TreeItemMode::Link
                } else if metadata.permissions().mode() & 0o111 != 0 {
                    TreeItemMode::BlobExecutable
                } else {
                    TreeItemMode::Blob
                }
            })
            .unwrap_or(TreeItemMode::Blob);
        insert_tree_path(&mut root, name, mode, *oid)?;
    }
    write_tree_node(storage, root)
}

fn index_mode(mode: u32) -> TreeItemMode {
    match mode & 0o170000 {
        0o120000 => TreeItemMode::Link,
        0o160000 => TreeItemMode::Commit,
        _ if mode & 0o111 != 0 => TreeItemMode::BlobExecutable,
        _ => TreeItemMode::Blob,
    }
}

fn insert_tree_path(
    node: &mut TreeNode,
    path: &str,
    mode: TreeItemMode,
    oid: ObjectHash,
) -> Result<(), SnapshotError> {
    let mut components = path.split('/').filter(|component| !component.is_empty());
    let Some(first) = components.next() else {
        return Err(SnapshotError::Object("tree path is empty".to_string()));
    };
    let rest = components.collect::<Vec<_>>();
    if rest.is_empty() {
        if node.directories.contains_key(first) {
            return Err(SnapshotError::Object(format!(
                "tree path conflicts with directory '{path}'"
            )));
        }
        node.entries.insert(first.to_string(), (mode, oid));
        return Ok(());
    }
    if node.entries.contains_key(first) {
        return Err(SnapshotError::Object(format!(
            "tree path conflicts with file '{first}'"
        )));
    }
    let child = node.directories.entry(first.to_string()).or_default();
    insert_tree_path(child, &rest.join("/"), mode, oid)
}

fn write_tree_node(storage: &ClientStorage, node: TreeNode) -> Result<ObjectHash, SnapshotError> {
    let mut items = Vec::with_capacity(node.entries.len() + node.directories.len());
    for (name, (mode, oid)) in node.entries {
        items.push(TreeItem::new(mode, oid, name));
    }
    for (name, child) in node.directories {
        let oid = write_tree_node(storage, child)?;
        items.push(TreeItem::new(TreeItemMode::Tree, oid, name));
    }
    items.sort_by_key(|item| {
        let mut key = item.name.as_bytes().to_vec();
        if item.mode == TreeItemMode::Tree {
            key.push(b'/');
        }
        key
    });
    let bytes = items
        .iter()
        .flat_map(|item| item.to_data())
        .collect::<Vec<_>>();
    let oid = ObjectHash::from_type_and_data(ObjectType::Tree, &bytes);
    put_content_addressed_object(storage, &oid, &bytes, ObjectType::Tree, "tree")?;
    Ok(oid)
}

fn workspace_id(scope: &PinnedRequestScope) -> String {
    scope
        .scope
        .worktree_id()
        .map(str::to_string)
        .unwrap_or_else(|| "main".to_string())
}

async fn read_head(scope: &PinnedRequestScope) -> Result<HeadState, SnapshotError> {
    use crate::internal::{
        branch::BranchStoreError, db::get_db_conn_instance_for_path, head::Head,
    };

    let path = scope.storage.join(crate::utils::util::DATABASE);
    let db = get_db_conn_instance_for_path(&path)
        .await
        .map_err(|error| {
            io::Error::new(
                error.kind(),
                format!(
                    "cannot open repository database '{}' to read HEAD for worktree '{}': {error}",
                    path.display(),
                    scope.worktree_root.display()
                ),
            )
        })?;
    let head = Head::current_for_scope_result_with_conn(&db, &scope.scope)
        .await
        .map_err(|error| {
            let kind = match &error {
                BranchStoreError::Corrupt { .. } => io::ErrorKind::InvalidData,
                _ => io::ErrorKind::Other,
            };
            io::Error::new(kind, format!(
                "cannot read authoritative HEAD for worktree '{}' from repository database '{}': {error}",
                scope.worktree_root.display(), path.display()
            ))
        })?;
    Ok(match head {
        Head::Branch(name) => HeadState::Symbolic {
            reference: format!("refs/heads/{name}"),
        },
        Head::Detached(oid) => HeadState::Detached { oid },
    })
}

#[cfg(test)]
mod gitlink_tests;

#[cfg(test)]
mod head_tests;

#[cfg(test)]
mod ignore_tests;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn untracked_manifest_is_deterministic() {
        let manifest = UntrackedManifest {
            schema_version: 1,
            files: BTreeMap::from([("a".into(), ObjectHash::new(&[1; 20]))]),
        };
        let bytes = serde_json::to_vec(&manifest).expect("manifest serializes");
        let again = serde_json::to_vec(&manifest).expect("manifest serializes twice");
        assert_eq!(bytes, again);
    }
}
