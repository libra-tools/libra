//! Tiered storage controller for Git objects. This module implements a tiered storage system that combines a local filesystem backend (LocalStorage) and a remote storage backend (RemoteStorage). The TieredStorage struct manages the logic for storing and retrieving Git objects based on their size, using an LRU cache to manage large objects stored locally as a cache layer.
use std::{
    fs,
    path::PathBuf,
    sync::{Arc, Mutex},
};

use async_trait::async_trait;
use git_internal::{
    errors::GitError,
    hash::{HashKind, ObjectHash},
    internal::object::types::ObjectType,
};
use lru_mem::{HeapSize, LruCache};

use super::{EvictReport, EvictRequest, Storage, local::LocalStorage, remote::RemoteStorage};
use crate::utils::read_policy::{ReadPolicy, read_policy};

/// Verify that a fetched object's bytes hash back to their claimed OID, before
/// the object is cached locally or returned.
///
/// lore.md §0.3 (取数即校验): a remote/durable-tier object must never be blindly
/// trusted — a corrupted or tampered payload must not poison the local cache or
/// reach the caller. The payload is reframed as a git object
/// `"<type> <len>\0<content>"` and hashed.
///
/// The hash algorithm is chosen from **`expected.kind()`**, NOT the ambient
/// thread-local `HashKind`. `ClientStorage::get` runs storage futures on a
/// spawned static-runtime worker thread (`client_storage.rs`) whose thread-local
/// `HashKind` is never set and defaults to SHA-1; hashing there via the
/// thread-local would recompute a SHA-1 OID and falsely reject a valid SHA-256
/// object. Deriving the algorithm from the requested OID is correct for both
/// SHA-1 and SHA-256 repositories regardless of which thread runs the check.
///
/// # Arguments
/// * `expected` - the OID the caller asked for.
/// * `obj_type` - the object type parsed from the fetched header.
/// * `data` - the fetched, header-stripped object content.
pub(crate) fn verify_fetched_object(
    expected: &ObjectHash,
    obj_type: ObjectType,
    data: &[u8],
) -> Result<(), GitError> {
    let type_bytes = obj_type.to_data().map_err(|e| {
        GitError::InvalidObjectInfo(format!("unknown object type for fetched object: {e}"))
    })?;
    // Reframe as a git object: "<type> <len>\0<content>".
    let mut framed = Vec::with_capacity(type_bytes.len() + data.len() + 24);
    framed.extend_from_slice(&type_bytes);
    framed.push(b' ');
    framed.extend_from_slice(data.len().to_string().as_bytes());
    framed.push(0);
    framed.extend_from_slice(data);

    let computed = match expected.kind() {
        HashKind::Sha1 => {
            use sha1::{Digest, Sha1};
            ObjectHash::Sha1(Sha1::digest(&framed).into())
        }
        HashKind::Sha256 => {
            use sha2::{Digest, Sha256};
            ObjectHash::Sha256(Sha256::digest(&framed).into())
        }
    };

    if &computed == expected {
        Ok(())
    } else {
        Err(GitError::InvalidObjectInfo(format!(
            "remote object {expected} failed integrity check: {obj_type} payload hashes to {computed}"
        )))
    }
}

/// Wrapper for cached file to handle deletion on eviction
#[derive(Debug)]
struct CachedFile {
    path: PathBuf,
    /// LRU accounting size in bytes — the **uncompressed** object
    /// length (`data.len()` at insert time), used as the resource cost
    /// for the `LruCache` budget.
    ///
    /// NOTE: this is *not* the literal on-disk byte count.
    /// [`LocalStorage::put`] writes zlib-compressed loose objects, so
    /// the actual file is typically smaller than `disk_size`. Using the
    /// uncompressed length makes the LRU budget a **conservative
    /// (over-estimating) upper bound** on real disk use — the cache
    /// evicts at or before the configured limit, never after, so the
    /// disk footprint stays bounded. Switching to the true compressed
    /// size would require stat-ing the file after each write.
    disk_size: usize,
}

impl HeapSize for CachedFile {
    fn heap_size(&self) -> usize {
        // The LRU cache bounds cached-object resource cost; we report
        // `disk_size` (the uncompressed object length — see the field
        // doc) as that cost, not the struct's in-memory size.
        self.disk_size
    }
}

impl Drop for CachedFile {
    fn drop(&mut self) {
        // Delete file when removed from LRU (or when TieredStorage is dropped)
        // Note: This might be dangerous if we are shutting down and want to keep cache?
        // But for "Cache", it's ephemeral.
        let _ = fs::remove_file(&self.path);
    }
}

/// Tiered storage controller
pub struct TieredStorage {
    local: LocalStorage,
    remote: RemoteStorage,
    threshold: usize,
    // LRU cache for tracking large files stored locally
    // Key: ObjectHash
    // Value: CachedFile (owns the cleanup responsibility)
    // Note: This tracks disk usage of cached files, not memory usage of the struct itself.
    lru: Arc<Mutex<LruCache<ObjectHash, CachedFile>>>,
}

impl TieredStorage {
    pub fn new(
        local: LocalStorage,
        remote: RemoteStorage,
        threshold: usize,
        disk_usage_limit: usize,
    ) -> Self {
        Self {
            local,
            remote,
            threshold,
            lru: Arc::new(Mutex::new(LruCache::new(disk_usage_limit))),
        }
    }

    /// Write a freshly-fetched (already-verified) object into the local cache,
    /// tracking large objects in the LRU so they remain subject to
    /// `LIBRA_STORAGE_CACHE_SIZE` eviction. Small objects are stored permanently
    /// (untracked). Shared by the `get` fetch paths.
    async fn cache_fetched_object(
        &self,
        hash: &ObjectHash,
        data: &[u8],
        obj_type: ObjectType,
    ) -> Result<(), GitError> {
        self.local.put(hash, data, obj_type).await?;
        if data.len() >= self.threshold {
            // Pre-trim victims INSIDE the lock, delete their files OUTSIDE
            // it (lore.md:698 — the put/get/heal hot path must not perform
            // unlink I/O while holding the LRU mutex). Deletion stays
            // synchronous on this call (a fire-and-forget task could die at
            // process exit and silently leak past the budget).
            let victims: Vec<CachedFile> = {
                // Recover from a poisoned lock rather than panicking during
                // get/put/heal: the LRU only guards cache bookkeeping (never
                // the object bytes, which are already written), so its
                // contents stay valid even if another thread panicked while
                // holding the lock.
                let mut lru = self
                    .lru
                    .lock()
                    .unwrap_or_else(|poisoned| poisoned.into_inner());
                // If this object is already tracked (e.g. a `--remote` refresh
                // or a heal of a corrupt cached object), the file was just
                // rewritten in place at the same content-addressed path. Only
                // touch recency — re-inserting a new `CachedFile` would evict
                // the old entry, whose `Drop` deletes that very path, removing
                // the object we just wrote.
                if lru.get(hash).is_some() {
                    Vec::new()
                } else {
                    let mut victims = Vec::new();
                    let incoming = data.len();
                    while lru.current_size() + incoming > lru.max_size()
                        && let Some((_, victim)) = lru.remove_lru()
                    {
                        victims.push(victim);
                    }
                    let path = self.local.get_obj_path(hash);
                    let _ = lru.insert(
                        *hash,
                        CachedFile {
                            path,
                            disk_size: incoming,
                        },
                    );
                    victims
                }
            };
            // Dropping the victims unlinks their files — lock released.
            drop(victims);
        }
        Ok(())
    }
}

#[async_trait]
impl Storage for TieredStorage {
    async fn get(&self, hash: &ObjectHash) -> Result<(Vec<u8>, ObjectType), GitError> {
        let policy = read_policy();

        // `--remote`: refresh from the durable tier even on a local hit. Fall
        // back to the local copy only when the object is absent remotely.
        if policy == ReadPolicy::Remote {
            match self.remote.get(hash).await {
                Ok((data, obj_type)) => {
                    verify_fetched_object(hash, obj_type, &data)?;
                    self.cache_fetched_object(hash, &data, obj_type).await?;
                    return Ok((data, obj_type));
                }
                Err(GitError::ObjectNotFound(_)) => { /* fall back to local below */ }
                Err(err) => return Err(err),
            }
        }

        // Local first (Auto and LocalOnly).
        if self.local.exist(hash).await {
            // If it's in LRU, access it to update recency. Recover from a
            // poisoned lock rather than crashing a read (the LRU guards only
            // bookkeeping, not the object bytes).
            {
                let mut lru = self
                    .lru
                    .lock()
                    .unwrap_or_else(|poisoned| poisoned.into_inner());
                let _ = lru.get(hash);
            }
            match self.local.get(hash).await {
                Ok(hit) => return Ok(hit),
                // SELF-HEALING READ (lore.md 2.9): the file can vanish
                // between exist() and get() — an evictor, an external
                // cleaner, or a crash artifact. Under any policy that may
                // reach the durable tier, fall through to the remote fetch
                // (which re-verifies and re-caches) instead of failing.
                Err(_) if policy != ReadPolicy::LocalOnly => {}
                Err(error) => return Err(error),
            }
        }

        // Not local. Under `--offline`/`--local`, never reach for the durable
        // tier — surface a clear, actionable error instead (lore.md §0.8).
        if policy == ReadPolicy::LocalOnly {
            return Err(GitError::ObjectNotFound(format!(
                "object {hash} is not in the local store; the offline/local read \
                 policy forbids fetching it from the durable tier (drop --offline/--local \
                 or run without it to fetch)"
            )));
        }

        // Auto (or `--remote` remote-miss fallthrough): fetch from the durable tier.
        let (data, obj_type) = self.remote.get(hash).await?;
        // Verify-on-cache: reject a payload that does not hash to the requested
        // OID before it can poison the local cache (lore.md §0.3).
        verify_fetched_object(hash, obj_type, &data)?;
        self.cache_fetched_object(hash, &data, obj_type).await?;
        Ok((data, obj_type))
    }

    async fn put(
        &self,
        hash: &ObjectHash,
        data: &[u8],
        obj_type: ObjectType,
    ) -> Result<String, GitError> {
        // Always write to remote (Persistence).
        let remote_res = self.remote.put(hash, data, obj_type).await?;

        // Cache locally (small = permanent, large = LRU-tracked) through the same
        // helper as the fetch paths, which touches-don't-reinserts an already-
        // tracked object so re-putting a large object cannot drop its `CachedFile`
        // and delete the file it just wrote.
        self.cache_fetched_object(hash, data, obj_type).await?;

        Ok(remote_res)
    }

    async fn exist(&self, hash: &ObjectHash) -> bool {
        // Check local first (fast)
        if self.local.exist(hash).await {
            return true;
        }
        // Then remote
        self.remote.exist(hash).await
    }

    /// Answer local hits without any round trip, then batch-probe the remote
    /// for the misses in a single bounded-concurrency call (`lore.md` §0.6).
    async fn exist_batch(&self, hashes: &[ObjectHash]) -> Vec<bool> {
        let mut results = vec![false; hashes.len()];
        // Indices (and hashes) that missed locally and need a remote probe.
        let mut remote_indices = Vec::new();
        let mut remote_hashes = Vec::new();
        for (idx, hash) in hashes.iter().enumerate() {
            if self.local.exist(hash).await {
                results[idx] = true;
            } else {
                remote_indices.push(idx);
                remote_hashes.push(*hash);
            }
        }
        if !remote_hashes.is_empty() {
            let remote_results = self.remote.exist_batch(&remote_hashes).await;
            for (idx, exists) in remote_indices.into_iter().zip(remote_results) {
                results[idx] = exists;
            }
        }
        results
    }

    async fn search(&self, prefix: &str) -> Vec<ObjectHash> {
        let (local_res, remote_res) =
            futures::future::join(self.local.search(prefix), self.remote.search(prefix)).await;

        let mut results = std::collections::HashSet::new();
        results.extend(local_res);
        results.extend(remote_res);

        results.into_iter().collect()
    }

    /// Re-fetch a missing or corrupted object from the durable (remote) tier,
    /// verify it, and (over)write it into the local store. lore.md §0.4.
    ///
    /// This deliberately bypasses the local-first short-circuit in [`Self::get`]:
    /// a corrupted local object must be replaced with a fresh, verified copy, so
    /// the fetch always goes to the durable tier. The local write only happens
    /// after verification succeeds, so a failed or absent remote never destroys
    /// the existing (even if corrupt) local object — and a bad payload is never
    /// persisted (no fabrication). `remote.get` inherits object_store's bounded
    /// 429/`SlowDown`/5xx backoff (lore.md §0.2); `verify_fetched_object` is the
    /// same integrity check as verify-on-cache (lore.md §0.3).
    async fn exist_checked(&self, hash: &ObjectHash) -> Result<bool, GitError> {
        if self.local.exist(hash).await {
            return Ok(true);
        }
        self.remote.exist_checked(hash).await
    }

    /// Evict verified-durable large loose objects until under budget
    /// (lore.md 2.9). Safety: every victim gets an error-aware durability
    /// probe IMMEDIATELY before its unlink — confirmed-absent and
    /// probe-error objects are skipped (and counted separately: absence is
    /// actionable by push/backup, an error is an outage); three consecutive
    /// leading probe errors abort the whole run (unreachable tier, nothing
    /// deleted). RESIDUAL RISK (documented): presence ≠ integrity — a
    /// corrupt remote copy would make the local one the only good copy;
    /// v1 accepts this citing S3/R2 server-side integrity (a --verify deep
    /// probe is a recorded follow-up).
    async fn evict_local(&self, request: EvictRequest) -> Result<Option<EvictReport>, GitError> {
        let mut report = EvictReport::default();
        let now = std::time::SystemTime::now();
        let mut rows = self.local.list_loose_with_meta();
        report.scanned = rows.len();
        rows.retain(|(_, _, _, size)| *size as usize >= self.threshold);
        report.candidate_count = rows.len();
        report.candidate_bytes = rows.iter().map(|(_, _, _, size)| *size).sum();

        // Victim order: oldest mtime first (materialization recency), then
        // larger-first, then OID — deterministic from filesystem state.
        rows.sort_by(|a, b| {
            a.2.cmp(&b.2)
                .then(b.3.cmp(&a.3))
                .then(a.0.to_string().cmp(&b.0.to_string()))
        });

        let mut remaining = report.candidate_bytes;
        let mut consecutive_leading_errors = 0usize;
        let mut probed_any_success = false;
        for (hash, path, mtime, size) in rows {
            if remaining <= request.budget_bytes {
                break;
            }
            if let Ok(age) = now.duration_since(mtime)
                && age.as_secs() < request.min_age_secs
            {
                report.skipped_recent += 1;
                continue;
            }
            // Error-aware probe immediately before the unlink.
            match self.remote.exist_checked(&hash).await {
                Ok(true) => {
                    probed_any_success = true;
                    consecutive_leading_errors = 0;
                    report.verified += 1;
                    if !request.dry_run {
                        if let Err(error) = std::fs::remove_file(&path)
                            && error.kind() != std::io::ErrorKind::NotFound
                        {
                            return Err(GitError::IOError(error));
                        }
                        // Keep any in-process LRU accounting coherent.
                        let mut lru = self
                            .lru
                            .lock()
                            .unwrap_or_else(|poisoned| poisoned.into_inner());
                        if let Some(entry) = lru.remove(&hash) {
                            // The file is already gone; don't let Drop
                            // unlink a path another process may recreate.
                            std::mem::forget(entry);
                        }
                    }
                    report.evicted += 1;
                    report.reclaimed_bytes += size;
                    remaining -= size;
                    if report.evicted_objects.len() < 100 {
                        report.evicted_objects.push((hash.to_string(), size));
                    }
                }
                Ok(false) => {
                    probed_any_success = true;
                    consecutive_leading_errors = 0;
                    report.skipped_absent += 1;
                }
                Err(_) => {
                    report.skipped_probe_error += 1;
                    if !probed_any_success {
                        consecutive_leading_errors += 1;
                        if consecutive_leading_errors >= 3 {
                            return Err(GitError::IOError(std::io::Error::other(
                                "the durable tier is unreachable (3 consecutive probe \
                                 failures); nothing was evicted — retry when the tier is \
                                 reachable",
                            )));
                        }
                    }
                }
            }
        }
        Ok(Some(report))
    }

    async fn heal(&self, hash: &ObjectHash) -> Result<bool, GitError> {
        let (data, obj_type) = match self.remote.get(hash).await {
            Ok(pair) => pair,
            // Not present in the durable tier: unrecoverable, but not an error —
            // the caller reports it rather than fabricating anything.
            Err(GitError::ObjectNotFound(_)) => return Ok(false),
            Err(err) => return Err(err),
        };
        verify_fetched_object(hash, obj_type, &data)?;
        // Overwrite/create the local object (LocalStorage::put truncates) and
        // track a large one in the LRU — shared with `get`, including the
        // touch-don't-reinsert handling for an already-cached object.
        self.cache_fetched_object(hash, &data, obj_type).await?;
        Ok(true)
    }
}

#[cfg(test)]
mod tests {
    use std::io::Write;

    use serial_test::serial;
    use tempfile::tempdir;

    use super::*;

    /// Create a real file of `size` bytes and wrap it in a `CachedFile`
    /// whose `disk_size` matches. Returns the file path so the test can
    /// assert presence/deletion.
    fn cached_file(dir: &std::path::Path, name: &str, size: usize) -> (PathBuf, CachedFile) {
        let path = dir.join(name);
        let mut f = fs::File::create(&path).expect("create cache file");
        f.write_all(&vec![0u8; size]).expect("write cache file");
        (
            path.clone(),
            CachedFile {
                path,
                disk_size: size,
            },
        )
    }

    /// Verify-on-cache accepts a payload that hashes to the requested OID and
    /// rejects any mismatch, under SHA-1. Covers the lore.md §0.3 requirement
    /// that both hash formats be exercised.
    #[test]
    fn verify_fetched_object_matches_and_mismatches_sha1() {
        use git_internal::hash::{HashKind, set_hash_kind_for_test};

        let _guard = set_hash_kind_for_test(HashKind::Sha1);
        let data = b"hello libra";
        let expected = ObjectHash::from_type_and_data(ObjectType::Blob, data);

        assert!(verify_fetched_object(&expected, ObjectType::Blob, data).is_ok());
        // Tampered content no longer hashes to the requested OID.
        assert!(verify_fetched_object(&expected, ObjectType::Blob, b"HELLO libra").is_err());
        // Wrong object type changes the `<type> <len>\0` framing → mismatch.
        assert!(verify_fetched_object(&expected, ObjectType::Commit, data).is_err());
    }

    /// Same contract under SHA-256, so an object-format-256 repository is also
    /// protected against a poisoned cache write.
    #[test]
    fn verify_fetched_object_matches_and_mismatches_sha256() {
        use git_internal::hash::{HashKind, set_hash_kind_for_test};

        let _guard = set_hash_kind_for_test(HashKind::Sha256);
        let data = b"hello libra";
        let expected = ObjectHash::from_type_and_data(ObjectType::Blob, data);

        assert!(verify_fetched_object(&expected, ObjectType::Blob, data).is_ok());
        assert!(verify_fetched_object(&expected, ObjectType::Blob, b"tampered").is_err());
    }

    /// Regression: `ClientStorage::get` runs the tiered fetch on a spawned
    /// static-runtime worker thread whose thread-local `HashKind` defaults to
    /// SHA-1. Verification MUST derive the algorithm from the requested OID, not
    /// the ambient thread-local — otherwise a valid SHA-256 object would be
    /// hashed as SHA-1 and falsely rejected. This test forces exactly that
    /// mismatch (ambient SHA-1, SHA-256 OID) and asserts the object still passes.
    #[test]
    fn verify_uses_requested_oid_kind_not_ambient_hash_kind() {
        use git_internal::hash::{HashKind, set_hash_kind_for_test};

        let data = b"hello libra";
        // Compute the SHA-256 OID under a scoped SHA-256 ambient kind.
        let expected_sha256 = {
            let _sha256 = set_hash_kind_for_test(HashKind::Sha256);
            ObjectHash::from_type_and_data(ObjectType::Blob, data)
        };
        assert!(matches!(expected_sha256, ObjectHash::Sha256(_)));

        // Now pin the ambient kind to SHA-1 (the spawned-worker default) and
        // confirm the SHA-256 object still verifies, and a tamper still fails.
        let _ambient_sha1 = set_hash_kind_for_test(HashKind::Sha1);
        assert!(verify_fetched_object(&expected_sha256, ObjectType::Blob, data).is_ok());
        assert!(verify_fetched_object(&expected_sha256, ObjectType::Blob, b"tampered").is_err());
    }

    /// End-to-end through `TieredStorage::get`: an object present only in the
    /// remote tier is fetched, verified, cached, and returned unchanged.
    // Serialised with the read-policy tests: `get` reads the process-global
    // read policy, so a concurrent policy test must not change it mid-run.
    #[tokio::test]
    #[serial]
    async fn tiered_get_verifies_and_caches_valid_remote_object() {
        use std::sync::Arc;

        use git_internal::hash::{HashKind, set_hash_kind_for_test};

        let _kind = set_hash_kind_for_test(HashKind::Sha1);
        let obj_type = ObjectType::Blob;
        let data = b"tiered verify happy path".to_vec();
        let hash = ObjectHash::from_type_and_data(obj_type, &data);

        let remote = RemoteStorage::new(Arc::new(object_store::memory::InMemory::new()));
        remote
            .put(&hash, &data, obj_type)
            .await
            .expect("seed remote");

        let local_dir = tempdir().expect("tempdir");
        let tiered = TieredStorage::new(
            LocalStorage::new(local_dir.path().to_path_buf()),
            remote,
            1 << 20,
            1 << 20,
        );

        // Empty local cache → fetch from remote, verify, cache, return.
        let (got, got_type) = tiered.get(&hash).await.expect("get should succeed");
        assert_eq!(got, data);
        assert_eq!(got_type, obj_type);
        assert!(tiered.local.exist(&hash).await, "object should be cached");
    }

    /// End-to-end: a remote object whose bytes do not hash to the requested OID
    /// (corruption/tampering in the durable tier) is rejected by `get`, and is
    /// NOT written into the local cache.
    #[tokio::test]
    #[serial]
    async fn tiered_get_rejects_and_does_not_cache_corrupted_remote_object() {
        use std::sync::Arc;

        use git_internal::hash::{HashKind, set_hash_kind_for_test};

        let _kind = set_hash_kind_for_test(HashKind::Sha1);
        let obj_type = ObjectType::Blob;
        let good = b"the original bytes".to_vec();
        let hash = ObjectHash::from_type_and_data(obj_type, &good);

        // Store DIFFERENT bytes at the requested OID's location.
        let remote = RemoteStorage::new(Arc::new(object_store::memory::InMemory::new()));
        remote
            .put(&hash, b"tampered payload", obj_type)
            .await
            .expect("seed remote");

        let local_dir = tempdir().expect("tempdir");
        let tiered = TieredStorage::new(
            LocalStorage::new(local_dir.path().to_path_buf()),
            remote,
            1 << 20,
            1 << 20,
        );

        assert!(
            tiered.get(&hash).await.is_err(),
            "corrupted object must be rejected"
        );
        assert!(
            !tiered.local.exist(&hash).await,
            "corrupted object must not be cached"
        );
    }

    /// `--offline`/`--local` (ReadPolicy::LocalOnly): a remote-only object is a
    /// clear error and is NOT fetched or cached.
    #[tokio::test]
    #[serial]
    async fn get_local_only_policy_forbids_remote_fetch() {
        use std::sync::Arc;

        use git_internal::hash::{HashKind, set_hash_kind_for_test};

        use crate::utils::read_policy::{ReadPolicy, read_policy, set_read_policy};

        let _kind = set_hash_kind_for_test(HashKind::Sha1);
        let obj_type = ObjectType::Blob;
        let data = b"remote only".to_vec();
        let hash = ObjectHash::from_type_and_data(obj_type, &data);
        let remote = RemoteStorage::new(Arc::new(object_store::memory::InMemory::new()));
        remote.put(&hash, &data, obj_type).await.unwrap();
        let dir = tempdir().unwrap();
        let tiered = TieredStorage::new(
            LocalStorage::new(dir.path().to_path_buf()),
            remote,
            1 << 20,
            1 << 20,
        );

        let previous = read_policy();
        set_read_policy(ReadPolicy::LocalOnly);
        let result = tiered.get(&hash).await;
        set_read_policy(previous);

        assert!(result.is_err(), "local-only must not fetch a remote object");
        assert!(
            !tiered.local.exist(&hash).await,
            "local-only must not cache the remote object"
        );
    }

    /// `--remote` (ReadPolicy::Remote): fetch/refresh from the durable tier and
    /// cache locally, even though the object was not present locally.
    #[tokio::test]
    #[serial]
    async fn get_remote_policy_fetches_and_caches() {
        use std::sync::Arc;

        use git_internal::hash::{HashKind, set_hash_kind_for_test};

        use crate::utils::read_policy::{ReadPolicy, read_policy, set_read_policy};

        let _kind = set_hash_kind_for_test(HashKind::Sha1);
        let obj_type = ObjectType::Blob;
        let data = b"refresh me".to_vec();
        let hash = ObjectHash::from_type_and_data(obj_type, &data);
        let remote = RemoteStorage::new(Arc::new(object_store::memory::InMemory::new()));
        remote.put(&hash, &data, obj_type).await.unwrap();
        let dir = tempdir().unwrap();
        let tiered = TieredStorage::new(
            LocalStorage::new(dir.path().to_path_buf()),
            remote,
            1 << 20,
            1 << 20,
        );

        let previous = read_policy();
        set_read_policy(ReadPolicy::Remote);
        let got = tiered.get(&hash).await;
        set_read_policy(previous);

        let (bytes, _) = got.expect("remote policy should fetch from the durable tier");
        assert_eq!(bytes, data);
        assert!(
            tiered.local.exist(&hash).await,
            "remote-fetched object should be cached locally"
        );
    }

    /// `--remote` falls back to the local copy when the object is absent from the
    /// durable tier.
    #[tokio::test]
    #[serial]
    async fn get_remote_policy_falls_back_to_local_on_remote_miss() {
        use std::sync::Arc;

        use git_internal::hash::{HashKind, set_hash_kind_for_test};

        use crate::utils::read_policy::{ReadPolicy, read_policy, set_read_policy};

        let _kind = set_hash_kind_for_test(HashKind::Sha1);
        let obj_type = ObjectType::Blob;
        let data = b"local only".to_vec();
        let hash = ObjectHash::from_type_and_data(obj_type, &data);
        // Empty remote; the object lives only locally.
        let remote = RemoteStorage::new(Arc::new(object_store::memory::InMemory::new()));
        let dir = tempdir().unwrap();
        let local = LocalStorage::new(dir.path().to_path_buf());
        local.put(&hash, &data, obj_type).await.unwrap();
        let tiered = TieredStorage::new(local, remote, 1 << 20, 1 << 20);

        let previous = read_policy();
        set_read_policy(ReadPolicy::Remote);
        let got = tiered.get(&hash).await;
        set_read_policy(previous);

        let (bytes, _) = got.expect("remote-miss should fall back to the local copy");
        assert_eq!(bytes, data);
    }

    /// Regression: a `--remote` refresh of a LARGE object already tracked in the
    /// LRU must not delete it. Re-inserting a fresh `CachedFile` would drop the
    /// old entry, whose `Drop` deletes the just-rewritten file at the same path.
    #[tokio::test]
    #[serial]
    async fn get_remote_refresh_of_large_cached_object_survives() {
        use std::sync::Arc;

        use git_internal::hash::{HashKind, set_hash_kind_for_test};

        use crate::utils::read_policy::{ReadPolicy, read_policy, set_read_policy};

        let _kind = set_hash_kind_for_test(HashKind::Sha1);
        let obj_type = ObjectType::Blob;
        let data = vec![7u8; 128]; // "large" relative to the threshold below
        let hash = ObjectHash::from_type_and_data(obj_type, &data);
        let remote = RemoteStorage::new(Arc::new(object_store::memory::InMemory::new()));
        remote.put(&hash, &data, obj_type).await.unwrap();
        let dir = tempdir().unwrap();
        // threshold=16 so the 128-byte object is LRU-tracked; cap generous.
        let tiered = TieredStorage::new(
            LocalStorage::new(dir.path().to_path_buf()),
            remote,
            16,
            1 << 20,
        );

        let previous = read_policy();

        // Auto fetch first: caches the large object into local + LRU.
        set_read_policy(ReadPolicy::Auto);
        let _ = tiered.get(&hash).await.expect("auto fetch");
        assert!(tiered.local.exist(&hash).await, "cached after auto get");

        // `--remote` refresh must keep the cached object, not delete it.
        set_read_policy(ReadPolicy::Remote);
        let (bytes, _) = tiered.get(&hash).await.expect("remote refresh");
        set_read_policy(previous);

        assert_eq!(bytes, data);
        assert!(
            tiered.local.exist(&hash).await,
            "large cached object must survive a --remote refresh"
        );
    }

    /// Regression: putting a LARGE object twice must not delete it. The second
    /// `put` re-caches an already-LRU-tracked object; re-inserting would drop the
    /// old `CachedFile`, deleting the just-written file.
    #[tokio::test]
    async fn put_large_object_twice_keeps_it_cached() {
        use std::sync::Arc;

        use git_internal::hash::{HashKind, set_hash_kind_for_test};

        let _kind = set_hash_kind_for_test(HashKind::Sha1);
        let obj_type = ObjectType::Blob;
        let data = vec![9u8; 128]; // "large" relative to the threshold below
        let hash = ObjectHash::from_type_and_data(obj_type, &data);
        let remote = RemoteStorage::new(Arc::new(object_store::memory::InMemory::new()));
        let dir = tempdir().unwrap();
        let tiered = TieredStorage::new(
            LocalStorage::new(dir.path().to_path_buf()),
            remote,
            16,
            1 << 20,
        );

        tiered.put(&hash, &data, obj_type).await.expect("first put");
        assert!(tiered.local.exist(&hash).await, "cached after first put");
        tiered
            .put(&hash, &data, obj_type)
            .await
            .expect("second put");
        assert!(
            tiered.local.exist(&hash).await,
            "large object must survive a second put"
        );
    }

    /// `heal` fetches a missing object from the durable (remote) tier, verifies
    /// it, and writes it into the local store.
    #[tokio::test]
    async fn heal_recreates_missing_object_from_remote() {
        use std::sync::Arc;

        use git_internal::hash::{HashKind, set_hash_kind_for_test};

        let _kind = set_hash_kind_for_test(HashKind::Sha1);
        let obj_type = ObjectType::Blob;
        let data = b"heal me".to_vec();
        let hash = ObjectHash::from_type_and_data(obj_type, &data);

        let remote = RemoteStorage::new(Arc::new(object_store::memory::InMemory::new()));
        remote
            .put(&hash, &data, obj_type)
            .await
            .expect("seed remote");

        let local_dir = tempdir().expect("tempdir");
        let tiered = TieredStorage::new(
            LocalStorage::new(local_dir.path().to_path_buf()),
            remote,
            1 << 20,
            1 << 20,
        );

        assert!(
            !tiered.local.exist(&hash).await,
            "precondition: absent local"
        );
        assert!(tiered.heal(&hash).await.expect("heal"), "should heal");
        assert!(tiered.local.exist(&hash).await, "healed into local store");
        let (got, _) = tiered.local.get(&hash).await.expect("local get");
        assert_eq!(got, data);
    }

    /// `heal` replaces a corrupt local object with a fresh verified copy from the
    /// durable tier (overwrite, not skip).
    #[tokio::test]
    async fn heal_overwrites_corrupt_local_object() {
        use std::sync::Arc;

        use git_internal::hash::{HashKind, set_hash_kind_for_test};

        let _kind = set_hash_kind_for_test(HashKind::Sha1);
        let obj_type = ObjectType::Blob;
        let good = b"the good bytes".to_vec();
        let hash = ObjectHash::from_type_and_data(obj_type, &good);

        let remote = RemoteStorage::new(Arc::new(object_store::memory::InMemory::new()));
        remote
            .put(&hash, &good, obj_type)
            .await
            .expect("seed remote");

        let local_dir = tempdir().expect("tempdir");
        let local = LocalStorage::new(local_dir.path().to_path_buf());
        // Corrupt the local copy: wrong bytes stored under the correct OID path.
        local
            .put(&hash, b"corrupt bytes", obj_type)
            .await
            .expect("seed corrupt local");
        let tiered = TieredStorage::new(local, remote, 1 << 20, 1 << 20);

        assert!(tiered.heal(&hash).await.expect("heal"), "should heal");
        let (got, _) = tiered.local.get(&hash).await.expect("local get");
        assert_eq!(got, good, "corrupt local object replaced with good bytes");
    }

    /// `heal` returns `Ok(false)` (unrecoverable, no fabrication) when the object
    /// is absent from the durable tier.
    #[tokio::test]
    async fn heal_returns_false_when_absent_from_remote() {
        use std::sync::Arc;

        use git_internal::hash::{HashKind, set_hash_kind_for_test};

        let _kind = set_hash_kind_for_test(HashKind::Sha1);
        let obj_type = ObjectType::Blob;
        let data = b"never uploaded".to_vec();
        let hash = ObjectHash::from_type_and_data(obj_type, &data);

        let remote = RemoteStorage::new(Arc::new(object_store::memory::InMemory::new()));
        let local_dir = tempdir().expect("tempdir");
        let tiered = TieredStorage::new(
            LocalStorage::new(local_dir.path().to_path_buf()),
            remote,
            1 << 20,
            1 << 20,
        );

        assert!(!tiered.heal(&hash).await.expect("heal"), "unrecoverable");
        assert!(!tiered.local.exist(&hash).await, "nothing fabricated");
    }

    /// A local-only backend has no durable tier and uses the default `heal`,
    /// which cannot repair anything.
    #[tokio::test]
    async fn local_storage_cannot_heal() {
        use git_internal::hash::{HashKind, set_hash_kind_for_test};

        let _kind = set_hash_kind_for_test(HashKind::Sha1);
        let hash = ObjectHash::from_type_and_data(ObjectType::Blob, b"x");
        let local_dir = tempdir().expect("tempdir");
        let local = LocalStorage::new(local_dir.path().to_path_buf());
        assert!(!local.heal(&hash).await.expect("heal"));
    }

    /// `RemoteStorage::exist_batch` reports presence per input hash, in order,
    /// and handles an empty batch.
    #[tokio::test]
    async fn remote_exist_batch_preserves_order() {
        use std::sync::Arc;

        use git_internal::hash::{HashKind, set_hash_kind_for_test};

        let _kind = set_hash_kind_for_test(HashKind::Sha1);
        let remote = RemoteStorage::new(Arc::new(object_store::memory::InMemory::new()));
        let present = ObjectHash::from_type_and_data(ObjectType::Blob, b"present");
        let absent = ObjectHash::from_type_and_data(ObjectType::Blob, b"absent");
        remote
            .put(&present, b"present", ObjectType::Blob)
            .await
            .unwrap();

        assert_eq!(
            remote.exist_batch(&[absent, present, absent]).await,
            vec![false, true, false]
        );
        assert!(remote.exist_batch(&[]).await.is_empty());
    }

    /// `exist_batch` respects the global `--max-connections` limit (lore.md §0.9)
    /// — even at a cap of 1 (fully sequential) the results are correct and
    /// ordered.
    #[tokio::test]
    #[serial]
    async fn remote_exist_batch_respects_max_connections() {
        use std::sync::Arc;

        use git_internal::hash::{HashKind, set_hash_kind_for_test};

        use crate::utils::resource_limits::{max_connections, set_max_connections};

        let _kind = set_hash_kind_for_test(HashKind::Sha1);
        let remote = RemoteStorage::new(Arc::new(object_store::memory::InMemory::new()));
        let present = ObjectHash::from_type_and_data(ObjectType::Blob, b"present");
        let absent = ObjectHash::from_type_and_data(ObjectType::Blob, b"absent");
        remote
            .put(&present, b"present", ObjectType::Blob)
            .await
            .unwrap();

        let previous = max_connections();
        set_max_connections(1);
        let flags = remote.exist_batch(&[present, absent, present]).await;
        set_max_connections(previous);

        assert_eq!(flags, vec![true, false, true]);
    }

    /// `TieredStorage::exist_batch` answers local hits and batches the remote
    /// misses, preserving order.
    #[tokio::test]
    async fn tiered_exist_batch_mixes_local_and_remote() {
        use std::sync::Arc;

        use git_internal::hash::{HashKind, set_hash_kind_for_test};

        let _kind = set_hash_kind_for_test(HashKind::Sha1);
        let remote = RemoteStorage::new(Arc::new(object_store::memory::InMemory::new()));
        let in_remote = ObjectHash::from_type_and_data(ObjectType::Blob, b"remote");
        let in_local = ObjectHash::from_type_and_data(ObjectType::Blob, b"local");
        let nowhere = ObjectHash::from_type_and_data(ObjectType::Blob, b"nowhere");
        remote
            .put(&in_remote, b"remote", ObjectType::Blob)
            .await
            .unwrap();

        let dir = tempdir().unwrap();
        let local = LocalStorage::new(dir.path().to_path_buf());
        local
            .put(&in_local, b"local", ObjectType::Blob)
            .await
            .unwrap();
        let tiered = TieredStorage::new(local, remote, 1 << 20, 1 << 20);

        assert_eq!(
            tiered.exist_batch(&[in_local, in_remote, nowhere]).await,
            vec![true, true, false]
        );
    }

    /// `HeapSize::heap_size` MUST report the `disk_size` accounting
    /// field (the uncompressed object length — see the field doc) so
    /// the `LruCache`'s budget bounds cached-object resource cost. If
    /// this returned the struct's in-memory size instead, the cache
    /// would never evict on the intended threshold and the local cache
    /// dir would grow unbounded. (`disk_size` over-estimates the true
    /// compressed on-disk size, making the bound conservative.)
    #[test]
    fn cached_file_heap_size_reports_disk_size() {
        let dir = tempdir().expect("tempdir");
        let (_path, cf) = cached_file(dir.path(), "obj", 4096);
        assert_eq!(cf.heap_size(), 4096);
    }

    /// Dropping a `CachedFile` MUST delete its backing file — this is
    /// how an LRU eviction reclaims disk. Without it, evicted cache
    /// entries leak on disk forever.
    #[test]
    fn dropping_cached_file_deletes_backing_file() {
        let dir = tempdir().expect("tempdir");
        let (path, cf) = cached_file(dir.path(), "obj", 16);
        assert!(path.exists());
        drop(cf);
        assert!(
            !path.exists(),
            "CachedFile drop must delete its backing file",
        );
    }

    /// The combined resource-bounding contract: inserting past the
    /// `LruCache` disk budget evicts the least-recently-used entry AND
    /// its `Drop` deletes that entry's file, while the retained entry's
    /// file survives. This is what keeps the local large-object cache
    /// bounded on disk.
    #[test]
    fn lru_eviction_deletes_evicted_cache_file() {
        let dir = tempdir().expect("tempdir");
        // `LruCache` charges key + value + struct overhead per entry
        // (not just `heap_size`), so an entry for a 1000-byte file is
        // ~1096 bytes. A 1500-byte budget therefore holds exactly one
        // such entry but not two — the headroom keeps the test robust
        // against the exact per-entry overhead.
        let mut lru: LruCache<ObjectHash, CachedFile> = LruCache::new(1500);

        let key_a = ObjectHash::new(&[1; 20]);
        let key_b = ObjectHash::new(&[2; 20]);
        let (path_a, cf_a) = cached_file(dir.path(), "a", 1000);
        let (path_b, cf_b) = cached_file(dir.path(), "b", 1000);

        lru.insert(key_a, cf_a).expect("insert a within budget");
        assert!(path_a.exists());

        // Inserting B exceeds the budget (two ~1096-byte entries), so
        // the LRU evicts A; A's CachedFile drop deletes A's file.
        lru.insert(key_b, cf_b).expect("insert b evicts a");

        assert!(
            !path_a.exists(),
            "evicted entry A's backing file must be deleted on eviction",
        );
        assert!(path_b.exists(), "retained entry B's file must survive");
        assert!(lru.get(&key_b).is_some(), "B must remain cached");
        assert!(lru.get(&key_a).is_none(), "A must have been evicted");
    }

    /// The 2.9 evictor: verified-durable objects are evicted oldest-first to
    /// budget; absent objects are skipped (never delete the only copy);
    /// dry-run deletes nothing; the min-age floor holds; and a local read
    /// after eviction self-heals from the durable tier.
    #[tokio::test]
    #[serial]
    async fn evict_local_verifies_budget_and_self_heals() {
        use git_internal::hash::{HashKind, set_hash_kind_for_test};

        use crate::utils::storage::EvictRequest;

        let _kind = set_hash_kind_for_test(HashKind::Sha1);
        let obj_type = ObjectType::Blob;
        let threshold = 8usize; // tiny threshold: everything below is permanent

        let remote = RemoteStorage::new(Arc::new(object_store::memory::InMemory::new()));
        let local_dir = tempdir().expect("tempdir");
        let tiered = TieredStorage::new(
            LocalStorage::new(local_dir.path().to_path_buf()),
            remote,
            threshold,
            1 << 20,
        );

        // Two large durable objects + one large LOCAL-ONLY object.
        let durable_a = b"durable object AAAAAAAA".to_vec();
        let durable_b = b"durable object BBBBBBBB".to_vec();
        let local_only = b"local only CCCCCCCCCCCC".to_vec();
        let hash_a = ObjectHash::from_type_and_data(obj_type, &durable_a);
        let hash_b = ObjectHash::from_type_and_data(obj_type, &durable_b);
        let hash_c = ObjectHash::from_type_and_data(obj_type, &local_only);
        tiered
            .put(&hash_a, &durable_a, obj_type)
            .await
            .expect("put a");
        tiered
            .put(&hash_b, &durable_b, obj_type)
            .await
            .expect("put b");
        // c bypasses the remote (local materialization only).
        tiered
            .local
            .put(&hash_c, &local_only, obj_type)
            .await
            .expect("local put c");

        // Dry run with budget 0: would evict both durable objects, skips the
        // local-only one, deletes nothing.
        let report = tiered
            .evict_local(EvictRequest {
                budget_bytes: 0,
                min_age_secs: 0,
                dry_run: true,
            })
            .await
            .expect("dry run")
            .expect("tiered");
        assert_eq!(report.evicted, 2, "{report:?}");
        assert_eq!(report.skipped_absent, 1, "{report:?}");
        assert!(tiered.local.exist(&hash_a).await, "dry run deletes nothing");

        // Min-age floor skips everything (all freshly materialized).
        let report = tiered
            .evict_local(EvictRequest {
                budget_bytes: 0,
                min_age_secs: 3600,
                dry_run: false,
            })
            .await
            .expect("aged run")
            .expect("tiered");
        assert_eq!(report.evicted, 0, "{report:?}");
        assert_eq!(report.skipped_recent, 3, "{report:?}");

        // Real eviction to zero budget: both durable objects go, the
        // local-only object stays (its durability is not confirmed).
        let report = tiered
            .evict_local(EvictRequest {
                budget_bytes: 0,
                min_age_secs: 0,
                dry_run: false,
            })
            .await
            .expect("evict")
            .expect("tiered");
        assert_eq!(report.evicted, 2, "{report:?}");
        assert!(!tiered.local.exist(&hash_a).await, "evicted from local");
        assert!(tiered.local.exist(&hash_c).await, "only copy never deleted");

        // Self-healing read: the evicted object comes back from the durable
        // tier transparently.
        let (got, _) = tiered.get(&hash_a).await.expect("self-heal fetch");
        assert_eq!(got, durable_a);
        assert!(tiered.local.exist(&hash_a).await, "re-cached after heal");
    }

    /// A wholly unreachable durable tier aborts the run with nothing deleted
    /// (probe error is never conflated with absence).
    #[tokio::test]
    #[serial]
    async fn evict_local_aborts_on_unreachable_tier() {
        use git_internal::hash::{HashKind, set_hash_kind_for_test};

        use crate::utils::storage::EvictRequest;

        #[derive(Debug)]
        struct FailingStore;
        impl std::fmt::Display for FailingStore {
            fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                write!(f, "FailingStore")
            }
        }
        #[async_trait::async_trait]
        impl object_store::ObjectStore for FailingStore {
            async fn put_opts(
                &self,
                _location: &object_store::path::Path,
                _payload: object_store::PutPayload,
                _opts: object_store::PutOptions,
            ) -> object_store::Result<object_store::PutResult> {
                Err(object_store::Error::Generic {
                    store: "failing",
                    source: "down".into(),
                })
            }
            async fn put_multipart_opts(
                &self,
                _location: &object_store::path::Path,
                _opts: object_store::PutMultipartOptions,
            ) -> object_store::Result<Box<dyn object_store::MultipartUpload>> {
                Err(object_store::Error::Generic {
                    store: "failing",
                    source: "down".into(),
                })
            }
            async fn get_opts(
                &self,
                _location: &object_store::path::Path,
                _options: object_store::GetOptions,
            ) -> object_store::Result<object_store::GetResult> {
                Err(object_store::Error::Generic {
                    store: "failing",
                    source: "down".into(),
                })
            }
            fn list(
                &self,
                _prefix: Option<&object_store::path::Path>,
            ) -> futures::stream::BoxStream<'static, object_store::Result<object_store::ObjectMeta>>
            {
                Box::pin(futures::stream::empty())
            }
            async fn list_with_delimiter(
                &self,
                _prefix: Option<&object_store::path::Path>,
            ) -> object_store::Result<object_store::ListResult> {
                Err(object_store::Error::Generic {
                    store: "failing",
                    source: "down".into(),
                })
            }
            async fn copy_opts(
                &self,
                _from: &object_store::path::Path,
                _to: &object_store::path::Path,
                _options: object_store::CopyOptions,
            ) -> object_store::Result<()> {
                Err(object_store::Error::Generic {
                    store: "failing",
                    source: "down".into(),
                })
            }
            fn delete_stream(
                &self,
                locations: futures::stream::BoxStream<
                    'static,
                    object_store::Result<object_store::path::Path>,
                >,
            ) -> futures::stream::BoxStream<'static, object_store::Result<object_store::path::Path>>
            {
                locations
            }
        }

        let _kind = set_hash_kind_for_test(HashKind::Sha1);
        let obj_type = ObjectType::Blob;
        let local_dir = tempdir().expect("tempdir");
        let tiered = TieredStorage::new(
            LocalStorage::new(local_dir.path().to_path_buf()),
            RemoteStorage::new(Arc::new(FailingStore)),
            8,
            1 << 20,
        );
        // Materialize large local objects directly.
        let mut hashes = Vec::new();
        for i in 0..3 {
            let data = format!("large local object number {i} XXXXXXXX").into_bytes();
            let hash = ObjectHash::from_type_and_data(obj_type, &data);
            tiered.local.put(&hash, &data, obj_type).await.expect("put");
            hashes.push(hash);
        }
        let error = tiered
            .evict_local(crate::utils::storage::EvictRequest {
                budget_bytes: 0,
                min_age_secs: 0,
                dry_run: false,
            })
            .await
            .expect_err("unreachable tier aborts");
        assert!(error.to_string().contains("unreachable"), "{error}");
        for hash in &hashes {
            assert!(tiered.local.exist(hash).await, "nothing deleted");
        }
        let _ = EvictRequest {
            budget_bytes: 0,
            min_age_secs: 0,
            dry_run: false,
        };
    }
}
