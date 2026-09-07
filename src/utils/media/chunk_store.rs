//! Local content-addressed media chunk store (lore.md §6).
//!
//! Deliberately NOT a Git [`crate::utils::storage::Storage`] backend: chunks are
//! RAW bytes keyed by their SHA-256, with no Git `<type> <len>\0` framing and no
//! zlib — so a chunk can never be mistaken for (or become) a Git object. It
//! lives at `.libra/media/chunks/<ab>/<chunk_hash>`, a physical sibling of
//! `objects/` (see [`crate::utils::path::media_chunks`]) that is never walked as
//! a loose-object store, and it bypasses [`crate::utils::client_storage`]
//! entirely so no `object_index`/cloud-backup rows are enqueued for non-Git
//! content. It reuses the crash-safe `write_atomic` temp+rename discipline and
//! re-verifies the SHA-256 of raw bytes on read (never trusting on-disk data).

use std::{
    io::{Read, Write},
    path::PathBuf,
};

use super::{is_sha256_hex, manifest::MediaManifest, sha256_hex};
use crate::utils::atomic_write;

#[derive(Debug, thiserror::Error)]
pub enum MediaStoreError {
    #[error("invalid chunk hash '{0}' (must be 64 lowercase-hex characters)")]
    InvalidHash(String),
    #[error("chunk '{0}' is not present in the local media store")]
    Missing(String),
    #[error(
        "chunk '{expected}' failed integrity check on read (store is corrupt; computed '{actual}')"
    )]
    Corrupt { expected: String, actual: String },
    #[error("media store io error at '{path}': {source}")]
    Io {
        path: String,
        source: std::io::Error,
    },
    #[error(
        "reassembled content digest '{actual}' does not match the manifest media_oid '{expected}'"
    )]
    MediaOidMismatch { expected: String, actual: String },
}

/// A local media chunk store rooted at `.libra/media/chunks`.
pub struct MediaChunkStore {
    root: PathBuf,
}

impl MediaChunkStore {
    pub fn put_manifest(&self, manifest: &MediaManifest) -> Result<(), MediaStoreError> {
        let path = self
            .root
            .parent()
            .unwrap_or(std::path::Path::new("."))
            .join("manifests")
            .join(format!("{}.json", manifest.media_oid));
        let io_error = |source| MediaStoreError::Io {
            path: path.display().to_string(),
            source,
        };
        manifest
            .validate()
            .map_err(|e| io_error(std::io::Error::other(e.to_string())))?;
        let json = manifest
            .to_json()
            .map_err(|e| io_error(std::io::Error::other(e.to_string())))?;
        atomic_write::write_atomic(&path, json.as_bytes(), atomic_write::sync_data_enabled())
            .map_err(io_error)
    }
    /// Use an explicit private cache root (also useful for independent clients).
    pub fn at(root: PathBuf) -> Self {
        Self { root }
    }
    /// Open the store at the repo's media-chunks root (created lazily on write).
    pub fn open() -> Self {
        Self {
            root: crate::utils::path::media_chunks(),
        }
    }

    /// Sharded path `<root>/<ab>/<chunk_hash>` (mirrors loose-object sharding).
    fn chunk_path(&self, chunk_hash: &str) -> PathBuf {
        self.root.join(&chunk_hash[0..2]).join(&chunk_hash[2..])
    }

    /// Store raw chunk bytes, returning their `chunk_hash`. Idempotent — a chunk
    /// already present (by content address) and still intact is not rewritten;
    /// but a corrupt on-disk chunk (disk rot) is REPAIRED by rewriting the
    /// correct bytes rather than silently trusted. Writes RAW bytes (no Git
    /// header, no zlib) via the crash-safe temp+rename discipline.
    pub fn put_chunk(&self, bytes: &[u8]) -> Result<String, MediaStoreError> {
        if bytes.len() > super::chunker::MAX_SIZE {
            return Err(MediaStoreError::Io {
                path: self.root.display().to_string(),
                source: std::io::Error::other("chunk exceeds FastCDC size limit"),
            });
        }
        let chunk_hash = sha256_hex(bytes);
        let path = self.chunk_path(&chunk_hash);
        if path.exists() {
            // Content-addressed: if the stored bytes still hash to `chunk_hash`
            // it is already durably present; otherwise fall through to rewrite.
            if self.get_chunk(&chunk_hash).is_ok() {
                return Ok(chunk_hash);
            }
        }
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).map_err(|source| MediaStoreError::Io {
                path: parent.display().to_string(),
                source,
            })?;
        }
        atomic_write::write_atomic(&path, bytes, atomic_write::sync_data_enabled()).map_err(
            |source| MediaStoreError::Io {
                path: path.display().to_string(),
                source,
            },
        )?;
        Ok(chunk_hash)
    }

    /// Read a chunk, re-verifying its SHA-256 == `chunk_hash` (never trust the
    /// on-disk bytes). Errors distinctly on absent vs corrupt.
    pub fn get_chunk(&self, chunk_hash: &str) -> Result<Vec<u8>, MediaStoreError> {
        if !is_sha256_hex(chunk_hash) {
            return Err(MediaStoreError::InvalidHash(chunk_hash.to_string()));
        }
        let path = self.chunk_path(chunk_hash);
        if path
            .metadata()
            .is_ok_and(|m| m.len() > super::chunker::MAX_SIZE as u64)
        {
            return Err(MediaStoreError::Io {
                path: path.display().to_string(),
                source: std::io::Error::other("cached chunk exceeds FastCDC size limit"),
            });
        }
        let bytes = match std::fs::File::open(&path).and_then(|file| {
            let mut bytes = Vec::new();
            file.take(super::chunker::MAX_SIZE as u64 + 1)
                .read_to_end(&mut bytes)?;
            if bytes.len() > super::chunker::MAX_SIZE {
                return Err(std::io::Error::other(
                    "cached chunk exceeds FastCDC size limit",
                ));
            }
            Ok(bytes)
        }) {
            Ok(b) => b,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                return Err(MediaStoreError::Missing(chunk_hash.to_string()));
            }
            Err(source) => {
                return Err(MediaStoreError::Io {
                    path: path.display().to_string(),
                    source,
                });
            }
        };
        let actual = sha256_hex(&bytes);
        if actual != chunk_hash {
            return Err(MediaStoreError::Corrupt {
                expected: chunk_hash.to_string(),
                actual,
            });
        }
        Ok(bytes)
    }

    /// Whether a chunk is present locally (does not re-verify — cheap existence).
    pub fn has_chunk(&self, chunk_hash: &str) -> bool {
        is_sha256_hex(chunk_hash) && self.chunk_path(chunk_hash).exists()
    }
}

/// Reassemble the media object described by `manifest` from `store` into `dest`,
/// verifying the full `media_oid` BEFORE publishing (verify-then-rename, §6.6:491
/// — the file is never published if the end-to-end digest mismatches). Each
/// chunk is streamed and independently SHA-256-verified by `get_chunk`.
pub fn reassemble(
    manifest: &MediaManifest,
    store: &MediaChunkStore,
    dest: &std::path::Path,
) -> Result<(), MediaStoreError> {
    use ring::digest::{Context, SHA256};

    manifest.validate().map_err(|e| MediaStoreError::Io {
        path: dest.display().to_string(),
        source: std::io::Error::other(e.to_string()),
    })?;

    let io_error = |source| MediaStoreError::Io {
        path: dest.display().to_string(),
        source,
    };
    // A leaf such as `asset.bin` has an empty parent. Resolve it before passing
    // it to the atomic writer, which requires a real staging/target directory.
    let target = std::path::absolute(dest).map_err(io_error)?;
    let parent = target
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .unwrap_or(std::path::Path::new("."));
    let mut writer = match std::fs::metadata(&target) {
        Ok(metadata) => crate::utils::atomic_stream::StreamingAtomicFile::new_in_with_permissions(
            parent,
            atomic_write::sync_data_enabled(),
            metadata.permissions(),
        ),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            crate::utils::atomic_stream::StreamingAtomicFile::new_in(
                parent,
                atomic_write::sync_data_enabled(),
            )
        }
        Err(error) => return Err(io_error(error)),
    }
    .map_err(io_error)?;
    let mut digest = Context::new(&SHA256);
    for entry in &manifest.chunks {
        let bytes = store.get_chunk(&entry.chunk_hash)?;
        if bytes.len() as u64 != entry.length {
            return Err(io_error(std::io::Error::other(
                "chunk length does not match manifest",
            )));
        }
        digest.update(&bytes);
        writer.write_all(&bytes).map_err(io_error)?;
    }
    let actual = hex::encode(digest.finish().as_ref());
    if actual != manifest.media_oid {
        return Err(MediaStoreError::MediaOidMismatch {
            expected: manifest.media_oid.clone(),
            actual,
        });
    }
    writer.persist(&target).map_err(io_error)?;
    Ok(())
}

/// Helper to slice a source file into its chunk bytes (by offset/length) so a
/// `media chunk --store` pass can persist them. Reads the whole span; a chunk is
/// bounded by [`super::chunker::MAX_SIZE`].
pub fn read_span(
    file: &mut std::fs::File,
    offset: u64,
    length: u64,
) -> Result<Vec<u8>, MediaStoreError> {
    use std::io::{Seek, SeekFrom};
    if length > super::chunker::MAX_SIZE as u64 {
        return Err(MediaStoreError::Io {
            path: "<media file>".into(),
            source: std::io::Error::other("chunk exceeds FastCDC size limit"),
        });
    }
    file.seek(SeekFrom::Start(offset))
        .map_err(|source| MediaStoreError::Io {
            path: "<media file>".to_string(),
            source,
        })?;
    let mut buf = vec![0u8; length as usize];
    file.read_exact(&mut buf)
        .map_err(|source| MediaStoreError::Io {
            path: "<media file>".to_string(),
            source,
        })?;
    Ok(buf)
}
