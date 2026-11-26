//! Storage trait and implementations for Git object storage.
//!
mod load_cost;
pub mod local;
pub mod remote;
pub mod tiered;

use std::{
    collections::{HashMap, HashSet, VecDeque},
    io,
};

use async_trait::async_trait;
use futures::{StreamExt, TryStreamExt, stream};
use git_internal::{errors::GitError, hash::ObjectHash, internal::object::types::ObjectType};

pub fn checked_probe_error(hash: &ObjectHash, error: GitError) -> GitError {
    contextual_storage_error("probe", hash, error)
}

pub fn checked_read_error(hash: &ObjectHash, error: GitError) -> GitError {
    contextual_storage_error("read", hash, error)
}

fn contextual_storage_error(operation: &str, hash: &ObjectHash, error: GitError) -> GitError {
    match error {
        GitError::IOError(error) => GitError::IOError(io::Error::new(
            error.kind(),
            format!("failed to {operation} object {hash}: {error}"),
        )),
        GitError::InvalidObjectInfo(detail) => {
            GitError::InvalidObjectInfo(format!("failed to {operation} object {hash}: {detail}"))
        }
        GitError::ObjectNotFound(detail) => {
            GitError::ObjectNotFound(format!("failed to {operation} object {hash}: {detail}"))
        }
        other => GitError::IOError(io::Error::other(format!(
            "failed to {operation} object {hash}: {other}"
        ))),
    }
}

const MAX_REMOTE_TYPE_PROBE_BYTES: u64 = 4 * 1024 * 1024;

/// Reserve each in-flight read's full bound before starting the next batch.
/// This keeps actual decoded bytes within the caller's response budget even
/// when all concurrent reads return at their allowed maximum.
pub fn bounded_read_batch_shape(
    remaining_bytes: u64,
    max_object_bytes: u64,
    max_in_flight: usize,
) -> (usize, u64) {
    let max_object_bytes = max_object_bytes.max(1);
    let per_object_limit = remaining_bytes.min(max_object_bytes);
    let slots = if remaining_bytes < max_object_bytes {
        1
    } else {
        (remaining_bytes / max_object_bytes).min(max_in_flight.max(1) as u64) as usize
    };
    (slots, per_object_limit)
}

/// Abstract storage backend interface for Git objects
#[async_trait]
pub trait Storage: Send + Sync {
    /// Retrieve an object by its hash
    /// Returns the raw decompressed data and the object type.
    /// If the object is not found, returns `GitError::ObjectNotFound`.
    async fn get(&self, hash: &ObjectHash) -> Result<(Vec<u8>, ObjectType), GitError>;

    /// Retrieve an object while enforcing a conservative maximum load cost
    /// before materializing it. Backends that cannot enforce the bound fail
    /// closed instead of downloading or decoding an unbounded payload.
    async fn get_with_limit(
        &self,
        _hash: &ObjectHash,
        limit: u64,
    ) -> Result<(Vec<u8>, ObjectType), GitError> {
        Err(GitError::InvalidObjectInfo(format!(
            "storage backend cannot enforce a {limit}-byte bounded object read"
        )))
    }

    /// Fetch-specific bounded typed read. Unlike preview's `get_with_limit`,
    /// tiered and remote backends may consult durable storage while enforcing
    /// compressed and decoded size limits before materializing the payload.
    async fn get_typed_bounded(
        &self,
        hash: &ObjectHash,
        max_payload_bytes: u64,
    ) -> Result<(Vec<u8>, ObjectType), GitError> {
        let (data, object_type) = self.get_with_limit(hash, max_payload_bytes).await?;
        if u64::try_from(data.len()).map_or(true, |len| len > max_payload_bytes) {
            return Err(GitError::InvalidObjectInfo(format!(
                "object {hash} exceeds {max_payload_bytes} bytes"
            )));
        }
        let computed = ObjectHash::from_type_and_data_for_kind(hash.kind(), object_type, &data)
            .map_err(|error| {
                GitError::InvalidObjectInfo(format!(
                    "failed to verify bounded object {hash}: {error}"
                ))
            })?;
        if computed != *hash {
            return Err(GitError::InvalidObjectInfo(format!(
                "object {hash} failed integrity check: {object_type} payload hashes to {computed}"
            )));
        }
        Ok((data, object_type))
    }

    /// Probe an object's type without permitting an unbounded body read.
    /// Local storage overrides this to inspect only loose/pack headers;
    /// remote storage falls back to a verified 4 MiB typed read because its
    /// object-store metadata does not authenticate the Git object type.
    async fn object_type_bounded_probe(&self, hash: &ObjectHash) -> Result<ObjectType, GitError> {
        const MAX_REMOTE_TYPE_PROBE_BYTES: u64 = 4 * 1024 * 1024;
        self.get_typed_bounded(hash, MAX_REMOTE_TYPE_PROBE_BYTES)
            .await
            .map(|(_, object_type)| object_type)
    }

    /// Probe many object types without unbounded reads. Missing IDs are absent
    /// from the returned map; all other failures retain their failing OID.
    /// Remote fallback limits both each object and aggregate downloaded body
    /// bytes; backends with local indexes override this to inspect headers.
    async fn object_types_bounded_probe(
        &self,
        hashes: &[ObjectHash],
    ) -> Result<HashMap<ObjectHash, ObjectType>, GitError> {
        const MAX_TOTAL_TYPE_PROBE_BYTES: u64 = 256 * 1024 * 1024;
        self.object_types_bounded_probe_with_budget(hashes, MAX_TOTAL_TYPE_PROBE_BYTES)
            .await
            .map(|(found, _)| found)
    }

    /// Return the types and actual decoded bytes fetched from a durable tier.
    /// `remaining_remote_bytes` is shared across peel rounds by the caller.
    /// Local header-only overrides report zero; this default covers remote
    /// stores and fails once the remaining response budget is exhausted.
    async fn object_types_bounded_probe_with_budget(
        &self,
        hashes: &[ObjectHash],
        remaining_remote_bytes: u64,
    ) -> Result<(HashMap<ObjectHash, ObjectType>, u64), GitError> {
        const MAX_TOTAL_TYPE_PROBE_BYTES: u64 = 256 * 1024 * 1024;
        let unique: HashSet<ObjectHash> = hashes.iter().copied().collect();
        if !unique.is_empty() && remaining_remote_bytes == 0 {
            return Err(GitError::InvalidObjectInfo(
                "remote object type probe byte budget is exhausted; fetch fewer refs".to_string(),
            ));
        }
        let budget = remaining_remote_bytes.min(MAX_TOTAL_TYPE_PROBE_BYTES);
        let max_in_flight = crate::utils::resource_limits::max_connections().min(16);
        let mut remaining: VecDeque<_> = unique.into_iter().collect();
        let mut found = HashMap::new();
        let mut charged = 0u64;
        while !remaining.is_empty() {
            let available = budget - charged;
            if available == 0 {
                return Err(GitError::InvalidObjectInfo(
                    "remote object type probe byte budget is exhausted; fetch fewer refs"
                        .to_string(),
                ));
            }
            let (slots, per_object_limit) =
                bounded_read_batch_shape(available, MAX_REMOTE_TYPE_PROBE_BYTES, max_in_flight);
            let batch: Vec<_> = (0..slots).filter_map(|_| remaining.pop_front()).collect();
            let probes: Vec<Option<(ObjectHash, ObjectType, usize)>> = stream::iter(batch)
                .map(|hash| async move {
                    match self.get_typed_bounded(&hash, per_object_limit).await {
                        Ok((data, object_type)) => Ok(Some((hash, object_type, data.len()))),
                        Err(GitError::ObjectNotFound(_)) => Ok(None),
                        Err(GitError::InvalidObjectInfo(detail))
                            if per_object_limit < MAX_REMOTE_TYPE_PROBE_BYTES =>
                        {
                            Err(GitError::InvalidObjectInfo(format!(
                                "failed to read object {hash} within the remaining {per_object_limit}-byte remote type-probe budget; fetch fewer refs or check the object's integrity: {detail}"
                            )))
                        }
                        Err(error) => Err(checked_read_error(&hash, error)),
                    }
                })
                .buffer_unordered(slots)
                .try_collect()
                .await?;
            for probe in probes.into_iter().flatten() {
                let (hash, object_type, bytes) = probe;
                let bytes = u64::try_from(bytes).map_err(|error| {
                    GitError::InvalidObjectInfo(format!(
                        "object {hash} has an unrepresentable type probe size: {error}"
                    ))
                })?;
                charged = charged.checked_add(bytes).ok_or_else(|| {
                    GitError::InvalidObjectInfo(
                        "bounded object type probe byte count overflowed".to_string(),
                    )
                })?;
                if charged > budget {
                    return Err(GitError::InvalidObjectInfo(format!(
                        "bounded remote type probes exceed the remaining {budget}-byte response budget while reading object {hash}; request fewer refs or fetch without the shallow update"
                    )));
                }
                found.insert(hash, object_type);
            }
        }
        Ok((found, charged))
    }

    /// Fetch-only bounded commit read. Unlike preview's `get_with_limit`,
    /// tiered storage may consult its remote tier while honoring read policy.
    async fn get_commit_bounded(
        &self,
        hash: &ObjectHash,
        max_payload_bytes: u64,
    ) -> Result<(Vec<u8>, ObjectType), GitError> {
        let (data, object_type) = self.get_typed_bounded(hash, max_payload_bytes).await?;
        if object_type != ObjectType::Commit {
            return Err(GitError::InvalidObjectInfo(format!(
                "object {hash} is {object_type}, expected a commit"
            )));
        }
        Ok((data, object_type))
    }

    /// Store an object
    /// Takes the object hash, raw decompressed data, and object type.
    /// Returns the storage path or identifier.
    /// This operation should be idempotent.
    async fn put(
        &self,
        hash: &ObjectHash,
        data: &[u8],
        obj_type: ObjectType,
    ) -> Result<String, GitError>;

    /// Check if an object exists
    /// Returns true if the object exists in storage.
    async fn exist(&self, hash: &ObjectHash) -> bool;

    /// Return a conservative upper bound for bytes required to load the object
    /// without materializing it. Packed deltas include their instruction and
    /// base-chain reconstruction cost. `None` means this backend cannot provide
    /// a bounded local answer.
    async fn object_size(&self, _hash: &ObjectHash) -> Result<Option<u64>, GitError> {
        Ok(None)
    }

    /// Batch form of [`Self::object_size`], preserving input order.
    async fn object_sizes(&self, hashes: &[ObjectHash]) -> Result<Vec<Option<u64>>, GitError> {
        let mut sizes = Vec::with_capacity(hashes.len());
        for hash in hashes {
            sizes.push(self.object_size(hash).await?);
        }
        Ok(sizes)
    }

    /// Batch bounded-load preflight that stops once the sum of discovered load
    /// costs exceeds `aggregate_limit`. Implementations should avoid probing
    /// later payloads after the limit is crossed.
    async fn object_sizes_with_total_limit(
        &self,
        hashes: &[ObjectHash],
        aggregate_limit: u64,
    ) -> Result<Vec<Option<u64>>, GitError> {
        let mut sizes = Vec::with_capacity(hashes.len());
        let mut total = 0u64;
        for hash in hashes {
            let size = self.object_size(hash).await?;
            if let Some(size) = size {
                total = total
                    .checked_add(crate::utils::preview_object::charged_bytes(size))
                    .ok_or_else(|| {
                        GitError::InvalidObjectInfo(
                            "preview aggregate cache load cost exceeds u64".to_string(),
                        )
                    })?;
                if total > aggregate_limit {
                    return Err(GitError::InvalidObjectInfo(format!(
                        "preview aggregate cache load cost exceeds {aggregate_limit} bytes"
                    )));
                }
            }
            sizes.push(size);
        }
        Ok(sizes)
    }

    /// Return the exact decoded byte length of the object's payload without
    /// materializing it in memory. Unlike [`Self::object_size`], packed deltas
    /// report their reconstructed payload length (`result_size`) rather than
    /// their conservative delta-chain memory budget (`peak_bytes`). `None` means
    /// this backend cannot provide a bounded local answer.
    async fn decoded_object_size(&self, _hash: &ObjectHash) -> Result<Option<u64>, GitError> {
        Ok(None)
    }

    /// Batch form of [`Self::decoded_object_size`], preserving input order.
    async fn decoded_object_sizes(
        &self,
        hashes: &[ObjectHash],
    ) -> Result<Vec<Option<u64>>, GitError> {
        let mut sizes = Vec::with_capacity(hashes.len());
        for hash in hashes {
            sizes.push(self.decoded_object_size(hash).await?);
        }
        Ok(sizes)
    }

    /// Search for objects by hash prefix
    /// Returns a list of object hashes that match the given prefix.
    /// Note: Performance may vary significantly between backends (fast locally, potentially slow remotely).
    async fn search(&self, prefix: &str) -> Vec<ObjectHash>;

    /// Prefix search that keeps directory and pack-index read failures.
    ///
    /// The default calls [`Self::search`] and therefore still hides errors
    /// that implementation already turned into an empty list. Local storage
    /// overrides this. A missing objects directory is an empty loose result,
    /// not a read failure.
    async fn search_checked(&self, prefix: &str) -> Result<Vec<ObjectHash>, GitError> {
        Ok(self.search(prefix).await)
    }

    /// Batch existence check — returns one `bool` per input hash, in the same
    /// order (`lore.md` §0.6). Used as a dedup pre-check (e.g. "which of these
    /// objects does the remote already have before I upload?").
    ///
    /// The default runs `exist` sequentially: a correctness fallback with no
    /// speedup. The value is in backend overrides that probe in parallel —
    /// [`remote::RemoteStorage`] fires bounded-concurrency HEAD requests and
    /// [`tiered::TieredStorage`] answers local hits without any round trip and
    /// batches only the remote misses.
    async fn exist_batch(&self, hashes: &[ObjectHash]) -> Vec<bool> {
        let mut results = Vec::with_capacity(hashes.len());
        for hash in hashes {
            results.push(self.exist(hash).await);
        }
        results
    }

    /// Attempt to repair a missing or corrupted local object by re-fetching it
    /// from a durable tier, verifying that the fetched bytes hash to `hash`, and
    /// writing the object into the local store (`libra fsck --heal`, lore.md §0.4).
    ///
    /// # Returns
    /// * `Ok(true)` — the object was fetched, verified, and healed.
    /// * `Ok(false)` — this backend has no durable tier to heal from, or the
    ///   object is absent from that tier (unrecoverable). Backends MUST NOT
    ///   fabricate objects; only a payload that verifies against `hash` may be
    ///   written.
    ///
    /// The default implementation cannot heal (backends without a paired durable
    /// tier — local-only, remote-only, publish — return `Ok(false)`). Only
    /// [`tiered::TieredStorage`] overrides this.
    async fn heal(&self, _hash: &ObjectHash) -> Result<bool, GitError> {
        Ok(false)
    }

    /// Error-aware existence probe (lore.md 2.9): distinguishes a confirmed
    /// ABSENCE (`Ok(false)`) from a probe FAILURE (`Err` — outage, bad
    /// credentials). The plain `exist` collapses both into `false`, which is
    /// fine for read fallbacks but must never gate a deletion.
    async fn exist_checked(&self, hash: &ObjectHash) -> Result<bool, GitError> {
        Ok(self.exist(hash).await)
    }

    /// Error-aware batch probe. The default performs at most 16 probes at once
    /// and honors the configured connection cap; local and tiered stores may
    /// override it to reuse backend state.
    async fn exist_checked_batch(
        &self,
        hashes: &[ObjectHash],
    ) -> Result<HashMap<ObjectHash, bool>, GitError> {
        let max_in_flight = crate::utils::resource_limits::max_connections().min(16);
        let unique: HashSet<ObjectHash> = hashes.iter().copied().collect();
        stream::iter(unique)
            .map(|hash| async move {
                let exists = self
                    .exist_checked(&hash)
                    .await
                    .map_err(|error| checked_probe_error(&hash, error))?;
                Ok((hash, exists))
            })
            .buffer_unordered(max_in_flight)
            .try_collect()
            .await
    }

    /// Evict verified-durable large objects from the LOCAL tier until under
    /// budget (lore.md 2.9). `Ok(None)` = not a tiered store (nothing
    /// evictable). Deletion is gated on a per-object error-aware durability
    /// probe run immediately before each unlink — an object is never deleted
    /// on a probe ERROR, and a wholly unreachable tier aborts the run.
    async fn evict_local(&self, _request: EvictRequest) -> Result<Option<EvictReport>, GitError> {
        Ok(None)
    }

    /// Physically delete an object's PAYLOAD (lore.md 2.5 obliteration). The
    /// default is a no-op success (a local-only loose store deletes the file
    /// itself in the obliteration driver). Tiered stores override this to purge
    /// the durable-tier blob AND the in-memory LRU entry. Idempotent: deleting
    /// an already-absent payload succeeds.
    async fn delete_payload(&self, _hash: &ObjectHash) -> Result<(), GitError> {
        Ok(())
    }
}

/// Parameters for [`Storage::evict_local`].
#[derive(Debug, Clone)]
pub struct EvictRequest {
    /// Target budget for the local large-object cache (uncompressed bytes —
    /// the same conservative accounting as the in-process LRU).
    pub budget_bytes: u64,
    /// Skip objects materialized within this many seconds (mtime floor).
    pub min_age_secs: u64,
    /// Report what WOULD be evicted (probes still run); delete nothing.
    pub dry_run: bool,
}

/// Outcome of [`Storage::evict_local`].
#[derive(Debug, Clone, Default, serde::Serialize)]
pub struct EvictReport {
    /// Loose objects scanned.
    pub scanned: usize,
    /// Objects at/over the large threshold (eviction candidates before
    /// age/budget filters).
    pub candidate_count: usize,
    /// Their summed uncompressed bytes.
    pub candidate_bytes: u64,
    /// Candidates whose durability probe confirmed presence.
    pub verified: usize,
    /// Objects actually evicted (0 under dry-run).
    pub evicted: usize,
    /// Uncompressed bytes reclaimed (would-be reclaimed under dry-run).
    pub reclaimed_bytes: u64,
    /// Skipped: the durable tier CONFIRMED the object absent (push/backup to
    /// make it durable).
    pub skipped_absent: usize,
    /// Skipped: the durability probe ERRORED (outage ≠ absence; never
    /// deleted on error).
    pub skipped_probe_error: usize,
    /// Skipped: younger than the min-age floor.
    pub skipped_recent: usize,
    /// Evicted (or would-be) objects, capped: (oid, uncompressed bytes).
    pub evicted_objects: Vec<(String, u64)>,
}

#[cfg(test)]
mod tests {
    use super::{MAX_REMOTE_TYPE_PROBE_BYTES, bounded_read_batch_shape};

    #[test]
    fn bounded_type_probe_reserves_all_in_flight_payload_bytes() {
        for remaining in [
            1,
            MAX_REMOTE_TYPE_PROBE_BYTES - 1,
            MAX_REMOTE_TYPE_PROBE_BYTES,
            MAX_REMOTE_TYPE_PROBE_BYTES + 1,
            16 * MAX_REMOTE_TYPE_PROBE_BYTES - 1,
            16 * MAX_REMOTE_TYPE_PROBE_BYTES,
            256 * 1024 * 1024,
        ] {
            let (slots, per_object_limit) =
                bounded_read_batch_shape(remaining, MAX_REMOTE_TYPE_PROBE_BYTES, 16);
            assert!((1..=16).contains(&slots));
            assert!(per_object_limit <= MAX_REMOTE_TYPE_PROBE_BYTES);
            assert!((slots as u64) * per_object_limit <= remaining);
        }
        assert_eq!(
            bounded_read_batch_shape(3, MAX_REMOTE_TYPE_PROBE_BYTES, 16),
            (1, 3)
        );
        assert_eq!(
            bounded_read_batch_shape(
                2 * MAX_REMOTE_TYPE_PROBE_BYTES,
                MAX_REMOTE_TYPE_PROBE_BYTES,
                16,
            ),
            (2, MAX_REMOTE_TYPE_PROBE_BYTES)
        );
    }
}
