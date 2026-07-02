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
use std::{str::FromStr, sync::Arc};

use async_trait::async_trait;
use bytes::Bytes;
use futures::StreamExt;
use git_internal::{errors::GitError, hash::ObjectHash, internal::object::types::ObjectType};
use object_store::{ObjectStore, ObjectStoreExt, path::Path as ObjectPath};

use super::Storage;

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

    pub async fn put_metadata(&self, data: &[u8]) -> Result<(), GitError> {
        let path = match &self.key_prefix {
            Some(prefix) => ObjectPath::from(format!("{}/metadata.json", prefix)),
            None => ObjectPath::from("metadata.json"),
        };

        self.inner
            .put(&path, Bytes::copy_from_slice(data).into())
            .await
            .map_err(|e| GitError::IOError(std::io::Error::other(e)))?;

        Ok(())
    }

    pub async fn get_metadata(&self) -> Result<Vec<u8>, GitError> {
        let path = match &self.key_prefix {
            Some(prefix) => ObjectPath::from(format!("{}/metadata.json", prefix)),
            None => ObjectPath::from("metadata.json"),
        };

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
                    && let Ok(hash) = ObjectHash::from_str(&hash_str)
                {
                    results.push(hash);
                }
            }
        }
        results
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
