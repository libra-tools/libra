//! File-level undo history for AI-authored workspace edits.
//!
//! The store captures pre-edit file bytes for each tool-loop batch. Undo uses
//! those preimages to restore the last batch as one unit, giving Code UI users a
//! recovery path for uncommitted `apply_patch` edits.

use std::{
    collections::{BTreeSet, HashMap, HashSet},
    fs::{self, File},
    io::{self, Write},
    path::{Path, PathBuf},
    sync::atomic::{AtomicU64, Ordering},
};

use chrono::{DateTime, Utc};
use ring::digest::{SHA256, digest};
use serde::{Deserialize, Serialize};
use thiserror::Error;

const MANIFEST_FILE: &str = "manifest.json";
const LEGACY_MANIFEST_FILE: &str = "file_history.json";
const MAX_VERSIONS_PER_FILE: usize = 50;

#[derive(Debug, Error)]
pub enum FileHistoryError {
    #[error("{context}: {source}")]
    Io {
        context: String,
        #[source]
        source: io::Error,
    },
    #[error("{context}: {source}")]
    Serde {
        context: String,
        #[source]
        source: serde_json::Error,
    },
    #[error("path is outside the workspace: {path}")]
    PathOutsideWorkspace { path: PathBuf },
    #[error("no file edits are available to undo")]
    NoUndoBatch,
    #[error("undo snapshot is missing for {path}: {snapshot}")]
    SnapshotMissing { path: String, snapshot: String },
    #[error("undo preflight failed for {path}: {reason}")]
    Preflight { path: PathBuf, reason: String },
}

pub type Result<T> = std::result::Result<T, FileHistoryError>;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FileHistoryBatchRecord {
    pub batch_id: String,
    pub recorded_paths: usize,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UndoBatchReport {
    pub batch_id: String,
    pub restored_paths: usize,
}

#[derive(Debug, Clone)]
pub struct FileHistoryStore {
    session_root: PathBuf,
}

#[derive(Debug, Default, Serialize, Deserialize)]
struct FileHistoryManifest {
    #[serde(default)]
    batches: Vec<FileHistoryBatch>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct FileHistoryBatch {
    id: String,
    created_at: DateTime<Utc>,
    #[serde(default)]
    entries: Vec<FileHistoryEntry>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct FileHistoryEntry {
    path: String,
    existed: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    snapshot: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    size: Option<u64>,
}

#[derive(Debug, Clone)]
struct CurrentFileBackup {
    path: PathBuf,
    existed: bool,
    bytes: Option<Vec<u8>>,
}

impl FileHistoryStore {
    pub fn new(session_root: PathBuf) -> Self {
        Self { session_root }
    }

    pub fn record_preimages(
        &self,
        batch_id: &str,
        working_dir: &Path,
        paths: &BTreeSet<PathBuf>,
    ) -> Result<FileHistoryBatchRecord> {
        if paths.is_empty() {
            return Ok(FileHistoryBatchRecord {
                batch_id: batch_id.to_string(),
                recorded_paths: 0,
            });
        }

        self.ensure_history_dir()?;
        let mut manifest = self.load_manifest()?;
        let batch_index = manifest
            .batches
            .iter()
            .position(|batch| batch.id == batch_id)
            .unwrap_or_else(|| {
                manifest.batches.push(FileHistoryBatch {
                    id: batch_id.to_string(),
                    created_at: Utc::now(),
                    entries: Vec::new(),
                });
                manifest.batches.len() - 1
            });

        let existing_paths = manifest.batches[batch_index]
            .entries
            .iter()
            .map(|entry| entry.path.clone())
            .collect::<HashSet<_>>();

        let mut recorded_paths = 0usize;
        for path in paths {
            let relative = relative_workspace_path(working_dir, path)?;
            if existing_paths.contains(&relative) {
                continue;
            }

            let entry = match fs::read(path) {
                Ok(bytes) => {
                    let hash = snapshot_hash(&bytes);
                    self.write_snapshot_if_missing(&hash, &bytes)?;
                    FileHistoryEntry {
                        path: relative,
                        existed: true,
                        snapshot: Some(hash),
                        size: Some(bytes.len() as u64),
                    }
                }
                Err(error) if error.kind() == io::ErrorKind::NotFound => FileHistoryEntry {
                    path: relative,
                    existed: false,
                    snapshot: None,
                    size: None,
                },
                Err(source) => {
                    return Err(FileHistoryError::Io {
                        context: format!("failed to read preimage {}", path.display()),
                        source,
                    });
                }
            };
            manifest.batches[batch_index].entries.push(entry);
            recorded_paths += 1;
        }

        prune_manifest(&mut manifest);
        self.save_manifest(&manifest)?;

        Ok(FileHistoryBatchRecord {
            batch_id: batch_id.to_string(),
            recorded_paths,
        })
    }

    pub fn undo_latest_batch(&self, working_dir: &Path) -> Result<UndoBatchReport> {
        let mut manifest = self.load_manifest()?;
        let batch = manifest
            .batches
            .last()
            .cloned()
            .ok_or(FileHistoryError::NoUndoBatch)?;
        if batch.entries.is_empty() {
            return Err(FileHistoryError::NoUndoBatch);
        }

        let targets = batch_targets(working_dir, &batch)?;
        preflight_undo_targets(&targets)?;
        let current_backups = backup_current_files(&targets)?;

        match self.apply_undo_entries(&targets) {
            Ok(()) => {
                manifest.batches.pop();
                self.save_manifest(&manifest)?;
                self.prune_unreferenced_snapshots(&manifest)?;
                Ok(UndoBatchReport {
                    batch_id: batch.id,
                    restored_paths: targets.len(),
                })
            }
            Err(error) => {
                if let Err(rollback_error) = restore_current_backups(&current_backups) {
                    tracing::error!(
                        error = %rollback_error,
                        "failed to restore workspace after file undo error"
                    );
                }
                Err(error)
            }
        }
    }

    pub fn clear(&self) -> Result<()> {
        let history_dir = self.history_dir();
        if history_dir.exists() {
            fs::remove_dir_all(&history_dir).map_err(|source| FileHistoryError::Io {
                context: format!("failed to remove file history {}", history_dir.display()),
                source,
            })?;
        }
        let legacy_manifest = self.legacy_manifest_path();
        if legacy_manifest.exists() {
            fs::remove_file(&legacy_manifest).map_err(|source| FileHistoryError::Io {
                context: format!(
                    "failed to remove legacy file history manifest {}",
                    legacy_manifest.display()
                ),
                source,
            })?;
        }
        Ok(())
    }

    fn history_dir(&self) -> PathBuf {
        self.session_root.join("file_history")
    }

    fn manifest_path(&self) -> PathBuf {
        self.history_dir().join(MANIFEST_FILE)
    }

    fn legacy_manifest_path(&self) -> PathBuf {
        self.session_root.join(LEGACY_MANIFEST_FILE)
    }

    fn snapshot_path(&self, hash: &str) -> PathBuf {
        self.history_dir().join(hash)
    }

    fn ensure_history_dir(&self) -> Result<()> {
        let history_dir = self.history_dir();
        fs::create_dir_all(&history_dir).map_err(|source| FileHistoryError::Io {
            context: format!(
                "failed to create file history dir {}",
                history_dir.display()
            ),
            source,
        })
    }

    fn load_manifest(&self) -> Result<FileHistoryManifest> {
        let path = self.manifest_path();
        match fs::read_to_string(&path) {
            Ok(content) => {
                serde_json::from_str(&content).map_err(|source| FileHistoryError::Serde {
                    context: format!("failed to parse file history manifest {}", path.display()),
                    source,
                })
            }
            Err(error) if error.kind() == io::ErrorKind::NotFound => {
                // Pre-CH-04 sessions used a flat manifest at the session
                // root. Keep this fallback read-only: the next write goes
                // through save_manifest() and materializes the canonical
                // file_history/manifest.json location.
                let legacy_path = self.legacy_manifest_path();
                match fs::read_to_string(&legacy_path) {
                    Ok(content) => {
                        serde_json::from_str(&content).map_err(|source| FileHistoryError::Serde {
                            context: format!(
                                "failed to parse legacy file history manifest {}",
                                legacy_path.display()
                            ),
                            source,
                        })
                    }
                    Err(error) if error.kind() == io::ErrorKind::NotFound => {
                        Ok(FileHistoryManifest::default())
                    }
                    Err(source) => Err(FileHistoryError::Io {
                        context: format!(
                            "failed to read legacy file history manifest {}",
                            legacy_path.display()
                        ),
                        source,
                    }),
                }
            }
            Err(source) => Err(FileHistoryError::Io {
                context: format!("failed to read file history manifest {}", path.display()),
                source,
            }),
        }
    }

    fn save_manifest(&self, manifest: &FileHistoryManifest) -> Result<()> {
        self.ensure_history_dir()?;
        let path = self.manifest_path();
        let json =
            serde_json::to_vec_pretty(manifest).map_err(|source| FileHistoryError::Serde {
                context: "failed to serialize file history manifest".to_string(),
                source,
            })?;
        write_file_atomic(&path, &json)
    }

    fn write_snapshot_if_missing(&self, hash: &str, bytes: &[u8]) -> Result<()> {
        let path = self.snapshot_path(hash);
        if path.exists() {
            return Ok(());
        }
        write_file_atomic(&path, bytes)
    }

    fn prune_unreferenced_snapshots(&self, manifest: &FileHistoryManifest) -> Result<()> {
        let referenced = manifest
            .batches
            .iter()
            .flat_map(|batch| batch.entries.iter())
            .filter_map(|entry| entry.snapshot.as_deref())
            .collect::<HashSet<_>>();
        let history_dir = self.history_dir();
        if !history_dir.exists() {
            return Ok(());
        }
        for entry in fs::read_dir(&history_dir).map_err(|source| FileHistoryError::Io {
            context: format!("failed to read file history dir {}", history_dir.display()),
            source,
        })? {
            let entry = entry.map_err(|source| FileHistoryError::Io {
                context: format!("failed to read file history dir {}", history_dir.display()),
                source,
            })?;
            let path = entry.path();
            if path.file_name().and_then(|name| name.to_str()) == Some(MANIFEST_FILE) {
                continue;
            }
            let Some(name) = path.file_name().and_then(|name| name.to_str()) else {
                continue;
            };
            if !referenced.contains(name) {
                fs::remove_file(&path).map_err(|source| FileHistoryError::Io {
                    context: format!("failed to remove unreferenced snapshot {}", path.display()),
                    source,
                })?;
            }
        }
        Ok(())
    }

    fn apply_undo_entries(&self, targets: &[UndoTarget]) -> Result<()> {
        for target in targets {
            if target.entry.existed {
                let snapshot = target.entry.snapshot.as_deref().ok_or_else(|| {
                    FileHistoryError::SnapshotMissing {
                        path: target.entry.path.clone(),
                        snapshot: "<missing>".to_string(),
                    }
                })?;
                let snapshot_path = self.snapshot_path(snapshot);
                let bytes = fs::read(&snapshot_path).map_err(|source| {
                    if source.kind() == io::ErrorKind::NotFound {
                        FileHistoryError::SnapshotMissing {
                            path: target.entry.path.clone(),
                            snapshot: snapshot.to_string(),
                        }
                    } else {
                        FileHistoryError::Io {
                            context: format!(
                                "failed to read undo snapshot {}",
                                snapshot_path.display()
                            ),
                            source,
                        }
                    }
                })?;
                write_file_atomic(&target.path, &bytes)?;
            } else if target.path.exists() {
                fs::remove_file(&target.path).map_err(|source| FileHistoryError::Io {
                    context: format!("failed to remove added file {}", target.path.display()),
                    source,
                })?;
            }
        }
        Ok(())
    }
}

#[derive(Debug)]
struct UndoTarget {
    path: PathBuf,
    entry: FileHistoryEntry,
}

fn batch_targets(working_dir: &Path, batch: &FileHistoryBatch) -> Result<Vec<UndoTarget>> {
    batch
        .entries
        .iter()
        .map(|entry| {
            let path = working_dir.join(&entry.path);
            if !path_stays_within_workspace(working_dir, &path) {
                return Err(FileHistoryError::PathOutsideWorkspace { path });
            }
            Ok(UndoTarget {
                path,
                entry: entry.clone(),
            })
        })
        .collect()
}

fn relative_workspace_path(working_dir: &Path, path: &Path) -> Result<String> {
    let relative =
        path.strip_prefix(working_dir)
            .map_err(|_| FileHistoryError::PathOutsideWorkspace {
                path: path.to_path_buf(),
            })?;
    Ok(relative
        .components()
        .map(|component| component.as_os_str().to_string_lossy())
        .collect::<Vec<_>>()
        .join("/"))
}

fn path_stays_within_workspace(working_dir: &Path, path: &Path) -> bool {
    let mut depth = 0usize;
    for component in path
        .strip_prefix(working_dir)
        .ok()
        .into_iter()
        .flat_map(|path| path.components())
    {
        match component {
            std::path::Component::ParentDir => {
                if depth == 0 {
                    return false;
                }
                depth -= 1;
            }
            std::path::Component::Normal(_) => depth += 1,
            std::path::Component::CurDir => {}
            std::path::Component::RootDir | std::path::Component::Prefix(_) => return false,
        }
    }
    path.starts_with(working_dir)
}

fn preflight_undo_targets(targets: &[UndoTarget]) -> Result<()> {
    for target in targets {
        if target.path.is_dir() {
            return Err(FileHistoryError::Preflight {
                path: target.path.clone(),
                reason: "target is a directory; file undo will not remove directories".to_string(),
            });
        }
        if target.entry.existed {
            let Some(parent) = target.path.parent() else {
                continue;
            };
            if parent.exists() && !parent.is_dir() {
                return Err(FileHistoryError::Preflight {
                    path: target.path.clone(),
                    reason: format!("parent path is not a directory: {}", parent.display()),
                });
            }
            let mut ancestor = parent;
            while !ancestor.exists() {
                let Some(next) = ancestor.parent() else {
                    break;
                };
                ancestor = next;
            }
            if ancestor.exists() && !ancestor.is_dir() {
                return Err(FileHistoryError::Preflight {
                    path: target.path.clone(),
                    reason: format!("ancestor path is not a directory: {}", ancestor.display()),
                });
            }
        }
    }
    Ok(())
}

fn backup_current_files(targets: &[UndoTarget]) -> Result<Vec<CurrentFileBackup>> {
    targets
        .iter()
        .map(|target| match fs::read(&target.path) {
            Ok(bytes) => Ok(CurrentFileBackup {
                path: target.path.clone(),
                existed: true,
                bytes: Some(bytes),
            }),
            Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(CurrentFileBackup {
                path: target.path.clone(),
                existed: false,
                bytes: None,
            }),
            Err(source) => Err(FileHistoryError::Io {
                context: format!("failed to back up current file {}", target.path.display()),
                source,
            }),
        })
        .collect()
}

fn restore_current_backups(backups: &[CurrentFileBackup]) -> Result<()> {
    for backup in backups {
        if backup.existed {
            let bytes = backup.bytes.as_deref().unwrap_or_default();
            write_file_atomic(&backup.path, bytes)?;
        } else if backup.path.exists() {
            fs::remove_file(&backup.path).map_err(|source| FileHistoryError::Io {
                context: format!("failed to restore removed file {}", backup.path.display()),
                source,
            })?;
        }
    }
    Ok(())
}

fn write_file_atomic(path: &Path, bytes: &[u8]) -> Result<()> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).map_err(|source| FileHistoryError::Io {
            context: format!("failed to create parent directory {}", parent.display()),
            source,
        })?;
    }

    let temp_path = temp_path_for(path);
    let write_result = (|| -> Result<()> {
        let mut file = File::create(&temp_path).map_err(|source| FileHistoryError::Io {
            context: format!("failed to create temp file {}", temp_path.display()),
            source,
        })?;
        file.write_all(bytes)
            .map_err(|source| FileHistoryError::Io {
                context: format!("failed to write temp file {}", temp_path.display()),
                source,
            })?;
        file.sync_all().map_err(|source| FileHistoryError::Io {
            context: format!("failed to flush temp file {}", temp_path.display()),
            source,
        })?;
        fs::rename(&temp_path, path).map_err(|source| FileHistoryError::Io {
            context: format!(
                "failed to move temp file {} into {}",
                temp_path.display(),
                path.display()
            ),
            source,
        })?;
        Ok(())
    })();

    if write_result.is_err()
        && let Err(source) = fs::remove_file(&temp_path)
        && source.kind() != io::ErrorKind::NotFound
    {
        tracing::warn!(
            path = %temp_path.display(),
            error = %source,
            "failed to remove temporary file history write"
        );
    }
    write_result
}

fn temp_path_for(path: &Path) -> PathBuf {
    static TEMP_COUNTER: AtomicU64 = AtomicU64::new(0);
    let counter = TEMP_COUNTER.fetch_add(1, Ordering::Relaxed);
    let pid = std::process::id();
    let name = path
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("file");
    path.with_file_name(format!(".{name}.libra-tmp-{pid}-{counter}"))
}

fn snapshot_hash(bytes: &[u8]) -> String {
    hex::encode(digest(&SHA256, bytes).as_ref())
}

fn prune_manifest(manifest: &mut FileHistoryManifest) {
    let mut counts: HashMap<String, usize> = HashMap::new();
    for batch in manifest.batches.iter_mut().rev() {
        batch.entries.retain(|entry| {
            let count = counts.entry(entry.path.clone()).or_default();
            *count += 1;
            *count <= MAX_VERSIONS_PER_FILE
        });
    }
    manifest.batches.retain(|batch| !batch.entries.is_empty());
}

#[cfg(test)]
mod tests {
    use std::{fs, path::PathBuf};

    use tempfile::tempdir;

    use super::{FileHistoryError, FileHistoryStore};

    #[test]
    fn legacy_manifest_is_read_and_next_write_moves_to_canonical_path() {
        let dir = tempdir().expect("tempdir");
        let legacy = dir.path().join("file_history.json");
        fs::write(
            &legacy,
            r#"{"batches":[{"id":"legacy","created_at":"2026-09-08T00:00:00Z","entries":[]}]}"#,
        )
        .expect("legacy manifest");
        let store = FileHistoryStore::new(dir.path().to_path_buf());
        let manifest = store.load_manifest().expect("legacy read");
        assert_eq!(manifest.batches[0].id, "legacy");
        store.save_manifest(&manifest).expect("canonical write");
        assert!(dir.path().join("file_history/manifest.json").is_file());
        assert!(legacy.is_file());
    }

    #[test]
    fn file_history_error_display_pins_owned_variants() {
        assert_eq!(
            FileHistoryError::PathOutsideWorkspace {
                path: PathBuf::from("/etc/secrets"),
            }
            .to_string(),
            "path is outside the workspace: /etc/secrets",
        );
        assert_eq!(
            FileHistoryError::NoUndoBatch.to_string(),
            "no file edits are available to undo",
        );
        assert_eq!(
            FileHistoryError::SnapshotMissing {
                path: "src/main.rs".to_string(),
                snapshot: "abc123".to_string(),
            }
            .to_string(),
            "undo snapshot is missing for src/main.rs: abc123",
        );
        assert_eq!(
            FileHistoryError::Preflight {
                path: PathBuf::from("src/main.rs"),
                reason: "modified concurrently".to_string(),
            }
            .to_string(),
            "undo preflight failed for src/main.rs: modified concurrently",
        );
    }

    #[test]
    fn file_history_error_io_template_pins_context_colon_source() {
        let err = FileHistoryError::Io {
            context: "failed to read manifest".to_string(),
            source: std::io::Error::new(std::io::ErrorKind::NotFound, "missing"),
        };
        assert_eq!(err.to_string(), "failed to read manifest: missing");
    }
}
