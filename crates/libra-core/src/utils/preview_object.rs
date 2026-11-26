//! Bounded task-local objects used only by verbose commit previews.

use std::{
    cell::RefCell,
    collections::{HashMap, HashSet},
    fs,
    future::Future,
    io::{self, Read},
    path::{Path, PathBuf},
};

use git_internal::hash::ObjectHash;

/// A single preview blob may consume at most 32 MiB of memory and scratch disk.
pub const MAX_OBJECT_BYTES: u64 = 32 * 1024 * 1024;
/// One verbose preview may retain at most 64 MiB of object payloads.
pub const MAX_CACHE_BYTES: u64 = 64 * 1024 * 1024;
/// Bound metadata and inode growth even when every object is tiny.
const MAX_CACHE_OBJECTS: usize = 4_096;
/// Conservative accounting for one set entry, filesystem block, and inode.
const MIN_OBJECT_CHARGE: u64 = 4 * 1024;

/// Charge applied to one unique preview object for aggregate budgeting.
pub const fn charged_bytes(size: u64) -> u64 {
    if size < MIN_OBJECT_CHARGE {
        MIN_OBJECT_CHARGE
    } else {
        size
    }
}

struct PreviewCache {
    directory: PathBuf,
    bytes: u64,
    hashes: HashSet<ObjectHash>,
    pending: HashMap<u64, u64>,
    next_pending_id: u64,
    max_object_bytes: u64,
    max_cache_bytes: u64,
    max_cache_objects: usize,
}

#[derive(Debug)]
pub struct PendingReservation {
    id: u64,
    active: bool,
}

impl PendingReservation {
    /// Convert a provisional byte/count reservation into one hash-deduplicated
    /// cached object after its content has been read and hashed.
    pub fn cache(mut self, hash: ObjectHash, content: &[u8]) -> io::Result<()> {
        PREVIEW_CACHE
            .try_with(|state| {
                let mut state = state.borrow_mut();
                let reserved = state.pending.remove(&self.id).ok_or_else(|| {
                    io::Error::other("preview pending reservation is no longer active")
                })?;
                state.bytes = state.bytes.saturating_sub(reserved);
                reserve_locked(&mut state, hash, content.len() as u64)?;
                fs::write(state.directory.join(hash.to_string()), content)
            })
            .map_err(|_| io::Error::other("preview object cache is not initialized"))??;
        self.active = false;
        Ok(())
    }
}

impl Drop for PendingReservation {
    fn drop(&mut self) {
        if !self.active {
            return;
        }
        let _ = PREVIEW_CACHE.try_with(|state| {
            let mut state = state.borrow_mut();
            if let Some(charge) = state.pending.remove(&self.id) {
                state.bytes = state.bytes.saturating_sub(charge);
            }
        });
    }
}

/// Reserve bytes and one metadata slot before opening/reading an auto-stage
/// payload whose final object ID is not known yet.
pub fn reserve_pending(size: u64) -> io::Result<PendingReservation> {
    PREVIEW_CACHE
        .try_with(|state| {
            let mut state = state.borrow_mut();
            validate_limits(&state, size, state.hashes.len() + state.pending.len())?;
            let charge = charged_bytes(size);
            let id = state.next_pending_id;
            state.next_pending_id = state.next_pending_id.checked_add(1).ok_or_else(|| {
                io::Error::new(
                    io::ErrorKind::InvalidData,
                    "preview reservation ID overflow",
                )
            })?;
            state.bytes = state.bytes.checked_add(charge).ok_or_else(|| {
                io::Error::new(io::ErrorKind::InvalidData, "preview cache size overflow")
            })?;
            state.pending.insert(id, charge);
            Ok(PendingReservation { id, active: true })
        })
        .map_err(|_| io::Error::other("preview object cache is not initialized"))?
}

tokio::task_local! {
    static PREVIEW_CACHE: RefCell<PreviewCache>;
}

/// Run a preview with a command-scoped cache that disappears on every exit path.
pub async fn with_objects<T>(directory: PathBuf, future: impl Future<Output = T>) -> T {
    with_limits(
        directory,
        MAX_OBJECT_BYTES,
        MAX_CACHE_BYTES,
        MAX_CACHE_OBJECTS,
        future,
    )
    .await
}

async fn with_limits<T>(
    directory: PathBuf,
    max_object_bytes: u64,
    max_cache_bytes: u64,
    max_cache_objects: usize,
    future: impl Future<Output = T>,
) -> T {
    PREVIEW_CACHE
        .scope(
            RefCell::new(PreviewCache {
                directory,
                bytes: 0,
                hashes: HashSet::new(),
                pending: HashMap::new(),
                next_pending_id: 1,
                max_object_bytes,
                max_cache_bytes,
                max_cache_objects,
            }),
            future,
        )
        .await
}

/// Cache one raw blob after checking both the per-object and aggregate limits.
#[cfg(test)]
pub fn cache(hash: ObjectHash, content: &[u8]) -> io::Result<()> {
    reserve(hash, content.len() as u64)?;
    PREVIEW_CACHE
        .try_with(|state| {
            fs::write(state.borrow().directory.join(hash.to_string()), content)?;
            Ok(())
        })
        .map_err(|_| io::Error::other("preview object cache is not initialized"))?
}

/// Reserve one unique object before any caller loads its payload into memory.
pub fn reserve(hash: ObjectHash, size: u64) -> io::Result<()> {
    PREVIEW_CACHE
        .try_with(|state| {
            let mut state = state.borrow_mut();
            if state.hashes.contains(&hash) {
                return Ok(());
            }
            reserve_locked(&mut state, hash, size)
        })
        .map_err(|_| io::Error::other("preview object cache is not initialized"))?
}

/// Reject a whole not-yet-sized batch before the storage backend is asked to
/// inspect it. This keeps the object-count bound ahead of loose decompression
/// and pack-index scans.
pub fn ensure_object_capacity(additional: usize) -> io::Result<()> {
    PREVIEW_CACHE
        .try_with(|state| {
            let state = state.borrow();
            let current = state.hashes.len() + state.pending.len();
            let total = current.checked_add(additional).ok_or_else(|| {
                io::Error::new(io::ErrorKind::InvalidData, "preview object count overflow")
            })?;
            if total > state.max_cache_objects {
                return Err(object_count_error(state.max_cache_objects));
            }
            Ok(())
        })
        .map_err(|_| io::Error::other("preview object cache is not initialized"))?
}

/// Remaining aggregate payload budget before any storage sizing work starts.
pub fn remaining_cache_bytes() -> io::Result<u64> {
    PREVIEW_CACHE
        .try_with(|state| {
            let state = state.borrow();
            Ok(state.max_cache_bytes.saturating_sub(state.bytes))
        })
        .map_err(|_| io::Error::other("preview object cache is not initialized"))?
}

fn reserve_locked(state: &mut PreviewCache, hash: ObjectHash, size: u64) -> io::Result<()> {
    if state.hashes.contains(&hash) {
        return Ok(());
    }
    validate_limits(state, size, state.hashes.len() + state.pending.len())?;
    let charge = charged_bytes(size);
    state.bytes = state
        .bytes
        .checked_add(charge)
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidData, "preview cache size overflow"))?;
    state.hashes.insert(hash);
    Ok(())
}

fn validate_limits(state: &PreviewCache, size: u64, object_count: usize) -> io::Result<()> {
    if size > state.max_object_bytes {
        return Err(limit_error("object", state.max_object_bytes));
    }
    if object_count >= state.max_cache_objects {
        return Err(object_count_error(state.max_cache_objects));
    }
    let charge = charged_bytes(size);
    let total = state
        .bytes
        .checked_add(charge)
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidData, "preview cache size overflow"))?;
    if total > state.max_cache_bytes {
        return Err(limit_error("aggregate cache", state.max_cache_bytes));
    }
    Ok(())
}

fn object_count_error(limit: usize) -> io::Error {
    io::Error::new(
        io::ErrorKind::InvalidData,
        format!("preview object count exceeds {limit}; rerun without --verbose"),
    )
}

pub fn is_active() -> bool {
    PREVIEW_CACHE.try_with(|_| ()).is_ok()
}

pub fn contains(hash: &ObjectHash) -> bool {
    PREVIEW_CACHE
        .try_with(|state| state.borrow().hashes.contains(hash))
        .unwrap_or(false)
}

/// Read one cached raw blob without exposing it outside the preview task.
pub fn read(hash: &ObjectHash) -> io::Result<Option<Vec<u8>>> {
    let path = match PREVIEW_CACHE.try_with(|state| state.borrow().directory.join(hash.to_string()))
    {
        Ok(path) => path,
        Err(_) => return Ok(None),
    };
    match fs::read(path) {
        Ok(content) => Ok(Some(content)),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(error),
    }
}

/// Read at most `limit + 1` bytes so a growing file cannot bypass the memory cap.
pub fn read_file_bounded(path: &Path, limit: u64) -> io::Result<Vec<u8>> {
    let file = fs::File::open(path)?;
    let mut content = Vec::new();
    file.take(limit.saturating_add(1))
        .read_to_end(&mut content)?;
    if content.len() as u64 > limit {
        return Err(limit_error("object", limit));
    }
    Ok(content)
}

fn limit_error(kind: &str, limit: u64) -> io::Error {
    io::Error::new(
        io::ErrorKind::InvalidData,
        format!("preview {kind} exceeds {limit} bytes; rerun without --verbose"),
    )
}

#[cfg(test)]
mod tests {
    use git_internal::{hash::ObjectHash, internal::object::types::ObjectType};

    use super::{cache, read_file_bounded, reserve, reserve_pending, with_limits};

    #[tokio::test]
    async fn cache_enforces_per_object_and_aggregate_limits() {
        let temp = tempfile::tempdir().expect("create preview cache directory");
        with_limits(temp.path().to_path_buf(), 4, 8_191, 8, async {
            let first = b"four";
            let first_hash = ObjectHash::from_type_and_data(ObjectType::Blob, first);
            cache(first_hash, first).expect("cache object at per-object limit");

            let second = b"tri";
            let second_hash = ObjectHash::from_type_and_data(ObjectType::Blob, second);
            let error = cache(second_hash, second).expect_err("aggregate limit must fail");
            assert!(error.to_string().contains("aggregate"), "{error}");
        })
        .await;
    }

    #[tokio::test]
    async fn cache_bounds_tiny_object_metadata_by_count() {
        let temp = tempfile::tempdir().expect("create preview cache directory");
        with_limits(temp.path().to_path_buf(), 4, 16 * 1024, 2, async {
            for &byte in b"ab" {
                let content = [byte];
                let hash = ObjectHash::from_type_and_data(ObjectType::Blob, &content);
                reserve(hash, 1).expect("reserve within object count");
            }
            let third = ObjectHash::from_type_and_data(ObjectType::Blob, b"c");
            let error = reserve(third, 1).expect_err("object count must be bounded");
            assert!(error.to_string().contains("object count"), "{error}");
        })
        .await;
    }

    #[tokio::test]
    async fn pending_reservations_enforce_limits_before_content_is_loaded() {
        let temp = tempfile::tempdir().expect("create preview cache directory");
        with_limits(temp.path().to_path_buf(), 8, 8_191, 2, async {
            let _first = reserve_pending(4).expect("reserve first pending object");
            let aggregate = reserve_pending(4).expect_err("second object must exceed aggregate");
            assert!(aggregate.to_string().contains("aggregate"), "{aggregate}");
        })
        .await;

        let temp = tempfile::tempdir().expect("create preview cache directory");
        with_limits(temp.path().to_path_buf(), 8, 16 * 1024, 1, async {
            let _first = reserve_pending(1).expect("reserve first pending object");
            let count = reserve_pending(1).expect_err("second object must exceed count");
            assert!(count.to_string().contains("object count"), "{count}");
        })
        .await;
    }

    #[test]
    fn bounded_file_read_rejects_content_beyond_limit() {
        let temp = tempfile::tempdir().expect("create bounded-read directory");
        let path = temp.path().join("large.bin");
        std::fs::write(&path, b"12345").expect("write bounded-read fixture");
        let error = read_file_bounded(&path, 4).expect_err("oversized file must fail");
        assert!(error.to_string().contains("4 bytes"), "{error}");
    }
}
