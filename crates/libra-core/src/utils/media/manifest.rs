//! Media manifest model (lore.md §6.3) — the versioned description of a media
//! object's FastCDC chunking.
//!
//! Local layouts are paged: `.libra/media/manifests/<media_oid>/summary.json`
//! plus `pages/<page_no>.json`. Page boundaries are a transport/storage split
//! and do not participate in the canonical id (P-01 / P-01a). There is no
//! product-wide chunk-count cap.
//!
//! FROZEN schema (§6.1): field names and semantics are fixed for cross-client
//! byte-identical determinism. The optional strong per-chunk `checksum` field
//! (spec §6.3 names it `crc32c`) is RESERVED but left UNSET in v1 — the crate
//! `crc32fast` computes IEEE 802.3 CRC-32, NOT Castagnoli CRC-32C, and baking a
//! mislabeled value into a frozen schema would be unfixable without a v2. The
//! authoritative per-chunk integrity in v1 is `chunk_hash` (SHA-256 of the raw
//! chunk); a true CRC-32C is a forward-compatible future addition.

use std::{collections::HashSet, path::Path};

use ring::digest::{Context, SHA256};
use serde::{Deserialize, Serialize};

use super::{
    chunker::{self, Chunk},
    is_sha256_hex,
};

/// Manifest schema version (bumped on an incompatible on-disk change).
pub const MANIFEST_VERSION: u32 = 1;
/// Media metadata envelope: one page, summary, or status body (C-02).
pub const MAX_ENVELOPE_SIZE: usize = 1_048_576;
/// Wire alias of [`MAX_ENVELOPE_SIZE`]. A full manifest is not one request body.
pub const MAX_MANIFEST_SIZE: usize = MAX_ENVELOPE_SIZE;
/// Longest chunk list in one page (P-01a).
pub const MAX_PAGE_ENTRIES: usize = 4096;
/// Compact canonical entries-array budget. The remaining 64 KiB of the 1 MiB
/// envelope is reserved for fixed page wrap fields (P-01a).
pub const MAX_PAGE_ENTRIES_BYTES: usize = 960 * 1024;
pub const MAX_CREATED_BY_BYTES: usize = 4096;
pub const HASH_ALGORITHM: &str = "sha256";
pub const COMPRESSION_NONE: &str = "none";
pub const MANIFEST_PAGING: &str = "v1";

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

/// Paging summary (P-01 / P-02). Not a full layout.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ManifestSummary {
    pub version: u32,
    pub algorithm: String,
    pub hash_algorithm: String,
    pub oid: String,
    pub size: u64,
    pub chunk_count: u64,
    pub page_count: u32,
    pub manifest_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub created_by: Option<CreatedBy>,
}

/// One immutable page of chunk entries (P-01a / P-02).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ManifestPage {
    pub page_no: u32,
    pub entries: Vec<ChunkEntry>,
}

/// Running P-01a budget for one page. Byte length matches
/// [`canonical_entries_bytes`] and only grows as entries are appended.
#[derive(Debug, Clone, Copy)]
pub struct PageBudget {
    pub entries: usize,
    pub bytes: usize,
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
                version: env!("LIBRA_PKG_VERSION").to_string(),
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
        let mut hasher = CanonicalIdHasher::new(
            self.version,
            &self.algorithm,
            &self.hash_algorithm,
            &self.media_oid,
            self.media_size,
        )?;
        for entry in &self.chunks {
            hasher.push(entry)?;
        }
        Ok(hasher.finish())
    }

    /// Validate the frozen invariants: version/algorithm/hash, a 64-hex
    /// `media_oid`, first-chunk-offset-0, contiguity, and that the chunk lengths
    /// sum to `media_size`. Returns an actionable error on any violation.
    pub fn validate(&self) -> Result<(), ManifestError> {
        if self
            .fallback_oid
            .as_ref()
            .is_some_and(|oid| oid != &self.media_oid)
        {
            return Err(ManifestError::Invalid("mismatched fallback_oid".into()));
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

impl PageBudget {
    pub fn new() -> Self {
        Self {
            entries: 0,
            bytes: 0,
        }
    }

    /// Append `entry` when it still fits. `Ok(false)` means the page must be
    /// flushed first; the budget is unchanged in that case.
    pub fn try_push(&mut self, entry: &ChunkEntry) -> Result<bool, ManifestError> {
        let entry_len = serde_json::to_vec(entry)
            .map_err(|error| ManifestError::Serde(error.to_string()))?
            .len();
        let next = if self.entries == 0 {
            entry_len.checked_add(2)
        } else {
            self.bytes
                .checked_add(entry_len)
                .and_then(|sum| sum.checked_add(1))
        }
        .ok_or_else(|| ManifestError::Invalid("page size overflow".into()))?;
        if self.entries + 1 > MAX_PAGE_ENTRIES || next > MAX_PAGE_ENTRIES_BYTES {
            return Ok(false);
        }
        self.entries += 1;
        self.bytes = next;
        Ok(true)
    }
}

impl Default for PageBudget {
    fn default() -> Self {
        Self::new()
    }
}

/// Compact canonical encoding of a chunk-entry slice (identity / P-01a).
pub fn canonical_entries_bytes(entries: &[ChunkEntry]) -> Result<Vec<u8>, ManifestError> {
    serde_json::to_vec(entries).map_err(|error| ManifestError::Serde(error.to_string()))
}

/// Longest prefix length under the P-01a entry-count and byte budgets.
pub fn longest_page_prefix(chunks: &[ChunkEntry]) -> Result<usize, ManifestError> {
    if chunks.is_empty() {
        return Ok(0);
    }
    let max = chunks.len().min(MAX_PAGE_ENTRIES);
    let mut lo = 1usize;
    let mut hi = max;
    let mut best = 0usize;
    while lo <= hi {
        let mid = (lo + hi) / 2;
        let bytes = canonical_entries_bytes(&chunks[..mid])?;
        if bytes.len() <= MAX_PAGE_ENTRIES_BYTES {
            best = mid;
            lo = mid + 1;
        } else if mid == 1 {
            return Err(ManifestError::Invalid(
                "single chunk entry exceeds page byte budget".into(),
            ));
        } else {
            hi = mid - 1;
        }
    }
    if best == 0 {
        return Err(ManifestError::Invalid(
            "unable to form a non-empty page under P-01a budgets".into(),
        ));
    }
    Ok(best)
}

/// Deterministic P-01a page split. Empty input yields zero pages. Every
/// non-final page is the longest legal prefix of the remainder; the final
/// page is non-empty.
pub fn split_pages(chunks: &[ChunkEntry]) -> Result<Vec<Vec<ChunkEntry>>, ManifestError> {
    if chunks.is_empty() {
        return Ok(Vec::new());
    }
    let mut pages = Vec::new();
    let mut rest = chunks;
    while !rest.is_empty() {
        let n = longest_page_prefix(rest)?;
        pages.push(rest[..n].to_vec());
        rest = &rest[n..];
    }
    Ok(pages)
}

/// Streaming SHA-256 of the canonical identity tuple. Page boundaries are not
/// inputs. The digest matches `serde_json` of
/// `(version, algorithm, hash_algorithm, media_oid, media_size, chunks)`.
pub struct CanonicalIdHasher {
    ctx: Context,
    any: bool,
}

impl CanonicalIdHasher {
    pub fn new(
        version: u32,
        algorithm: &str,
        hash_algorithm: &str,
        media_oid: &str,
        media_size: u64,
    ) -> Result<Self, ManifestError> {
        let prefix =
            serde_json::to_vec(&(version, algorithm, hash_algorithm, media_oid, media_size))
                .map_err(|error| ManifestError::Serde(error.to_string()))?;
        if prefix.last() != Some(&b']') {
            return Err(ManifestError::Invalid(
                "canonical identity prefix is not a JSON array".into(),
            ));
        }
        let mut ctx = Context::new(&SHA256);
        ctx.update(&prefix[..prefix.len() - 1]);
        ctx.update(b",");
        ctx.update(b"[");
        Ok(Self { ctx, any: false })
    }

    pub fn push(&mut self, entry: &ChunkEntry) -> Result<(), ManifestError> {
        if self.any {
            self.ctx.update(b",");
        }
        self.any = true;
        let bytes =
            serde_json::to_vec(entry).map_err(|error| ManifestError::Serde(error.to_string()))?;
        self.ctx.update(&bytes);
        Ok(())
    }

    pub fn finish(mut self) -> String {
        self.ctx.update(b"]");
        self.ctx.update(b"]");
        hex::encode(self.ctx.finish().as_ref())
    }
}

pub fn local_created_by() -> CreatedBy {
    CreatedBy {
        client: "libra".to_string(),
        version: env!("LIBRA_PKG_VERSION").to_string(),
        capabilities: vec![chunker::ALGORITHM.to_string(), HASH_ALGORITHM.to_string()],
    }
}

impl ManifestPage {
    pub fn from_json(text: &str) -> Result<Self, ManifestError> {
        if text.len() > MAX_ENVELOPE_SIZE {
            return Err(ManifestError::Invalid(
                "page envelope exceeds size limit".into(),
            ));
        }
        let page: ManifestPage =
            serde_json::from_str(text).map_err(|error| ManifestError::Serde(error.to_string()))?;
        page.validate()?;
        Ok(page)
    }

    pub fn validate(&self) -> Result<(), ManifestError> {
        if self.entries.is_empty() {
            return Err(ManifestError::Invalid(
                "page entries must be non-empty".into(),
            ));
        }
        if self.entries.len() > MAX_PAGE_ENTRIES {
            return Err(ManifestError::Invalid(
                "page exceeds max_page_entries".into(),
            ));
        }
        let bytes = canonical_entries_bytes(&self.entries)?;
        if bytes.len() > MAX_PAGE_ENTRIES_BYTES {
            return Err(ManifestError::Invalid(
                "page entries exceed compact byte budget".into(),
            ));
        }
        for (i, entry) in self.entries.iter().enumerate() {
            validate_page_entry(i, entry)?;
        }
        Ok(())
    }
}

impl ManifestSummary {
    pub fn from_json(text: &str) -> Result<Self, ManifestError> {
        if text.len() > MAX_ENVELOPE_SIZE {
            return Err(ManifestError::Invalid(
                "summary envelope exceeds size limit".into(),
            ));
        }
        let summary: ManifestSummary =
            serde_json::from_str(text).map_err(|error| ManifestError::Serde(error.to_string()))?;
        summary.validate()?;
        Ok(summary)
    }

    pub fn validate(&self) -> Result<(), ManifestError> {
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
        if self.hash_algorithm != HASH_ALGORITHM {
            return Err(ManifestError::Invalid(format!(
                "unsupported hash algorithm '{}' (oid must be sha256)",
                self.hash_algorithm
            )));
        }
        if !is_sha256_hex(&self.oid) || !is_sha256_hex(&self.manifest_id) {
            return Err(ManifestError::Invalid(
                "oid and manifest_id must be 64 lowercase-hex characters".into(),
            ));
        }
        if let Some(created_by) = &self.created_by {
            let bytes = serde_json::to_vec(created_by)
                .map_err(|error| ManifestError::Serde(error.to_string()))?;
            if bytes.len() > MAX_CREATED_BY_BYTES {
                return Err(ManifestError::Invalid(
                    "created_by exceeds 4096 bytes".into(),
                ));
            }
        }
        Ok(())
    }
}

pub fn validate_page_entry(i: usize, entry: &ChunkEntry) -> Result<(), ManifestError> {
    if entry.checksum.is_some() {
        return Err(ManifestError::Invalid(format!(
            "chunk {i} has unsupported checksum"
        )));
    }
    let max = chunker::MAX_SIZE as u64;
    if entry.length == 0 || entry.length > max {
        return Err(ManifestError::Invalid(format!(
            "chunk {i} has invalid length"
        )));
    }
    if !is_sha256_hex(&entry.chunk_hash) {
        return Err(ManifestError::Invalid(format!(
            "chunk {i} chunk_hash must be 64 lowercase-hex characters"
        )));
    }
    if entry.compression != COMPRESSION_NONE {
        return Err(ManifestError::Invalid(format!(
            "chunk {i} has unsupported compression '{}' (supports only 'none')",
            entry.compression
        )));
    }
    if entry.encoded_length != entry.length {
        return Err(ManifestError::Invalid(format!(
            "chunk {i} encoded_length {} must equal length {}",
            entry.encoded_length, entry.length
        )));
    }
    Ok(())
}

/// Next covered offset, or an error for overlap, a gap, or `u64` overflow.
pub fn advance_coverage(expected: u64, entry: &ChunkEntry) -> Result<u64, ManifestError> {
    if entry.offset < expected {
        return Err(ManifestError::Invalid(format!(
            "chunk offset {} overlaps (expected {expected})",
            entry.offset
        )));
    }
    if entry.offset > expected {
        return Err(ManifestError::Invalid(format!(
            "chunk offset {} breaks contiguity (expected {expected})",
            entry.offset
        )));
    }
    expected
        .checked_add(entry.length)
        .ok_or_else(|| ManifestError::Invalid("chunk offset overflow".into()))
}

/// A non-final page must be the longest P-01a prefix of itself plus the next entry.
pub fn non_final_page_is_longest(
    page: &[ChunkEntry],
    next: &ChunkEntry,
) -> Result<(), ManifestError> {
    if page.is_empty() {
        return Err(ManifestError::Invalid(
            "non-final page must be non-empty".into(),
        ));
    }
    let mut extended = Vec::with_capacity(page.len() + 1);
    extended.extend_from_slice(page);
    extended.push(next.clone());
    let legal = longest_page_prefix(&extended)?;
    if legal != page.len() {
        return Err(ManifestError::Invalid(
            "non-final page is not the longest P-01a prefix".into(),
        ));
    }
    Ok(())
}

/// Parse one page JSON envelope (≤1 MiB) and validate P-01a entry budgets.
pub fn parse_page_envelope(bytes: &[u8]) -> Result<ManifestPage, ManifestError> {
    if bytes.len() > MAX_ENVELOPE_SIZE {
        return Err(ManifestError::Invalid(
            "page envelope exceeds size limit".into(),
        ));
    }
    let text = std::str::from_utf8(bytes)
        .map_err(|_| ManifestError::Invalid("page envelope is not utf-8".into()))?;
    ManifestPage::from_json(text)
}

/// Rejects an empty cursor and any cursor already observed on this stream.
#[derive(Debug, Default)]
pub struct CursorGuard {
    seen: HashSet<String>,
}

impl CursorGuard {
    pub fn new() -> Self {
        Self {
            seen: HashSet::new(),
        }
    }

    pub fn observe(&mut self, next: Option<&str>) -> Result<(), ManifestError> {
        let Some(cursor) = next else {
            return Ok(());
        };
        if cursor.is_empty() {
            return Err(ManifestError::Invalid("empty paging cursor".into()));
        }
        if !self.seen.insert(cursor.to_owned()) {
            return Err(ManifestError::Invalid("paging cursor cycled".into()));
        }
        Ok(())
    }
}

impl MediaManifest {
    /// Bounded summary of an in-memory manifest. Paging does not change [`Self::id`].
    pub fn summary(&self) -> Result<ManifestSummary, ManifestError> {
        let pages = split_pages(&self.chunks)?;
        let page_count = u32::try_from(pages.len())
            .map_err(|_| ManifestError::Invalid("page_count exceeds u32".into()))?;
        let chunk_count = u64::try_from(self.chunks.len())
            .map_err(|_| ManifestError::Invalid("chunk_count exceeds u64".into()))?;
        Ok(ManifestSummary {
            version: self.version,
            algorithm: self.algorithm.clone(),
            hash_algorithm: self.hash_algorithm.clone(),
            oid: self.media_oid.clone(),
            size: self.media_size,
            chunk_count,
            page_count,
            manifest_id: self.id()?,
            created_by: Some(self.created_by.clone()),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample() -> MediaManifest {
        MediaManifest {
            version: 1,
            algorithm: "fastcdc-v2020-32k".to_string(),
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
                capabilities: vec!["fastcdc-v2020-32k".to_string(), "sha256".to_string()],
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

    fn tuple_id(manifest: &MediaManifest) -> String {
        let bytes = serde_json::to_vec(&(
            manifest.version,
            &manifest.algorithm,
            &manifest.hash_algorithm,
            &manifest.media_oid,
            manifest.media_size,
            &manifest.chunks,
        ))
        .unwrap();
        super::super::sha256_hex(&bytes)
    }

    fn synthetic_entry(index: u64, length: u64) -> ChunkEntry {
        ChunkEntry {
            offset: index.checked_mul(length).unwrap(),
            length,
            chunk_hash: format!("{index:064x}"),
            encoded_length: length,
            compression: COMPRESSION_NONE.to_string(),
            checksum: None,
        }
    }

    fn manifest_from(chunks: Vec<ChunkEntry>) -> MediaManifest {
        let media_size = chunks.iter().map(|chunk| chunk.length).sum();
        MediaManifest {
            version: MANIFEST_VERSION,
            algorithm: chunker::ALGORITHM.to_string(),
            hash_algorithm: HASH_ALGORITHM.to_string(),
            media_oid: "a".repeat(64),
            media_size,
            chunks,
            created_by: local_created_by(),
            fallback_oid: None,
        }
    }

    #[test]
    fn canonical_id_matches_tuple_encoding_and_ignores_pages() {
        let chunks: Vec<_> = (0..4097).map(|i| synthetic_entry(i, 32 * 1024)).collect();
        let manifest = manifest_from(chunks);
        let id = manifest.id().unwrap();
        assert_eq!(id, tuple_id(&manifest));
        assert_eq!(id.len(), 64);

        let pages = split_pages(&manifest.chunks).unwrap();
        assert_eq!(pages.len(), 2);
        assert_eq!(pages[0].len(), MAX_PAGE_ENTRIES);
        assert_eq!(pages[1].len(), 1);
        let mut hasher = CanonicalIdHasher::new(
            manifest.version,
            &manifest.algorithm,
            &manifest.hash_algorithm,
            &manifest.media_oid,
            manifest.media_size,
        )
        .unwrap();
        for page in &pages {
            for entry in page {
                hasher.push(entry).unwrap();
            }
        }
        assert_eq!(hasher.finish(), id);

        let mut changed = manifest.clone();
        changed.created_by.client = "other".to_string();
        changed.fallback_oid = Some(changed.media_oid.clone());
        assert_eq!(changed.id().unwrap(), id);
    }

    #[test]
    fn empty_manifest_has_zero_pages_and_stable_id() {
        let manifest = manifest_from(Vec::new());
        assert!(split_pages(&manifest.chunks).unwrap().is_empty());
        assert_eq!(manifest.id().unwrap(), tuple_id(&manifest));
    }

    #[test]
    fn page_budget_matches_canonical_bytes_and_byte_split() {
        let mut budget = PageBudget::new();
        let mut kept = Vec::new();
        for i in 0..8u64 {
            let entry = synthetic_entry(i, 1024);
            assert!(budget.try_push(&entry).unwrap());
            kept.push(entry);
        }
        assert_eq!(budget.bytes, canonical_entries_bytes(&kept).unwrap().len());

        let fat: Vec<_> = (0..200)
            .map(|i| ChunkEntry {
                offset: i,
                length: 1,
                chunk_hash: "ab".repeat(4000),
                encoded_length: 1,
                compression: COMPRESSION_NONE.to_string(),
                checksum: None,
            })
            .collect();
        let pages = split_pages(&fat).unwrap();
        assert!(pages.len() >= 2);
        assert!(pages[0].len() < MAX_PAGE_ENTRIES);
        assert!(canonical_entries_bytes(&pages[0]).unwrap().len() <= MAX_PAGE_ENTRIES_BYTES);
        let mut too_far = pages[0].clone();
        too_far.push(pages[1][0].clone());
        assert!(canonical_entries_bytes(&too_far).unwrap().len() > MAX_PAGE_ENTRIES_BYTES);

        let mut greedy = PageBudget::new();
        let mut greedy_len = 0usize;
        for entry in &fat {
            if !greedy.try_push(entry).unwrap() {
                break;
            }
            greedy_len += 1;
        }
        assert_eq!(greedy_len, pages[0].len());

        let huge = vec![ChunkEntry {
            offset: 0,
            length: 1,
            chunk_hash: "a".repeat(MAX_PAGE_ENTRIES_BYTES),
            encoded_length: 1,
            compression: COMPRESSION_NONE.to_string(),
            checksum: None,
        }];
        assert!(split_pages(&huge).is_err());
    }

    #[test]
    fn product_chunk_cap_is_gone() {
        let chunks: Vec<_> = (0..8193).map(|i| synthetic_entry(i, 1)).collect();
        let manifest = manifest_from(chunks);
        manifest.validate().unwrap();
        assert_eq!(manifest.id().unwrap(), tuple_id(&manifest));
    }

    #[test]
    fn page_envelope_and_coverage_matrix() {
        let one = ManifestPage {
            page_no: 0,
            entries: vec![synthetic_entry(0, 32)],
        };
        let bytes = serde_json::to_vec(&one).unwrap();
        assert!(bytes.len() <= MAX_ENVELOPE_SIZE);
        assert_eq!(parse_page_envelope(&bytes).unwrap(), one);
        assert!(parse_page_envelope(&vec![b'x'; MAX_ENVELOPE_SIZE + 1]).is_err());

        let mut over = one.clone();
        over.entries = (0..=MAX_PAGE_ENTRIES as u64)
            .map(|i| synthetic_entry(i, 1))
            .collect();
        assert!(over.validate().is_err());

        let mut covered = 0u64;
        for entry in &one.entries {
            covered = advance_coverage(covered, entry).unwrap();
        }
        assert_eq!(covered, 32);
        assert!(advance_coverage(10, &synthetic_entry(0, 1)).is_err());
        let mut gap = synthetic_entry(0, 1);
        gap.offset = 12;
        assert!(advance_coverage(10, &gap).is_err());
        let mut overflow = synthetic_entry(0, 2);
        overflow.offset = u64::MAX - 1;
        assert!(advance_coverage(u64::MAX - 1, &overflow).is_err());

        let mut guard = CursorGuard::new();
        guard.observe(Some("a")).unwrap();
        guard.observe(None).unwrap();
        assert!(guard.observe(Some("")).is_err());
        assert!(guard.observe(Some("a")).is_err());
    }

    #[test]
    fn deterministic_page_counts_and_non_final_prefix() {
        for (count, pages) in [(4096usize, 1usize), (4097, 2), (65536, 16), (65537, 17)] {
            let chunks: Vec<_> = (0..count as u64).map(|i| synthetic_entry(i, 32)).collect();
            let split = split_pages(&chunks).unwrap();
            assert_eq!(split.len(), pages, "{count} entries");
            assert!(
                split
                    .iter()
                    .rev()
                    .skip(1)
                    .all(|page| page.len() == MAX_PAGE_ENTRIES)
            );
            assert!(!split.last().unwrap().is_empty());
            let logical = (count as u64).checked_mul(256 * 1024).unwrap();
            assert!(logical > 2 * 1024 * 1024 * 1024 || count < 8192);
        }

        let page: Vec<_> = (0..MAX_PAGE_ENTRIES as u64)
            .map(|i| synthetic_entry(i, 32))
            .collect();
        let next = synthetic_entry(MAX_PAGE_ENTRIES as u64, 32);
        non_final_page_is_longest(&page, &next).unwrap();
        let short = &page[..page.len() - 1];
        assert!(non_final_page_is_longest(short, &page[page.len() - 1]).is_err());
    }
}
