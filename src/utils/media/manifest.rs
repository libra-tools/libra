//! Media manifest model (lore.md §6.3) — the versioned description of a media
//! object's FastCDC chunking. Serialized as a content-addressed JSON file under
//! `.libra/media/manifests/<media_oid>.json` (no SQLite table, zero migration).
//!
//! FROZEN schema (§6.1): field names and semantics are fixed for cross-client
//! byte-identical determinism. The optional strong per-chunk `checksum` field
//! (spec §6.3 names it `crc32c`) is RESERVED but left UNSET in v1 — the crate
//! `crc32fast` computes IEEE 802.3 CRC-32, NOT Castagnoli CRC-32C, and baking a
//! mislabeled value into a frozen schema would be unfixable without a v2. The
//! authoritative per-chunk integrity in v1 is `chunk_hash` (SHA-256 of the raw
//! chunk); a true CRC-32C is a forward-compatible future addition.

use std::path::Path;

use serde::{Deserialize, Serialize};

use super::{
    chunker::{self, Chunk},
    is_sha256_hex,
};

/// Manifest schema version (bumped on an incompatible on-disk change).
pub const MANIFEST_VERSION: u32 = 1;
pub const MAX_MANIFEST_SIZE: usize = 10 * 1024 * 1024;
pub const MAX_CHUNKS: usize = 8192;

/// One chunk entry in the manifest.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ChunkEntry {
    pub offset: u64,
    pub length: u64,
    /// Lowercase-hex SHA-256 of the RAW (uncompressed) chunk bytes.
    pub chunk_hash: String,
    /// Stored (post-compression) length. Equal to `length` for `compression:none`.
    pub encoded_length: u64,
    /// Compression codec for the stored chunk. v1 is always `"none"`.
    pub compression: String,
    /// Reserved optional strong per-chunk checksum (spec `crc32c`). UNSET in v1
    /// (see module docs); serialized only when present.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub checksum: Option<String>,
}

/// Client provenance — NO user identity/hostname/email (§6.3 privacy, lore:333).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CreatedBy {
    pub client: String,
    pub version: String,
    pub capabilities: Vec<String>,
}

/// A media manifest (v1).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MediaManifest {
    pub version: u32,
    pub algorithm: String,
    pub hash_algorithm: String,
    /// SHA-256 of the FULL raw media content (always sha256, independent of the
    /// repository `core.objectformat`) — byte-identical to a standard LFS
    /// pointer's `oid sha256:…`.
    pub media_oid: String,
    pub media_size: u64,
    pub chunks: Vec<ChunkEntry>,
    pub created_by: CreatedBy,
    /// Optional pointer to a complete standard LFS media object for fallback.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub fallback_oid: Option<String>,
}

/// Errors from manifest construction/validation.
#[derive(Debug, thiserror::Error)]
pub enum ManifestError {
    #[error("failed to read media file '{path}': {source}")]
    Io {
        path: String,
        source: std::io::Error,
    },
    #[error("manifest is malformed: {0}")]
    Invalid(String),
    #[error("failed to (de)serialize manifest: {0}")]
    Serde(String),
}

impl MediaManifest {
    /// Build a manifest by FastCDC-chunking `path`. The `media_oid` is computed
    /// by [`crate::utils::lfs::calc_lfs_file_hash`] (ring SHA-256 over the whole
    /// file) so it matches the standard LFS pointer OID regardless of the repo
    /// hash kind. Returns the manifest plus the ordered chunks (with raw bytes
    /// resolvable from `path` by offset/length for storage).
    pub fn build_from_file(path: impl AsRef<Path>) -> Result<(Self, Vec<Chunk>), ManifestError> {
        let path = path.as_ref();
        let media_oid =
            crate::utils::lfs::calc_lfs_file_hash(path).map_err(|source| ManifestError::Io {
                path: path.display().to_string(),
                source,
            })?;
        let media_size = std::fs::metadata(path)
            .map_err(|source| ManifestError::Io {
                path: path.display().to_string(),
                source,
            })?
            .len();
        let file = std::fs::File::open(path).map_err(|source| ManifestError::Io {
            path: path.display().to_string(),
            source,
        })?;
        let chunks = chunker::chunk_reader(std::io::BufReader::new(file)).map_err(|source| {
            ManifestError::Io {
                path: path.display().to_string(),
                source,
            }
        })?;
        let entries = chunks
            .iter()
            .map(|c| ChunkEntry {
                offset: c.offset,
                length: c.length,
                chunk_hash: c.chunk_hash.clone(),
                encoded_length: c.length,
                compression: "none".to_string(),
                checksum: None,
            })
            .collect();
        let manifest = MediaManifest {
            version: MANIFEST_VERSION,
            algorithm: chunker::ALGORITHM.to_string(),
            hash_algorithm: "sha256".to_string(),
            media_oid,
            media_size,
            chunks: entries,
            created_by: CreatedBy {
                client: "libra".to_string(),
                version: env!("CARGO_PKG_VERSION").to_string(),
                capabilities: vec![chunker::ALGORITHM.to_string(), "sha256".to_string()],
            },
            fallback_oid: None,
        };
        Ok((manifest, chunks))
    }

    /// Parse + fully validate a manifest from JSON text.
    pub fn from_json(text: &str) -> Result<Self, ManifestError> {
        if text.len() > MAX_MANIFEST_SIZE {
            return Err(ManifestError::Invalid("manifest exceeds size limit".into()));
        }
        let manifest: MediaManifest =
            serde_json::from_str(text).map_err(|e| ManifestError::Serde(e.to_string()))?;
        manifest.validate()?;
        Ok(manifest)
    }

    /// Serialize to canonical JSON.
    pub fn to_json(&self) -> Result<String, ManifestError> {
        serde_json::to_string_pretty(self).map_err(|e| ManifestError::Serde(e.to_string()))
    }

    /// Protocol identity shared with Mega. Provenance and the fallback pointer
    /// do not change the identity of the frozen content-defined chunk layout.
    pub fn id(&self) -> Result<String, ManifestError> {
        self.validate()?;
        let bytes = serde_json::to_vec(&(
            self.version,
            &self.algorithm,
            &self.hash_algorithm,
            &self.media_oid,
            self.media_size,
            &self.chunks,
        ))
        .map_err(|error| ManifestError::Serde(error.to_string()))?;
        Ok(super::sha256_hex(&bytes))
    }

    /// Validate the frozen invariants: version/algorithm/hash, a 64-hex
    /// `media_oid`, first-chunk-offset-0, contiguity, and that the chunk lengths
    /// sum to `media_size`. Returns an actionable error on any violation.
    pub fn validate(&self) -> Result<(), ManifestError> {
        if self.chunks.len() > MAX_CHUNKS
            || self
                .fallback_oid
                .as_ref()
                .is_some_and(|oid| oid != &self.media_oid)
        {
            return Err(ManifestError::Invalid(
                "too many chunks or mismatched fallback_oid".into(),
            ));
        }
        if self.version != MANIFEST_VERSION {
            return Err(ManifestError::Invalid(format!(
                "unsupported manifest version {} (this binary supports {MANIFEST_VERSION})",
                self.version
            )));
        }
        if self.algorithm != chunker::ALGORITHM {
            return Err(ManifestError::Invalid(format!(
                "unsupported chunk algorithm '{}' (expected '{}')",
                self.algorithm,
                chunker::ALGORITHM
            )));
        }
        if self.hash_algorithm != "sha256" {
            return Err(ManifestError::Invalid(format!(
                "unsupported hash algorithm '{}' (media_oid must be sha256)",
                self.hash_algorithm
            )));
        }
        if !is_sha256_hex(&self.media_oid) {
            return Err(ManifestError::Invalid(
                "media_oid must be exactly 64 lowercase-hex characters".to_string(),
            ));
        }
        let mut expected_offset = 0u64;
        for (i, c) in self.chunks.iter().enumerate() {
            if c.length == 0 || c.length > chunker::MAX_SIZE as u64 || c.checksum.is_some() {
                return Err(ManifestError::Invalid(format!(
                    "chunk {i} has invalid length or unsupported checksum"
                )));
            }
            if c.offset != expected_offset {
                return Err(ManifestError::Invalid(format!(
                    "chunk {i} offset {} breaks contiguity (expected {expected_offset})",
                    c.offset
                )));
            }
            if !is_sha256_hex(&c.chunk_hash) {
                return Err(ManifestError::Invalid(format!(
                    "chunk {i} chunk_hash must be 64 lowercase-hex characters"
                )));
            }
            // Frozen v1 schema: the only compression codec is "none", and a
            // stored (encoded) length must equal the raw length.
            if c.compression != "none" {
                return Err(ManifestError::Invalid(format!(
                    "chunk {i} has unsupported compression '{}' (v1 supports only 'none')",
                    c.compression
                )));
            }
            if c.encoded_length != c.length {
                return Err(ManifestError::Invalid(format!(
                    "chunk {i} encoded_length {} must equal length {} for uncompressed v1 chunks",
                    c.encoded_length, c.length
                )));
            }
            expected_offset = expected_offset
                .checked_add(c.length)
                .ok_or_else(|| ManifestError::Invalid("chunk offset overflow".into()))?;
        }
        if expected_offset != self.media_size {
            return Err(ManifestError::Invalid(format!(
                "chunk lengths sum to {expected_offset} but media_size is {}",
                self.media_size
            )));
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample() -> MediaManifest {
        MediaManifest {
            version: 1,
            algorithm: "fastcdc-v1".to_string(),
            hash_algorithm: "sha256".to_string(),
            media_oid: "a".repeat(64),
            media_size: 10,
            chunks: vec![
                ChunkEntry {
                    offset: 0,
                    length: 6,
                    chunk_hash: "b".repeat(64),
                    encoded_length: 6,
                    compression: "none".to_string(),
                    checksum: None,
                },
                ChunkEntry {
                    offset: 6,
                    length: 4,
                    chunk_hash: "c".repeat(64),
                    encoded_length: 4,
                    compression: "none".to_string(),
                    checksum: None,
                },
            ],
            created_by: CreatedBy {
                client: "libra".to_string(),
                version: "0".to_string(),
                capabilities: vec!["fastcdc-v1".to_string(), "sha256".to_string()],
            },
            fallback_oid: None,
        }
    }

    #[test]
    fn round_trips_and_validates() {
        let m = sample();
        m.validate().unwrap();
        let json = m.to_json().unwrap();
        let back = MediaManifest::from_json(&json).unwrap();
        assert_eq!(m, back);
        // checksum:None is omitted from the wire form (frozen-schema hygiene).
        assert!(!json.contains("checksum"));
    }

    #[test]
    fn rejects_zero_oversize_chunks_and_mismatched_fallback() {
        for length in [0, u64::MAX, chunker::MAX_SIZE as u64 + 1] {
            let mut m = sample();
            m.chunks[0].length = length;
            m.chunks[0].encoded_length = length;
            assert!(m.validate().is_err());
        }
        let mut m = sample();
        m.fallback_oid = Some("f".repeat(64));
        assert!(m.validate().is_err());
    }

    #[test]
    fn rejects_bad_version_algo_oid_and_contiguity() {
        let mut m = sample();
        m.version = 2;
        assert!(m.validate().is_err());

        let mut m = sample();
        m.algorithm = "fastcdc-v2".to_string();
        assert!(m.validate().is_err());

        let mut m = sample();
        m.media_oid = "xyz".to_string();
        assert!(m.validate().is_err());

        let mut m = sample();
        m.chunks[1].offset = 99; // break contiguity
        assert!(m.validate().is_err());

        let mut m = sample();
        m.media_size = 999; // sum mismatch
        assert!(m.validate().is_err());
    }
}
