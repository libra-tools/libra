//! Local filesystem storage backend for Git objects.
//! This module implements the `Storage` trait for a local filesystem backend. It supports both loose objects and packed objects, allowing for efficient storage and retrieval of Git objects on disk.
//! The `LocalStorage` struct provides methods to read and write Git objects, as well as to search for objects by prefix. It handles the Git object storage format, including zlib compression for loose objects
//! and the pack file format for packed objects. The implementation also includes caching mechanisms for pack objects to improve performance when accessing packed data.
use std::{
    fs, io,
    io::{Read, Seek, Write},
    path::{Path, PathBuf},
    str::FromStr,
    sync::{Arc, Mutex},
};

use async_trait::async_trait;
use byteorder::{BigEndian, ReadBytesExt};
use flate2::{Compression, read::ZlibDecoder, write::ZlibEncoder};
use git_internal::{
    errors::GitError,
    hash::{HashKind, ObjectHash, get_hash_kind, set_hash_kind},
    internal::{
        object::types::ObjectType,
        pack::{Pack, cache_object::CacheObject},
    },
    utils::read_sha,
};
use lru_mem::LruCache;
use once_cell::sync::Lazy;

use crate::{command, utils::storage::Storage};

/// Cache for pack objects, keyed by "pack_file_name-offset"
static PACK_OBJ_CACHE: Lazy<Mutex<LruCache<String, CacheObject>>> =
    Lazy::new(|| Mutex::new(LruCache::new(1024 * 1024 * 200)));

const IDX_MAGIC: [u8; 4] = [0xFF, 0x74, 0x4F, 0x63];
const FANOUT: u64 = 256 * 4;

/// Index version for pack files
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum IdxVersion {
    V1,
    V2,
}

/// Local filesystem storage backend
#[derive(Default, Clone)]
pub struct LocalStorage {
    base_path: PathBuf,
    hash_kind: Option<HashKind>, // Capture hash kind from creation thread
    /// lore.md 2.3: flattened, transitive alternate object stores this store
    /// borrows FROM. Each is a plain (alternate-free) store, so a borrowed read
    /// probes them without recursion. `Arc` keeps `LocalStorage` cheaply
    /// cloneable and finitely-sized.
    alternates: Vec<std::sync::Arc<LocalStorage>>,
}

impl LocalStorage {
    pub fn new(base_path: PathBuf) -> Self {
        fs::create_dir_all(&base_path).unwrap_or_else(|err| {
            panic!(
                "LocalStorage::new({}): create_dir_all failed: {err}",
                base_path.display()
            )
        });
        Self {
            base_path,
            hash_kind: Some(get_hash_kind()),
            alternates: Vec::new(),
        }
    }

    /// Open an existing object dir WITHOUT creating it (lore.md 2.3): an
    /// alternate base may be missing or read-only, and auto-creating it would
    /// mask a dangling alternate. No alternates of its own (the chain is
    /// pre-flattened by the caller).
    fn open_no_create(base_path: PathBuf) -> Self {
        Self {
            base_path,
            hash_kind: Some(get_hash_kind()),
            alternates: Vec::new(),
        }
    }

    /// Build a store whose read path also consults the repo's alternate chain
    /// (`objects/info/alternates`, transitive). Used by `ClientStorage::init`.
    pub fn new_with_alternates(base_path: PathBuf) -> Self {
        let mut store = Self::new(base_path.clone());
        store.alternates = crate::internal::alternates::resolve_chain(&base_path)
            .into_iter()
            .map(|dir| std::sync::Arc::new(Self::open_no_create(dir)))
            .collect();
        store
    }

    /// Read an object's bytes from THIS store only (loose→pack), no alternates.
    fn get_here(&self, hash: &ObjectHash) -> Result<Option<(Vec<u8>, ObjectType)>, GitError> {
        if self.exist_loosely(hash) {
            let raw_data = self.read_raw_data(hash)?;
            let data = Self::decompress_zlib(&raw_data)?;
            let (type_str, _, end_of_header) = Self::parse_header(&data)?;
            let obj_type = ObjectType::from_string(&type_str)?;
            Ok(Some((data[end_of_header + 1..].to_vec(), obj_type)))
        } else {
            Ok(self.get_from_pack(hash)?.map(|x| (x.0, x.1)))
        }
    }

    /// Transforms an object hash into a path like "ab/cdef...". This is used for loose objects.
    fn transform_path(&self, hash: &ObjectHash) -> String {
        let hash = hash.to_string();
        // INVARIANT: `hash` is the lowercase-hex string from `ObjectHash::to_string()`
        // (SHA-1 / SHA-256), so every byte of the resulting path is ASCII alphanumeric
        // and therefore valid UTF-8. `OsString::into_string()` only returns Err on
        // non-UTF-8 byte sequences, which cannot occur here.
        Path::new(&hash[0..2])
            .join(&hash[2..hash.len()])
            .into_os_string()
            .into_string()
            .expect("hex object hash always round-trips through OsString as UTF-8")
    }

    /// Gets the full path to an object file based on its hash. For example, "base_path/ab/cdef...".
    pub(crate) fn get_obj_path(&self, obj_id: &ObjectHash) -> PathBuf {
        Path::new(&self.base_path).join(self.transform_path(obj_id))
    }

    /// Checks if a loose object exists by looking for its file. This is a quick check before looking into packs.
    fn exist_loosely(&self, obj_id: &ObjectHash) -> bool {
        let path = self.get_obj_path(obj_id);
        Path::exists(&path)
    }

    /// Reads the raw compressed data of a loose object from the filesystem. This is used when we know the object exists as a loose object.
    fn read_raw_data(&self, obj_id: &ObjectHash) -> Result<Vec<u8>, io::Error> {
        let path = self.get_obj_path(obj_id);
        let mut file = fs::File::open(path)?;
        let mut buffer = Vec::new();
        file.read_to_end(&mut buffer)?;
        Ok(buffer)
    }

    /// Decompresses zlib-compressed data, which is the format used for loose objects. This is used after reading the raw data of a loose object.
    fn decompress_zlib(data: &[u8]) -> io::Result<Vec<u8>> {
        let mut decoder = ZlibDecoder::new(data);
        let mut decompressed_data = Vec::new();
        decoder.read_to_end(&mut decompressed_data)?;
        Ok(decompressed_data)
    }

    /// Compresses data using zlib, which is the format used for storing loose objects. This is used before writing a new loose object to the filesystem.
    fn compress_zlib(data: &[u8]) -> io::Result<Vec<u8>> {
        let mut encoder = ZlibEncoder::new(Vec::new(), Compression::default());
        encoder.write_all(data)?;
        let compressed_data = encoder.finish()?;
        Ok(compressed_data)
    }

    /// Parses the header of a loose object, which has the format "type size\0".
    /// This is used after decompressing a loose object's data to extract its
    /// type and size.
    ///
    /// Returns [`GitError::InvalidObjectInfo`] for any of the corruption shapes
    /// that previously panicked: missing `\0` terminator, non-UTF-8 header bytes,
    /// missing type prefix, missing size, non-numeric size, or size mismatch
    /// against the decompressed payload.
    /// Enumerate loose objects with metadata for the evictor (lore.md 2.9):
    /// `(hash, path, mtime, uncompressed_size)`. The size comes from a
    /// PARTIAL zlib decode (header only, bounded) — full decompression of
    /// every large object per scan would be a real I/O cost. Non-OID files
    /// and unparseable objects are skipped (healing is fsck's job).
    pub fn list_loose_with_meta(&self) -> Vec<(ObjectHash, PathBuf, std::time::SystemTime, u64)> {
        let mut rows = Vec::new();
        let Ok(shards) = fs::read_dir(&self.base_path) else {
            return rows;
        };
        for shard in shards.flatten() {
            let shard_name = shard.file_name().to_string_lossy().into_owned();
            if shard_name.len() != 2 || !shard_name.chars().all(|c| c.is_ascii_hexdigit()) {
                continue; // pack/, info/, temp files
            }
            let Ok(entries) = fs::read_dir(shard.path()) else {
                continue;
            };
            for entry in entries.flatten() {
                let rest = entry.file_name().to_string_lossy().into_owned();
                let oid_hex = format!("{shard_name}{rest}");
                let Ok(hash) = ObjectHash::from_str(&oid_hex) else {
                    continue;
                };
                let Ok(meta) = entry.metadata() else {
                    continue;
                };
                let mtime = meta.modified().unwrap_or(std::time::UNIX_EPOCH);
                let Some(size) = Self::peek_uncompressed_size(&entry.path()) else {
                    continue;
                };
                rows.push((hash, entry.path(), mtime, size));
            }
        }
        rows
    }

    /// Partially decode a loose object's zlib stream — just enough to read
    /// the `"<type> <len>\0"` header — and return `<len>`. `None` on any
    /// parse failure (the object is then not an eviction candidate).
    pub fn peek_uncompressed_size(path: &Path) -> Option<u64> {
        use std::io::Read;
        let file = fs::File::open(path).ok()?;
        let mut decoder = flate2::read::ZlibDecoder::new(file);
        let mut header = [0u8; 64];
        let mut filled = 0usize;
        while filled < header.len() {
            match decoder.read(&mut header[filled..]) {
                Ok(0) => break,
                Ok(n) => {
                    filled += n;
                    if header[..filled].contains(&0) {
                        break;
                    }
                }
                Err(_) => return None,
            }
        }
        let nul = header[..filled].iter().position(|b| *b == 0)?;
        let text = std::str::from_utf8(&header[..nul]).ok()?;
        let (_, len) = text.split_once(' ')?;
        len.parse().ok()
    }

    fn parse_header(data: &[u8]) -> Result<(String, usize, usize), GitError> {
        let end_of_header = data
            .iter()
            .position(|&b| b == b'\0')
            .ok_or_else(|| GitError::InvalidObjectInfo("missing header terminator".to_string()))?;
        let header_str = std::str::from_utf8(&data[..end_of_header])
            .map_err(|e| GitError::InvalidObjectInfo(format!("non-UTF-8 header bytes: {e}")))?;

        let mut parts = header_str.splitn(2, ' ');
        let obj_type = parts
            .next()
            .ok_or_else(|| {
                GitError::InvalidObjectInfo("missing object type in header".to_string())
            })?
            .to_string();
        let size_str = parts.next().ok_or_else(|| {
            GitError::InvalidObjectInfo("missing object size in header".to_string())
        })?;
        let size = size_str.parse::<usize>().map_err(|e| {
            GitError::InvalidObjectInfo(format!(
                "non-numeric object size '{size_str}' in header: {e}"
            ))
        })?;
        let expected = data.len() - 1 - end_of_header;
        if size != expected {
            return Err(GitError::InvalidObjectInfo(format!(
                "object size mismatch: header says {size}, payload is {expected}"
            )));
        }
        Ok((obj_type, size, end_of_header))
    }

    // --- Pack related methods ---

    fn list_all_packs(&self) -> Vec<PathBuf> {
        let pack_dir = self.base_path.join("pack");
        if !pack_dir.exists() {
            return Vec::new();
        }
        let mut packs = Vec::new();
        let entries = match fs::read_dir(&pack_dir) {
            Ok(entries) => entries,
            Err(err) => {
                tracing::warn!(
                    pack_dir = %pack_dir.display(),
                    error = %err,
                    "failed to read pack directory, skipping"
                );
                return packs;
            }
        };
        for entry in entries {
            let path = match entry {
                Ok(entry) => entry.path(),
                Err(err) => {
                    tracing::warn!(
                        pack_dir = %pack_dir.display(),
                        error = %err,
                        "skipping unreadable pack directory entry"
                    );
                    continue;
                }
            };
            if path.is_file() && path.extension().is_some_and(|ext| ext == "pack") {
                packs.push(path);
            }
        }
        packs
    }

    fn list_all_idx(&self) -> Vec<PathBuf> {
        let packs = self.list_all_packs();
        let mut idxs = Vec::new();
        for pack in packs {
            let idx = pack.with_extension("idx");
            let want_v2 = get_hash_kind() == HashKind::Sha256;
            let needs_rebuild = if idx.exists() {
                if want_v2 {
                    !matches!(Self::read_idx_version_path(&idx), Ok(IdxVersion::V2))
                } else {
                    false
                }
            } else {
                true
            };

            if needs_rebuild {
                let (Some(pack_str), Some(idx_str)) = (pack.to_str(), idx.to_str()) else {
                    tracing::warn!(
                        pack = %pack.display(),
                        idx = %idx.display(),
                        "skipping pack with non-UTF-8 path; cannot pass to build_index"
                    );
                    continue;
                };
                let build_result = if want_v2 {
                    command::index_pack::build_index_v2(pack_str, idx_str)
                } else {
                    command::index_pack::build_index_v1(pack_str, idx_str)
                };
                if let Err(err) = build_result {
                    tracing::warn!(
                        pack = %pack.display(),
                        idx = %idx.display(),
                        error = %err,
                        "failed to (re)build pack index; skipping this pack"
                    );
                    continue;
                }
            }
            idxs.push(idx);
        }
        idxs
    }

    fn read_idx_version(file: &mut fs::File) -> Result<IdxVersion, io::Error> {
        let mut header = [0u8; 4];
        file.read_exact(&mut header)?;
        if header == IDX_MAGIC {
            let mut version_buf = [0u8; 4];
            file.read_exact(&mut version_buf)?;
            let version = u32::from_be_bytes(version_buf);
            if version != 2 {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    format!("unsupported pack index version {version}"),
                ));
            }
            Ok(IdxVersion::V2)
        } else {
            file.seek(io::SeekFrom::Start(0))?;
            Ok(IdxVersion::V1)
        }
    }

    fn read_idx_version_path(idx_file: &Path) -> Result<IdxVersion, io::Error> {
        let mut idx_file = fs::File::open(idx_file)?;
        Self::read_idx_version(&mut idx_file)
    }

    fn read_idx_fanout(idx_file: &Path) -> Result<(IdxVersion, [u32; 256]), io::Error> {
        let mut idx_file = fs::File::open(idx_file)?;
        let version = Self::read_idx_version(&mut idx_file)?;
        let fanout_offset = match version {
            IdxVersion::V1 => 0,
            IdxVersion::V2 => 8,
        };
        idx_file.seek(io::SeekFrom::Start(fanout_offset))?;
        let mut fanout: [u32; 256] = [0; 256];
        let mut buf = [0; 4];
        for slot in fanout.iter_mut() {
            idx_file.read_exact(&mut buf)?;
            *slot = u32::from_be_bytes(buf);
        }
        Ok((version, fanout))
    }

    fn read_idx(idx_file: &Path, obj_id: &ObjectHash) -> Result<Option<u64>, io::Error> {
        let (version, fanout) = Self::read_idx_fanout(idx_file)?;
        let mut idx_file = fs::File::open(idx_file)?;

        let first_byte = obj_id.as_ref()[0];
        let start = if first_byte == 0 {
            0
        } else {
            fanout[first_byte as usize - 1] as usize
        };
        let end = fanout[first_byte as usize] as usize;
        let object_count = fanout[255] as u64;
        let hash_size = get_hash_kind().size() as u64;

        match version {
            IdxVersion::V1 => {
                if hash_size != 20 {
                    return Err(io::Error::new(
                        io::ErrorKind::InvalidData,
                        "pack index v1 only supports sha1",
                    ));
                }
                idx_file.seek(io::SeekFrom::Start(FANOUT + 24 * start as u64))?;
                for _ in start..end {
                    let offset = idx_file.read_u32::<BigEndian>()?;
                    let hash = read_sha(&mut idx_file)?;

                    if &hash == obj_id {
                        return Ok(Some(offset as u64));
                    }
                }
                Ok(None)
            }
            IdxVersion::V2 => {
                let names_offset = FANOUT + 8;
                idx_file.seek(io::SeekFrom::Start(names_offset + hash_size * start as u64))?;
                let mut found_index = None;
                for i in start..end {
                    let hash = read_sha(&mut idx_file)?;
                    if &hash == obj_id {
                        found_index = Some(i as u64);
                        break;
                    }
                }
                let Some(index) = found_index else {
                    return Ok(None);
                };

                let crc_offset = names_offset + object_count * hash_size;
                let offsets_offset = crc_offset + object_count * 4;
                idx_file.seek(io::SeekFrom::Start(offsets_offset + index * 4))?;
                let offset = idx_file.read_u32::<BigEndian>()?;
                if offset & 0x8000_0000 != 0 {
                    let large_index = (offset & 0x7fff_ffff) as u64;
                    let large_offsets_offset = offsets_offset + object_count * 4;
                    idx_file.seek(io::SeekFrom::Start(large_offsets_offset + large_index * 8))?;
                    let large_offset = idx_file.read_u64::<BigEndian>()?;
                    Ok(Some(large_offset))
                } else {
                    Ok(Some(offset as u64))
                }
            }
        }
    }

    fn read_pack_obj(pack_file: &Path, offset: u64) -> Result<CacheObject, GitError> {
        let file_name = pack_file
            .file_name()
            .and_then(|n| n.to_str())
            .ok_or_else(|| {
                GitError::InvalidObjectInfo(format!(
                    "pack path has no UTF-8 file name: {}",
                    pack_file.display()
                ))
            })?
            .to_owned();
        let cache_key = format!("{:?}-{}", file_name, offset);

        // INVARIANT: PACK_OBJ_CACHE mutex poisoning would require an earlier
        // panic while holding the lock; treated as unrecoverable here.
        if let Some(cached) = PACK_OBJ_CACHE
            .lock()
            .expect("PACK_OBJ_CACHE mutex poisoned")
            .get(&cache_key)
        {
            return Ok(cached.clone());
        }

        let obj = {
            let file = fs::File::open(pack_file)?;
            let mut pack_reader = io::BufReader::new(&file);
            pack_reader.seek(io::SeekFrom::Start(offset))?;
            {
                let mut offset = offset as usize;
                Pack::decode_pack_object(&mut pack_reader, &mut offset)?
            }
        };
        let obj = obj.ok_or_else(|| {
            GitError::InvalidObjectInfo(format!(
                "Failed to decode pack object at offset {}",
                offset
            ))
        })?;
        let full_obj = match obj.object_type() {
            ObjectType::OffsetDelta => {
                // INVARIANT: obj.object_type() == OffsetDelta implies offset_delta() is Some.
                //
                // NOTE: git-internal's `offset_delta()` returns the ABSOLUTE base
                // offset (it stores `init_offset - delta_distance` internally),
                // NOT the raw OFS_DELTA distance. Use it directly as the base
                // offset — do not subtract it from `offset` again. Subtracting
                // reads from the wrong location and fails with a "corrupt deflate
                // stream" error on OFS_DELTA packs (e.g. packs fetched from
                // GitHub); libra-produced packs use REF_DELTA (the HashDelta arm
                // below) so this path was previously never exercised.
                let base_offset = obj
                    .offset_delta()
                    .expect("OffsetDelta object must have offset_delta")
                    as u64;
                let base_obj = Self::read_pack_obj(pack_file, base_offset)?;
                let base_obj = Arc::new(base_obj);
                Pack::rebuild_delta(obj, base_obj)
            }
            ObjectType::HashDelta => {
                // INVARIANT: obj.object_type() == HashDelta implies hash_delta() is Some.
                let base_hash = obj
                    .hash_delta()
                    .expect("HashDelta object must have hash_delta");
                let idx_file = pack_file.with_extension("idx");
                let base_offset = Self::read_idx(&idx_file, &base_hash)?.ok_or_else(|| {
                    GitError::InvalidObjectInfo(format!(
                        "HashDelta base {base_hash} not found in pack idx {}",
                        idx_file.display()
                    ))
                })?;
                let base_obj = Self::read_pack_obj(pack_file, base_offset)?;
                let base_obj = Arc::new(base_obj);
                Pack::rebuild_delta(obj, base_obj)
            }
            _ => obj,
        };

        if PACK_OBJ_CACHE
            .lock()
            .expect("PACK_OBJ_CACHE mutex poisoned")
            .insert(cache_key, full_obj.clone())
            .is_err()
        {
            tracing::warn!("Pack object cache: entry too large to cache");
        }
        Ok(full_obj)
    }

    fn get_from_pack(
        &self,
        obj_id: &ObjectHash,
    ) -> Result<Option<(Vec<u8>, ObjectType)>, GitError> {
        let idxes = self.list_all_idx();
        for idx in idxes {
            let res = Self::read_pack_by_idx(&idx, obj_id)?;
            if let Some(data) = res {
                return Ok(Some((data.data_decompressed.clone(), data.object_type())));
            }
        }
        Ok(None)
    }

    fn read_pack_by_idx(
        idx_file: &Path,
        obj_id: &ObjectHash,
    ) -> Result<Option<CacheObject>, GitError> {
        let pack_file = idx_file.with_extension("pack");
        let res = Self::read_idx(idx_file, obj_id)?;
        match res {
            None => Ok(None),
            Some(offset) => {
                let res = Self::read_pack_obj(&pack_file, offset)?;
                Ok(Some(res))
            }
        }
    }
}

#[async_trait]
impl Storage for LocalStorage {
    async fn get(&self, hash: &ObjectHash) -> Result<(Vec<u8>, ObjectType), GitError> {
        let self_clone = self.clone();
        let hash = *hash;

        // Use spawn_blocking for IO operations
        tokio::task::spawn_blocking(move || {
            if let Some(kind) = self_clone.hash_kind {
                set_hash_kind(kind);
            }
            // Self first (loose -> pack).
            if let Some(found) = self_clone.get_here(&hash)? {
                return Ok(found);
            }
            // lore.md 2.3: borrow from the alternate chain on a local miss.
            // Every borrowed hit is FULL-BYTE OID-verified before it is
            // returned, so a tampered/mismatched alternate can never poison a
            // read (§7.6 read-verify).
            for alt in &self_clone.alternates {
                if let Some((payload, obj_type)) = alt.get_here(&hash)? {
                    super::tiered::verify_fetched_object(&hash, obj_type, &payload)?;
                    return Ok((payload, obj_type));
                }
            }
            Err(GitError::ObjectNotFound(hash.to_string()))
        })
        .await
        .map_err(|e| GitError::IOError(io::Error::other(e)))?
    }

    async fn put(
        &self,
        hash: &ObjectHash,
        data: &[u8],
        obj_type: ObjectType,
    ) -> Result<String, GitError> {
        let self_clone = self.clone();
        let hash = *hash;
        let data = data.to_vec();

        tokio::task::spawn_blocking(move || {
            if let Some(kind) = self_clone.hash_kind {
                set_hash_kind(kind);
            }
            let path = self_clone.get_obj_path(&hash);

            let header = format!("{} {}\0", obj_type, data.len());
            let full_content = [header.as_bytes().to_vec(), data].concat();

            // Atomic loose-object write (lore.md §7.7): a crash mid-write must
            // never leave a half-written object at the final path (which fsck /
            // reconcile would then read as corrupt). fsync only when
            // `--sync-data` is requested (§0.5) — the default keeps object writes
            // fast while still crash-atomic.
            crate::utils::atomic_write::write_atomic(
                &path,
                &Self::compress_zlib(&full_content)?,
                crate::utils::atomic_write::sync_data_enabled(),
            )?;
            path.to_str().map(str::to_owned).ok_or_else(|| {
                GitError::InvalidArgument(format!(
                    "loose object path is not valid UTF-8: {}",
                    path.display()
                ))
            })
        })
        .await
        .map_err(|e| GitError::IOError(io::Error::other(e)))?
    }

    async fn exist(&self, hash: &ObjectHash) -> bool {
        let self_clone = self.clone();
        let hash = *hash;

        tokio::task::spawn_blocking(move || {
            if let Some(kind) = self_clone.hash_kind {
                set_hash_kind(kind);
            }
            let path = self_clone.get_obj_path(&hash);
            if Path::exists(&path) {
                return true;
            }
            match self_clone.get_from_pack(&hash) {
                Ok(Some(_)) => return true,
                Ok(None) => {}
                Err(err) => {
                    // exist() returns bool, so any pack-read failure is treated as "not present".
                    // Log so a corrupt pack doesn't silently cause re-fetch loops.
                    tracing::warn!(
                        hash = %hash,
                        error = %err,
                        "failed to consult pack while checking object existence; assuming missing"
                    );
                }
            }
            // lore.md 2.3: a borrowed-but-present object is NOT missing. VERIFY
            // the borrowed bytes (Codex P2): a corrupt/tampered alternate must
            // not make `exist` claim presence and cause fetch/connectivity code
            // to skip a valid object. Only a byte-verified alternate hit counts.
            self_clone.alternates.iter().any(|alt| {
                matches!(
                    alt.get_here(&hash),
                    Ok(Some((ref payload, obj_type)))
                        if super::tiered::verify_fetched_object(&hash, obj_type, payload).is_ok()
                )
            })
        })
        .await
        .unwrap_or(false)
    }

    async fn search(&self, prefix: &str) -> Vec<ObjectHash> {
        let self_clone = self.clone();
        let prefix = prefix.to_string();

        tokio::task::spawn_blocking(move || {
            if let Some(kind) = self_clone.hash_kind {
                set_hash_kind(kind);
            }
            let mut objects = Vec::new();
            // Loose objects: walk objects/AB/CDEF... directories. Skip-and-warn on any
            // filesystem hiccup so a single bad entry doesn't kill the whole search.
            if let Ok(paths) = fs::read_dir(&self_clone.base_path) {
                for entry in paths {
                    let path = match entry {
                        Ok(entry) => entry.path(),
                        Err(err) => {
                            tracing::warn!(
                                base = %self_clone.base_path.display(),
                                error = %err,
                                "skipping unreadable objects/ entry during search"
                            );
                            continue;
                        }
                    };
                    let Some(dir_name) = path
                        .file_name()
                        .and_then(|n| n.to_str())
                        .filter(|n| n.len() == 2)
                    else {
                        continue;
                    };
                    if !path.is_dir() {
                        continue;
                    }
                    if !prefix.starts_with(dir_name)
                        && !dir_name.starts_with(&prefix[..std::cmp::min(2, prefix.len())])
                    {
                        continue;
                    }

                    let parent_name = dir_name.to_string();
                    if let Ok(sub_paths) = fs::read_dir(&path) {
                        for sub_entry in sub_paths {
                            let sub_path = match sub_entry {
                                Ok(entry) => entry.path(),
                                Err(err) => {
                                    tracing::warn!(
                                        dir = %path.display(),
                                        error = %err,
                                        "skipping unreadable inner objects/ entry during search"
                                    );
                                    continue;
                                }
                            };
                            if !sub_path.is_file() {
                                continue;
                            }
                            let Some(file_name) = sub_path.file_name().and_then(|n| n.to_str())
                            else {
                                tracing::warn!(
                                    sub_path = %sub_path.display(),
                                    "skipping loose-object entry with non-UTF-8 file name"
                                );
                                continue;
                            };
                            let full_hash = format!("{parent_name}{file_name}");
                            if full_hash.starts_with(&prefix)
                                && let Ok(hash) = ObjectHash::from_str(&full_hash)
                            {
                                objects.push(hash);
                            }
                        }
                    }
                }
            }

            // Pack objects
            let idxes = self_clone.list_all_idx();
            for idx in idxes {
                if let Ok(objs) = Self::list_idx_objects(&idx) {
                    for obj in objs {
                        if obj.to_string().starts_with(&prefix) {
                            objects.push(obj);
                        }
                    }
                }
            }
            objects
        })
        .await
        .unwrap_or_default()
    }
}

impl LocalStorage {
    /// Lists all object hashes contained in a pack index file. This is used for searching objects by prefix in packs.
    fn list_idx_objects(idx_file: &Path) -> Result<Vec<ObjectHash>, io::Error> {
        let (version, fanout) = Self::read_idx_fanout(idx_file)?;
        let mut idx_file = fs::File::open(idx_file)?;
        let object_count = fanout[255] as u64;
        let hash_size = get_hash_kind().size() as u64;

        let names_offset = match version {
            IdxVersion::V1 => FANOUT,
            IdxVersion::V2 => FANOUT + 8,
        };
        idx_file.seek(io::SeekFrom::Start(names_offset))?;

        let mut objs = Vec::new();
        match version {
            IdxVersion::V1 => {
                if hash_size != 20 {
                    return Err(io::Error::new(
                        io::ErrorKind::InvalidData,
                        "pack index v1 only supports sha1",
                    ));
                }
                for _ in 0..object_count {
                    let _offset = idx_file.read_u32::<BigEndian>()?;
                    let hash = read_sha(&mut idx_file)?;
                    objs.push(hash);
                }
            }
            IdxVersion::V2 => {
                for _ in 0..object_count {
                    let hash = read_sha(&mut idx_file)?;
                    objs.push(hash);
                }
            }
        }
        Ok(objs)
    }
}

#[cfg(test)]
mod tests {
    //! Unit-test the loose-object header parser. Validates the v0.17.226
    //! `Result<_, GitError>` migration — each corruption shape that used to
    //! panic is now a `GitError::InvalidObjectInfo` with a descriptive detail.

    use super::*;

    /// Build a valid loose-object header for `(type, payload)`.
    fn header_bytes(obj_type: &str, payload: &[u8]) -> Vec<u8> {
        let mut bytes = format!("{} {}\0", obj_type, payload.len()).into_bytes();
        bytes.extend_from_slice(payload);
        bytes
    }

    #[test]
    fn parse_header_accepts_well_formed_header() {
        let data = header_bytes("blob", b"hello world");
        let (kind, size, end) = LocalStorage::parse_header(&data).expect("valid header parses");
        assert_eq!(kind, "blob");
        assert_eq!(size, b"hello world".len());
        assert_eq!(end, "blob 11".len());
    }

    #[test]
    fn parse_header_rejects_missing_terminator() {
        let err = LocalStorage::parse_header(b"blob 4abcd")
            .expect_err("missing NUL terminator should fail");
        assert!(
            matches!(&err, GitError::InvalidObjectInfo(detail) if detail.contains("missing header terminator")),
            "unexpected err: {err:?}"
        );
    }

    #[test]
    fn parse_header_rejects_missing_size_segment() {
        let mut data = b"blob\0".to_vec();
        data.extend_from_slice(b"payload");
        let err = LocalStorage::parse_header(&data).expect_err("missing size segment should fail");
        assert!(
            matches!(&err, GitError::InvalidObjectInfo(detail) if detail.contains("missing object size")),
            "unexpected err: {err:?}"
        );
    }

    #[test]
    fn parse_header_rejects_non_numeric_size() {
        let mut data = b"blob abc\0".to_vec();
        data.extend_from_slice(b"xyz");
        let err = LocalStorage::parse_header(&data).expect_err("non-numeric size should fail");
        assert!(
            matches!(&err, GitError::InvalidObjectInfo(detail) if detail.contains("non-numeric object size")),
            "unexpected err: {err:?}"
        );
    }

    #[test]
    fn parse_header_rejects_size_mismatch() {
        // Header claims size 100 but only 5 payload bytes follow.
        let mut data = b"blob 100\0".to_vec();
        data.extend_from_slice(b"short");
        let err = LocalStorage::parse_header(&data).expect_err("size mismatch should fail");
        assert!(
            matches!(&err, GitError::InvalidObjectInfo(detail) if detail.contains("object size mismatch")),
            "unexpected err: {err:?}"
        );
    }

    /// Pre-NUL header bytes that are not valid UTF-8 must surface as
    /// `InvalidObjectInfo("non-UTF-8 header bytes: …")`. v0.17.228 deferred
    /// this branch as "contrived", but `\xFF\xFF\xFF\0payload` is in fact a
    /// minimal way to exercise the path: the position-of-\0 check passes
    /// (terminator at offset 3) and the slice [0..3] is then invalid UTF-8.
    #[test]
    fn parse_header_rejects_non_utf8_header_bytes() {
        // 3 invalid-UTF-8 bytes followed by NUL terminator and a 0-length payload.
        let data = [0xFFu8, 0xFFu8, 0xFFu8, b'\0'];
        let err = LocalStorage::parse_header(&data).expect_err("non-UTF-8 header should fail");
        assert!(
            matches!(&err, GitError::InvalidObjectInfo(detail) if detail.contains("non-UTF-8 header bytes")),
            "unexpected err: {err:?}"
        );
    }

    /// `put` writes loose objects through `write_atomic` (lore.md §7.7): the
    /// object round-trips, and the shard directory holds only the final object
    /// with no leftover temp file.
    #[tokio::test]
    async fn put_writes_loose_object_atomically() {
        use git_internal::{
            hash::{HashKind, ObjectHash, set_hash_kind_for_test},
            internal::object::types::ObjectType,
        };

        let _kind = set_hash_kind_for_test(HashKind::Sha1);
        let dir = tempfile::tempdir().unwrap();
        let storage = LocalStorage::new(dir.path().to_path_buf());
        let data = b"atomic loose object".to_vec();
        let hash = ObjectHash::from_type_and_data(ObjectType::Blob, &data);

        storage
            .put(&hash, &data, ObjectType::Blob)
            .await
            .expect("put");

        let (got, obj_type) = storage.get(&hash).await.expect("get");
        assert_eq!(got, data);
        assert_eq!(obj_type, ObjectType::Blob);

        let shard = dir.path().join(&hash.to_string()[0..2]);
        let entries: Vec<_> = std::fs::read_dir(&shard)
            .unwrap()
            .map(|e| e.unwrap().file_name())
            .collect();
        assert_eq!(
            entries.len(),
            1,
            "shard should hold only the final object (no stray temp), got: {entries:?}"
        );
    }

    /// Regression test for OFS_DELTA base-offset resolution in `read_pack_obj`.
    ///
    /// git-internal's `offset_delta()` returns the ABSOLUTE base offset, so
    /// `read_pack_obj` must use it directly — it must NOT subtract it from the
    /// delta object's own offset. The buggy double-subtraction read from the
    /// wrong location and failed with "corrupt deflate stream" when reading
    /// OFS_DELTA packs (e.g. packs fetched from GitHub; libra's own packs use
    /// REF_DELTA, which is why this path was previously never exercised).
    ///
    /// Fixture `ofs-delta-sha1.pack` stores blob `1b59abc0…` as an OFS_DELTA
    /// (offset 420) against base blob `b1a36d77…` (offset 241); the buggy code
    /// would read at 420-241=179 (garbage) instead of 241.
    #[test]
    fn read_pack_obj_resolves_ofs_delta_base() {
        use std::str::FromStr;

        set_hash_kind(HashKind::Sha1);

        let dir = tempfile::tempdir().expect("tempdir");
        let pack = dir.path().join("ofs-delta-sha1.pack");
        let fixture =
            Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/data/packs/ofs-delta-sha1.pack");
        std::fs::copy(&fixture, &pack).expect("copy fixture pack");

        let idx = dir.path().join("ofs-delta-sha1.idx");
        command::index_pack::build_index_v1(pack.to_str().unwrap(), idx.to_str().unwrap())
            .expect("build v1 index for fixture");

        let ofs_delta = ObjectHash::from_str("1b59abc09609574e73330d56815f04ebb4d9bd72").unwrap();
        let obj = LocalStorage::read_pack_by_idx(&idx, &ofs_delta)
            .expect("reading the OFS_DELTA object must resolve its base offset correctly")
            .expect("object must be present in the pack");

        assert_eq!(obj.object_type(), ObjectType::Blob);
        let expected = "libra ofs-delta base line\n".repeat(400).into_bytes();
        assert_eq!(
            obj.data_decompressed, expected,
            "OFS_DELTA object must reconstruct to the correct blob contents"
        );
    }
}
