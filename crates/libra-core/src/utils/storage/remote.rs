//! Remote object storage backend for Git objects
//!
//! This module provides an interface to interact with remote object storage services (like S3, R2).
//! It supports storing Git objects with a directory structure similar to the local object store,
//! but with optional prefixing for multi-tenant isolation.
//!
//! # Path Structure
//!
//! - Without prefix: `aa/bbcc...` (Standard Git object layout)
//! - With prefix: `prefix/objects/aa/bbcc...` (Isolated layout, e.g. `repo_id/objects/...`)
use std::{io::Read, sync::Arc};

use async_trait::async_trait;
use bytes::Bytes;
use futures::StreamExt;
use git_internal::{errors::GitError, hash::ObjectHash, internal::object::types::ObjectType};
use object_store::{ObjectStore, ObjectStoreExt, path::Path as ObjectPath};

use super::Storage;

/// Checks the bytes an upload sends before the upload takes effect.
pub trait UploadCheck: Send {
    /// See the next bytes about to be sent.
    fn update(&mut self, bytes: &[u8]);
    /// Whether everything sent is what the caller meant to send; an error
    /// stops the upload before the PUT or the completion.
    fn verify(&mut self) -> Result<(), String>;
}

/// Receives a cleanup failure from a background abort, which has no caller
/// left to return it to.
pub type AbandonedUploadReport = Arc<dyn Fn(String) + Send + Sync>;

/// A multipart upload that is aborted in the background, best effort, if it
/// is dropped before the owner completes or aborts it, e.g. when a sync is
/// cancelled.
struct PendingUpload {
    upload: Option<Box<dyn object_store::MultipartUpload>>,
    location: ObjectPath,
    report: AbandonedUploadReport,
}

impl Drop for PendingUpload {
    fn drop(&mut self) {
        if let Some(mut upload) = self.upload.take()
            && let Ok(runtime) = tokio::runtime::Handle::try_current()
        {
            let location = self.location.clone();
            let report = Arc::clone(&self.report);
            runtime.spawn(async move {
                if let Err(error) = upload.abort().await {
                    warn_abandoned_upload(&location, &error, &report);
                }
            });
        }
    }
}

/// Report a background abort that failed: no caller is left to return it
/// to, so it goes to the caller's report (always shown) and the log.
fn warn_abandoned_upload(
    location: &ObjectPath,
    error: &object_store::Error,
    report: &AbandonedUploadReport,
) {
    let message = format!(
        "aborting the multipart metadata upload of {location} for a cancelled cloud sync failed: {error}; its parts stay in the bucket until the bucket's lifecycle rule for incomplete multipart uploads removes them"
    );
    tracing::warn!("{message}");
    report(message);
}

/// Send `first` and then the rest of `source` (starting with the byte
/// `carry` already read) as `chunk_size` parts, one at a time, and complete
/// the upload.
async fn upload_remaining_parts(
    upload: &mut dyn object_store::MultipartUpload,
    first: Vec<u8>,
    carry: u8,
    source: &mut impl Read,
    chunk_size: usize,
    check: &mut dyn UploadCheck,
) -> Result<(), GitError> {
    let to_git = |e: object_store::Error| GitError::IOError(std::io::Error::other(e));
    check.update(&first);
    upload
        .put_part(Bytes::from(first).into())
        .await
        .map_err(to_git)?;
    let mut carry = Some(carry);
    loop {
        let mut part = vec![0u8; chunk_size];
        let mut filled = 0;
        if let Some(byte) = carry.take() {
            part[0] = byte;
            filled = 1;
        }
        filled += read_full(source, &mut part[filled..]).map_err(GitError::IOError)?;
        if filled == 0 {
            break;
        }
        part.truncate(filled);
        check.update(&part);
        upload
            .put_part(Bytes::from(part).into())
            .await
            .map_err(to_git)?;
        if filled < chunk_size {
            break;
        }
    }
    check
        .verify()
        .map_err(|message| GitError::IOError(std::io::Error::other(message)))?;
    upload.complete().await.map(|_| ()).map_err(to_git)
}

/// Fill `buffer` from `source` until it is full or the source ends; returns
/// how many bytes were read.
fn read_full(source: &mut impl Read, buffer: &mut [u8]) -> std::io::Result<usize> {
    let mut filled = 0;
    while filled < buffer.len() {
        match source.read(&mut buffer[filled..]) {
            Ok(0) => break,
            Ok(read) => filled += read,
            Err(error) if error.kind() == std::io::ErrorKind::Interrupted => {}
            Err(error) => return Err(error),
        }
    }
    Ok(filled)
}

/// Remote object storage backend
/// Adapts object_store crate to Libra's StorageTrait
pub struct RemoteStorage {
    inner: Arc<dyn ObjectStore>,
    key_prefix: Option<String>,
}

impl RemoteStorage {
    /// Create a new RemoteStorage instance from an existing ObjectStore
    pub fn new(inner: Arc<dyn ObjectStore>) -> Self {
        Self {
            inner,
            key_prefix: None,
        }
    }

    pub fn new_with_prefix(inner: Arc<dyn ObjectStore>, key_prefix: String) -> Self {
        let key_prefix = key_prefix.trim_matches('/').to_string();
        let key_prefix = if key_prefix.is_empty() {
            None
        } else {
            Some(key_prefix)
        };
        Self { inner, key_prefix }
    }

    pub fn object_store(&self) -> Arc<dyn ObjectStore> {
        Arc::clone(&self.inner)
    }

    /// Convert ObjectHash to storage path (aa/bbcc...)
    fn hash_to_path(&self, hash: &ObjectHash) -> ObjectPath {
        let h = hash.to_string();
        match &self.key_prefix {
            Some(prefix) => {
                ObjectPath::from(format!("{}/objects/{}/{}", prefix, &h[0..2], &h[2..]))
            }
            None => ObjectPath::from(format!("{}/{}", &h[0..2], &h[2..])),
        }
    }

    fn metadata_path(&self) -> ObjectPath {
        match &self.key_prefix {
            Some(prefix) => ObjectPath::from(format!("{}/metadata.json", prefix)),
            None => ObjectPath::from("metadata.json"),
        }
    }

    pub async fn put_metadata(&self, data: &[u8]) -> Result<(), GitError> {
        let path = self.metadata_path();

        self.inner
            .put(&path, Bytes::copy_from_slice(data).into())
            .await
            .map_err(|e| GitError::IOError(std::io::Error::other(e)))?;

        Ok(())
    }

    /// Upload the metadata from a local file without reading it into memory
    /// whole: one PUT when it is at most `chunk_size` bytes, otherwise a
    /// multipart upload of `chunk_size` parts sent one at a time, so memory
    /// stays one chunk whatever the file size. Any failure after the upload
    /// starts — a part, the completion or reading the file — aborts it, and a
    /// failed abort is reported with the original error. A cancelled upload,
    /// including one cancelled while the multipart upload is being started,
    /// is aborted in the background on a best-effort basis: a runtime that
    /// shuts down first, a killed process or a lost start response can still
    /// leave parts behind, which only the bucket's lifecycle rule for
    /// incomplete multipart uploads removes. Multipart stores need
    /// `chunk_size` of at least 5 MiB.
    pub async fn put_metadata_from_file(
        &self,
        file: &std::path::Path,
        chunk_size: usize,
        check: &mut dyn UploadCheck,
        report: AbandonedUploadReport,
    ) -> Result<(), GitError> {
        let path = self.metadata_path();
        let chunk_size = chunk_size.max(1);
        let mut source = std::fs::File::open(file).map_err(GitError::IOError)?;
        let mut first = vec![0u8; chunk_size];
        let filled = read_full(&mut source, &mut first).map_err(GitError::IOError)?;
        let mut next = [0u8; 1];
        let more = if filled == chunk_size {
            read_full(&mut source, &mut next).map_err(GitError::IOError)?
        } else {
            0
        };
        if more == 0 {
            first.truncate(filled);
            check.update(&first);
            check
                .verify()
                .map_err(|message| GitError::IOError(std::io::Error::other(message)))?;
            self.inner
                .put(&path, Bytes::from(first).into())
                .await
                .map_err(|e| GitError::IOError(std::io::Error::other(e)))?;
            return Ok(());
        }

        // Start the upload on its own task: if this future is cancelled while
        // the start is in flight, the task still receives the handle and
        // aborts it instead of handing it to a caller that is gone.
        // The task arms the guard before handing it over, so the upload is
        // aborted in the background whenever the guard is dropped unfinished:
        // by a caller that is gone, by a handoff that is never received, or
        // by a sync cancelled later.
        let (started, start) = tokio::sync::oneshot::channel();
        let store = Arc::clone(&self.inner);
        let location = path.clone();
        tokio::spawn(async move {
            let started_upload = store
                .put_multipart(&location)
                .await
                .map(|upload| PendingUpload {
                    upload: Some(upload),
                    location,
                    report,
                });
            // An undelivered guard is dropped here and aborts the upload.
            let _ = started.send(started_upload);
        });
        let mut pending = start
            .await
            .map_err(|_| {
                GitError::IOError(std::io::Error::other(
                    "the multipart metadata upload could not be started",
                ))
            })?
            .map_err(|e| GitError::IOError(std::io::Error::other(e)))?;
        let outcome = match pending.upload.as_deref_mut() {
            Some(upload) => {
                upload_remaining_parts(upload, first, next[0], &mut source, chunk_size, check).await
            }
            None => Ok(()),
        };
        let Err(error) = outcome else {
            // Completed: nothing left to abort.
            pending.upload = None;
            return Ok(());
        };
        // The upload stays in the guard while it is aborted, so a sync
        // cancelled during the abort still aborts it in the background.
        let aborted = match pending.upload.as_deref_mut() {
            Some(upload) => upload.abort().await,
            None => Ok(()),
        };
        pending.upload = None;
        match aborted {
            Ok(()) => Err(error),
            Err(abort_error) => Err(GitError::IOError(std::io::Error::other(format!(
                "{error}; aborting the unfinished multipart upload of {path} also failed: {abort_error}. Its parts stay in the bucket until removed: give the bucket a lifecycle rule that aborts incomplete multipart uploads, or abort it in the storage provider's console"
            )))),
        }
    }

    pub async fn get_metadata(&self) -> Result<Vec<u8>, GitError> {
        let path = self.metadata_path();

        let result = self.inner.get(&path).await.map_err(|e| match e {
            object_store::Error::NotFound { .. } => {
                GitError::ObjectNotFound(format!("Metadata not found: {}", e))
            }
            _ => GitError::IOError(std::io::Error::other(e)),
        })?;

        let bytes = result
            .bytes()
            .await
            .map_err(|e| GitError::IOError(std::io::Error::other(e)))?;

        Ok(bytes.to_vec())
    }
}

#[async_trait]
impl Storage for RemoteStorage {
    /// Get object from remote storage
    /// Downloads, decompresses, and strips header
    async fn get(&self, hash: &ObjectHash) -> Result<(Vec<u8>, ObjectType), GitError> {
        let path = self.hash_to_path(hash);
        let result = self.inner.get(&path).await.map_err(|e| match e {
            object_store::Error::NotFound { .. } => {
                GitError::ObjectNotFound(format!("Remote object not found: {}", e))
            }
            _ => GitError::IOError(std::io::Error::other(e)),
        })?;

        let bytes = result
            .bytes()
            .await
            .map_err(|e| GitError::IOError(std::io::Error::other(e)))?;

        // Decompress
        let mut decoder = flate2::read::ZlibDecoder::new(&bytes[..]);
        let mut decompressed = Vec::new();
        std::io::Read::read_to_end(&mut decoder, &mut decompressed)?;

        // Strip header
        let end_of_header = decompressed
            .iter()
            .position(|&b| b == 0)
            .ok_or_else(|| GitError::InvalidObjectInfo("No header terminator".into()))?;

        // Parse type
        let header_str = std::str::from_utf8(&decompressed[..end_of_header])
            .map_err(|_| GitError::InvalidObjectInfo("Invalid UTF-8 in header".into()))?;
        let obj_type_str = header_str.split(' ').next().unwrap_or("");
        let obj_type = ObjectType::from_string(obj_type_str)?;

        Ok((decompressed[end_of_header + 1..].to_vec(), obj_type))
    }

    async fn get_typed_bounded(
        &self,
        hash: &ObjectHash,
        max_payload_bytes: u64,
    ) -> Result<(Vec<u8>, ObjectType), GitError> {
        const HEADER_BUDGET: u64 = 64;
        const COMPRESSED_OVERHEAD_BUDGET: u64 = 64 * 1024;

        let max_compressed_bytes = max_payload_bytes
            .checked_add(COMPRESSED_OVERHEAD_BUDGET)
            .ok_or_else(|| GitError::InvalidObjectInfo("object read limit overflows u64".into()))?;
        let max_decoded_bytes = max_payload_bytes
            .checked_add(HEADER_BUDGET)
            .and_then(|bound| bound.checked_add(1))
            .ok_or_else(|| GitError::InvalidObjectInfo("object read limit overflows u64".into()))?;
        let path = self.hash_to_path(hash);
        let result = self.inner.get(&path).await.map_err(|error| match error {
            object_store::Error::NotFound { .. } => {
                GitError::ObjectNotFound(format!("Remote object not found: {error}"))
            }
            _ => GitError::IOError(std::io::Error::other(error)),
        })?;

        // Check the reported size before consuming the body. Also bound each
        // streamed chunk: a backend must not make a stale Content-Length turn
        // into an unbounded allocation in `GetResult::bytes()`.
        if result.meta.size > max_compressed_bytes || result.range != (0..result.meta.size) {
            return Err(GitError::InvalidObjectInfo(format!(
                "compressed object {hash} exceeds the bounded read or has invalid metadata"
            )));
        }
        let reported_size = result.meta.size;
        let mut compressed = Vec::new();
        let mut stream = result.into_stream();
        while let Some(chunk) = stream.next().await {
            let chunk = chunk.map_err(|error| GitError::IOError(std::io::Error::other(error)))?;
            let next_len = u64::try_from(compressed.len())
                .ok()
                .and_then(|length| length.checked_add(chunk.len() as u64))
                .ok_or_else(|| {
                    GitError::InvalidObjectInfo("compressed object size overflows u64".into())
                })?;
            if next_len > max_compressed_bytes {
                return Err(GitError::InvalidObjectInfo(format!(
                    "compressed object {hash} exceeds {max_compressed_bytes} bytes"
                )));
            }
            compressed.extend_from_slice(&chunk);
        }
        if u64::try_from(compressed.len()).ok() != Some(reported_size) {
            return Err(GitError::InvalidObjectInfo(format!(
                "compressed object {hash} length differs from storage metadata"
            )));
        }

        let decoder = flate2::read::ZlibDecoder::new(compressed.as_slice());
        let mut decoded = Vec::new();
        decoder
            .take(max_decoded_bytes)
            .read_to_end(&mut decoded)
            .map_err(|error| {
                GitError::InvalidObjectInfo(format!(
                    "remote object {hash} has invalid zlib data: {error}; verify the remote object and retry the fetch"
                ))
            })?;
        if u64::try_from(decoded.len()).map_or(true, |length| length == max_decoded_bytes) {
            return Err(GitError::InvalidObjectInfo(format!(
                "object {hash} exceeds {max_payload_bytes} bytes"
            )));
        }
        let header_end = decoded.iter().position(|byte| *byte == 0).ok_or_else(|| {
            GitError::InvalidObjectInfo(format!("remote object {hash} has no header terminator"))
        })?;
        if header_end > HEADER_BUDGET as usize {
            return Err(GitError::InvalidObjectInfo(format!(
                "remote object {hash} header is too long"
            )));
        }
        let header = std::str::from_utf8(&decoded[..header_end]).map_err(|_| {
            GitError::InvalidObjectInfo(format!("remote object {hash} header is not UTF-8"))
        })?;
        let (kind, declared_length) = header.split_once(' ').ok_or_else(|| {
            GitError::InvalidObjectInfo(format!("remote object {hash} header has no size"))
        })?;
        let object_type = ObjectType::from_string(kind).map_err(|error| {
            GitError::InvalidObjectInfo(format!(
                "remote object {hash} has invalid type header: {error}"
            ))
        })?;
        let declared_length = declared_length.parse::<u64>().map_err(|_| {
            GitError::InvalidObjectInfo(format!("remote object {hash} header has invalid size"))
        })?;
        let payload = &decoded[header_end + 1..];
        if u64::try_from(payload.len()).map_or(true, |length| length > max_payload_bytes) {
            return Err(GitError::InvalidObjectInfo(format!(
                "object {hash} exceeds {max_payload_bytes} bytes"
            )));
        }
        if u64::try_from(payload.len()).ok() != Some(declared_length) {
            return Err(GitError::InvalidObjectInfo(format!(
                "object {hash} length differs from its header"
            )));
        }
        let computed = ObjectHash::from_type_and_data_for_kind(hash.kind(), object_type, payload)
            .map_err(|error| {
            GitError::InvalidObjectInfo(format!("failed to verify remote object {hash}: {error}"))
        })?;
        if computed != *hash {
            return Err(GitError::InvalidObjectInfo(format!(
                "remote object {hash} has mismatched content ID {computed}"
            )));
        }
        Ok((payload.to_vec(), object_type))
    }

    async fn get_commit_bounded(
        &self,
        hash: &ObjectHash,
        max_payload_bytes: u64,
    ) -> Result<(Vec<u8>, ObjectType), GitError> {
        let (payload, object_type) = self.get_typed_bounded(hash, max_payload_bytes).await?;
        if object_type != ObjectType::Commit {
            return Err(GitError::InvalidObjectInfo(format!(
                "object {hash} is {object_type}, expected a commit"
            )));
        }
        Ok((payload, object_type))
    }

    /// Put object to remote storage
    /// Constructs header, compresses, and uploads
    async fn put(
        &self,
        hash: &ObjectHash,
        data: &[u8],
        obj_type: ObjectType,
    ) -> Result<String, GitError> {
        let path = self.hash_to_path(hash);

        // Construct header + content
        let header = format!("{} {}\0", obj_type, data.len());
        let mut full_content = Vec::with_capacity(header.len() + data.len());
        full_content.extend_from_slice(header.as_bytes());
        full_content.extend_from_slice(data);

        // Compress
        let mut encoder =
            flate2::write::ZlibEncoder::new(Vec::new(), flate2::Compression::default());
        std::io::Write::write_all(&mut encoder, &full_content)?;
        let compressed = encoder.finish()?;

        // Upload
        self.inner
            .put(&path, Bytes::from(compressed).into())
            .await
            .map_err(|e| GitError::IOError(std::io::Error::other(e)))?;

        Ok(path.to_string())
    }

    async fn exist(&self, hash: &ObjectHash) -> bool {
        let path = self.hash_to_path(hash);
        self.inner.head(&path).await.is_ok()
    }

    async fn delete_payload(&self, hash: &ObjectHash) -> Result<(), GitError> {
        let path = self.hash_to_path(hash);
        match self.inner.delete(&path).await {
            Ok(()) => Ok(()),
            // Idempotent: an already-absent blob is a success.
            Err(object_store::Error::NotFound { .. }) => Ok(()),
            Err(error) => Err(GitError::IOError(std::io::Error::other(format!(
                "failed to delete durable-tier payload for {hash}: {error}"
            )))),
        }
    }

    async fn exist_checked(&self, hash: &ObjectHash) -> Result<bool, GitError> {
        let path = self.hash_to_path(hash);
        match self.inner.head(&path).await {
            Ok(_) => Ok(true),
            // A confirmed miss — the only case that may gate an eviction.
            Err(object_store::Error::NotFound { .. }) => Ok(false),
            // Everything else (outage, credentials, throttling) is an ERROR,
            // never conflated with absence.
            Err(error) => Err(GitError::IOError(std::io::Error::other(format!(
                "durable-tier probe failed for {hash}: {error}"
            )))),
        }
    }

    async fn search(&self, prefix: &str) -> Vec<ObjectHash> {
        let list_prefix = if prefix.len() >= 2 {
            // Optimization: Git objects are stored in xx/yyyy...
            // If we have at least 2 chars, we can narrow down to the directory "xx".
            // We don't use the full prefix (e.g. "aabb") for the list_prefix because
            // object_store paths are segment-based, and "aa/bb" is not considered a parent of "aa/bbcc...".
            // So we list "aa" and filter client-side.
            match &self.key_prefix {
                Some(p) => ObjectPath::from(format!("{}/objects/{}", p, &prefix[0..2])),
                None => ObjectPath::from(&prefix[0..2]),
            }
        } else {
            // If < 2 chars, we must list the root. This is expensive but necessary for correctness.
            match &self.key_prefix {
                Some(p) => ObjectPath::from(format!("{}/objects", p)),
                None => ObjectPath::from(""),
            }
        };

        let mut results = Vec::new();

        // Use list instead of list_with_delimiter to get all objects under the prefix
        let mut stream = self.inner.list(Some(&list_prefix));

        while let Some(item) = stream.next().await {
            if let Ok(meta) = item {
                // path is like "aa/bbcc..."
                let mut path_str = meta.location.to_string();
                if let Some(p) = &self.key_prefix {
                    let expected = format!("{}/objects/", p);
                    if !path_str.starts_with(&expected) {
                        continue;
                    }
                    path_str = path_str[expected.len()..].to_string();
                }
                // Remove '/' to get hash "aabbcc..."
                let hash_str = path_str.replace('/', "");

                if hash_str.starts_with(prefix)
                    && let Ok(hash) = crate::internal::object_format::parse_repo_oid(&hash_str)
                {
                    results.push(hash);
                }
            }
        }
        results
    }

    /// Prefix listing that keeps durable-tier list failures.
    ///
    /// `object_store::Error::NotFound` is a missing prefix or object and is
    /// skipped. Any other list error fails the search instead of returning
    /// the objects already seen. A non-ASCII prefix that cannot be split on
    /// a two-byte boundary lists from the store root and then filters.
    async fn search_checked(&self, prefix: &str) -> Result<Vec<ObjectHash>, GitError> {
        let list_prefix = if let Some(shard) = prefix.get(..2) {
            match &self.key_prefix {
                Some(key_prefix) => ObjectPath::from(format!("{key_prefix}/objects/{shard}")),
                None => ObjectPath::from(shard),
            }
        } else {
            match &self.key_prefix {
                Some(key_prefix) => ObjectPath::from(format!("{key_prefix}/objects")),
                None => ObjectPath::from(""),
            }
        };
        let mut results = Vec::new();
        let mut stream = self.inner.list(Some(&list_prefix));
        while let Some(item) = stream.next().await {
            let meta = match item {
                Ok(meta) => meta,
                Err(object_store::Error::NotFound { .. }) => continue,
                Err(error) => {
                    return Err(GitError::IOError(std::io::Error::other(format!(
                        "failed to list durable-tier objects for prefix '{prefix}': {error}"
                    ))));
                }
            };
            let mut path_str = meta.location.to_string();
            if let Some(key_prefix) = &self.key_prefix {
                let expected = format!("{key_prefix}/objects/");
                if !path_str.starts_with(&expected) {
                    continue;
                }
                path_str = path_str[expected.len()..].to_string();
            }
            let hash_str = path_str.replace('/', "");
            if hash_str.starts_with(prefix)
                && let Ok(hash) = crate::internal::object_format::parse_repo_oid(&hash_str)
            {
                results.push(hash);
            }
        }
        Ok(results)
    }

    /// Bounded-concurrency batch existence probe (`lore.md` §0.6): fire up to
    /// `max_connections()` HEAD requests at once instead of `N` sequential round
    /// trips, preserving input order. Each probe inherits object_store's
    /// 429/`SlowDown`/5xx backoff (lore.md §0.2). The concurrency cap is the
    /// global `--max-connections` / `LIBRA_MAX_CONNECTIONS` limit (lore.md §0.9),
    /// so a large batch on a big repo or CI run cannot exhaust connections.
    async fn exist_batch(&self, hashes: &[ObjectHash]) -> Vec<bool> {
        let max_concurrent = crate::utils::resource_limits::max_connections();
        futures::stream::iter(hashes.iter().copied())
            .map(|hash| async move { self.exist(&hash).await })
            .buffered(max_concurrent)
            .collect()
            .await
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use bytes::Bytes;
    use git_internal::{
        hash::{HashKind, ObjectHash, set_hash_kind_for_test},
        internal::object::types::ObjectType,
    };
    use object_store::{ObjectStoreExt, memory::InMemory};

    use super::{RemoteStorage, Storage};

    fn test_hash() -> ObjectHash {
        crate::internal::object_format::parse_repo_oid("1111111111111111111111111111111111111111")
            .expect("test hash is valid")
    }

    #[tokio::test]
    async fn bounded_commit_read_succeeds() {
        let _hash_kind = set_hash_kind_for_test(HashKind::Sha1);
        let remote = RemoteStorage::new(Arc::new(InMemory::new()));
        let payload = b"tree 2222222222222222222222222222222222222222\n\nmessage\n";
        let hash =
            ObjectHash::from_type_and_data_for_kind(HashKind::Sha1, ObjectType::Commit, payload)
                .expect("commit hash is supported");
        remote
            .put(&hash, payload, ObjectType::Commit)
            .await
            .expect("store test commit");

        let (actual, kind) = remote
            .get_commit_bounded(&hash, payload.len() as u64)
            .await
            .expect("bounded commit read");
        assert_eq!(actual, payload);
        assert_eq!(kind, ObjectType::Commit);
    }

    #[tokio::test]
    async fn bounded_commit_read_rejects_large_compressed_object_before_materializing() {
        let _hash_kind = set_hash_kind_for_test(HashKind::Sha1);
        let store = Arc::new(InMemory::new());
        let remote = RemoteStorage::new(store.clone());
        let hash = test_hash();
        store
            .put(
                &remote.hash_to_path(&hash),
                Bytes::from(vec![0; 66_000]).into(),
            )
            .await
            .expect("store oversized compressed bytes");

        let error = remote
            .get_commit_bounded(&hash, 100)
            .await
            .expect_err("large compressed object must be rejected");
        assert!(error.to_string().contains("compressed object"), "{error}");
    }

    #[tokio::test]
    async fn bounded_commit_read_rejects_compression_bomb() {
        let _hash_kind = set_hash_kind_for_test(HashKind::Sha1);
        let remote = RemoteStorage::new(Arc::new(InMemory::new()));
        let hash = test_hash();
        remote
            .put(&hash, &vec![b'a'; 200_000], ObjectType::Commit)
            .await
            .expect("store compressed test commit");

        let error = remote
            .get_commit_bounded(&hash, 1_024)
            .await
            .expect_err("large decoded object must be rejected");
        assert!(error.to_string().contains("exceeds 1024 bytes"), "{error}");
    }

    #[tokio::test]
    async fn bounded_commit_read_rejects_wrong_object_type() {
        let _hash_kind = set_hash_kind_for_test(HashKind::Sha1);
        let remote = RemoteStorage::new(Arc::new(InMemory::new()));
        let hash = ObjectHash::from_type_and_data_for_kind(
            HashKind::Sha1,
            ObjectType::Blob,
            b"not a commit",
        )
        .expect("blob hash is supported");
        remote
            .put(&hash, b"not a commit", ObjectType::Blob)
            .await
            .expect("store test blob");

        let error = remote
            .get_commit_bounded(&hash, 100)
            .await
            .expect_err("blob is not a commit");
        assert!(error.to_string().contains("expected a commit"), "{error}");
    }

    #[tokio::test]
    async fn bounded_commit_read_rejects_content_id_mismatch() {
        let _hash_kind = set_hash_kind_for_test(HashKind::Sha1);
        let remote = RemoteStorage::new(Arc::new(InMemory::new()));
        let requested_hash = test_hash();
        let payload = b"tree 2222222222222222222222222222222222222222\n\nwrong object\n";
        remote
            .put(&requested_hash, payload, ObjectType::Commit)
            .await
            .expect("store commit under incorrect ID");

        let error = remote
            .get_commit_bounded(&requested_hash, 100)
            .await
            .expect_err("mismatched commit ID must be rejected");
        assert!(
            error.to_string().contains(&requested_hash.to_string()),
            "{error}"
        );
        assert!(
            error.to_string().contains("mismatched content ID"),
            "{error}"
        );
    }

    #[tokio::test]
    async fn bounded_commit_read_reports_corrupt_zlib_as_invalid_object() {
        let _hash_kind = set_hash_kind_for_test(HashKind::Sha1);
        let store = Arc::new(InMemory::new());
        let remote = RemoteStorage::new(store.clone());
        let hash = test_hash();
        store
            .put(
                &remote.hash_to_path(&hash),
                Bytes::from_static(b"not a zlib stream").into(),
            )
            .await
            .expect("store corrupted compressed bytes");

        let error = remote
            .get_commit_bounded(&hash, 100)
            .await
            .expect_err("corrupt zlib must fail as invalid object");
        assert!(matches!(
            &error,
            git_internal::errors::GitError::InvalidObjectInfo(_)
        ));
        assert!(error.to_string().contains(&hash.to_string()), "{error}");
    }

    #[tokio::test]
    async fn bounded_commit_read_reports_unknown_type_as_invalid_object() {
        let _hash_kind = set_hash_kind_for_test(HashKind::Sha1);
        let store = Arc::new(InMemory::new());
        let remote = RemoteStorage::new(store.clone());
        let hash = test_hash();
        let mut encoder =
            flate2::write::ZlibEncoder::new(Vec::new(), flate2::Compression::default());
        std::io::Write::write_all(&mut encoder, b"unknown 3\0abc")
            .expect("encode malformed object");
        let compressed = encoder.finish().expect("finish malformed object");
        store
            .put(&remote.hash_to_path(&hash), Bytes::from(compressed).into())
            .await
            .expect("store malformed type header");

        let error = remote
            .get_commit_bounded(&hash, 100)
            .await
            .expect_err("unknown type must fail as invalid object");
        assert!(matches!(
            &error,
            git_internal::errors::GitError::InvalidObjectInfo(_)
        ));
        assert!(error.to_string().contains(&hash.to_string()), "{error}");
    }

    #[tokio::test]
    async fn bounded_typed_read_accepts_annotated_tag_with_explicit_hash_kind() {
        let _hash_kind = set_hash_kind_for_test(HashKind::Sha1);
        let remote = RemoteStorage::new(Arc::new(InMemory::new()));
        let payload = b"object 2222222222222222222222222222222222222222\ntype commit\ntag v1\ntagger Test <test@example.com> 0 +0000\n\nrelease\n";
        let hash =
            ObjectHash::from_type_and_data_for_kind(HashKind::Sha256, ObjectType::Tag, payload)
                .expect("tag hash is supported");
        remote
            .put(&hash, payload, ObjectType::Tag)
            .await
            .expect("store test tag");

        let (actual, kind) = remote
            .get_typed_bounded(&hash, payload.len() as u64)
            .await
            .expect("bounded tag read");
        assert_eq!(actual, payload);
        assert_eq!(kind, ObjectType::Tag);
    }

    #[tokio::test]
    async fn bounded_typed_read_rejects_tag_with_wrong_oid() {
        let _hash_kind = set_hash_kind_for_test(HashKind::Sha1);
        let remote = RemoteStorage::new(Arc::new(InMemory::new()));
        let hash = test_hash();
        remote
            .put(&hash, b"tag payload", ObjectType::Tag)
            .await
            .expect("store tag under wrong ID");

        let error = remote
            .get_typed_bounded(&hash, 100)
            .await
            .expect_err("tag with wrong ID must be rejected");
        assert!(error.to_string().contains(&hash.to_string()), "{error}");
        assert!(
            error.to_string().contains("mismatched content ID"),
            "{error}"
        );
    }

    #[tokio::test]
    async fn bounded_typed_read_rejects_oversized_tag() {
        let _hash_kind = set_hash_kind_for_test(HashKind::Sha1);
        let remote = RemoteStorage::new(Arc::new(InMemory::new()));
        let hash = test_hash();
        remote
            .put(&hash, &vec![b'a'; 200_000], ObjectType::Tag)
            .await
            .expect("store large compressed tag");

        let error = remote
            .get_typed_bounded(&hash, 1_024)
            .await
            .expect_err("oversized tag must be rejected");
        assert!(error.to_string().contains("exceeds 1024 bytes"), "{error}");
    }

    #[derive(Clone, Copy, Debug)]
    enum ListFault {
        Empty,
        NotFound,
        Down,
    }

    #[derive(Clone, Copy, Debug)]
    struct ListStore {
        fault: ListFault,
    }

    impl std::fmt::Display for ListStore {
        fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            f.write_str("ListStore")
        }
    }

    #[async_trait::async_trait]
    impl object_store::ObjectStore for ListStore {
        async fn put_opts(
            &self,
            _location: &object_store::path::Path,
            _payload: object_store::PutPayload,
            _opts: object_store::PutOptions,
        ) -> object_store::Result<object_store::PutResult> {
            Err(object_store::Error::Generic {
                store: "list-store",
                source: "unused".into(),
            })
        }

        async fn put_multipart_opts(
            &self,
            _location: &object_store::path::Path,
            _opts: object_store::PutMultipartOptions,
        ) -> object_store::Result<Box<dyn object_store::MultipartUpload>> {
            Err(object_store::Error::Generic {
                store: "list-store",
                source: "unused".into(),
            })
        }

        async fn get_opts(
            &self,
            _location: &object_store::path::Path,
            _options: object_store::GetOptions,
        ) -> object_store::Result<object_store::GetResult> {
            Err(object_store::Error::Generic {
                store: "list-store",
                source: "unused".into(),
            })
        }

        fn list(
            &self,
            _prefix: Option<&object_store::path::Path>,
        ) -> futures::stream::BoxStream<'static, object_store::Result<object_store::ObjectMeta>>
        {
            match self.fault {
                ListFault::Empty => Box::pin(futures::stream::empty()),
                ListFault::NotFound => Box::pin(futures::stream::once(async {
                    Err(object_store::Error::NotFound {
                        path: "ab".into(),
                        source: "gone".into(),
                    })
                })),
                ListFault::Down => Box::pin(futures::stream::once(async {
                    Err(object_store::Error::Generic {
                        store: "list-store",
                        source: "list down".into(),
                    })
                })),
            }
        }

        async fn list_with_delimiter(
            &self,
            _prefix: Option<&object_store::path::Path>,
        ) -> object_store::Result<object_store::ListResult> {
            Err(object_store::Error::Generic {
                store: "list-store",
                source: "unused".into(),
            })
        }

        async fn copy_opts(
            &self,
            _from: &object_store::path::Path,
            _to: &object_store::path::Path,
            _options: object_store::CopyOptions,
        ) -> object_store::Result<()> {
            Err(object_store::Error::Generic {
                store: "list-store",
                source: "unused".into(),
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

    /// Counts how the metadata reaches the store and can fail one multipart
    /// step; everything else goes to an in-memory store.
    #[derive(Debug, Default)]
    struct UploadCounts {
        puts: std::sync::atomic::AtomicUsize,
        multiparts: std::sync::atomic::AtomicUsize,
        parts: std::sync::atomic::AtomicUsize,
        aborts: std::sync::atomic::AtomicUsize,
    }

    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    enum UploadFault {
        None,
        Part(usize),
        Complete,
        /// The first part fails and so does the abort.
        PartAndAbort,
        /// The second part never finishes.
        HangPart(usize),
        /// The first part fails and the first abort never finishes.
        PartAndHangAbort,
        /// Starting the multipart upload takes 200 ms.
        SlowStart,
        /// Starting takes 200 ms and the abort fails.
        SlowStartAndAbort,
        /// The second part never finishes and the abort fails.
        HangPartAndAbort,
    }

    #[derive(Debug)]
    struct CountingStore {
        inner: Arc<InMemory>,
        counts: Arc<UploadCounts>,
        fault: UploadFault,
    }

    impl std::fmt::Display for CountingStore {
        fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            f.write_str("CountingStore")
        }
    }

    #[derive(Debug)]
    struct CountingUpload {
        inner: Box<dyn object_store::MultipartUpload>,
        counts: Arc<UploadCounts>,
        fault: UploadFault,
    }

    fn injected(step: &str) -> object_store::Error {
        object_store::Error::Generic {
            store: "counting-store",
            source: format!("injected {step} failure").into(),
        }
    }

    #[async_trait::async_trait]
    impl object_store::MultipartUpload for CountingUpload {
        fn put_part(&mut self, data: object_store::PutPayload) -> object_store::UploadPart {
            use std::sync::atomic::Ordering;
            let part = self.counts.parts.fetch_add(1, Ordering::SeqCst) + 1;
            match self.fault {
                UploadFault::Part(failing) if failing == part => {
                    return Box::pin(async { Err(injected("part")) });
                }
                UploadFault::PartAndAbort | UploadFault::PartAndHangAbort if part == 1 => {
                    return Box::pin(async { Err(injected("part")) });
                }
                UploadFault::HangPart(hanging) if hanging == part => {
                    return Box::pin(futures::future::pending());
                }
                UploadFault::HangPartAndAbort if part == 2 => {
                    return Box::pin(futures::future::pending());
                }
                _ => {}
            }
            self.inner.put_part(data)
        }

        async fn complete(&mut self) -> object_store::Result<object_store::PutResult> {
            if self.fault == UploadFault::Complete {
                return Err(injected("complete"));
            }
            self.inner.complete().await
        }

        async fn abort(&mut self) -> object_store::Result<()> {
            let earlier = self
                .counts
                .aborts
                .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            match self.fault {
                UploadFault::PartAndAbort
                | UploadFault::SlowStartAndAbort
                | UploadFault::HangPartAndAbort => return Err(injected("abort")),
                UploadFault::PartAndHangAbort if earlier == 0 => {
                    futures::future::pending::<()>().await;
                }
                _ => {}
            }
            self.inner.abort().await
        }
    }

    #[async_trait::async_trait]
    impl object_store::ObjectStore for CountingStore {
        async fn put_opts(
            &self,
            location: &object_store::path::Path,
            payload: object_store::PutPayload,
            opts: object_store::PutOptions,
        ) -> object_store::Result<object_store::PutResult> {
            self.counts
                .puts
                .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            self.inner.put_opts(location, payload, opts).await
        }

        async fn put_multipart_opts(
            &self,
            location: &object_store::path::Path,
            opts: object_store::PutMultipartOptions,
        ) -> object_store::Result<Box<dyn object_store::MultipartUpload>> {
            self.counts
                .multiparts
                .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            if matches!(
                self.fault,
                UploadFault::SlowStart | UploadFault::SlowStartAndAbort
            ) {
                tokio::time::sleep(std::time::Duration::from_millis(200)).await;
            }
            let inner = self.inner.put_multipart_opts(location, opts).await?;
            Ok(Box::new(CountingUpload {
                inner,
                counts: self.counts.clone(),
                fault: self.fault,
            }))
        }

        async fn get_opts(
            &self,
            location: &object_store::path::Path,
            options: object_store::GetOptions,
        ) -> object_store::Result<object_store::GetResult> {
            self.inner.get_opts(location, options).await
        }

        fn list(
            &self,
            prefix: Option<&object_store::path::Path>,
        ) -> futures::stream::BoxStream<'static, object_store::Result<object_store::ObjectMeta>>
        {
            self.inner.list(prefix)
        }

        async fn list_with_delimiter(
            &self,
            prefix: Option<&object_store::path::Path>,
        ) -> object_store::Result<object_store::ListResult> {
            self.inner.list_with_delimiter(prefix).await
        }

        async fn copy_opts(
            &self,
            from: &object_store::path::Path,
            to: &object_store::path::Path,
            options: object_store::CopyOptions,
        ) -> object_store::Result<()> {
            self.inner.copy_opts(from, to, options).await
        }

        fn delete_stream(
            &self,
            locations: futures::stream::BoxStream<
                'static,
                object_store::Result<object_store::path::Path>,
            >,
        ) -> futures::stream::BoxStream<'static, object_store::Result<object_store::path::Path>>
        {
            self.inner.delete_stream(locations)
        }
    }

    /// Accepts whatever is sent.
    struct AcceptAll;

    impl super::UploadCheck for AcceptAll {
        fn update(&mut self, _bytes: &[u8]) {}

        fn verify(&mut self) -> Result<(), String> {
            Ok(())
        }
    }

    /// Accepts only exactly `expected`.
    struct ExpectBytes {
        expected: Vec<u8>,
        sent: Vec<u8>,
    }

    impl super::UploadCheck for ExpectBytes {
        fn update(&mut self, bytes: &[u8]) {
            self.sent.extend_from_slice(bytes);
        }

        fn verify(&mut self) -> Result<(), String> {
            if self.sent == self.expected {
                Ok(())
            } else {
                Err("sent bytes mismatch the expected bytes".to_string())
            }
        }
    }

    fn no_report() -> super::AbandonedUploadReport {
        Arc::new(|_message: String| {})
    }

    fn counting_storage(fault: UploadFault) -> (RemoteStorage, Arc<UploadCounts>) {
        let counts = Arc::new(UploadCounts::default());
        let store = CountingStore {
            inner: Arc::new(InMemory::new()),
            counts: counts.clone(),
            fault,
        };
        (RemoteStorage::new(Arc::new(store)), counts)
    }

    fn metadata_file(len: usize) -> (tempfile::TempDir, std::path::PathBuf, Vec<u8>) {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("refs.json");
        let bytes: Vec<u8> = (0..len).map(|n| b'a' + (n % 26) as u8).collect();
        std::fs::write(&path, &bytes).expect("write metadata file");
        (dir, path, bytes)
    }

    #[tokio::test]
    async fn capture_snapshot_paging_metadata_upload_uses_one_put_up_to_one_chunk() {
        use std::sync::atomic::Ordering;

        // (file length, PUTs, multipart uploads, parts) with 8-byte chunks:
        // up to and including one chunk is one PUT; beyond it, parts of one
        // chunk each, the last one shorter.
        for (len, puts, multiparts, parts) in [
            (0, 1, 0, 0),
            (7, 1, 0, 0),
            (8, 1, 0, 0),
            (9, 0, 1, 2),
            (16, 0, 1, 2),
            (26, 0, 1, 4),
        ] {
            let (_dir, path, bytes) = metadata_file(len);
            let (storage, counts) = counting_storage(UploadFault::None);
            storage
                .put_metadata_from_file(&path, 8, &mut AcceptAll, no_report())
                .await
                .expect("upload metadata");
            assert_eq!(
                (
                    counts.puts.load(Ordering::SeqCst),
                    counts.multiparts.load(Ordering::SeqCst),
                    counts.parts.load(Ordering::SeqCst),
                    counts.aborts.load(Ordering::SeqCst),
                ),
                (puts, multiparts, parts, 0),
                "{len} bytes"
            );
            assert_eq!(
                storage.get_metadata().await.expect("read back"),
                bytes,
                "{len} bytes"
            );
        }
    }

    #[tokio::test]
    async fn capture_snapshot_paging_metadata_upload_aborts_a_failed_multipart() {
        use std::sync::atomic::Ordering;

        // A failure in the first part, the last part or the completion aborts
        // the upload, returns the error, and leaves no metadata object.
        for fault in [
            UploadFault::Part(1),
            UploadFault::Part(3),
            UploadFault::Complete,
        ] {
            let (_dir, path, _) = metadata_file(20);
            let (storage, counts) = counting_storage(fault);
            let error = storage
                .put_metadata_from_file(&path, 8, &mut AcceptAll, no_report())
                .await
                .expect_err("the injected failure is returned");
            assert!(error.to_string().contains("injected"), "{fault:?}: {error}");
            assert_eq!(counts.aborts.load(Ordering::SeqCst), 1, "{fault:?}");
            assert!(storage.get_metadata().await.is_err(), "{fault:?}");
        }
    }

    #[tokio::test]
    async fn capture_snapshot_paging_metadata_upload_reports_a_failed_abort() {
        use std::sync::atomic::Ordering;

        let (_dir, path, _) = metadata_file(20);
        let (storage, counts) = counting_storage(UploadFault::PartAndAbort);
        let error = storage
            .put_metadata_from_file(&path, 8, &mut AcceptAll, no_report())
            .await
            .expect_err("the part failure is returned");
        let detail = error.to_string();
        assert!(detail.contains("injected part failure"), "{detail}");
        assert!(
            detail.contains("aborting the unfinished multipart upload"),
            "{detail}"
        );
        assert!(detail.contains("injected abort failure"), "{detail}");
        assert!(detail.contains("lifecycle rule"), "{detail}");
        assert_eq!(counts.aborts.load(Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn capture_snapshot_paging_metadata_upload_aborts_when_cancelled() {
        use std::sync::atomic::Ordering;

        // The second part never finishes; dropping the upload future (as a
        // cancelled sync does) still aborts the multipart upload.
        let (_dir, path, _) = metadata_file(20);
        let (storage, counts) = counting_storage(UploadFault::HangPart(2));
        let cancelled = tokio::time::timeout(
            std::time::Duration::from_millis(100),
            storage.put_metadata_from_file(&path, 8, &mut AcceptAll, no_report()),
        )
        .await;
        assert!(cancelled.is_err(), "the upload was still running");
        for _ in 0..100 {
            if counts.aborts.load(Ordering::SeqCst) == 1 {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
        assert_eq!(counts.aborts.load(Ordering::SeqCst), 1);
        assert!(storage.get_metadata().await.is_err());
    }

    #[tokio::test]
    async fn capture_snapshot_paging_metadata_upload_aborts_when_cancelled_during_abort() {
        use std::sync::atomic::Ordering;

        // The first part fails and the abort that follows never finishes;
        // cancelling the upload then still aborts it in the background.
        let (_dir, path, _) = metadata_file(20);
        let (storage, counts) = counting_storage(UploadFault::PartAndHangAbort);
        let cancelled = tokio::time::timeout(
            std::time::Duration::from_millis(100),
            storage.put_metadata_from_file(&path, 8, &mut AcceptAll, no_report()),
        )
        .await;
        assert!(cancelled.is_err(), "the abort was still running");
        for _ in 0..100 {
            if counts.aborts.load(Ordering::SeqCst) == 2 {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
        assert_eq!(
            counts.aborts.load(Ordering::SeqCst),
            2,
            "the cancelled abort is retried in the background"
        );
        assert!(storage.get_metadata().await.is_err());
    }

    #[tokio::test]
    async fn capture_snapshot_paging_metadata_upload_aborts_when_cancelled_while_starting() {
        use std::sync::atomic::Ordering;

        // The upload is cancelled while the multipart start is still in
        // flight; the start task aborts the handle it then receives.
        let (_dir, path, _) = metadata_file(20);
        let (storage, counts) = counting_storage(UploadFault::SlowStart);
        let cancelled = tokio::time::timeout(
            std::time::Duration::from_millis(50),
            storage.put_metadata_from_file(&path, 8, &mut AcceptAll, no_report()),
        )
        .await;
        assert!(cancelled.is_err(), "the start was still running");
        for _ in 0..100 {
            if counts.aborts.load(Ordering::SeqCst) == 1 {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
        assert_eq!(counts.multiparts.load(Ordering::SeqCst), 1);
        assert_eq!(counts.parts.load(Ordering::SeqCst), 0);
        assert_eq!(counts.aborts.load(Ordering::SeqCst), 1);
        assert!(storage.get_metadata().await.is_err());
    }

    #[tokio::test]
    async fn capture_snapshot_paging_metadata_upload_reports_a_failed_background_abort() {
        use std::sync::atomic::Ordering;

        // A cancelled upload whose background abort fails — cancelled while
        // starting, or while a part is in flight — hands the failure to the
        // caller's report, with no tracing subscriber installed (the CLI's
        // default), naming the object, the abort error and the lifecycle rule.
        for fault in [
            UploadFault::SlowStartAndAbort,
            UploadFault::HangPartAndAbort,
        ] {
            let reported = Arc::new(std::sync::Mutex::new(Vec::<String>::new()));
            let sink = reported.clone();
            let report: super::AbandonedUploadReport = Arc::new(move |message: String| {
                sink.lock().expect("report buffer").push(message);
            });
            let (_dir, path, _) = metadata_file(20);
            let (storage, counts) = counting_storage(fault);
            let cancelled = tokio::time::timeout(
                std::time::Duration::from_millis(50),
                storage.put_metadata_from_file(&path, 8, &mut AcceptAll, report),
            )
            .await;
            assert!(
                cancelled.is_err(),
                "{fault:?}: the upload was still running"
            );
            for _ in 0..100 {
                if !reported.lock().expect("report buffer").is_empty() {
                    break;
                }
                tokio::time::sleep(std::time::Duration::from_millis(10)).await;
            }
            assert_eq!(counts.aborts.load(Ordering::SeqCst), 1, "{fault:?}");
            let reported = reported.lock().expect("report buffer").clone();
            assert_eq!(reported.len(), 1, "{fault:?}: {reported:?}");
            assert!(
                reported[0].contains("metadata.json"),
                "{fault:?}: {reported:?}"
            );
            assert!(
                reported[0].contains("injected abort failure"),
                "{fault:?}: {reported:?}"
            );
            assert!(
                reported[0].contains("lifecycle rule"),
                "{fault:?}: {reported:?}"
            );
        }
    }

    #[tokio::test]
    async fn capture_snapshot_paging_metadata_upload_refuses_unexpected_bytes() {
        use std::sync::atomic::Ordering;

        // When the bytes read for the upload are not what the caller expects,
        // nothing takes effect: no single PUT, or an aborted multipart upload.
        for (len, aborts) in [(5usize, 0usize), (20, 1)] {
            let (_dir, path, bytes) = metadata_file(len);
            let (storage, counts) = counting_storage(UploadFault::None);
            let mut check = ExpectBytes {
                expected: bytes.iter().rev().copied().collect(),
                sent: Vec::new(),
            };
            let error = storage
                .put_metadata_from_file(&path, 8, &mut check, no_report())
                .await
                .expect_err("unexpected bytes are refused");
            assert!(error.to_string().contains("mismatch"), "{len}: {error}");
            assert_eq!(counts.puts.load(Ordering::SeqCst), 0, "{len}");
            assert_eq!(counts.aborts.load(Ordering::SeqCst), aborts, "{len}");
            assert!(storage.get_metadata().await.is_err(), "{len}");
        }
    }

    #[tokio::test]
    async fn capture_snapshot_paging_metadata_upload_aborts_an_undelivered_handoff() {
        use std::sync::atomic::Ordering;

        // The start handoff delivered the armed guard, but the caller was
        // cancelled before receiving it: dropping the undelivered value still
        // aborts the upload.
        let counts = Arc::new(UploadCounts::default());
        let store = CountingStore {
            inner: Arc::new(InMemory::new()),
            counts: counts.clone(),
            fault: UploadFault::None,
        };
        let location = object_store::path::Path::from("metadata.json");
        let upload = store
            .put_multipart(&location)
            .await
            .expect("start an upload");
        let (started, start) = tokio::sync::oneshot::channel::<super::PendingUpload>();
        let delivered = started.send(super::PendingUpload {
            upload: Some(upload),
            location,
            report: no_report(),
        });
        assert!(delivered.is_ok(), "the receiver was still waiting");
        drop(start);
        for _ in 0..100 {
            if counts.aborts.load(Ordering::SeqCst) == 1 {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
        assert_eq!(counts.aborts.load(Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn remote_search_checked_distinguishes_list_failure_from_absence() {
        let absent = RemoteStorage::new(Arc::new(ListStore {
            fault: ListFault::Empty,
        }));
        let matches = absent
            .search_checked("ab")
            .await
            .expect("an empty listing is absence");
        assert!(matches.is_empty());
        let non_ascii = absent
            .search_checked("你")
            .await
            .expect("a non-ascii prefix lists from the root");
        assert!(non_ascii.is_empty());

        let missing = RemoteStorage::new(Arc::new(ListStore {
            fault: ListFault::NotFound,
        }));
        let matches = missing
            .search_checked("ab")
            .await
            .expect("a NotFound listing is absence");
        assert!(matches.is_empty());

        let down = RemoteStorage::new(Arc::new(ListStore {
            fault: ListFault::Down,
        }));
        let error = down
            .search_checked("ab")
            .await
            .expect_err("a list failure must stay an error");
        let rendered = error.to_string();
        assert!(
            rendered.contains("failed to list durable-tier objects"),
            "{rendered}"
        );
        assert!(rendered.contains("list down"), "{rendered}");
        assert!(
            down.search("ab").await.is_empty(),
            "the Vec search still skips a list failure"
        );
    }
}
