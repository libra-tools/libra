//! Local content-addressed media chunk store (lore.md §6).
//!
//! Deliberately NOT a Git [`crate::utils::storage::Storage`] backend: chunks are
//! RAW bytes keyed by their SHA-256, with no Git `<type> <len>\0` framing and no
//! zlib — so a chunk can never be mistaken for (or become) a Git object. It
//! lives at `.libra/media/fastcdc-v2020-32k/chunks/<ab>/<chunk_hash>`, a physical
//! sibling of `objects/` (see [`crate::utils::path::media_chunks`]) that is never walked as
//! a loose-object store, and it bypasses [`crate::utils::client_storage`]
//! entirely so no `object_index`/cloud-backup rows are enqueued for non-Git
//! content. It reuses the crash-safe `write_atomic` temp+rename discipline and
//! re-verifies the SHA-256 of raw bytes on read (never trusting on-disk data).

use std::{
    fs::File,
    io::{Read, Write},
    path::{Path, PathBuf},
};

use super::{
    chunker, is_sha256_hex,
    manifest::{
        self, ChunkEntry, CreatedBy, ManifestPage, ManifestSummary, MediaManifest, PageBudget,
    },
    page_cache::{self, PageKey},
    sha256_hex,
};
use crate::utils::atomic_write;

/// Directory scope segment for the local derived index (`scope + manifest_id`).
pub const LOCAL_INDEX_SCOPE: &str = "local";

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
    #[error("{0}")]
    Invalid(String),
    #[error("chunk hash {hash} length conflict across pages: stored {stored}, new {new}")]
    HashLengthConflict { hash: String, stored: u64, new: u64 },
}

impl From<std::io::Error> for MediaStoreError {
    fn from(source: std::io::Error) -> Self {
        Self::Io {
            path: "<media>".to_string(),
            source,
        }
    }
}

/// A local media chunk store rooted at `.libra/media/<namespace>/chunks`.
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

    /// Page/index directory plus the chunk directory to write.
    ///
    /// `.libra/media/chunks` (legacy v1) is redirected to
    /// `.libra/media/fastcdc-v2020-32k` and is not read or deleted. A store
    /// already rooted at `…/chunks` keeps that directory.
    pub fn resolved_cache(&self) -> (PathBuf, Self) {
        let chunks = &self.root;
        let Some(parent) = chunks.parent() else {
            return (
                chunks.clone(),
                Self {
                    root: chunks.clone(),
                },
            );
        };
        if chunks.file_name() == Some(std::ffi::OsStr::new("chunks"))
            && parent.file_name() == Some(std::ffi::OsStr::new("media"))
        {
            let layout = parent.join(crate::utils::path::MEDIA_CACHE_NAMESPACE);
            return (
                layout.clone(),
                Self {
                    root: layout.join("chunks"),
                },
            );
        }
        (
            parent.to_path_buf(),
            Self {
                root: chunks.clone(),
            },
        )
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

const HASH_MAGIC: &[u8; 4] = b"LMH1";
const OFFSET_MAGIC: &[u8; 4] = b"LMO1";
const HASH_HEADER: u64 = 32;
const HASH_SLOT: u64 = 64;
const OFFSET_HEADER: u64 = 16;
const OFFSET_SLOT: u64 = 64;
const INITIAL_HASH_CAP: u64 = 1024;

fn io_err(path: &Path, source: std::io::Error) -> MediaStoreError {
    MediaStoreError::Io {
        path: path.display().to_string(),
        source,
    }
}

fn invalid(message: impl Into<String>) -> MediaStoreError {
    MediaStoreError::Invalid(message.into())
}

fn map_manifest(err: manifest::ManifestError) -> MediaStoreError {
    MediaStoreError::Invalid(err.to_string())
}

fn arr4(bytes: &[u8]) -> [u8; 4] {
    let mut out = [0u8; 4];
    out.copy_from_slice(bytes);
    out
}

fn arr8(bytes: &[u8]) -> [u8; 8] {
    let mut out = [0u8; 8];
    out.copy_from_slice(bytes);
    out
}

fn decode_hash(hash_hex: &str) -> Result<[u8; 32], MediaStoreError> {
    if !is_sha256_hex(hash_hex) {
        return Err(MediaStoreError::InvalidHash(hash_hex.to_string()));
    }
    let mut out = [0u8; 32];
    hex::decode_to_slice(hash_hex, &mut out)
        .map_err(|_| MediaStoreError::InvalidHash(hash_hex.to_string()))?;
    Ok(out)
}

fn hash_len(capacity: u64) -> Result<usize, MediaStoreError> {
    if !capacity.is_power_of_two() || capacity < 2 {
        return Err(invalid("hash index capacity must be a power of two"));
    }
    let slots = capacity
        .checked_mul(HASH_SLOT)
        .ok_or_else(|| invalid("hash index size overflow"))?;
    usize::try_from(HASH_HEADER + slots).map_err(|_| invalid("hash index size overflow"))
}

fn empty_hash_image(capacity: u64) -> Result<Vec<u8>, MediaStoreError> {
    let mut bytes = vec![0u8; hash_len(capacity)?];
    write_hash_header(&mut bytes, capacity, 0)?;
    Ok(bytes)
}

fn empty_offset_image() -> Vec<u8> {
    let mut bytes = vec![0u8; OFFSET_HEADER as usize];
    bytes[..4].copy_from_slice(OFFSET_MAGIC);
    bytes[4..8].copy_from_slice(&1u32.to_le_bytes());
    bytes
}

pub struct HashSlot {
    occupied: bool,
    hash: [u8; 32],
    length: u64,
    offset: u64,
    page_no: u32,
}

enum Place {
    Inserted,
    Duplicate,
}

fn hash_slot_range(index: u64) -> Result<std::ops::Range<usize>, MediaStoreError> {
    let start = usize::try_from(HASH_HEADER + index * HASH_SLOT)
        .map_err(|_| invalid("hash index offset overflow"))?;
    Ok(start..start + HASH_SLOT as usize)
}

fn read_hash_slot(bytes: &[u8], index: u64) -> Result<HashSlot, MediaStoreError> {
    let buf = bytes
        .get(hash_slot_range(index)?)
        .ok_or_else(|| invalid("hash index truncated"))?;
    let mut hash = [0u8; 32];
    hash.copy_from_slice(&buf[1..33]);
    Ok(HashSlot {
        occupied: buf[0] == 1,
        hash,
        length: u64::from_le_bytes(arr8(&buf[33..41])),
        offset: u64::from_le_bytes(arr8(&buf[41..49])),
        page_no: u32::from_le_bytes(arr4(&buf[49..53])),
    })
}

fn write_hash_slot(
    bytes: &mut [u8],
    index: u64,
    hash: &[u8; 32],
    length: u64,
    offset: u64,
    page_no: u32,
) -> Result<(), MediaStoreError> {
    let range = hash_slot_range(index)?;
    let buf = bytes
        .get_mut(range)
        .ok_or_else(|| invalid("hash index truncated"))?;
    buf[0] = 1;
    buf[1..33].copy_from_slice(hash);
    buf[33..41].copy_from_slice(&length.to_le_bytes());
    buf[41..49].copy_from_slice(&offset.to_le_bytes());
    buf[49..53].copy_from_slice(&page_no.to_le_bytes());
    Ok(())
}

fn bucket(hash: &[u8; 32], capacity: u64) -> u64 {
    // Fold every byte, then avalanche. A raw prefix (or a left-padded hex
    // counter) must not collapse onto one slot.
    let mut z = u64::from_le_bytes(arr8(&hash[0..8]))
        ^ u64::from_le_bytes(arr8(&hash[8..16])).rotate_left(17)
        ^ u64::from_le_bytes(arr8(&hash[16..24])).rotate_left(31)
        ^ u64::from_le_bytes(arr8(&hash[24..32])).rotate_left(47);
    z = (z ^ (z >> 30)).wrapping_mul(0xBF58476D1CE4E5B9);
    z = (z ^ (z >> 27)).wrapping_mul(0x94D049BB133111EB);
    z ^= z >> 31;
    z & (capacity - 1)
}

fn place_hash(
    bytes: &mut [u8],
    capacity: u64,
    hash: &[u8; 32],
    length: u64,
    offset: u64,
    page_no: u32,
) -> Result<Place, MediaStoreError> {
    let start = bucket(hash, capacity);
    for probe in 0..capacity {
        let index = (start + probe) & (capacity - 1);
        let slot = read_hash_slot(bytes, index)?;
        if !slot.occupied {
            write_hash_slot(bytes, index, hash, length, offset, page_no)?;
            return Ok(Place::Inserted);
        }
        if slot.hash == *hash {
            if slot.length != length {
                return Err(MediaStoreError::HashLengthConflict {
                    hash: hex::encode(hash),
                    stored: slot.length,
                    new: length,
                });
            }
            return Ok(Place::Duplicate);
        }
    }
    Err(invalid("hash index is full"))
}

fn lookup_hash_slot(
    bytes: &[u8],
    capacity: u64,
    hash: &[u8; 32],
) -> Result<Option<HashSlot>, MediaStoreError> {
    let start = bucket(hash, capacity);
    for probe in 0..capacity {
        let index = (start + probe) & (capacity - 1);
        let slot = read_hash_slot(bytes, index)?;
        if !slot.occupied {
            return Ok(None);
        }
        if slot.hash == *hash {
            return Ok(Some(slot));
        }
    }
    Ok(None)
}

fn write_hash_header(bytes: &mut [u8], capacity: u64, count: u64) -> Result<(), MediaStoreError> {
    let buf = bytes
        .get_mut(..HASH_HEADER as usize)
        .ok_or_else(|| invalid("hash index truncated"))?;
    buf[..4].copy_from_slice(HASH_MAGIC);
    buf[4..8].copy_from_slice(&1u32.to_le_bytes());
    buf[8..16].copy_from_slice(&capacity.to_le_bytes());
    buf[16..24].copy_from_slice(&count.to_le_bytes());
    Ok(())
}

fn write_offset_header(bytes: &mut [u8], count: u64) -> Result<(), MediaStoreError> {
    let buf = bytes
        .get_mut(..OFFSET_HEADER as usize)
        .ok_or_else(|| invalid("offset index truncated"))?;
    buf[..4].copy_from_slice(OFFSET_MAGIC);
    buf[4..8].copy_from_slice(&1u32.to_le_bytes());
    buf[8..16].copy_from_slice(&count.to_le_bytes());
    Ok(())
}

/// Derived hash → length and offset → chunk index. Keyed on disk by
/// `scope/manifest_id` (the directory), not by repository config. Safe to delete;
/// [`DiskMediaIndex::rebuild`] restores it from immutable pages.
pub struct DiskMediaIndex {
    dir: PathBuf,
    hash_bytes: Vec<u8>,
    offset_bytes: Vec<u8>,
    capacity: u64,
    hash_count: u64,
    offset_count: u64,
    expected_offset: u64,
}

impl DiskMediaIndex {
    pub fn create(dir: &Path) -> Result<Self, MediaStoreError> {
        Self::create_with_capacity(dir, INITIAL_HASH_CAP)
    }

    pub fn create_with_capacity(dir: &Path, capacity: u64) -> Result<Self, MediaStoreError> {
        std::fs::create_dir_all(dir).map_err(|source| io_err(dir, source))?;
        Ok(Self {
            dir: dir.to_path_buf(),
            hash_bytes: empty_hash_image(capacity)?,
            offset_bytes: empty_offset_image(),
            capacity,
            hash_count: 0,
            offset_count: 0,
            expected_offset: 0,
        })
    }

    pub fn open(dir: &Path) -> Result<Self, MediaStoreError> {
        let hash_path = dir.join("hash.idx");
        let offset_path = dir.join("offset.idx");
        let hash_bytes = std::fs::read(&hash_path).map_err(|source| io_err(&hash_path, source))?;
        let offset_bytes =
            std::fs::read(&offset_path).map_err(|source| io_err(&offset_path, source))?;
        let (capacity, hash_count) = read_hash_header(&hash_bytes)?;
        let offset_count = read_offset_header(&offset_bytes)?;
        if hash_bytes.len() != hash_len(capacity)? {
            return Err(invalid("hash index length does not match its header"));
        }
        let expected_offset = if offset_count == 0 {
            0
        } else {
            let last = read_offset_record(&offset_bytes, offset_count - 1)?;
            last.offset
                .checked_add(last.length)
                .ok_or_else(|| invalid("chunk offset overflow"))?
        };
        Ok(Self {
            dir: dir.to_path_buf(),
            hash_bytes,
            offset_bytes,
            capacity,
            hash_count,
            offset_count,
            expected_offset,
        })
    }

    #[cfg(test)]
    #[cfg(test)]
    pub fn unique_count(&self) -> u64 {
        self.hash_count
    }

    /// `Ok(true)` when `hash_hex` was not yet present. Same hash with a different
    /// length is a hard error, including across pages.
    pub fn insert(
        &mut self,
        hash_hex: &str,
        length: u64,
        offset: u64,
        page_no: u32,
    ) -> Result<bool, MediaStoreError> {
        if offset != self.expected_offset {
            return Err(invalid(format!(
                "chunk offset {offset} breaks contiguity (expected {})",
                self.expected_offset
            )));
        }
        let next = offset
            .checked_add(length)
            .ok_or_else(|| invalid("chunk offset overflow"))?;
        let hash = decode_hash(hash_hex)?;
        let fresh =
            if let Some(existing) = lookup_hash_slot(&self.hash_bytes, self.capacity, &hash)? {
                if existing.length != length {
                    return Err(MediaStoreError::HashLengthConflict {
                        hash: hash_hex.to_string(),
                        stored: existing.length,
                        new: length,
                    });
                }
                false
            } else {
                if self
                    .hash_count
                    .checked_add(1)
                    .and_then(|count| count.checked_mul(2))
                    .is_none_or(|loaded| loaded > self.capacity)
                {
                    self.grow_hash()?;
                }
                match place_hash(
                    &mut self.hash_bytes,
                    self.capacity,
                    &hash,
                    length,
                    offset,
                    page_no,
                )? {
                    Place::Inserted => self.hash_count += 1,
                    Place::Duplicate => {
                        return Err(invalid("hash index lost a lookup that reported a new hash"));
                    }
                }
                true
            };
        self.append_offset(&hash, length, offset, page_no)?;
        self.expected_offset = next;
        Ok(fresh)
    }

    pub fn lookup_hash(&mut self, hash_hex: &str) -> Result<Option<HashSlot>, MediaStoreError> {
        let hash = decode_hash(hash_hex)?;
        lookup_hash_slot(&self.hash_bytes, self.capacity, &hash)
    }

    /// `(offset, length)` for a content hash, if the derived index contains it.
    pub fn lookup_span(&mut self, hash_hex: &str) -> Result<Option<(u64, u64)>, MediaStoreError> {
        Ok(self
            .lookup_hash(hash_hex)?
            .map(|slot| (slot.offset, slot.length)))
    }

    pub fn lookup_offset(&mut self, target: u64) -> Result<Option<OffsetRecord>, MediaStoreError> {
        if self.offset_count == 0 {
            return Ok(None);
        }
        let mut lo = 0u64;
        let mut hi = self.offset_count;
        while lo < hi {
            let mid = lo + (hi - lo) / 2;
            let record = read_offset_record(&self.offset_bytes, mid)?;
            if record.offset <= target {
                lo = mid + 1;
            } else {
                hi = mid;
            }
        }
        if lo == 0 {
            return Ok(None);
        }
        let record = read_offset_record(&self.offset_bytes, lo - 1)?;
        let end = record
            .offset
            .checked_add(record.length)
            .ok_or_else(|| invalid("chunk offset overflow"))?;
        if target < end {
            Ok(Some(record))
        } else {
            Ok(None)
        }
    }

    pub fn finish(&mut self) -> Result<(), MediaStoreError> {
        write_hash_header(&mut self.hash_bytes, self.capacity, self.hash_count)?;
        write_offset_header(&mut self.offset_bytes, self.offset_count)?;
        let hash_path = self.dir.join("hash.idx");
        let offset_path = self.dir.join("offset.idx");
        atomic_write::write_atomic(
            &hash_path,
            &self.hash_bytes,
            atomic_write::sync_data_enabled(),
        )
        .map_err(|source| io_err(&hash_path, source))?;
        atomic_write::write_atomic(
            &offset_path,
            &self.offset_bytes,
            atomic_write::sync_data_enabled(),
        )
        .map_err(|source| io_err(&offset_path, source))?;
        Ok(())
    }

    pub fn rebuild(pages: &Path, page_count: u32, dir: &Path) -> Result<Self, MediaStoreError> {
        if dir.exists() {
            std::fs::remove_dir_all(dir).map_err(|source| io_err(dir, source))?;
        }
        let mut index = Self::create(dir)?;
        let manifest_id = dir
            .file_name()
            .and_then(|name| name.to_str())
            .unwrap_or("unknown");
        let mut expected = 0u64;
        for page_no in 0..page_count {
            let page = read_page_file(pages, LOCAL_INDEX_SCOPE, manifest_id, page_no)?;
            for entry in page.entries {
                if entry.offset != expected {
                    return Err(invalid(format!(
                        "chunk offset {} breaks contiguity (expected {expected})",
                        entry.offset
                    )));
                }
                index.insert(&entry.chunk_hash, entry.length, entry.offset, page_no)?;
                expected = expected
                    .checked_add(entry.length)
                    .ok_or_else(|| invalid("chunk offset overflow"))?;
            }
        }
        index.finish()?;
        Ok(index)
    }

    fn append_offset(
        &mut self,
        hash: &[u8; 32],
        length: u64,
        offset: u64,
        page_no: u32,
    ) -> Result<(), MediaStoreError> {
        let mut buf = [0u8; OFFSET_SLOT as usize];
        buf[..8].copy_from_slice(&offset.to_le_bytes());
        buf[8..16].copy_from_slice(&length.to_le_bytes());
        buf[16..20].copy_from_slice(&page_no.to_le_bytes());
        buf[24..56].copy_from_slice(hash);
        let at = usize::try_from(OFFSET_HEADER + self.offset_count * OFFSET_SLOT)
            .map_err(|_| invalid("offset index offset overflow"))?;
        let end = at + OFFSET_SLOT as usize;
        if self.offset_bytes.len() < end {
            self.offset_bytes.resize(end, 0);
        }
        self.offset_bytes[at..end].copy_from_slice(&buf);
        self.offset_count += 1;
        Ok(())
    }

    fn grow_hash(&mut self) -> Result<(), MediaStoreError> {
        let new_cap = self
            .capacity
            .checked_mul(2)
            .ok_or_else(|| invalid("hash index capacity overflow"))?;
        let mut new_bytes = empty_hash_image(new_cap)?;
        for index in 0..self.capacity {
            let slot = read_hash_slot(&self.hash_bytes, index)?;
            if !slot.occupied {
                continue;
            }
            match place_hash(
                &mut new_bytes,
                new_cap,
                &slot.hash,
                slot.length,
                slot.offset,
                slot.page_no,
            )? {
                Place::Inserted => {}
                Place::Duplicate => {
                    return Err(invalid("hash index corrupt during grow"));
                }
            }
        }
        write_hash_header(&mut new_bytes, new_cap, self.hash_count)?;
        self.hash_bytes = new_bytes;
        self.capacity = new_cap;
        Ok(())
    }
}

pub struct OffsetRecord {
    pub offset: u64,
    pub length: u64,
    pub page_no: u32,
    pub hash: [u8; 32],
}

fn read_hash_header(bytes: &[u8]) -> Result<(u64, u64), MediaStoreError> {
    let buf = bytes
        .get(..HASH_HEADER as usize)
        .ok_or_else(|| invalid("hash index truncated"))?;
    if &buf[..4] != HASH_MAGIC {
        return Err(invalid("hash index magic mismatch"));
    }
    if u32::from_le_bytes(arr4(&buf[4..8])) != 1 {
        return Err(invalid("hash index version mismatch"));
    }
    let capacity = u64::from_le_bytes(arr8(&buf[8..16]));
    let count = u64::from_le_bytes(arr8(&buf[16..24]));
    if capacity < 2 || !capacity.is_power_of_two() || count > capacity {
        return Err(invalid("hash index header is corrupt"));
    }
    Ok((capacity, count))
}

fn read_offset_header(bytes: &[u8]) -> Result<u64, MediaStoreError> {
    let buf = bytes
        .get(..OFFSET_HEADER as usize)
        .ok_or_else(|| invalid("offset index truncated"))?;
    if &buf[..4] != OFFSET_MAGIC {
        return Err(invalid("offset index magic mismatch"));
    }
    if u32::from_le_bytes(arr4(&buf[4..8])) != 1 {
        return Err(invalid("offset index version mismatch"));
    }
    Ok(u64::from_le_bytes(arr8(&buf[8..16])))
}

fn read_offset_record(bytes: &[u8], index: u64) -> Result<OffsetRecord, MediaStoreError> {
    let start = usize::try_from(OFFSET_HEADER + index * OFFSET_SLOT)
        .map_err(|_| invalid("offset index offset overflow"))?;
    let buf = bytes
        .get(start..start + OFFSET_SLOT as usize)
        .ok_or_else(|| invalid("offset index truncated"))?;
    let mut hash = [0u8; 32];
    hash.copy_from_slice(&buf[24..56]);
    Ok(OffsetRecord {
        offset: u64::from_le_bytes(arr8(&buf[..8])),
        length: u64::from_le_bytes(arr8(&buf[8..16])),
        page_no: u32::from_le_bytes(arr4(&buf[16..20])),
        hash,
    })
}

pub fn read_envelope_file(path: &Path) -> Result<String, MediaStoreError> {
    let file = File::open(path).map_err(|source| io_err(path, source))?;
    let mut buf = Vec::new();
    file.take((manifest::MAX_ENVELOPE_SIZE as u64) + 1)
        .read_to_end(&mut buf)
        .map_err(|source| io_err(path, source))?;
    if buf.len() > manifest::MAX_ENVELOPE_SIZE {
        return Err(invalid(format!(
            "envelope '{}' exceeds {} bytes",
            path.display(),
            manifest::MAX_ENVELOPE_SIZE
        )));
    }
    String::from_utf8(buf)
        .map_err(|_| invalid(format!("envelope '{}' is not utf-8", path.display())))
}

pub fn repo_media_root() -> Result<PathBuf, MediaStoreError> {
    crate::utils::path::media_chunks()
        .parent()
        .map(Path::to_path_buf)
        .ok_or_else(|| invalid("media chunks path has no parent"))
}

pub fn summary_path(media_root: &Path, oid: &str) -> PathBuf {
    media_root.join("manifests").join(oid).join("summary.json")
}

fn pages_dir(media_root: &Path, oid: &str) -> PathBuf {
    media_root.join("manifests").join(oid).join("pages")
}

fn index_dir_for(media_root: &Path, manifest_id: &str) -> PathBuf {
    media_root
        .join("index")
        .join(LOCAL_INDEX_SCOPE)
        .join(manifest_id)
}

pub fn open_index(media_root: &Path, manifest_id: &str) -> Result<DiskMediaIndex, MediaStoreError> {
    DiskMediaIndex::open(&index_dir_for(media_root, manifest_id))
}

pub fn prepare_index(
    media_root: &Path,
    manifest_id: &str,
) -> Result<DiskMediaIndex, MediaStoreError> {
    let dir = index_dir_for(media_root, manifest_id);
    if dir.exists() {
        std::fs::remove_dir_all(&dir).map_err(|source| io_err(&dir, source))?;
    }
    DiskMediaIndex::create(&dir)
}

pub fn load_local_page(
    media_root: &Path,
    oid: &str,
    manifest_id: &str,
    page_no: u32,
) -> Result<ManifestPage, MediaStoreError> {
    read_page_file(
        &pages_dir(media_root, oid),
        LOCAL_INDEX_SCOPE,
        manifest_id,
        page_no,
    )
}

pub fn write_page(
    media_root: &Path,
    oid: &str,
    page: &ManifestPage,
) -> Result<(), MediaStoreError> {
    page.validate().map_err(map_manifest)?;
    let bytes = serde_json::to_vec(page).map_err(|err| invalid(err.to_string()))?;
    if bytes.len() > manifest::MAX_ENVELOPE_SIZE {
        return Err(invalid("page envelope exceeds size limit"));
    }
    let dir = pages_dir(media_root, oid);
    std::fs::create_dir_all(&dir).map_err(|source| io_err(&dir, source))?;
    let path = dir.join(format!("{:08}.json", page.page_no));
    atomic_write::write_atomic(&path, &bytes, atomic_write::sync_data_enabled())
        .map_err(|source| io_err(&path, source))
}

pub fn write_summary(media_root: &Path, summary: &ManifestSummary) -> Result<(), MediaStoreError> {
    summary.validate().map_err(map_manifest)?;
    let bytes = serde_json::to_vec(summary).map_err(|err| invalid(err.to_string()))?;
    if bytes.len() > manifest::MAX_ENVELOPE_SIZE {
        return Err(invalid("summary envelope exceeds size limit"));
    }
    let path = summary_path(media_root, &summary.oid);
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(|source| io_err(parent, source))?;
    }
    atomic_write::write_atomic(&path, &bytes, atomic_write::sync_data_enabled())
        .map_err(|source| io_err(&path, source))
}

pub fn load_summary(media_root: &Path, oid: &str) -> Result<ManifestSummary, MediaStoreError> {
    if !is_sha256_hex(oid) {
        return Err(MediaStoreError::InvalidHash(oid.to_string()));
    }
    let path = summary_path(media_root, oid);
    let text = read_envelope_file(&path)?;
    ManifestSummary::from_json(&text).map_err(map_manifest)
}

fn read_page_file(
    pages: &Path,
    scope: &str,
    manifest_id: &str,
    page_no: u32,
) -> Result<ManifestPage, MediaStoreError> {
    let cache = page_cache::PageCache::shared();
    let key = PageKey {
        scope: scope.to_string(),
        manifest_id: manifest_id.to_string(),
        page_no,
    };
    let text = if let Some(hit) = cache.get(&key) {
        String::from_utf8(hit).map_err(|_| invalid("cached manifest page is not utf-8"))?
    } else {
        let path = pages.join(format!("{page_no:08}.json"));
        let text = read_envelope_file(&path)?;
        cache.insert(key, text.clone().into_bytes());
        text
    };
    ManifestPage::from_json(&text).map_err(map_manifest)
}

pub struct StreamOutcome {
    pub summary: ManifestSummary,
    pub unique_chunks: u64,
    pub max_buffered_entries: usize,
    pub manifest_path: PathBuf,
}

struct PartialDirs {
    paths: Vec<PathBuf>,
    committed: bool,
}

impl Drop for PartialDirs {
    fn drop(&mut self) {
        if self.committed {
            return;
        }
        for path in &self.paths {
            let _ = std::fs::remove_dir_all(path);
        }
    }
}

fn replace_dir(from: &Path, to: &Path) -> Result<(), MediaStoreError> {
    if to.exists() {
        std::fs::remove_dir_all(to).map_err(|source| io_err(to, source))?;
    }
    if let Some(parent) = to.parent() {
        std::fs::create_dir_all(parent).map_err(|source| io_err(parent, source))?;
    }
    std::fs::rename(from, to).map_err(|source| io_err(to, source))
}

/// Chunk `path` one page at a time into `media_root`. The canonical id is hashed
/// as entries are flushed; page boundaries are not identity inputs. Chunk bytes
/// are stored only when `store_chunks` is set.
pub fn stream_media_file(
    path: &Path,
    media_root: &Path,
    store_chunks: bool,
) -> Result<StreamOutcome, MediaStoreError> {
    stream_media_file_with_prior(path, media_root, store_chunks, None)
}

/// Like [`stream_media_file`], optionally applying ADR-FL-04 prior coherence.
///
/// `prior_manifest` may be a paged summary path/directory or a whole
/// [`MediaManifest`] JSON file. A structurally illegal prior fails before any
/// new cache layout is published. A valid prior whose size differs from the
/// source falls back to a cold cut. Same-length priors reuse only hash-matched
/// spans and re-chunk dirty regions (absorbing neighbors or cold-cutting when
/// a non-tail fragment would be below [`chunker::MIN_SIZE`]).
pub fn stream_media_file_with_prior(
    path: &Path,
    media_root: &Path,
    store_chunks: bool,
    prior_manifest: Option<&Path>,
) -> Result<StreamOutcome, MediaStoreError> {
    let media_oid =
        crate::utils::lfs::calc_lfs_file_hash(path).map_err(|source| io_err(path, source))?;
    let media_size = std::fs::metadata(path)
        .map_err(|source| io_err(path, source))?
        .len();
    let prior_spans = match prior_manifest {
        Some(prior_path) => Some(load_prior_spans(prior_path)?),
        None => None,
    };
    let planned = match prior_spans {
        Some(prior) if prior.size != media_size => None,
        Some(prior) => {
            let mut file = File::open(path).map_err(|source| io_err(path, source))?;
            chunker::rechunk_with_prior(&mut file, media_size, &prior.spans)
                .map_err(|source| io_err(path, source))?
        }
        None => None,
    };
    match planned {
        Some(chunks) => commit_chunk_plan(
            path,
            media_root,
            store_chunks,
            &media_oid,
            media_size,
            &chunks,
        ),
        None => cold_cut_stream(path, media_root, store_chunks, &media_oid, media_size),
    }
}

pub struct PriorLayout {
    pub size: u64,
    pub spans: Vec<chunker::PriorSpan>,
}

/// Load and structurally validate a prior layout. Does not compare bytes.
pub fn load_prior_spans(prior_path: &Path) -> Result<PriorLayout, MediaStoreError> {
    let (summary_dir, summary) = resolve_prior_summary(prior_path)?;
    if let Some(summary) = summary {
        summary.validate().map_err(map_manifest)?;
        let mut spans = Vec::with_capacity(summary.chunk_count as usize);
        let mut covered = 0u64;
        for page_no in 0..summary.page_count {
            let page_path = summary_dir.join("pages").join(format!("{page_no:08}.json"));
            let text = read_envelope_file(&page_path)?;
            let page = ManifestPage::from_json(&text).map_err(map_manifest)?;
            if page.page_no != page_no {
                return Err(invalid(format!(
                    "prior page_no {} does not match {page_no}",
                    page.page_no
                )));
            }
            page.validate().map_err(map_manifest)?;
            for entry in page.entries {
                if entry.offset != covered {
                    return Err(invalid(format!(
                        "prior chunk offset {} breaks contiguity (expected {covered})",
                        entry.offset
                    )));
                }
                covered = entry
                    .offset
                    .checked_add(entry.length)
                    .ok_or_else(|| invalid("prior chunk offset overflow"))?;
                spans.push(chunker::PriorSpan {
                    offset: entry.offset,
                    length: entry.length,
                    chunk_hash: entry.chunk_hash,
                });
            }
        }
        if covered != summary.size {
            return Err(invalid(format!(
                "prior chunk lengths sum to {covered} but size is {}",
                summary.size
            )));
        }
        if spans.len() as u64 != summary.chunk_count {
            return Err(invalid(format!(
                "prior chunk_count {} does not match {} entries",
                summary.chunk_count,
                spans.len()
            )));
        }
        return Ok(PriorLayout {
            size: summary.size,
            spans,
        });
    }
    // Whole-file MediaManifest JSON (tests / compact layouts).
    let text = read_envelope_file(prior_path)?;
    let manifest = MediaManifest::from_json(&text).map_err(map_manifest)?;
    let spans = manifest
        .chunks
        .iter()
        .map(|entry| chunker::PriorSpan {
            offset: entry.offset,
            length: entry.length,
            chunk_hash: entry.chunk_hash.clone(),
        })
        .collect();
    Ok(PriorLayout {
        size: manifest.media_size,
        spans,
    })
}

fn resolve_prior_summary(
    prior_path: &Path,
) -> Result<(PathBuf, Option<ManifestSummary>), MediaStoreError> {
    let summary_file = if prior_path.is_dir() {
        prior_path.join("summary.json")
    } else if prior_path
        .file_name()
        .is_some_and(|name| name == "summary.json")
    {
        prior_path.to_path_buf()
    } else {
        // Not a summary path — caller may treat it as a whole MediaManifest.
        return Ok((PathBuf::new(), None));
    };
    if !summary_file.is_file() {
        return Err(invalid(format!(
            "prior manifest summary '{}' is missing",
            summary_file.display()
        )));
    }
    let text = read_envelope_file(&summary_file)?;
    let summary = ManifestSummary::from_json(&text).map_err(map_manifest)?;
    Ok((
        summary_file
            .parent()
            .map(Path::to_path_buf)
            .ok_or_else(|| invalid("prior summary has no parent directory"))?,
        Some(summary),
    ))
}

fn cold_cut_stream(
    path: &Path,
    media_root: &Path,
    store_chunks: bool,
    media_oid: &str,
    media_size: u64,
) -> Result<StreamOutcome, MediaStoreError> {
    let (manifest_partial, index_partial, mut partial) = begin_partial_dirs(media_root, media_oid)?;
    let mut writer = ManifestWriter::create(
        &manifest_partial,
        &index_partial,
        media_oid,
        media_size,
        manifest::local_created_by(),
    )?;
    let store = MediaChunkStore::at(media_root.join("chunks"));
    let file = File::open(path).map_err(|source| io_err(path, source))?;
    chunker::visit_chunks(
        std::io::BufReader::new(file),
        |chunk, data| -> Result<(), MediaStoreError> {
            if store_chunks {
                store.put_chunk(data)?;
            }
            writer.push(entry_from_chunk(chunk))?;
            Ok(())
        },
    )?;
    finish_partial(
        media_root,
        media_oid,
        &manifest_partial,
        &index_partial,
        &mut partial,
        writer,
    )
}

fn commit_chunk_plan(
    path: &Path,
    media_root: &Path,
    store_chunks: bool,
    media_oid: &str,
    media_size: u64,
    chunks: &[chunker::Chunk],
) -> Result<StreamOutcome, MediaStoreError> {
    let (manifest_partial, index_partial, mut partial) = begin_partial_dirs(media_root, media_oid)?;
    let mut writer = ManifestWriter::create(
        &manifest_partial,
        &index_partial,
        media_oid,
        media_size,
        manifest::local_created_by(),
    )?;
    let store = MediaChunkStore::at(media_root.join("chunks"));
    let mut file = File::open(path).map_err(|source| io_err(path, source))?;
    for chunk in chunks {
        if store_chunks {
            let data = chunker::read_span(&mut file, chunk.offset, chunk.length)
                .map_err(|source| io_err(path, source))?;
            if sha256_hex(&data) != chunk.chunk_hash {
                return Err(invalid(
                    "planned chunk hash does not match source bytes".to_string(),
                ));
            }
            store.put_chunk(&data)?;
        }
        writer.push(entry_from_chunk(chunk))?;
    }
    finish_partial(
        media_root,
        media_oid,
        &manifest_partial,
        &index_partial,
        &mut partial,
        writer,
    )
}

fn entry_from_chunk(chunk: &chunker::Chunk) -> ChunkEntry {
    ChunkEntry {
        offset: chunk.offset,
        length: chunk.length,
        chunk_hash: chunk.chunk_hash.clone(),
        encoded_length: chunk.length,
        compression: manifest::COMPRESSION_NONE.to_string(),
        checksum: None,
    }
}

fn begin_partial_dirs(
    media_root: &Path,
    media_oid: &str,
) -> Result<(PathBuf, PathBuf, PartialDirs), MediaStoreError> {
    let manifest_partial = media_root
        .join("manifests")
        .join(format!(".partial-{media_oid}"));
    let index_partial = media_root
        .join("index")
        .join(LOCAL_INDEX_SCOPE)
        .join(format!(".partial-{media_oid}"));
    if manifest_partial.exists() {
        std::fs::remove_dir_all(&manifest_partial)
            .map_err(|source| io_err(&manifest_partial, source))?;
    }
    if index_partial.exists() {
        std::fs::remove_dir_all(&index_partial).map_err(|source| io_err(&index_partial, source))?;
    }
    let partial = PartialDirs {
        paths: vec![manifest_partial.clone(), index_partial.clone()],
        committed: false,
    };
    std::fs::create_dir_all(manifest_partial.join("pages"))
        .map_err(|source| io_err(&manifest_partial, source))?;
    Ok((manifest_partial, index_partial, partial))
}

fn finish_partial(
    media_root: &Path,
    media_oid: &str,
    manifest_partial: &Path,
    index_partial: &Path,
    partial: &mut PartialDirs,
    writer: ManifestWriter,
) -> Result<StreamOutcome, MediaStoreError> {
    let finished = writer.finish()?;
    let index_final = index_dir_for(media_root, &finished.summary.manifest_id);
    let manifest_final = media_root.join("manifests").join(media_oid);
    replace_dir(index_partial, &index_final)?;
    replace_dir(manifest_partial, &manifest_final)?;
    partial.committed = true;
    Ok(StreamOutcome {
        manifest_path: summary_path(media_root, media_oid),
        summary: finished.summary,
        unique_chunks: finished.unique_chunks,
        max_buffered_entries: finished.max_buffered_entries,
    })
}

/// Verify a cached layout still matches `path` (size, full-file oid, per-chunk
/// hashes). Used before prepare so a changed source fails closed without a
/// remote submit. Missing/evicted pages return `Ok(None)` so upload may cold-cut.
pub fn verify_cached_layout(
    media_root: &Path,
    oid: &str,
    size: u64,
    path: &Path,
) -> Result<Option<ManifestSummary>, MediaStoreError> {
    let summary = match load_summary(media_root, oid) {
        Ok(summary) => summary,
        Err(MediaStoreError::Io { source, .. })
            if source.kind() == std::io::ErrorKind::NotFound =>
        {
            return Ok(None);
        }
        Err(err) => return Err(err),
    };
    if summary.oid != oid || summary.size != size {
        return Ok(None);
    }
    let actual_oid =
        crate::utils::lfs::calc_lfs_file_hash(path).map_err(|source| io_err(path, source))?;
    let actual_size = std::fs::metadata(path)
        .map_err(|source| io_err(path, source))?
        .len();
    if actual_oid != oid || actual_size != size {
        return Err(invalid(
            "LFS source changed before FastCDC prepare".to_string(),
        ));
    }
    let mut file = File::open(path).map_err(|source| io_err(path, source))?;
    let mut covered = 0u64;
    for page_no in 0..summary.page_count {
        let page = match load_local_page(media_root, oid, &summary.manifest_id, page_no) {
            Ok(page) => page,
            Err(MediaStoreError::Io { source, .. })
                if source.kind() == std::io::ErrorKind::NotFound =>
            {
                return Ok(None);
            }
            Err(err) => return Err(err),
        };
        if page.page_no != page_no {
            return Err(invalid(format!(
                "stored page_no {} does not match {page_no}",
                page.page_no
            )));
        }
        for entry in &page.entries {
            if entry.offset != covered {
                return Err(invalid(format!(
                    "cached chunk offset {} breaks contiguity (expected {covered})",
                    entry.offset
                )));
            }
            let hash = chunker::hash_span(&mut file, entry.offset, entry.length)
                .map_err(|source| io_err(path, source))?;
            if hash != entry.chunk_hash {
                return Err(invalid(
                    "LFS source changed before FastCDC prepare".to_string(),
                ));
            }
            covered = entry
                .offset
                .checked_add(entry.length)
                .ok_or_else(|| invalid("chunk offset overflow"))?;
        }
    }
    if covered != size {
        return Err(invalid(format!(
            "cached layout covers {covered} bytes but size is {size}"
        )));
    }
    Ok(Some(summary))
}

struct ManifestWriter {
    manifest_dir: PathBuf,
    pages_dir: PathBuf,
    index: DiskMediaIndex,
    hasher: manifest::CanonicalIdHasher,
    page: Vec<ChunkEntry>,
    budget: PageBudget,
    page_no: u32,
    chunk_count: u64,
    covered: u64,
    unique: u64,
    max_buffered: usize,
    media_oid: String,
    media_size: u64,
    created_by: CreatedBy,
}

struct FinishedManifest {
    summary: ManifestSummary,
    unique_chunks: u64,
    max_buffered_entries: usize,
}

impl ManifestWriter {
    fn create(
        manifest_dir: &Path,
        index_dir: &Path,
        media_oid: &str,
        media_size: u64,
        created_by: CreatedBy,
    ) -> Result<Self, MediaStoreError> {
        let pages = manifest_dir.join("pages");
        std::fs::create_dir_all(&pages).map_err(|source| io_err(&pages, source))?;
        let hasher = manifest::CanonicalIdHasher::new(
            manifest::MANIFEST_VERSION,
            chunker::ALGORITHM,
            manifest::HASH_ALGORITHM,
            media_oid,
            media_size,
        )
        .map_err(map_manifest)?;
        Ok(Self {
            manifest_dir: manifest_dir.to_path_buf(),
            pages_dir: pages,
            index: DiskMediaIndex::create(index_dir)?,
            hasher,
            page: Vec::new(),
            budget: PageBudget::new(),
            page_no: 0,
            chunk_count: 0,
            covered: 0,
            unique: 0,
            max_buffered: 0,
            media_oid: media_oid.to_string(),
            media_size,
            created_by,
        })
    }

    fn push(&mut self, entry: ChunkEntry) -> Result<(), MediaStoreError> {
        if entry.offset != self.covered {
            return Err(invalid(format!(
                "chunk offset {} breaks contiguity (expected {})",
                entry.offset, self.covered
            )));
        }
        let next_covered = entry
            .offset
            .checked_add(entry.length)
            .ok_or_else(|| invalid("chunk offset overflow"))?;
        if !self.budget.try_push(&entry).map_err(map_manifest)? {
            if self.page.is_empty() {
                return Err(invalid(
                    "single chunk entry exceeds page byte budget".to_string(),
                ));
            }
            self.flush_page()?;
            if !self.budget.try_push(&entry).map_err(map_manifest)? {
                return Err(invalid(
                    "single chunk entry exceeds page byte budget".to_string(),
                ));
            }
        }
        self.page.push(entry);
        self.max_buffered = self.max_buffered.max(self.page.len());
        self.covered = next_covered;
        Ok(())
    }

    fn flush_page(&mut self) -> Result<(), MediaStoreError> {
        if self.page.is_empty() {
            return Ok(());
        }
        let page_no = self.page_no;
        let page = ManifestPage {
            page_no,
            entries: std::mem::take(&mut self.page),
        };
        page.validate().map_err(map_manifest)?;
        let bytes = serde_json::to_vec(&page).map_err(|err| invalid(err.to_string()))?;
        if bytes.len() > manifest::MAX_ENVELOPE_SIZE {
            return Err(invalid("page envelope exceeds size limit"));
        }
        let path = self.pages_dir.join(format!("{page_no:08}.json"));
        atomic_write::write_atomic(&path, &bytes, atomic_write::sync_data_enabled())
            .map_err(|source| io_err(&path, source))?;
        for entry in &page.entries {
            self.hasher.push(entry).map_err(map_manifest)?;
            if self
                .index
                .insert(&entry.chunk_hash, entry.length, entry.offset, page_no)?
            {
                self.unique += 1;
            }
        }
        self.chunk_count += page.entries.len() as u64;
        self.budget = PageBudget::new();
        self.page_no = self
            .page_no
            .checked_add(1)
            .ok_or_else(|| invalid("page_no overflow"))?;
        Ok(())
    }

    fn finish(mut self) -> Result<FinishedManifest, MediaStoreError> {
        self.flush_page()?;
        if self.covered != self.media_size {
            return Err(invalid(format!(
                "chunk lengths sum to {} but media_size is {}",
                self.covered, self.media_size
            )));
        }
        let manifest_id = self.hasher.finish();
        let summary = ManifestSummary {
            version: manifest::MANIFEST_VERSION,
            algorithm: chunker::ALGORITHM.to_string(),
            hash_algorithm: manifest::HASH_ALGORITHM.to_string(),
            oid: self.media_oid,
            size: self.media_size,
            chunk_count: self.chunk_count,
            page_count: self.page_no,
            manifest_id,
            created_by: Some(self.created_by),
        };
        summary.validate().map_err(map_manifest)?;
        let bytes = serde_json::to_vec(&summary).map_err(|err| invalid(err.to_string()))?;
        if bytes.len() > manifest::MAX_ENVELOPE_SIZE {
            return Err(invalid("summary envelope exceeds size limit"));
        }
        let path = self.manifest_dir.join("summary.json");
        atomic_write::write_atomic(&path, &bytes, atomic_write::sync_data_enabled())
            .map_err(|source| io_err(&path, source))?;
        self.index.finish()?;
        Ok(FinishedManifest {
            summary,
            unique_chunks: self.unique,
            max_buffered_entries: self.max_buffered,
        })
    }
}

fn open_or_rebuild(
    media_root: &Path,
    summary: &ManifestSummary,
) -> Result<DiskMediaIndex, MediaStoreError> {
    let dir = index_dir_for(media_root, &summary.manifest_id);
    if let Ok(index) = DiskMediaIndex::open(&dir) {
        return Ok(index);
    }
    DiskMediaIndex::rebuild(
        &pages_dir(media_root, &summary.oid),
        summary.page_count,
        &dir,
    )
}

/// Reassemble from immutable pages, one page at a time, checking the derived
/// index for hash length and offset. A missing index is rebuilt from the pages.
pub fn reassemble_paged(
    media_root: &Path,
    summary: &ManifestSummary,
    dest: &Path,
) -> Result<(), MediaStoreError> {
    use ring::digest::{Context, SHA256};

    summary.validate().map_err(map_manifest)?;
    let mut index = open_or_rebuild(media_root, summary)?;
    let store = MediaChunkStore::at(media_root.join("chunks"));
    let io_error = |source| MediaStoreError::Io {
        path: dest.display().to_string(),
        source,
    };
    let target = std::path::absolute(dest).map_err(io_error)?;
    let parent = target
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
        .unwrap_or(Path::new("."));
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
    let mut covered = 0u64;
    let pages = pages_dir(media_root, &summary.oid);
    for page_no in 0..summary.page_count {
        let page = read_page_file(&pages, LOCAL_INDEX_SCOPE, &summary.manifest_id, page_no)?;
        if page.page_no != page_no {
            return Err(invalid(format!(
                "stored page_no {} does not match {page_no}",
                page.page_no
            )));
        }
        for entry in &page.entries {
            if entry.offset != covered {
                return Err(invalid(format!(
                    "chunk offset {} breaks contiguity (expected {covered})",
                    entry.offset
                )));
            }
            let hashed = index.lookup_hash(&entry.chunk_hash)?.ok_or_else(|| {
                invalid(format!(
                    "derived index has no entry for hash {}",
                    entry.chunk_hash
                ))
            })?;
            if hashed.length != entry.length {
                return Err(MediaStoreError::HashLengthConflict {
                    hash: entry.chunk_hash.clone(),
                    stored: hashed.length,
                    new: entry.length,
                });
            }
            let located = index.lookup_offset(entry.offset)?.ok_or_else(|| {
                invalid(format!(
                    "derived index has no chunk covering offset {}",
                    entry.offset
                ))
            })?;
            if located.offset != entry.offset
                || located.length != entry.length
                || located.page_no != page_no
                || located.hash.as_slice() != decode_hash(&entry.chunk_hash)?.as_slice()
            {
                return Err(invalid(format!(
                    "derived index offset {} does not match page {page_no}",
                    entry.offset
                )));
            }
            let bytes = store.get_chunk(&entry.chunk_hash)?;
            if bytes.len() as u64 != entry.length {
                return Err(io_error(std::io::Error::other(
                    "chunk length does not match manifest",
                )));
            }
            digest.update(&bytes);
            writer.write_all(&bytes).map_err(io_error)?;
            covered = covered
                .checked_add(entry.length)
                .ok_or_else(|| invalid("chunk offset overflow"))?;
        }
    }
    if covered != summary.size {
        return Err(invalid(format!(
            "chunk lengths sum to {covered} but media size is {}",
            summary.size
        )));
    }
    let actual = hex::encode(digest.finish().as_ref());
    if actual != summary.oid {
        return Err(MediaStoreError::MediaOidMismatch {
            expected: summary.oid.clone(),
            actual,
        });
    }
    writer.persist(&target).map_err(io_error)?;
    Ok(())
}

/// ADR-FL-03 range bounds: zero length requires `offset ≤ size`; any positive
/// span must fit entirely inside `[0, size)` without overflow.
pub fn validate_byte_range(size: u64, offset: u64, length: u64) -> Result<(), MediaStoreError> {
    if length == 0 {
        if offset > size {
            return Err(invalid(format!(
                "range offset {offset} is past object size {size}"
            )));
        }
        return Ok(());
    }
    let end = offset
        .checked_add(length)
        .ok_or_else(|| invalid("range offset+length overflow"))?;
    if end > size {
        return Err(invalid(format!(
            "range [{offset}, {end}) extends past object size {size}"
        )));
    }
    Ok(())
}

/// Write a byte slice assembled from covering chunks to a new file.
///
/// Does not modify hydrate state, the tracked pointer, or a whole-object LFS
/// cache entry. The destination must not exist (including as a symlink); the
/// publish uses same-directory temp + hard-link no-clobber semantics.
pub fn write_range_export(
    store: &MediaChunkStore,
    covering: &[ChunkEntry],
    offset: u64,
    length: u64,
    dest: &Path,
) -> Result<(), MediaStoreError> {
    let io_error = |source| MediaStoreError::Io {
        path: dest.display().to_string(),
        source,
    };
    let target = std::path::absolute(dest).map_err(io_error)?;
    let parent = target
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
        .unwrap_or(Path::new("."));
    let mut writer = crate::utils::atomic_stream::StreamingAtomicFile::new_in(
        parent,
        atomic_write::sync_data_enabled(),
    )
    .map_err(io_error)?;
    if length == 0 {
        return writer.persist_create_new(&target).map_err(io_error);
    }
    let end = offset
        .checked_add(length)
        .ok_or_else(|| invalid("range offset+length overflow"))?;
    let mut written = 0u64;
    let mut expected = covering.first().map(|entry| entry.offset);
    for entry in covering {
        if let Some(want) = expected
            && entry.offset != want
        {
            return Err(invalid(format!(
                "covering chunk offset {} breaks contiguity (expected {want})",
                entry.offset
            )));
        }
        let chunk_end = entry
            .offset
            .checked_add(entry.length)
            .ok_or_else(|| invalid("chunk offset overflow"))?;
        if chunk_end <= offset || entry.offset >= end {
            expected = Some(chunk_end);
            continue;
        }
        let bytes = store.get_chunk(&entry.chunk_hash)?;
        if bytes.len() as u64 != entry.length {
            return Err(io_error(std::io::Error::other(
                "chunk length does not match manifest",
            )));
        }
        let skip = offset.saturating_sub(entry.offset) as usize;
        let take_end = (end.min(chunk_end) - entry.offset) as usize;
        let slice = bytes
            .get(skip..take_end)
            .ok_or_else(|| invalid("range slice exceeds chunk bytes"))?;
        writer.write_all(slice).map_err(io_error)?;
        written = written
            .checked_add(slice.len() as u64)
            .ok_or_else(|| invalid("range write length overflow"))?;
        expected = Some(chunk_end);
    }
    if written != length {
        return Err(invalid(format!(
            "range export wrote {written} bytes but requested {length}"
        )));
    }
    writer.persist_create_new(&target).map_err(io_error)
}

#[cfg(test)]
mod paging_tests {
    use super::*;

    #[test]
    fn legacy_v1_cache_root_is_not_the_namespace() {
        assert_eq!(
            crate::utils::path::MEDIA_CACHE_NAMESPACE,
            crate::utils::media::chunker::ALGORITHM
        );
        let legacy = Path::new("/repo/.libra/media/chunks");
        let store = MediaChunkStore::at(legacy.to_path_buf());
        let (layout, chunks) = store.resolved_cache();
        assert_eq!(layout, Path::new("/repo/.libra/media/fastcdc-v2020-32k"));
        assert_eq!(
            chunks.root,
            Path::new("/repo/.libra/media/fastcdc-v2020-32k/chunks")
        );
        let current = MediaChunkStore::at(chunks.root.clone());
        let (again, same) = current.resolved_cache();
        assert_eq!(again, layout);
        assert_eq!(same.root, chunks.root);
    }

    fn entry(index: u64, length: u64) -> ChunkEntry {
        ChunkEntry {
            offset: index.saturating_mul(length),
            length,
            chunk_hash: format!("{index:064x}"),
            encoded_length: length,
            compression: manifest::COMPRESSION_NONE.to_string(),
            checksum: None,
        }
    }

    #[test]
    fn hash_index_grows_and_finds_offsets() {
        let dir = tempfile::tempdir().unwrap();
        let mut index = DiskMediaIndex::create_with_capacity(dir.path(), 16).unwrap();
        for i in 0..40u64 {
            assert!(index.insert(&format!("{i:064x}"), 10, i * 10, 0).unwrap());
        }
        index.finish().unwrap();
        let mut opened = DiskMediaIndex::open(dir.path()).unwrap();
        assert_eq!(opened.unique_count(), 40);
        let at = opened.lookup_offset(25).unwrap().unwrap();
        assert_eq!(at.offset, 20);
        assert_eq!(at.length, 10);
        let hashed = opened.lookup_hash(&format!("{:064x}", 3)).unwrap().unwrap();
        assert_eq!(hashed.length, 10);
        assert_eq!(hashed.offset, 30);
    }

    #[test]
    fn cross_page_hash_length_conflict_is_rejected_and_same_length_is_kept() {
        let dir = tempfile::tempdir().unwrap();
        let mut writer = ManifestWriter::create(
            &dir.path().join("manifest"),
            &dir.path().join("index"),
            &"a".repeat(64),
            4098,
            manifest::local_created_by(),
        )
        .unwrap();
        for i in 0..4096u64 {
            writer.push(entry(i, 1)).unwrap();
        }
        let mut conflict = entry(0, 1);
        conflict.offset = 4096;
        conflict.length = 2;
        conflict.encoded_length = 2;
        writer.push(conflict).unwrap();
        assert!(writer.finish().is_err());

        let manifest_dir = dir.path().join("manifest-ok");
        let index_dir = dir.path().join("index-ok");
        let mut writer = ManifestWriter::create(
            &manifest_dir,
            &index_dir,
            &"b".repeat(64),
            4097,
            manifest::local_created_by(),
        )
        .unwrap();
        for i in 0..4096u64 {
            writer.push(entry(i, 1)).unwrap();
        }
        let mut again = entry(0, 1);
        again.offset = 4096;
        writer.push(again).unwrap();
        let finished = writer.finish().unwrap();
        assert_eq!(finished.unique_chunks, 4096);
        let mut index = DiskMediaIndex::open(&index_dir).unwrap();
        let first = index.lookup_offset(0).unwrap().unwrap();
        let second = index.lookup_offset(4096).unwrap().unwrap();
        assert_eq!(first.hash, second.hash);
        assert_eq!(first.length, second.length);
    }

    #[test]
    fn gap_is_rejected() {
        let dir = tempfile::tempdir().unwrap();
        let mut writer = ManifestWriter::create(
            &dir.path().join("m"),
            &dir.path().join("i"),
            &"c".repeat(64),
            3,
            manifest::local_created_by(),
        )
        .unwrap();
        writer.push(entry(0, 1)).unwrap();
        let mut skipped = entry(2, 1);
        skipped.offset = 5;
        assert!(writer.push(skipped).is_err());
    }

    #[test]
    fn logical_16gib_past_65536_chunks_stays_within_a_page_buffer() {
        let dir = tempfile::tempdir().unwrap();
        let count = 65_537u64;
        let length = chunker::MAX_SIZE as u64;
        let size = count * length;
        assert!(count > 65_536);
        assert!(size > 16 * 1024 * 1024 * 1024);
        let oid = "d".repeat(64);
        let mut side = manifest::CanonicalIdHasher::new(
            manifest::MANIFEST_VERSION,
            chunker::ALGORITHM,
            manifest::HASH_ALGORITHM,
            &oid,
            size,
        )
        .unwrap();
        let mut writer = ManifestWriter::create(
            &dir.path().join("manifest"),
            &dir.path().join("index"),
            &oid,
            size,
            manifest::local_created_by(),
        )
        .unwrap();
        for i in 0..count {
            let entry = entry(i, length);
            side.push(&entry).unwrap();
            writer.push(entry).unwrap();
        }
        let finished = writer.finish().unwrap();
        assert_eq!(finished.summary.chunk_count, count);
        assert_eq!(finished.summary.size, size);
        assert_eq!(finished.summary.page_count, 17);
        assert!(finished.max_buffered_entries <= manifest::MAX_PAGE_ENTRIES);
        assert_eq!(finished.unique_chunks, count);
        assert_eq!(finished.summary.manifest_id, side.finish());
        let boundary = 4096 * length;
        let mut index = DiskMediaIndex::open(&dir.path().join("index")).unwrap();
        let at_boundary = index.lookup_offset(boundary).unwrap().unwrap();
        assert_eq!(at_boundary.offset, boundary);
        assert_eq!(at_boundary.page_no, 1);
        let previous = index.lookup_offset(boundary - 1).unwrap().unwrap();
        assert_eq!(previous.page_no, 0);
        assert_eq!(previous.offset, boundary - length);
        drop(index);
        std::fs::remove_dir_all(dir.path().join("index")).unwrap();
        let mut rebuilt = DiskMediaIndex::rebuild(
            &dir.path().join("manifest").join("pages"),
            finished.summary.page_count,
            &dir.path().join("index"),
        )
        .unwrap();
        let again = rebuilt.lookup_offset(boundary).unwrap().unwrap();
        assert_eq!(again.offset, boundary);
        assert_eq!(again.page_no, 1);
    }

    #[test]
    fn stream_file_roundtrip_rebuilds_a_deleted_index() {
        let dir = tempfile::tempdir().unwrap();
        let source = dir.path().join("blob.bin");
        std::fs::write(&source, b"libra-media-paging").unwrap();
        let root = dir.path().join("media");
        let outcome = stream_media_file(&source, &root, true).unwrap();
        assert_eq!(outcome.summary.chunk_count, 1);
        assert!(outcome.max_buffered_entries <= manifest::MAX_PAGE_ENTRIES);
        let dest = dir.path().join("out.bin");
        reassemble_paged(&root, &outcome.summary, &dest).unwrap();
        assert_eq!(std::fs::read(&dest).unwrap(), b"libra-media-paging");
        std::fs::remove_dir_all(root.join("index")).unwrap();
        let dest2 = dir.path().join("out2.bin");
        reassemble_paged(&root, &outcome.summary, &dest2).unwrap();
        assert_eq!(std::fs::read(dest2).unwrap(), b"libra-media-paging");
        assert!(root.join("index").join(LOCAL_INDEX_SCOPE).exists());
    }

    fn fixed_seq_bytes(len: usize) -> Vec<u8> {
        let mut out = Vec::with_capacity(len);
        let mut counter: u64 = 0;
        while out.len() < len {
            let mut h = ring::digest::Context::new(&ring::digest::SHA256);
            h.update(b"libra-fastcdc-fixture");
            h.update(&counter.to_le_bytes());
            let raw = h.finish();
            let take = (len - out.len()).min(raw.as_ref().len());
            out.extend_from_slice(&raw.as_ref()[..take]);
            counter += 1;
        }
        out
    }

    #[test]
    fn prior_same_length_edit_reuses_matching_spans_in_store() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("media");
        let original = dir.path().join("orig.bin");
        let edited = dir.path().join("edit.bin");
        let bytes = fixed_seq_bytes(1_048_576);
        std::fs::write(&original, &bytes).unwrap();
        let prior = stream_media_file(&original, &root, true).unwrap();
        let prior_path = prior.manifest_path.clone();

        let mut edited_bytes = bytes.clone();
        let cold = chunker::chunk_bytes(&bytes);
        let mid = &cold[cold.len() / 2];
        let flip = mid.offset as usize + mid.length as usize / 2;
        edited_bytes[flip] ^= 0xff;
        std::fs::write(&edited, &edited_bytes).unwrap();

        let coherent =
            stream_media_file_with_prior(&edited, &root, true, Some(&prior_path)).unwrap();
        // Coherent layout is stored under the new oid and must cover the file.
        assert_eq!(coherent.summary.size, edited_bytes.len() as u64);
        assert_ne!(coherent.summary.oid, prior.summary.oid);
        assert!(coherent.manifest_path.is_file());
        let prior_page =
            load_local_page(&root, &prior.summary.oid, &prior.summary.manifest_id, 0).unwrap();
        let new_page = load_local_page(
            &root,
            &coherent.summary.oid,
            &coherent.summary.manifest_id,
            0,
        )
        .unwrap();
        let reused = prior_page
            .entries
            .iter()
            .filter(|old| {
                new_page.entries.iter().any(|new| {
                    new.offset == old.offset
                        && new.length == old.length
                        && new.chunk_hash == old.chunk_hash
                })
            })
            .count();
        assert!(
            reused >= 1,
            "same-length prior must reuse at least one matching span"
        );
        let verified =
            verify_cached_layout(&root, &coherent.summary.oid, coherent.summary.size, &edited)
                .unwrap()
                .expect("cached coherent layout verifies");
        assert_eq!(verified.manifest_id, coherent.summary.manifest_id);
    }

    #[test]
    fn prior_length_change_or_absent_matches_cold_cut() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("media");
        let short = dir.path().join("short.bin");
        let long = dir.path().join("long.bin");
        std::fs::write(&short, fixed_seq_bytes(65_536)).unwrap();
        std::fs::write(&long, fixed_seq_bytes(131_072)).unwrap();
        let prior = stream_media_file(&short, &root, true).unwrap();
        let with_prior =
            stream_media_file_with_prior(&long, &root, true, Some(&prior.manifest_path)).unwrap();
        let cold = stream_media_file(&long, &dir.path().join("cold"), true).unwrap();
        assert_eq!(with_prior.summary.manifest_id, cold.summary.manifest_id);
        assert_eq!(with_prior.summary.chunk_count, cold.summary.chunk_count);

        let none =
            stream_media_file_with_prior(&long, &dir.path().join("none"), true, None).unwrap();
        assert_eq!(none.summary.manifest_id, cold.summary.manifest_id);
    }

    #[test]
    fn illegal_prior_fails_without_publishing_new_cache() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("media");
        let source = dir.path().join("blob.bin");
        std::fs::write(&source, fixed_seq_bytes(65_536)).unwrap();
        let oid = crate::utils::lfs::calc_lfs_file_hash(&source).unwrap();
        let bad_prior = dir.path().join("bad-prior.json");
        std::fs::write(&bad_prior, b"{\"not\":\"a-manifest\"}").unwrap();
        let err = match stream_media_file_with_prior(&source, &root, true, Some(&bad_prior)) {
            Ok(_) => panic!("illegal prior must fail"),
            Err(err) => err,
        };
        let message = format!("{err}");
        assert!(
            message.contains("malformed")
                || message.contains("serialize")
                || message.contains("manifest")
                || message.contains("invalid"),
            "{err}"
        );
        assert!(
            !summary_path(&root, &oid).exists(),
            "illegal prior must not publish a new cache layout"
        );
        assert!(
            !root
                .join("manifests")
                .join(format!(".partial-{oid}"))
                .exists()
        );
    }
}
