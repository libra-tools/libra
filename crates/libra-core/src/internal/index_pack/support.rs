use std::{
    fs::File,
    io::{BufWriter, Read, Seek, Write},
    path::{Path, PathBuf},
    sync::{Arc, Mutex, MutexGuard},
};

use git_internal::{
    errors::GitError,
    hash::{HashKind, ObjectHash},
    internal::{object::types::ObjectType, pack::entry::Entry},
};

use crate::utils::error::{CliError, CliResult, StableErrorCode};

const INDEX_WRITE_ERROR_PREFIX: &str = "index write failed";
const ISSUE_URL: &str = "https://github.com/libra-tools/libra/issues";
const MAX_COMMIT_EDGE_SPOOL_BYTES: u64 = 1024 * 1024 * 1024;
const COMMIT_EDGE_CHUNK_SIZE: usize = 4096;
const COMMIT_EDGE_WRITE_BUFFER_BYTES: usize = 64 * 1024;
const MAX_COMMIT_ID_BYTES: usize = 64 * 1024 * 1024;
const COMMIT_ID_RESERVE_STEP: usize = 4096;

/// Cloud index metadata from fetch's existing pack decode, including resolved deltas.
#[derive(Default)]
pub struct ReceivedObjects(Mutex<Vec<(ObjectHash, ObjectType, i64)>>);

impl ReceivedObjects {
    pub fn record(&self, entry: &Entry) -> Result<(), GitError> {
        lock_state(&self.0, "received object metadata")?.push((
            entry.hash,
            entry.obj_type,
            entry.data.len() as i64,
        ));
        Ok(())
    }

    pub fn take_rows(&self) -> Result<Vec<(String, String, i64)>, GitError> {
        Ok(lock_state(&self.0, "received object metadata")?
            .drain(..)
            .map(|(oid, kind, size)| (oid.to_string(), kind.to_string(), size))
            .collect())
    }
}

/// An unnamed temporary file of fixed-width child/parent object-ID pairs.
/// The file is removed by the OS when its final handle is dropped, including
/// when pack decoding or index writing returns an error.
pub struct PackCommitEdges {
    file: BufWriter<File>,
    kind: HashKind,
    written_edges: u64,
    remaining_edges: u64,
    commit_ids: Vec<[u8; 32]>,
}

impl PackCommitEdges {
    pub fn new(directory: &Path, kind: HashKind) -> Result<Self, GitError> {
        let file = tempfile::tempfile_in(directory)
            .map_err(|error| index_write_error("creating commit edge spool", error))?;
        Ok(Self {
            file: BufWriter::with_capacity(COMMIT_EDGE_WRITE_BUFFER_BYTES, file),
            kind,
            written_edges: 0,
            remaining_edges: 0,
            commit_ids: Vec::new(),
        })
    }

    fn record_commit_id(&mut self, child: ObjectHash, max_ids: usize) -> Result<(), GitError> {
        if self.commit_ids.len() >= max_ids {
            return Err(GitError::InvalidPackFile(format!(
                "pack has more than {max_ids} commits; commit ID buffer exceeds {MAX_COMMIT_ID_BYTES} bytes"
            )));
        }
        if self.commit_ids.len() == self.commit_ids.capacity() {
            let additional = (max_ids - self.commit_ids.len()).min(COMMIT_ID_RESERVE_STEP);
            self.commit_ids
                .try_reserve_exact(additional)
                .map_err(|error| {
                    GitError::PackEncodeError(format!(
                        "cannot reserve memory for in-pack commit IDs: {error}"
                    ))
                })?;
        }
        let mut bytes = [0_u8; 32];
        bytes[..self.kind.size()].copy_from_slice(child.as_ref());
        self.commit_ids.push(bytes);
        Ok(())
    }

    pub fn record_parents(
        &mut self,
        child: ObjectHash,
        parents: &[ObjectHash],
    ) -> Result<(), GitError> {
        child
            .ensure_kind(self.kind)
            .map_err(|error| GitError::InvalidHashValue(error.to_string()))?;
        self.record_commit_id(child, MAX_COMMIT_ID_BYTES / std::mem::size_of::<[u8; 32]>())?;
        let additional: u64 = parents.len().try_into().map_err(|_| {
            GitError::InvalidPackFile("commit parent count exceeds spool limit".to_string())
        })?;
        let total_edges = self.written_edges.checked_add(additional).ok_or_else(|| {
            GitError::InvalidPackFile("commit edge spool exceeds size limit".to_string())
        })?;
        let total_bytes = total_edges
            .checked_mul((2 * self.kind.size()) as u64)
            .ok_or_else(|| {
                GitError::InvalidPackFile("commit edge spool exceeds size limit".to_string())
            })?;
        if total_bytes > MAX_COMMIT_EDGE_SPOOL_BYTES {
            return Err(GitError::InvalidPackFile(format!(
                "commit edge spool exceeds {} bytes",
                MAX_COMMIT_EDGE_SPOOL_BYTES
            )));
        }
        let width = self.kind.size();
        let mut record = [0_u8; 64];
        record[..width].copy_from_slice(child.as_ref());
        for parent in parents {
            parent
                .ensure_kind(self.kind)
                .map_err(|error| GitError::InvalidHashValue(error.to_string()))?;
            record[width..2 * width].copy_from_slice(parent.as_ref());
            self.file
                .write_all(&record[..2 * width])
                .map_err(|error| index_write_error("writing commit edge spool", error))?;
        }
        self.written_edges = total_edges;
        Ok(())
    }

    pub fn finish(&mut self) -> Result<(), GitError> {
        self.file
            .flush()
            .map_err(|error| index_write_error("flushing commit edge spool", error))?;
        self.file
            .get_mut()
            .rewind()
            .map_err(|error| index_write_error("rewinding commit edge spool", error))?;
        self.commit_ids.sort_unstable();
        self.commit_ids.dedup();
        self.remaining_edges = self.written_edges;
        Ok(())
    }

    /// `finish` sorts the IDs before this is used by fetch connectivity checks.
    pub fn contains_commit(&self, oid: &ObjectHash) -> bool {
        if oid.kind() != self.kind {
            return false;
        }
        let mut bytes = [0_u8; 32];
        bytes[..self.kind.size()].copy_from_slice(oid.as_ref());
        self.commit_ids.binary_search(&bytes).is_ok()
    }

    /// Return at most 4096 edges; an empty chunk marks the end of the spool.
    pub fn next_chunk(&mut self) -> Result<Vec<(ObjectHash, ObjectHash)>, GitError> {
        let count = self.remaining_edges.min(COMMIT_EDGE_CHUNK_SIZE as u64) as usize;
        let mut chunk = Vec::with_capacity(count);
        let width = self.kind.size();
        let mut records = vec![0_u8; count * 2 * width];
        self.file
            .get_mut()
            .read_exact(&mut records)
            .map_err(|error| {
                GitError::InvalidPackFile(format!("cannot read commit edge spool: {error}"))
            })?;
        for record in records.chunks_exact(2 * width) {
            let child = ObjectHash::from_bytes_for_kind(self.kind, &record[..width])
                .map_err(|error| GitError::InvalidHashValue(error.to_string()))?;
            let parent = ObjectHash::from_bytes_for_kind(self.kind, &record[width..2 * width])
                .map_err(|error| GitError::InvalidHashValue(error.to_string()))?;
            chunk.push((child, parent));
        }
        self.remaining_edges -= count as u64;
        Ok(chunk)
    }
}

pub fn index_pack_error(err: GitError) -> CliError {
    let stable_code = match err {
        GitError::PackEncodeError(ref message) if message.starts_with(INDEX_WRITE_ERROR_PREFIX) => {
            StableErrorCode::IoWriteFailed
        }
        GitError::IOError(_) => StableErrorCode::IoReadFailed,
        GitError::InvalidArgument(_) => StableErrorCode::CliInvalidArguments,
        GitError::InvalidPackFile(_)
        | GitError::InvalidPackHeader(_)
        | GitError::InvalidIdxFile(_)
        | GitError::ConversionError(_)
        | GitError::DeltaObjectError(_)
        | GitError::InvalidHashValue(_)
        | GitError::InvalidObjectInfo(_)
        | GitError::ObjectNotFound(_) => StableErrorCode::RepoCorrupt,
        _ => StableErrorCode::InternalInvariant,
    };

    let cli =
        CliError::fatal(format!("failed to build pack index: {err}")).with_stable_code(stable_code);
    if stable_code == StableErrorCode::InternalInvariant {
        cli.with_hint(format!("this is a bug; please report it at {ISSUE_URL}"))
    } else {
        cli
    }
}

pub fn format_io_error(err: &std::io::Error) -> String {
    match err.kind() {
        std::io::ErrorKind::NotFound => "No such file or directory".to_string(),
        std::io::ErrorKind::PermissionDenied => "Permission denied".to_string(),
        _ => err.to_string(),
    }
}

pub fn index_write_error(action: &str, error: std::io::Error) -> GitError {
    GitError::PackEncodeError(format!(
        "{INDEX_WRITE_ERROR_PREFIX} while {action}: {error}"
    ))
}

pub fn keep_file_path(pack_file: &str) -> PathBuf {
    PathBuf::from(pack_file).with_extension("keep")
}

pub fn write_keep_file(keep_file: &str, message: &str) -> CliResult<()> {
    let mut file = std::fs::File::create(keep_file).map_err(|e| {
        CliError::fatal(format!(
            "could not create '{}' for writing: {}",
            keep_file,
            format_io_error(&e)
        ))
        .with_stable_code(StableErrorCode::IoWriteFailed)
    })?;

    if !message.is_empty() {
        writeln!(file, "{message}").map_err(|e| {
            CliError::fatal(format!(
                "could not write keep message to '{}': {}",
                keep_file,
                format_io_error(&e)
            ))
            .with_stable_code(StableErrorCode::IoWriteFailed)
        })?;
    }

    Ok(())
}

pub fn lock_state<'a, T>(mutex: &'a Mutex<T>, label: &str) -> Result<MutexGuard<'a, T>, GitError> {
    mutex
        .lock()
        .map_err(|_| GitError::PackEncodeError(format!("{label} mutex poisoned")))
}

pub fn take_arc_mutex<T>(arc: Arc<Mutex<T>>, label: &str) -> Result<T, GitError> {
    let mutex = Arc::try_unwrap(arc).map_err(|_| {
        GitError::PackEncodeError(format!("{label} still has outstanding references"))
    })?;
    mutex
        .into_inner()
        .map_err(|_| GitError::PackEncodeError(format!("{label} mutex poisoned")))
}

pub fn record_first_pack_error(slot: &Arc<Mutex<Option<GitError>>>, error: GitError) {
    if let Ok(mut guard) = slot.lock()
        && guard.is_none()
    {
        *guard = Some(error);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn commit_edge_spool_is_bounded_and_preserves_explicit_hash_kind() {
        for kind in [HashKind::Sha1, HashKind::Sha256, HashKind::Blake3] {
            let dir = tempfile::tempdir().expect("create spool directory");
            let child = ObjectHash::new_for_kind(kind, b"child");
            let parent = ObjectHash::new_for_kind(kind, b"parent");
            let mut spool = PackCommitEdges::new(dir.path(), kind).expect("create spool");
            spool
                .record_parents(child, &[])
                .expect("root commit needs no edge");
            let parents = vec![parent; COMMIT_EDGE_CHUNK_SIZE + 1];
            spool
                .record_parents(child, &parents)
                .expect("write parent edges");
            spool.finish().expect("rewind spool");
            assert!(spool.contains_commit(&child));
            assert_eq!(
                spool.commit_ids.len(),
                1,
                "duplicate commit IDs are deduplicated"
            );
            assert!(!spool.contains_commit(&ObjectHash::new_for_kind(kind, b"absent")));
            let other_kind = match kind {
                HashKind::Sha1 => HashKind::Sha256,
                HashKind::Sha256 => HashKind::Blake3,
                HashKind::Blake3 => HashKind::Sha256,
            };
            assert!(!spool.contains_commit(&ObjectHash::new_for_kind(other_kind, b"child")));

            let first = spool.next_chunk().expect("first bounded chunk");
            assert_eq!(first.len(), COMMIT_EDGE_CHUNK_SIZE);
            assert!(first.iter().all(|edge| *edge == (child, parent)));
            assert_eq!(
                spool.next_chunk().expect("second chunk"),
                vec![(child, parent)]
            );
            assert!(spool.next_chunk().expect("end of spool").is_empty());

            drop(spool);
            assert!(
                dir.path()
                    .read_dir()
                    .expect("inspect spool directory")
                    .next()
                    .is_none(),
                "the temporary spool must not leave a named file behind"
            );
        }
    }

    #[test]
    fn commit_edge_spool_rejects_size_limit_before_writing() {
        let dir = tempfile::tempdir().expect("create spool directory");
        let kind = HashKind::Sha1;
        let child = ObjectHash::new_for_kind(kind, b"child");
        let parent = ObjectHash::new_for_kind(kind, b"parent");
        let mut spool = PackCommitEdges::new(dir.path(), kind).expect("create spool");
        spool.written_edges = MAX_COMMIT_EDGE_SPOOL_BYTES / (2 * kind.size()) as u64;

        assert!(matches!(
            spool.record_parents(child, &[parent]),
            Err(GitError::InvalidPackFile(_))
        ));
        assert_eq!(
            spool
                .file
                .get_ref()
                .metadata()
                .expect("spool metadata")
                .len(),
            0
        );
    }

    #[test]
    fn commit_edge_spool_rejects_commit_id_limit() {
        let dir = tempfile::tempdir().expect("create spool directory");
        let kind = HashKind::Sha1;
        let child = ObjectHash::new_for_kind(kind, b"child");
        let mut spool = PackCommitEdges::new(dir.path(), kind).expect("create spool");
        spool.record_commit_id(child, 1).expect("first commit ID");

        assert!(matches!(
            spool.record_commit_id(child, 1),
            Err(GitError::InvalidPackFile(_))
        ));
        assert_eq!(spool.commit_ids.len(), 1);
    }

    #[test]
    fn index_pack_error_maps_wrapped_write_failures_to_io_write_failed() {
        let cli_error = index_pack_error(index_write_error(
            "writing index data",
            std::io::Error::new(std::io::ErrorKind::PermissionDenied, "permission denied"),
        ));

        assert_eq!(cli_error.stable_code(), StableErrorCode::IoWriteFailed);
    }

    #[test]
    fn lock_state_reports_poisoned_mutex() {
        let mutex = Arc::new(Mutex::new(1_u8));
        let poisoned = Arc::clone(&mutex);
        let _ = std::thread::spawn(move || {
            let _guard = poisoned.lock().unwrap();
            panic!("poison test mutex");
        })
        .join();

        let err = lock_state(&mutex, "index entry buffer").expect_err("mutex should be poisoned");
        match err {
            GitError::PackEncodeError(message) => {
                assert_eq!(message, "index entry buffer mutex poisoned");
            }
            other => panic!("unexpected error: {other:?}"),
        }
    }

    #[test]
    fn take_arc_mutex_reports_outstanding_references() {
        let mutex = Arc::new(Mutex::new(vec![1_u8]));
        let _extra_ref = Arc::clone(&mutex);

        let err =
            take_arc_mutex(mutex, "index entry buffer").expect_err("extra Arc ref should fail");
        match err {
            GitError::PackEncodeError(message) => {
                assert_eq!(
                    message,
                    "index entry buffer still has outstanding references"
                );
            }
            other => panic!("unexpected error: {other:?}"),
        }
    }

    #[test]
    fn take_arc_mutex_reports_poisoned_mutex() {
        let mutex = Arc::new(Mutex::new(vec![1_u8]));
        let poisoned = Arc::clone(&mutex);
        let _ = std::thread::spawn(move || {
            let _guard = poisoned.lock().unwrap();
            panic!("poison test mutex");
        })
        .join();

        let err =
            take_arc_mutex(mutex, "index entry buffer").expect_err("mutex should be poisoned");
        match err {
            GitError::PackEncodeError(message) => {
                assert_eq!(message, "index entry buffer mutex poisoned");
            }
            other => panic!("unexpected error: {other:?}"),
        }
    }

    #[test]
    fn index_pack_error_maps_unknown_git_error_with_issue_url_hint() {
        let cli_error = index_pack_error(GitError::UnCompletedPackObject(
            "synthetic uncompleted pack object".to_string(),
        ));

        assert_eq!(cli_error.stable_code(), StableErrorCode::InternalInvariant);
        assert!(
            cli_error
                .hints()
                .iter()
                .any(|h| h.as_str().contains("issues")),
            "InternalInvariant fall-through must include the Issues URL hint, got hints: {:?}",
            cli_error.hints()
        );
    }
}
