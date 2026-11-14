//! Local filesystem storage backend for Git objects.
//! This module implements the `Storage` trait for a local filesystem backend. It supports both loose objects and packed objects, allowing for efficient storage and retrieval of Git objects on disk.
//! The `LocalStorage` struct provides methods to read and write Git objects, as well as to search for objects by prefix. It handles the Git object storage format, including zlib compression for loose objects
//! and the pack file format for packed objects. The implementation also includes caching mechanisms for pack objects to improve performance when accessing packed data.
use std::{
    collections::{HashMap, HashSet},
    fs, io,
    io::{Read, Seek, Write},
    path::{Path, PathBuf},
    sync::{Arc, Mutex},
    time::{Duration, Instant},
};

use async_trait::async_trait;
use byteorder::{BigEndian, ReadBytesExt};
use flate2::{Compression, write::ZlibEncoder};
use git_internal::{
    errors::GitError,
    hash::{HashKind, ObjectHash, set_hash_kind},
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
const MAX_TYPE_PROBE_DELTA_DEPTH: usize = 128;
const MAX_TYPE_PROBE_LOOSE_HEADER: usize = 64;
const MAX_TYPE_PROBE_COMPRESSED_HEADER: u64 = 4096;
const PACK_INSTALL_WAIT: Duration = Duration::from_secs(30);
const PACK_INSTALL_POLL: Duration = Duration::from_millis(20);

#[derive(Default)]
struct TypeProbeState {
    visiting_offsets: HashSet<(PathBuf, u64)>,
    resolved_hashes: HashMap<ObjectHash, ObjectType>,
}

#[derive(Clone)]
struct PackSnapshot {
    ready_indexes: Vec<PathBuf>,
    pending_packs: Vec<PathBuf>,
    orphan_packs: Vec<PathBuf>,
}

/// Index version for pack files
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum IdxVersion {
    V1,
    V2,
}

/// Reuse one index fanout and file handle while checking a batch of objects.
struct CheckedPackIndex {
    index_path: PathBuf,
    pack_path: PathBuf,
    opened: Option<OpenedCheckedPackIndex>,
}

struct OpenedCheckedPackIndex {
    index_file: fs::File,
    pack_len: u64,
    version: IdxVersion,
    fanout: [u32; 256],
}

/// A type probe keeps one index open while resolving every remaining hash in a
/// pack, then drops the handle before visiting the next pack.
struct TypeProbePackIndex {
    file: fs::File,
    version: IdxVersion,
    fanout: [u32; 256],
}

impl TypeProbePackIndex {
    fn open(path: &Path) -> Result<Self, GitError> {
        let mut file = fs::File::open(path).map_err(GitError::IOError)?;
        let (version, fanout) =
            LocalStorage::read_idx_fanout_from_open(&mut file).map_err(GitError::IOError)?;
        Ok(Self {
            file,
            version,
            fanout,
        })
    }

    fn lookup(&mut self, hash: &ObjectHash) -> Result<Option<u64>, GitError> {
        LocalStorage::read_idx_from_open_binary(&mut self.file, self.version, &self.fanout, hash)
            .map_err(GitError::IOError)
    }
}

impl CheckedPackIndex {
    fn contains(&mut self, hash: &ObjectHash) -> Result<bool, GitError> {
        if self.opened.is_none() {
            let pack_metadata = fs::metadata(&self.pack_path).map_err(GitError::IOError)?;
            if !pack_metadata.is_file() {
                return Err(GitError::InvalidObjectInfo(format!(
                    "pack '{}' is not a file",
                    self.pack_path.display()
                )));
            }
            let mut index_file = fs::File::open(&self.index_path).map_err(GitError::IOError)?;
            let (version, fanout) = LocalStorage::read_idx_fanout_from_open(&mut index_file)
                .map_err(GitError::IOError)?;
            self.opened = Some(OpenedCheckedPackIndex {
                index_file,
                pack_len: pack_metadata.len(),
                version,
                fanout,
            });
        }
        let Some(opened) = self.opened.as_mut() else {
            return Err(GitError::InvalidObjectInfo(format!(
                "pack index '{}' could not be opened",
                self.index_path.display()
            )));
        };
        let offset = LocalStorage::read_idx_from_open_binary(
            &mut opened.index_file,
            opened.version,
            &opened.fanout,
            hash,
        )
        .map_err(GitError::IOError)?;
        let Some(offset) = offset else {
            return Ok(false);
        };
        if offset >= opened.pack_len {
            return Err(GitError::InvalidObjectInfo(format!(
                "pack index '{}' points outside its pack",
                self.index_path.display()
            )));
        }
        let mut pack_file = fs::File::open(&self.pack_path).map_err(GitError::IOError)?;
        pack_file
            .seek(io::SeekFrom::Start(offset))
            .map_err(GitError::IOError)?;
        let mut first_byte = [0u8; 1];
        pack_file
            .read_exact(&mut first_byte)
            .map_err(GitError::IOError)?;
        Ok(true)
    }
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
    /// Determine an object's type from its loose or pack header. This never
    /// materializes the body of a large blob or tree while checking a commit
    /// parent supplied by an untrusted remote.
    pub(crate) async fn object_type_bounded_probe(
        &self,
        hash: &ObjectHash,
    ) -> Result<ObjectType, GitError> {
        self.object_types_bounded_probe(&[*hash])
            .await?
            .remove(hash)
            .ok_or_else(|| GitError::ObjectNotFound(hash.to_string()))
    }

    pub(crate) async fn object_types_bounded_probe(
        &self,
        hashes: &[ObjectHash],
    ) -> Result<HashMap<ObjectHash, ObjectType>, GitError> {
        if hashes.is_empty() {
            return Ok(HashMap::new());
        }
        let storage = self.clone();
        let hashes = hashes.to_vec();
        tokio::task::spawn_blocking(move || {
            if let Some(kind) = storage.hash_kind {
                set_hash_kind(kind);
            }
            let mut result = HashMap::new();
            let mut state = TypeProbeState::default();
            let mut processed = vec![HashSet::new(); storage.alternates.len() + 1];
            let started = Instant::now();
            loop {
                let mut pending = None;
                let mut orphan = None;
                let issue = storage.object_types_batch_here(
                    &hashes,
                    &mut result,
                    &mut state,
                    &mut processed[0],
                )?;
                Self::remember_pack_issue(issue, &mut pending, &mut orphan);
                let mut seen = HashSet::new();
                let mut missing: Vec<_> = hashes
                    .iter()
                    .copied()
                    .filter(|hash| seen.insert(*hash) && !result.contains_key(hash))
                    .collect();
                for (index, alternate) in storage.alternates.iter().enumerate() {
                    if missing.is_empty() {
                        break;
                    }
                    let issue = alternate.object_types_batch_here(
                        &missing,
                        &mut result,
                        &mut state,
                        &mut processed[index + 1],
                    )?;
                    Self::remember_pack_issue(issue, &mut pending, &mut orphan);
                    missing.retain(|hash| !result.contains_key(hash));
                }
                if missing.is_empty() {
                    return Ok(result);
                }
                if let Some(issue) = pending {
                    if started.elapsed() < PACK_INSTALL_WAIT {
                        std::thread::sleep(PACK_INSTALL_POLL);
                        continue;
                    }
                    return Err(super::checked_read_error(
                        &missing[0],
                        Self::unresolved_pack_error(&issue, true),
                    ));
                }
                if let Some(issue) = orphan {
                    return Err(super::checked_read_error(
                        &missing[0],
                        Self::unresolved_pack_error(&issue, false),
                    ));
                }
                return Ok(result);
            }
        })
        .await
        .map_err(|error| GitError::IOError(io::Error::other(error)))?
    }

    fn object_types_batch_here(
        &self,
        hashes: &[ObjectHash],
        result: &mut HashMap<ObjectHash, ObjectType>,
        state: &mut TypeProbeState,
        processed: &mut HashSet<PathBuf>,
    ) -> Result<Option<PackSnapshot>, GitError> {
        let mut seen = HashSet::new();
        let mut missing = Vec::new();
        for hash in hashes {
            if !seen.insert(*hash) || result.contains_key(hash) {
                continue;
            }
            let loose = self.get_obj_path(hash);
            match fs::symlink_metadata(&loose) {
                Ok(metadata) if metadata.is_file() => {
                    let kind = Self::object_type_from_loose_header(&loose)
                        .map_err(|error| super::checked_read_error(hash, error))?;
                    result.insert(*hash, kind);
                    state.resolved_hashes.insert(*hash, kind);
                }
                Ok(_) => {
                    return Err(super::checked_read_error(
                        hash,
                        GitError::InvalidObjectInfo(format!(
                            "object path '{}' is not a file",
                            loose.display()
                        )),
                    ));
                }
                Err(error) if error.kind() == io::ErrorKind::NotFound => missing.push(*hash),
                Err(error) => {
                    return Err(super::checked_read_error(hash, GitError::IOError(error)));
                }
            }
        }
        if missing.is_empty() {
            return Ok(None);
        }

        let pack_dir = self.base_path.join("pack");
        let snapshot = Self::pack_snapshot(&pack_dir)
            .map_err(|error| super::checked_read_error(&missing[0], error))?;
        for index_path in &snapshot.ready_indexes {
            if missing.is_empty() {
                break;
            }
            if !processed.insert(index_path.clone()) {
                continue;
            }
            let pack = index_path.with_extension("pack");
            let metadata = fs::metadata(&pack).map_err(|error| {
                super::checked_read_error(&missing[0], GitError::IOError(error))
            })?;
            if !metadata.is_file() {
                return Err(super::checked_read_error(
                    &missing[0],
                    GitError::InvalidObjectInfo(format!("pack '{}' is not a file", pack.display())),
                ));
            }
            let pack_len = metadata.len();
            let mut index = TypeProbePackIndex::open(index_path)
                .map_err(|error| super::checked_read_error(&missing[0], error))?;
            let mut unresolved = Vec::new();
            for hash in missing {
                let offset = index
                    .lookup(&hash)
                    .map_err(|error| super::checked_read_error(&hash, error))?;
                let Some(offset) = offset else {
                    unresolved.push(hash);
                    continue;
                };
                if !(12..pack_len).contains(&offset) {
                    return Err(super::checked_read_error(
                        &hash,
                        GitError::InvalidObjectInfo(format!(
                            "pack index '{}' points outside its pack",
                            index_path.display()
                        )),
                    ));
                }
                let kind = Self::object_type_at_pack_offset(
                    &pack,
                    offset,
                    0,
                    state,
                    self,
                    Some(&mut index),
                )
                .map_err(|error| super::checked_read_error(&hash, error))?;
                result.insert(hash, kind);
                state.resolved_hashes.insert(hash, kind);
                state.visiting_offsets.clear();
            }
            missing = unresolved;
        }
        Ok((!missing.is_empty()).then_some(snapshot))
    }

    fn object_type_for_hash(
        &self,
        hash: &ObjectHash,
        depth: usize,
        state: &mut TypeProbeState,
    ) -> Result<Option<ObjectType>, GitError> {
        if let Some(kind) = state.resolved_hashes.get(hash) {
            return Ok(Some(*kind));
        }
        let mut processed = vec![HashSet::new(); self.alternates.len() + 1];
        let started = Instant::now();
        loop {
            let mut pending = None;
            let mut orphan = None;
            let (found, issue) = self.object_type_here(hash, depth, state, &mut processed[0])?;
            Self::remember_pack_issue(issue, &mut pending, &mut orphan);
            if let Some(kind) = found {
                state.resolved_hashes.insert(*hash, kind);
                return Ok(Some(kind));
            }
            for (index, alternate) in self.alternates.iter().enumerate() {
                let (found, issue) =
                    alternate.object_type_here(hash, depth, state, &mut processed[index + 1])?;
                Self::remember_pack_issue(issue, &mut pending, &mut orphan);
                if let Some(kind) = found {
                    state.resolved_hashes.insert(*hash, kind);
                    return Ok(Some(kind));
                }
            }
            if let Some(issue) = pending {
                if started.elapsed() < PACK_INSTALL_WAIT {
                    std::thread::sleep(PACK_INSTALL_POLL);
                    continue;
                }
                return Err(Self::unresolved_pack_error(&issue, true));
            }
            if let Some(issue) = orphan {
                return Err(Self::unresolved_pack_error(&issue, false));
            }
            return Ok(None);
        }
    }

    fn object_type_here(
        &self,
        hash: &ObjectHash,
        depth: usize,
        state: &mut TypeProbeState,
        processed: &mut HashSet<PathBuf>,
    ) -> Result<(Option<ObjectType>, Option<PackSnapshot>), GitError> {
        let loose = self.get_obj_path(hash);
        match fs::symlink_metadata(&loose) {
            Ok(metadata) if metadata.is_file() => {
                return Self::object_type_from_loose_header(&loose).map(|kind| (Some(kind), None));
            }
            Ok(_) => {
                return Err(GitError::InvalidObjectInfo(format!(
                    "object path '{}' is not a file",
                    loose.display()
                )));
            }
            Err(error) if error.kind() == io::ErrorKind::NotFound => {}
            Err(error) => return Err(GitError::IOError(error)),
        }

        let pack_dir = self.base_path.join("pack");
        let snapshot = Self::pack_snapshot(&pack_dir)?;
        for index in &snapshot.ready_indexes {
            if !processed.insert(index.clone()) {
                continue;
            }
            let pack = index.with_extension("pack");
            let pack_len = fs::metadata(&pack).map_err(GitError::IOError)?.len();
            let mut index_file = TypeProbePackIndex::open(index)?;
            let offset = index_file.lookup(hash)?;
            if let Some(offset) = offset {
                if !(12..pack_len).contains(&offset) {
                    return Err(GitError::InvalidObjectInfo(format!(
                        "pack index '{}' points outside its pack",
                        index.display()
                    )));
                }
                return Self::object_type_at_pack_offset(
                    &pack,
                    offset,
                    depth,
                    state,
                    self,
                    Some(&mut index_file),
                )
                .map(|kind| (Some(kind), None));
            }
        }
        Ok((None, Some(snapshot)))
    }

    fn object_type_from_loose_header(path: &Path) -> Result<ObjectType, GitError> {
        let file = fs::File::open(path).map_err(GitError::IOError)?;
        let mut decoder =
            flate2::read::ZlibDecoder::new(file.take(MAX_TYPE_PROBE_COMPRESSED_HEADER));
        let mut header = Vec::with_capacity(MAX_TYPE_PROBE_LOOSE_HEADER);
        loop {
            if header.len() == MAX_TYPE_PROBE_LOOSE_HEADER {
                return Err(GitError::InvalidObjectInfo(format!(
                    "loose object header at '{}' exceeds {MAX_TYPE_PROBE_LOOSE_HEADER} bytes",
                    path.display()
                )));
            }
            let mut byte = [0u8; 1];
            decoder.read_exact(&mut byte).map_err(|error| {
                GitError::InvalidObjectInfo(format!(
                    "cannot read loose object header at '{}': {error}",
                    path.display()
                ))
            })?;
            if byte[0] == 0 {
                break;
            }
            header.push(byte[0]);
        }
        let text = std::str::from_utf8(&header).map_err(|error| {
            GitError::InvalidObjectInfo(format!(
                "loose object header at '{}' is not UTF-8: {error}",
                path.display()
            ))
        })?;
        let (kind, size) = text.split_once(' ').ok_or_else(|| {
            GitError::InvalidObjectInfo(format!(
                "loose object at '{}' has an invalid header",
                path.display()
            ))
        })?;
        if kind.is_empty() || size.is_empty() || size.contains(' ') || size.parse::<u64>().is_err()
        {
            return Err(GitError::InvalidObjectInfo(format!(
                "loose object at '{}' has an invalid header",
                path.display()
            )));
        }
        ObjectType::from_string(kind).map_err(|error| {
            GitError::InvalidObjectInfo(format!(
                "loose object at '{}' has an invalid type: {error}",
                path.display()
            ))
        })
    }

    fn object_type_at_pack_offset(
        pack: &Path,
        offset: u64,
        depth: usize,
        state: &mut TypeProbeState,
        storage: &Self,
        index: Option<&mut TypeProbePackIndex>,
    ) -> Result<ObjectType, GitError> {
        if depth >= MAX_TYPE_PROBE_DELTA_DEPTH {
            return Err(GitError::InvalidObjectInfo(format!(
                "delta chain at offset {offset} in '{}' exceeds depth {MAX_TYPE_PROBE_DELTA_DEPTH}",
                pack.display()
            )));
        }
        if !state.visiting_offsets.insert((pack.to_path_buf(), offset)) {
            return Err(GitError::InvalidObjectInfo(format!(
                "delta cycle at offset {offset} in '{}'",
                pack.display()
            )));
        }
        let mut file = fs::File::open(pack).map_err(GitError::IOError)?;
        file.seek(io::SeekFrom::Start(offset))
            .map_err(GitError::IOError)?;
        let (kind, _) = Self::read_type_probe_pack_header(&mut file, pack, offset)?;
        match kind {
            1..=4 => ObjectType::from_pack_type_u8(kind),
            5 | 6 => {
                let distance = Self::read_type_probe_ofs_distance(&mut file, pack, offset)?;
                let base = offset
                    .checked_sub(distance)
                    .filter(|base| *base >= 12)
                    .ok_or_else(|| {
                        GitError::InvalidObjectInfo(format!(
                            "OFS_DELTA at offset {offset} in '{}' points before its pack",
                            pack.display()
                        ))
                    })?;
                Self::object_type_at_pack_offset(pack, base, depth + 1, state, storage, index)
            }
            7 => {
                let base = ObjectHash::from_stream_for_kind(
                    git_internal::hash::get_hash_kind(),
                    &mut file,
                )
                .map_err(|error| {
                    GitError::InvalidObjectInfo(format!(
                        "cannot read REF_DELTA base at offset {offset} in '{}': {error}",
                        pack.display()
                    ))
                })?;
                if let Some(kind) = state.resolved_hashes.get(&base) {
                    return Ok(*kind);
                }
                if let Some(index) = index
                    && let Some(base_offset) = index.lookup(&base)?
                {
                    let kind = Self::object_type_at_pack_offset(
                        pack,
                        base_offset,
                        depth + 1,
                        state,
                        storage,
                        Some(index),
                    )?;
                    state.resolved_hashes.insert(base, kind);
                    return Ok(kind);
                }
                storage
                    .object_type_for_hash(&base, depth + 1, state)?
                    .ok_or_else(|| {
                        GitError::ObjectNotFound(format!(
                            "REF_DELTA base {base} for pack '{}'",
                            pack.display()
                        ))
                    })
            }
            _ => Err(GitError::InvalidObjectInfo(format!(
                "unsupported pack object type {kind} at offset {offset} in '{}'",
                pack.display()
            ))),
        }
    }

    fn read_type_probe_pack_header(
        file: &mut fs::File,
        pack: &Path,
        offset: u64,
    ) -> Result<(u8, u64), GitError> {
        let mut byte = [0u8; 1];
        file.read_exact(&mut byte).map_err(GitError::IOError)?;
        let mut current = byte[0];
        let kind = (current >> 4) & 7;
        let mut size = u64::from(current & 15);
        let mut shift = 4u32;
        while current & 0x80 != 0 {
            file.read_exact(&mut byte).map_err(GitError::IOError)?;
            current = byte[0];
            let part = u64::from(current & 0x7f);
            if shift >= 64 || part > (u64::MAX >> shift) {
                return Err(GitError::InvalidObjectInfo(format!(
                    "pack object size at offset {offset} in '{}' exceeds u64",
                    pack.display()
                )));
            }
            size |= part << shift;
            shift += 7;
        }
        Ok((kind, size))
    }

    fn read_type_probe_ofs_distance(
        file: &mut fs::File,
        pack: &Path,
        offset: u64,
    ) -> Result<u64, GitError> {
        let mut byte = [0u8; 1];
        file.read_exact(&mut byte).map_err(GitError::IOError)?;
        let mut current = byte[0];
        let mut distance = u64::from(current & 0x7f);
        let mut count = 1;
        while current & 0x80 != 0 {
            if count >= 10 {
                return Err(GitError::InvalidObjectInfo(format!(
                    "overlong OFS_DELTA distance at offset {offset} in '{}'",
                    pack.display()
                )));
            }
            file.read_exact(&mut byte).map_err(GitError::IOError)?;
            current = byte[0];
            distance = distance
                .checked_add(1)
                .and_then(|next| next.checked_mul(128))
                .and_then(|next| next.checked_add(u64::from(current & 0x7f)))
                .ok_or_else(|| {
                    GitError::InvalidObjectInfo(format!(
                        "OFS_DELTA distance at offset {offset} in '{}' exceeds u64",
                        pack.display()
                    ))
                })?;
            count += 1;
        }
        Ok(distance)
    }

    /// Snapshot usable indexes and packs whose index may still be publishing.
    /// Callers inspect healthy stores first and defer orphan errors until they
    /// know whether any requested object is still unresolved.
    fn pack_snapshot(pack_dir: &Path) -> Result<PackSnapshot, GitError> {
        let entries = match fs::read_dir(pack_dir) {
            Ok(entries) => entries,
            Err(error) if error.kind() == io::ErrorKind::NotFound => {
                return Ok(PackSnapshot {
                    ready_indexes: Vec::new(),
                    pending_packs: Vec::new(),
                    orphan_packs: Vec::new(),
                });
            }
            Err(error) => return Err(GitError::IOError(error)),
        };
        let mut packs = HashSet::new();
        let mut indexes = HashSet::new();
        for entry in entries {
            let path = entry.map_err(GitError::IOError)?.path();
            if path
                .extension()
                .is_some_and(|extension| extension == "pack")
            {
                packs.insert(path);
            } else if path.extension().is_some_and(|extension| extension == "idx") {
                indexes.insert(path);
            }
        }
        let mut snapshot = PackSnapshot {
            ready_indexes: Vec::new(),
            pending_packs: Vec::new(),
            orphan_packs: Vec::new(),
        };
        for pack in packs {
            let index = pack.with_extension("idx");
            let has_index = indexes.remove(&index);
            if Self::pack_install_lock_held(&pack)? {
                snapshot.pending_packs.push(pack);
            } else if has_index {
                snapshot.ready_indexes.push(index);
            } else {
                snapshot.orphan_packs.push(pack);
            }
        }
        // An index without its pack is also an incomplete store. Defer the
        // error if another healthy index already satisfies the whole query.
        for index in indexes {
            snapshot.orphan_packs.push(index.with_extension("pack"));
        }
        snapshot.ready_indexes.sort();
        Ok(snapshot)
    }

    fn unresolved_pack_error(snapshot: &PackSnapshot, timed_out: bool) -> GitError {
        if timed_out && let Some(pack) = snapshot.pending_packs.first() {
            return GitError::InvalidObjectInfo(format!(
                "timed out waiting for another fetch to finish installing pack '{}'; retry after it finishes",
                pack.display()
            ));
        }
        if let Some(pack) = snapshot.orphan_packs.first() {
            return GitError::InvalidObjectInfo(format!(
                "pack '{}' has no complete index; run 'libra index-pack <pack>' to rebuild it, then retry",
                pack.display()
            ));
        }
        GitError::InvalidObjectInfo(format!(
            "{} for another fetch to finish installing pack '{}'; retry after it finishes",
            if timed_out {
                "timed out waiting"
            } else {
                "waiting"
            },
            snapshot.pending_packs.first().map_or_else(
                || "<unknown>".to_string(),
                |pack| pack.display().to_string()
            )
        ))
    }

    fn remember_pack_issue(
        issue: Option<PackSnapshot>,
        pending: &mut Option<PackSnapshot>,
        orphan: &mut Option<PackSnapshot>,
    ) {
        if let Some(issue) = issue {
            if pending.is_none() && !issue.pending_packs.is_empty() {
                *pending = Some(issue.clone());
            }
            if orphan.is_none() && !issue.orphan_packs.is_empty() {
                *orphan = Some(issue);
            }
        }
    }

    #[cfg(unix)]
    fn pack_install_lock_held(pack: &Path) -> Result<bool, GitError> {
        use std::os::fd::AsRawFd;

        let lock_path = pack.with_extension("install.lock");
        let file = match fs::File::open(&lock_path) {
            Ok(file) => file,
            Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(false),
            Err(error) => return Err(GitError::IOError(error)),
        };
        // SAFETY: flock uses the live descriptor owned by `file`; dropping it
        // releases an uncontended probe lock immediately.
        if unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) } == 0 {
            return Ok(false);
        }
        let error = io::Error::last_os_error();
        match error.raw_os_error() {
            Some(code) if code == libc::EWOULDBLOCK || code == libc::EAGAIN => Ok(true),
            _ => Err(GitError::IOError(error)),
        }
    }

    #[cfg(windows)]
    fn pack_install_lock_held(pack: &Path) -> Result<bool, GitError> {
        use std::os::windows::fs::OpenOptionsExt;

        let lock_path = pack.with_extension("install.lock");
        match fs::OpenOptions::new()
            .read(true)
            .share_mode(0)
            .open(lock_path)
        {
            Ok(_) => Ok(false),
            Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(false),
            Err(error) if matches!(error.raw_os_error(), Some(32 | 33)) => Ok(true),
            Err(error) => Err(GitError::IOError(error)),
        }
    }

    #[cfg(all(not(unix), not(windows)))]
    fn pack_install_lock_held(_pack: &Path) -> Result<bool, GitError> {
        Err(GitError::IOError(io::Error::new(
            io::ErrorKind::Unsupported,
            "cross-process pack installation locking is unsupported on this platform",
        )))
    }

    fn checked_loose_present(&self, hash: &ObjectHash) -> Result<bool, GitError> {
        let loose = self.get_obj_path(hash);
        match fs::symlink_metadata(&loose) {
            Ok(metadata) if metadata.is_file() => {
                fs::File::open(&loose).map_err(GitError::IOError)?;
                Ok(true)
            }
            Ok(_) => Err(GitError::InvalidObjectInfo(format!(
                "object path '{}' is not a file",
                loose.display()
            ))),
            Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(false),
            Err(error) => Err(GitError::IOError(error)),
        }
    }

    /// Presence probe that preserves filesystem and pack-index failures.
    fn exist_checked_here(&self, hash: &ObjectHash) -> Result<bool, GitError> {
        if self.checked_loose_present(hash)? {
            return Ok(true);
        }

        let pack_dir = self.base_path.join("pack");
        let entries = match fs::read_dir(&pack_dir) {
            Ok(entries) => entries,
            Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(false),
            Err(error) => return Err(GitError::IOError(error)),
        };
        let mut packs = HashSet::new();
        let mut indexes = Vec::new();
        for entry in entries {
            let entry = entry.map_err(GitError::IOError)?;
            let path = entry.path();
            if path
                .extension()
                .is_some_and(|extension| extension == "pack")
            {
                packs.insert(path);
            } else if path.extension().is_some_and(|extension| extension == "idx") {
                indexes.push(path);
            }
        }
        let indexed_packs: HashSet<_> = indexes
            .iter()
            .map(|index| index.with_extension("pack"))
            .collect();
        for pack in &packs {
            if !indexed_packs.contains(pack) {
                return Err(GitError::InvalidObjectInfo(format!(
                    "pack '{}' has no complete index; run 'libra index-pack <pack>' to rebuild it, then retry",
                    pack.display()
                )));
            }
        }
        for index in indexes {
            let pack = index.with_extension("pack");
            let pack_metadata = fs::metadata(&pack).map_err(GitError::IOError)?;
            if !pack_metadata.is_file() {
                return Err(GitError::InvalidObjectInfo(format!(
                    "pack '{}' is not a file",
                    pack.display()
                )));
            }
            let Some(offset) = Self::read_idx(&index, hash).map_err(GitError::IOError)? else {
                continue;
            };
            if offset >= pack_metadata.len() {
                return Err(GitError::InvalidObjectInfo(format!(
                    "pack index '{}' points outside its pack",
                    index.display()
                )));
            }
            let mut file = fs::File::open(&pack).map_err(GitError::IOError)?;
            file.seek(io::SeekFrom::Start(offset))
                .map_err(GitError::IOError)?;
            let mut first_byte = [0u8; 1];
            file.read_exact(&mut first_byte)
                .map_err(GitError::IOError)?;
            return Ok(true);
        }
        Ok(false)
    }

    /// Probe each store's loose files and pack directory once for a batch.
    /// Visit each index once, keeping only that index's file open while checking
    /// the unresolved hashes. This bounds file descriptors even with many packs.
    fn exist_checked_batch_here(
        &self,
        hashes: &[ObjectHash],
        processed: &mut HashSet<PathBuf>,
    ) -> Result<(HashMap<ObjectHash, bool>, Option<PackSnapshot>), GitError> {
        let mut results = HashMap::with_capacity(hashes.len());
        let mut seen = HashSet::with_capacity(hashes.len());
        let mut missing = Vec::new();
        for hash in hashes {
            if !seen.insert(*hash) {
                continue;
            }
            if self
                .checked_loose_present(hash)
                .map_err(|error| super::checked_probe_error(hash, error))?
            {
                results.insert(*hash, true);
            } else {
                missing.push(*hash);
            }
        }
        if missing.is_empty() {
            return Ok((results, None));
        }

        let pack_dir = self.base_path.join("pack");
        let snapshot = Self::pack_snapshot(&pack_dir)
            .map_err(|error| super::checked_probe_error(&missing[0], error))?;
        for index_path in &snapshot.ready_indexes {
            if missing.is_empty() {
                break;
            }
            if !processed.insert(index_path.clone()) {
                continue;
            }
            let mut index = CheckedPackIndex {
                pack_path: index_path.with_extension("pack"),
                index_path: index_path.clone(),
                opened: None,
            };
            let mut unresolved = Vec::new();
            for hash in missing {
                if index
                    .contains(&hash)
                    .map_err(|error| super::checked_probe_error(&hash, error))?
                {
                    results.insert(hash, true);
                } else {
                    unresolved.push(hash);
                }
            }
            missing = unresolved;
        }
        let issue = (!missing.is_empty()).then_some(snapshot);
        for hash in missing {
            results.insert(hash, false);
        }
        Ok((results, issue))
    }

    pub fn try_new(base_path: PathBuf) -> io::Result<Self> {
        fs::create_dir_all(&base_path).map_err(|err| {
            io::Error::new(
                err.kind(),
                format!(
                    "LocalStorage::try_new({}): create_dir_all failed: {err}",
                    base_path.display()
                ),
            )
        })?;
        Ok(Self {
            base_path,
            hash_kind: Some(git_internal::hash::get_hash_kind()),
            alternates: Vec::new(),
        })
    }

    /// Open an existing object dir WITHOUT creating it (lore.md 2.3): an
    /// alternate base may be missing or read-only, and auto-creating it would
    /// mask a dangling alternate. No alternates of its own (the chain is
    /// pre-flattened by the caller).
    pub(crate) fn open_no_create(base_path: PathBuf) -> Self {
        Self {
            base_path,
            hash_kind: Some(git_internal::hash::get_hash_kind()),
            alternates: Vec::new(),
        }
    }

    /// Build a store whose read path also consults the repo's alternate chain
    /// (`objects/info/alternates`, transitive). Used by `ClientStorage::try_init`.
    pub fn try_new_with_alternates(base_path: PathBuf) -> io::Result<Self> {
        let mut store = Self::try_new(base_path.clone())?;
        store.alternates = crate::internal::alternates::resolve_chain(&base_path)
            .into_iter()
            .map(|dir| std::sync::Arc::new(Self::open_no_create(dir)))
            .collect();
        Ok(store)
    }

    /// Like [`Self::new_with_alternates`], but never creates directories (WIO-03
    /// read-only worker path).
    pub(crate) fn open_no_create_with_alternates(base_path: PathBuf) -> Self {
        let mut store = Self::open_no_create(base_path.clone());
        store.alternates = crate::internal::alternates::resolve_chain(&base_path)
            .into_iter()
            .map(|dir| std::sync::Arc::new(Self::open_no_create(dir)))
            .collect();
        store
    }

    /// Read an object's bytes from THIS store only (loose→pack), no alternates.
    fn get_here(&self, hash: &ObjectHash) -> Result<Option<(Vec<u8>, ObjectType)>, GitError> {
        self.get_here_with_limit(hash, None)
    }

    fn get_here_with_limit(
        &self,
        hash: &ObjectHash,
        max_load_cost: Option<u64>,
    ) -> Result<Option<(Vec<u8>, ObjectType)>, GitError> {
        if self.exist_loosely(hash) {
            super::load_cost::read_loose(&self.get_obj_path(hash), max_load_cost).map(Some)
        } else {
            if let Some(limit) = max_load_cost {
                let Some(cost) = self.object_sizes_here(&[*hash])?[0] else {
                    return Ok(None);
                };
                if cost > limit {
                    return Err(GitError::InvalidObjectInfo(format!(
                        "packed object {hash} has load cost {cost} bytes, which exceeds preview limit of {limit} bytes"
                    )));
                }
                return self.get_from_existing_indexed_pack_uncached(hash);
            }
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
                let Ok(hash) = crate::internal::object_format::parse_repo_oid(&oid_hex) else {
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

    #[cfg(test)]
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

    /// Loose and pack prefix search. A missing objects or pack directory is
    /// an empty contribution. Permission errors, unreadable entries, and a
    /// pack index that cannot be read are returned.
    fn search_checked_blocking(&self, prefix: &str) -> Result<Vec<ObjectHash>, io::Error> {
        if let Some(kind) = self.hash_kind {
            set_hash_kind(kind);
        }
        let mut objects = Vec::new();
        self.collect_loose_prefix(&mut objects, prefix)?;
        self.collect_pack_prefix(&mut objects, prefix)?;
        objects.sort_unstable();
        objects.dedup();
        Ok(objects)
    }

    fn collect_loose_prefix(
        &self,
        objects: &mut Vec<ObjectHash>,
        prefix: &str,
    ) -> Result<(), io::Error> {
        let entries = match fs::read_dir(&self.base_path) {
            Ok(entries) => entries,
            Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(()),
            Err(error) => return Err(error),
        };
        for entry in entries {
            let path = entry?.path();
            let Some(dir_name) = path
                .file_name()
                .and_then(|name| name.to_str())
                .filter(|name| name.len() == 2)
            else {
                continue;
            };
            let metadata = match fs::symlink_metadata(&path) {
                Ok(metadata) => metadata,
                Err(error) if error.kind() == io::ErrorKind::NotFound => continue,
                Err(error) => return Err(error),
            };
            if !metadata.is_dir() {
                continue;
            }
            if !prefix.starts_with(dir_name)
                && !dir_name.starts_with(&prefix[..std::cmp::min(2, prefix.len())])
            {
                continue;
            }
            let nested = match fs::read_dir(&path) {
                Ok(nested) => nested,
                Err(error) if error.kind() == io::ErrorKind::NotFound => continue,
                Err(error) => return Err(error),
            };
            let parent_name = dir_name.to_string();
            for nested in nested {
                let nested_path = nested?.path();
                let metadata = match fs::symlink_metadata(&nested_path) {
                    Ok(metadata) => metadata,
                    Err(error) if error.kind() == io::ErrorKind::NotFound => continue,
                    Err(error) => return Err(error),
                };
                if !metadata.is_file() {
                    continue;
                }
                let Some(file_name) = nested_path.file_name().and_then(|name| name.to_str()) else {
                    tracing::warn!(
                        path = %nested_path.display(),
                        "skipping loose-object entry with non-UTF-8 file name"
                    );
                    continue;
                };
                let full_hash = format!("{parent_name}{file_name}");
                if full_hash.starts_with(prefix)
                    && let Ok(hash) = crate::internal::object_format::parse_repo_oid(&full_hash)
                {
                    objects.push(hash);
                }
            }
        }
        Ok(())
    }

    fn collect_pack_prefix(
        &self,
        objects: &mut Vec<ObjectHash>,
        prefix: &str,
    ) -> Result<(), io::Error> {
        for index in self.list_indexes_checked()? {
            for hash in Self::list_idx_objects(&index)? {
                if hash.to_string().starts_with(prefix) {
                    objects.push(hash);
                }
            }
        }
        Ok(())
    }

    fn list_packs_checked(&self) -> Result<Vec<PathBuf>, io::Error> {
        let pack_dir = self.base_path.join("pack");
        let entries = match fs::read_dir(&pack_dir) {
            Ok(entries) => entries,
            Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(Vec::new()),
            Err(error) => return Err(error),
        };
        let mut packs = Vec::new();
        for entry in entries {
            let path = entry?.path();
            let metadata = match fs::symlink_metadata(&path) {
                Ok(metadata) => metadata,
                Err(error) if error.kind() == io::ErrorKind::NotFound => continue,
                Err(error) => return Err(error),
            };
            if metadata.is_file() && path.extension().is_some_and(|ext| ext == "pack") {
                packs.push(path);
            }
        }
        Ok(packs)
    }

    fn list_indexes_checked(&self) -> Result<Vec<PathBuf>, io::Error> {
        let mut indexes = Vec::new();
        for pack in self.list_packs_checked()? {
            let index = pack.with_extension("idx");
            let (Some(pack_str), Some(index_str)) = (pack.to_str(), index.to_str()) else {
                tracing::warn!(
                    pack = %pack.display(),
                    index = %index.display(),
                    "skipping pack with non-UTF-8 path"
                );
                continue;
            };
            let want_v2 = crate::internal::object_format::pack_index_is_v2(
                git_internal::hash::get_hash_kind(),
            );
            let needs_rebuild = if index.exists() {
                if want_v2 {
                    !matches!(Self::read_idx_version_path(&index), Ok(IdxVersion::V2))
                } else {
                    false
                }
            } else {
                true
            };
            if needs_rebuild {
                let built = if want_v2 {
                    command::index_pack::build_index_v2(pack_str, index_str)
                } else {
                    command::index_pack::build_index_v1(pack_str, index_str)
                };
                built.map_err(|error| {
                    io::Error::other(format!(
                        "failed to read pack index '{}': {error}",
                        index.display()
                    ))
                })?;
            }
            indexes.push(index);
        }
        Ok(indexes)
    }

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
            let want_v2 = crate::internal::object_format::pack_index_is_v2(
                git_internal::hash::get_hash_kind(),
            );
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

    /// Read-only object count of a pack index, from its fanout tail. Used by
    /// `count-objects -v`; does not load the name table.
    pub(crate) fn pack_index_object_count(idx_file: &Path) -> Result<u64, io::Error> {
        let mut idx_file = fs::File::open(idx_file)?;
        let (_version, fanout) = Self::read_idx_fanout_from_open(&mut idx_file)?;
        Ok(u64::from(fanout[255]))
    }

    /// Read-only membership probe against a pack index via the existing binary
    /// search. Used by `count-objects -v` prune-packable.
    pub(crate) fn pack_index_contains(
        idx_file: &Path,
        obj_id: &ObjectHash,
    ) -> Result<bool, io::Error> {
        let mut idx_file = fs::File::open(idx_file)?;
        let (version, fanout) = Self::read_idx_fanout_from_open(&mut idx_file)?;
        Ok(Self::read_idx_from_open_binary(&mut idx_file, version, &fanout, obj_id)?.is_some())
    }

    fn read_idx_version(file: &mut (impl Read + Seek)) -> Result<IdxVersion, io::Error> {
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
        Self::read_idx_fanout_from_open(&mut idx_file)
    }

    fn read_idx_fanout_from_open(
        idx_file: &mut (impl Read + Seek),
    ) -> Result<(IdxVersion, [u32; 256]), io::Error> {
        let version = Self::read_idx_version(idx_file)?;
        let fanout_offset = match version {
            IdxVersion::V1 => 0,
            IdxVersion::V2 => 8,
        };
        idx_file.seek(io::SeekFrom::Start(fanout_offset))?;
        let mut fanout: [u32; 256] = [0; 256];
        let mut bytes = [0; FANOUT as usize];
        idx_file.read_exact(&mut bytes)?;
        for (slot, chunk) in fanout.iter_mut().zip(bytes.as_chunks::<4>().0.iter()) {
            *slot = u32::from_be_bytes([chunk[0], chunk[1], chunk[2], chunk[3]]);
        }
        if fanout.windows(2).any(|pair| pair[0] > pair[1]) {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "pack index fanout counts are not monotonic",
            ));
        }
        Ok((version, fanout))
    }

    fn read_idx(idx_file: &Path, obj_id: &ObjectHash) -> Result<Option<u64>, io::Error> {
        let mut idx_file = fs::File::open(idx_file)?;
        Self::read_idx_from_reader(&mut idx_file, obj_id)
    }

    fn read_idx_from_reader(
        idx_file: &mut (impl Read + Seek),
        obj_id: &ObjectHash,
    ) -> Result<Option<u64>, io::Error> {
        let (version, fanout) = Self::read_idx_fanout_from_open(idx_file)?;
        let count = u64::from(fanout[255]);
        let hash_size = git_internal::hash::get_hash_kind().size() as u64;
        let minimum_len = match version {
            IdxVersion::V1 => FANOUT + count * 24,
            IdxVersion::V2 => FANOUT + 8 + count * (hash_size + 8),
        };
        if idx_file.seek(io::SeekFrom::End(0))? < minimum_len {
            return Err(io::Error::new(
                io::ErrorKind::UnexpectedEof,
                "pack index object tables are truncated",
            ));
        }
        Self::read_idx_from_open_binary(idx_file, version, &fanout, obj_id)
    }

    /// Pack index names are sorted within each fanout bucket.
    fn read_idx_from_open_binary(
        idx_file: &mut (impl Read + Seek),
        version: IdxVersion,
        fanout: &[u32; 256],
        obj_id: &ObjectHash,
    ) -> Result<Option<u64>, io::Error> {
        let first_byte = usize::from(obj_id.as_ref()[0]);
        let mut low = if first_byte == 0 {
            0
        } else {
            u64::from(fanout[first_byte - 1])
        };
        let mut high = u64::from(fanout[first_byte]);
        let object_count = u64::from(fanout[255]);
        let hash_size = git_internal::hash::get_hash_kind().size() as u64;
        if version == IdxVersion::V1 && hash_size != 20 {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "pack index v1 only supports sha1",
            ));
        }

        let names_offset = match version {
            IdxVersion::V1 => FANOUT,
            IdxVersion::V2 => FANOUT + 8,
        };
        let mut found_index = None;
        while low < high {
            let index = low + (high - low) / 2;
            let name_position = match version {
                IdxVersion::V1 => names_offset + index * 24 + 4,
                IdxVersion::V2 => names_offset + index * hash_size,
            };
            idx_file.seek(io::SeekFrom::Start(name_position))?;
            let candidate = read_sha(&mut *idx_file)?;
            match candidate.as_ref().cmp(obj_id.as_ref()) {
                std::cmp::Ordering::Less => low = index + 1,
                std::cmp::Ordering::Greater => high = index,
                std::cmp::Ordering::Equal => {
                    found_index = Some(index);
                    break;
                }
            }
        }
        let Some(index) = found_index else {
            return Ok(None);
        };

        let offset_position = match version {
            IdxVersion::V1 => names_offset + index * 24,
            IdxVersion::V2 => {
                names_offset + object_count * hash_size + object_count * 4 + index * 4
            }
        };
        idx_file.seek(io::SeekFrom::Start(offset_position))?;
        let offset = idx_file.read_u32::<BigEndian>()?;
        if version == IdxVersion::V2 && offset & 0x8000_0000 != 0 {
            let large_index = u64::from(offset & 0x7fff_ffff);
            let large_offsets_offset = names_offset + object_count * hash_size + object_count * 8;
            let large_position = large_offsets_offset + large_index * 8;
            let end = idx_file.seek(io::SeekFrom::End(0))?;
            if large_index >= object_count || large_position + 8 + hash_size * 2 > end {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    "pack index large offset points outside its table",
                ));
            }
            idx_file.seek(io::SeekFrom::Start(large_position))?;
            Ok(Some(idx_file.read_u64::<BigEndian>()?))
        } else {
            Ok(Some(u64::from(offset)))
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
            _ => Ok(obj),
        }?;

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

    /// Decode one packed object without consulting or populating the process-wide
    /// pack cache. Bounded preview reads use this path so their preflighted peak
    /// remains the actual retained/transient payload bound.
    fn read_pack_obj_uncached(pack_file: &Path, offset: u64) -> Result<CacheObject, GitError> {
        let object = {
            let file = fs::File::open(pack_file)?;
            let mut reader = io::BufReader::new(&file);
            reader.seek(io::SeekFrom::Start(offset))?;
            let mut decoded_offset = offset as usize;
            Pack::decode_pack_object(&mut reader, &mut decoded_offset)?
        }
        .ok_or_else(|| {
            GitError::InvalidObjectInfo(format!("Failed to decode pack object at offset {offset}"))
        })?;

        match object.object_type() {
            ObjectType::OffsetDelta => {
                let base_offset = object.offset_delta().ok_or_else(|| {
                    GitError::InvalidObjectInfo(format!(
                        "OffsetDelta object at offset {offset} has no base offset"
                    ))
                })? as u64;
                let base = Arc::new(Self::read_pack_obj_uncached(pack_file, base_offset)?);
                Pack::rebuild_delta(object, base)
            }
            ObjectType::HashDelta => {
                let base_hash = object.hash_delta().ok_or_else(|| {
                    GitError::InvalidObjectInfo(format!(
                        "HashDelta object at offset {offset} has no base hash"
                    ))
                })?;
                let index = pack_file.with_extension("idx");
                let base_offset = Self::read_idx(&index, &base_hash)?.ok_or_else(|| {
                    GitError::InvalidObjectInfo(format!(
                        "HashDelta base {base_hash} not found in pack idx {}",
                        index.display()
                    ))
                })?;
                let base = Arc::new(Self::read_pack_obj_uncached(pack_file, base_offset)?);
                Pack::rebuild_delta(object, base)
            }
            _ => Ok(object),
        }
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

    /// Read from existing pack indexes without creating indexes or touching the
    /// process-wide pack cache.
    fn get_from_existing_indexed_pack_uncached(
        &self,
        obj_id: &ObjectHash,
    ) -> Result<Option<(Vec<u8>, ObjectType)>, GitError> {
        let mut packs = self.list_all_packs();
        packs.sort();
        for pack in packs {
            let index = pack.with_extension("idx");
            if !index.is_file() {
                continue;
            }
            let Some(offset) = Self::read_idx(&index, obj_id)? else {
                continue;
            };
            let mut object = Self::read_pack_obj_uncached(&pack, offset)?;
            let object_type = object.object_type();
            let payload = std::mem::take(&mut object.data_decompressed);
            return Ok(Some((payload, object_type)));
        }
        Ok(None)
    }

    fn object_sizes_here(&self, hashes: &[ObjectHash]) -> Result<Vec<Option<u64>>, GitError> {
        self.object_sizes_here_with_limit(hashes, None)
    }

    fn object_sizes_here_with_limit(
        &self,
        hashes: &[ObjectHash],
        aggregate_limit: Option<u64>,
    ) -> Result<Vec<Option<u64>>, GitError> {
        let mut sizes = vec![None; hashes.len()];
        let mut packed_hashes = Vec::new();
        let mut packed_positions = Vec::new();
        let mut aggregate_cost = 0u64;
        for (position, hash) in hashes.iter().enumerate() {
            let loose = self.get_obj_path(hash);
            if loose.exists() {
                let cost = super::load_cost::loose_cost(&loose)?;
                aggregate_cost = aggregate_cost
                    .checked_add(crate::utils::preview_object::charged_bytes(cost))
                    .ok_or_else(|| {
                        GitError::InvalidObjectInfo(
                            "preview aggregate cache load cost exceeds u64".to_string(),
                        )
                    })?;
                if let Some(limit) = aggregate_limit
                    && aggregate_cost > limit
                {
                    return Err(GitError::InvalidObjectInfo(format!(
                        "preview aggregate cache load cost exceeds {limit} bytes"
                    )));
                }
                sizes[position] = Some(cost);
            } else {
                packed_hashes.push(*hash);
                packed_positions.push(position);
            }
        }
        if !packed_hashes.is_empty() {
            // Read-only sizing must not rebuild a missing/incompatible index.
            let pack_dir = self.base_path.join("pack");
            let packed = match aggregate_limit {
                Some(limit) => super::load_cost::pack_costs_with_limit(
                    &pack_dir,
                    &packed_hashes,
                    limit.saturating_sub(aggregate_cost),
                )?,
                None => super::load_cost::pack_costs(&pack_dir, &packed_hashes)?,
            };
            for (position, cost) in packed_positions.into_iter().zip(packed) {
                sizes[position] = cost;
            }
        }
        Ok(sizes)
    }

    fn decoded_object_sizes_here(
        &self,
        hashes: &[ObjectHash],
    ) -> Result<Vec<Option<u64>>, GitError> {
        let mut sizes = vec![None; hashes.len()];
        let mut packed_hashes = Vec::new();
        let mut packed_positions = Vec::new();
        for (position, hash) in hashes.iter().enumerate() {
            let loose = self.get_obj_path(hash);
            if loose.exists() {
                sizes[position] = super::load_cost::loose_decoded_size(&loose)?;
            } else {
                packed_hashes.push(*hash);
                packed_positions.push(position);
            }
        }
        if !packed_hashes.is_empty() {
            // Read-only sizing must not rebuild a missing/incompatible index.
            let pack_dir = self.base_path.join("pack");
            let packed = super::load_cost::pack_decoded_sizes(&pack_dir, &packed_hashes)?;
            for (position, size) in packed_positions.into_iter().zip(packed) {
                sizes[position] = size;
            }
        }
        Ok(sizes)
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
    async fn object_type_bounded_probe(&self, hash: &ObjectHash) -> Result<ObjectType, GitError> {
        LocalStorage::object_type_bounded_probe(self, hash).await
    }

    async fn object_types_bounded_probe(
        &self,
        hashes: &[ObjectHash],
    ) -> Result<HashMap<ObjectHash, ObjectType>, GitError> {
        LocalStorage::object_types_bounded_probe(self, hashes).await
    }

    async fn object_types_bounded_probe_with_budget(
        &self,
        hashes: &[ObjectHash],
        _remaining_remote_bytes: u64,
    ) -> Result<(HashMap<ObjectHash, ObjectType>, u64), GitError> {
        let kinds = LocalStorage::object_types_bounded_probe(self, hashes).await?;
        Ok((kinds, 0))
    }

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

    async fn get_with_limit(
        &self,
        hash: &ObjectHash,
        limit: u64,
    ) -> Result<(Vec<u8>, ObjectType), GitError> {
        let self_clone = self.clone();
        let hash = *hash;
        tokio::task::spawn_blocking(move || {
            if let Some(kind) = self_clone.hash_kind {
                set_hash_kind(kind);
            }
            if let Some(found) = self_clone.get_here_with_limit(&hash, Some(limit))? {
                return Ok(found);
            }
            for alternate in &self_clone.alternates {
                if let Some((payload, obj_type)) =
                    alternate.get_here_with_limit(&hash, Some(limit))?
                {
                    super::tiered::verify_fetched_object(&hash, obj_type, &payload)?;
                    return Ok((payload, obj_type));
                }
            }
            Err(GitError::ObjectNotFound(hash.to_string()))
        })
        .await
        .map_err(|error| GitError::IOError(io::Error::other(error)))?
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

    async fn exist_checked(&self, hash: &ObjectHash) -> Result<bool, GitError> {
        let self_clone = self.clone();
        let hash = *hash;
        tokio::task::spawn_blocking(move || {
            if let Some(kind) = self_clone.hash_kind {
                set_hash_kind(kind);
            }
            if self_clone.exist_checked_here(&hash)? {
                return Ok(true);
            }
            for alternate in &self_clone.alternates {
                if !alternate.exist_checked_here(&hash)? {
                    continue;
                }
                // Borrowed hits must retain the existing full-byte OID check.
                // The limit prevents an advertised commit parent from forcing
                // an unbounded alternate object read.
                const MAX_ALTERNATE_PROBE_BYTES: u64 = 64 * 1024 * 1024;
                let (payload, object_type) = alternate
                    .get_here_with_limit(&hash, Some(MAX_ALTERNATE_PROBE_BYTES))?
                    .ok_or_else(|| {
                        GitError::ObjectNotFound(format!(
                            "alternate object {hash} disappeared during checked probe"
                        ))
                    })?;
                super::tiered::verify_fetched_object(&hash, object_type, &payload)?;
                return Ok(true);
            }
            Ok(false)
        })
        .await
        .map_err(|error| GitError::IOError(io::Error::other(error)))?
    }

    async fn exist_checked_batch(
        &self,
        hashes: &[ObjectHash],
    ) -> Result<HashMap<ObjectHash, bool>, GitError> {
        if hashes.is_empty() {
            return Ok(HashMap::new());
        }
        let self_clone = self.clone();
        let hashes = hashes.to_vec();
        tokio::task::spawn_blocking(move || {
            if let Some(kind) = self_clone.hash_kind {
                set_hash_kind(kind);
            }
            let mut results = HashMap::new();
            let mut processed = vec![HashSet::new(); self_clone.alternates.len() + 1];
            let started = Instant::now();
            loop {
                let mut pending = None;
                let mut orphan = None;
                let mut seen = HashSet::with_capacity(hashes.len());
                let mut unresolved: Vec<_> = hashes
                    .iter()
                    .copied()
                    .filter(|hash| seen.insert(*hash) && results.get(hash) != Some(&true))
                    .collect();
                let (primary_hits, issue) =
                    self_clone.exist_checked_batch_here(&unresolved, &mut processed[0])?;
                Self::remember_pack_issue(issue, &mut pending, &mut orphan);
                results.extend(primary_hits);
                unresolved.retain(|hash| results.get(hash) != Some(&true));
                for (index, alternate) in self_clone.alternates.iter().enumerate() {
                    if unresolved.is_empty() {
                        break;
                    }
                    let (alternate_hits, issue) = alternate
                        .exist_checked_batch_here(&unresolved, &mut processed[index + 1])?;
                    Self::remember_pack_issue(issue, &mut pending, &mut orphan);
                    let mut still_missing = Vec::new();
                    for hash in unresolved {
                        let present = alternate_hits.get(&hash).copied().ok_or_else(|| {
                            GitError::InvalidObjectInfo(format!(
                                "alternate checked probe omitted object {hash}"
                            ))
                        })?;
                        if !present {
                            still_missing.push(hash);
                            continue;
                        }
                        // Borrowed hits retain bounded, full-byte OID verification.
                        const MAX_ALTERNATE_PROBE_BYTES: u64 = 64 * 1024 * 1024;
                        let (payload, object_type) = alternate
                            .get_here_with_limit(&hash, Some(MAX_ALTERNATE_PROBE_BYTES))
                            .map_err(|error| super::checked_probe_error(&hash, error))?
                            .ok_or_else(|| {
                                GitError::ObjectNotFound(format!(
                                    "alternate object {hash} disappeared during checked probe"
                                ))
                            })?;
                        super::tiered::verify_fetched_object(&hash, object_type, &payload)
                            .map_err(|error| super::checked_probe_error(&hash, error))?;
                        results.insert(hash, true);
                    }
                    unresolved = still_missing;
                }
                if unresolved.is_empty() {
                    return Ok(results);
                }
                if let Some(issue) = pending {
                    if started.elapsed() < PACK_INSTALL_WAIT {
                        std::thread::sleep(PACK_INSTALL_POLL);
                        continue;
                    }
                    return Err(super::checked_probe_error(
                        &unresolved[0],
                        Self::unresolved_pack_error(&issue, true),
                    ));
                }
                if let Some(issue) = orphan {
                    return Err(super::checked_probe_error(
                        &unresolved[0],
                        Self::unresolved_pack_error(&issue, false),
                    ));
                }
                return Ok(results);
            }
        })
        .await
        .map_err(|error| GitError::IOError(io::Error::other(error)))?
    }

    async fn object_size(&self, hash: &ObjectHash) -> Result<Option<u64>, GitError> {
        let self_clone = self.clone();
        let hash = *hash;
        tokio::task::spawn_blocking(move || {
            if let Some(kind) = self_clone.hash_kind {
                set_hash_kind(kind);
            }
            let mut size = self_clone.object_sizes_here(&[hash])?[0];
            for alternate in &self_clone.alternates {
                if size.is_none() {
                    size = alternate.object_sizes_here(&[hash])?[0];
                }
            }
            Ok(size)
        })
        .await
        .map_err(|error| GitError::IOError(io::Error::other(error)))?
    }

    async fn object_sizes(&self, hashes: &[ObjectHash]) -> Result<Vec<Option<u64>>, GitError> {
        let self_clone = self.clone();
        let hashes = hashes.to_vec();
        tokio::task::spawn_blocking(move || {
            if let Some(kind) = self_clone.hash_kind {
                set_hash_kind(kind);
            }
            let mut sizes = self_clone.object_sizes_here(&hashes)?;
            for alternate in &self_clone.alternates {
                let missing_positions: Vec<_> = sizes
                    .iter()
                    .enumerate()
                    .filter_map(|(position, size)| size.is_none().then_some(position))
                    .collect();
                if missing_positions.is_empty() {
                    break;
                }
                let missing_hashes: Vec<_> = missing_positions
                    .iter()
                    .map(|position| hashes[*position])
                    .collect();
                let alternate_sizes = alternate.object_sizes_here(&missing_hashes)?;
                for (position, found) in missing_positions.into_iter().zip(alternate_sizes) {
                    if found.is_some() {
                        sizes[position] = found;
                    }
                }
            }
            Ok(sizes)
        })
        .await
        .map_err(|error| GitError::IOError(io::Error::other(error)))?
    }

    async fn object_sizes_with_total_limit(
        &self,
        hashes: &[ObjectHash],
        aggregate_limit: u64,
    ) -> Result<Vec<Option<u64>>, GitError> {
        let self_clone = self.clone();
        let hashes = hashes.to_vec();
        tokio::task::spawn_blocking(move || {
            if let Some(kind) = self_clone.hash_kind {
                set_hash_kind(kind);
            }
            let mut sizes =
                self_clone.object_sizes_here_with_limit(&hashes, Some(aggregate_limit))?;
            let mut used = sizes.iter().flatten().try_fold(0u64, |total, size| {
                total
                    .checked_add(crate::utils::preview_object::charged_bytes(*size))
                    .ok_or_else(|| {
                        GitError::InvalidObjectInfo(
                            "preview aggregate cache load cost exceeds u64".to_string(),
                        )
                    })
            })?;
            for alternate in &self_clone.alternates {
                let missing_positions: Vec<_> = sizes
                    .iter()
                    .enumerate()
                    .filter_map(|(position, size)| size.is_none().then_some(position))
                    .collect();
                if missing_positions.is_empty() {
                    break;
                }
                let missing_hashes: Vec<_> = missing_positions
                    .iter()
                    .map(|position| hashes[*position])
                    .collect();
                let alternate_sizes = alternate.object_sizes_here_with_limit(
                    &missing_hashes,
                    Some(aggregate_limit.saturating_sub(used)),
                )?;
                for (position, found) in missing_positions.into_iter().zip(alternate_sizes) {
                    if let Some(found) = found {
                        used = used
                            .checked_add(crate::utils::preview_object::charged_bytes(found))
                            .ok_or_else(|| {
                                GitError::InvalidObjectInfo(
                                    "preview aggregate cache load cost exceeds u64".to_string(),
                                )
                            })?;
                        sizes[position] = Some(found);
                    }
                }
            }
            Ok(sizes)
        })
        .await
        .map_err(|error| GitError::IOError(io::Error::other(error)))?
    }

    async fn decoded_object_size(&self, hash: &ObjectHash) -> Result<Option<u64>, GitError> {
        let self_clone = self.clone();
        let hash = *hash;
        tokio::task::spawn_blocking(move || {
            if let Some(kind) = self_clone.hash_kind {
                set_hash_kind(kind);
            }
            let mut size = self_clone.decoded_object_sizes_here(&[hash])?[0];
            for alternate in &self_clone.alternates {
                if size.is_none() {
                    size = alternate.decoded_object_sizes_here(&[hash])?[0];
                }
            }
            Ok(size)
        })
        .await
        .map_err(|error| GitError::IOError(io::Error::other(error)))?
    }

    async fn decoded_object_sizes(
        &self,
        hashes: &[ObjectHash],
    ) -> Result<Vec<Option<u64>>, GitError> {
        let self_clone = self.clone();
        let hashes = hashes.to_vec();
        tokio::task::spawn_blocking(move || {
            if let Some(kind) = self_clone.hash_kind {
                set_hash_kind(kind);
            }
            let mut sizes = self_clone.decoded_object_sizes_here(&hashes)?;
            for alternate in &self_clone.alternates {
                let missing_positions: Vec<_> = sizes
                    .iter()
                    .enumerate()
                    .filter_map(|(position, size)| size.is_none().then_some(position))
                    .collect();
                if missing_positions.is_empty() {
                    break;
                }
                let missing_hashes: Vec<_> = missing_positions
                    .iter()
                    .map(|position| hashes[*position])
                    .collect();
                let alternate_sizes = alternate.decoded_object_sizes_here(&missing_hashes)?;
                for (position, found) in missing_positions.into_iter().zip(alternate_sizes) {
                    if found.is_some() {
                        sizes[position] = found;
                    }
                }
            }
            Ok(sizes)
        })
        .await
        .map_err(|error| GitError::IOError(io::Error::other(error)))?
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
                                && let Ok(hash) =
                                    crate::internal::object_format::parse_repo_oid(&full_hash)
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

            // The same object can be reachable both as a loose object and from
            // one or more pack indexes (e.g. after `repack` without `-d`, or
            // several consecutive `repack -a -d` runs that each kept the prior
            // pack). A duplicate candidate must never be reported as ambiguity,
            // so sort and collapse identical OIDs before returning (ADR-MX-04).
            objects.sort_unstable();
            objects.dedup();
            objects
        })
        .await
        .unwrap_or_default()
    }

    async fn search_checked(&self, prefix: &str) -> Result<Vec<ObjectHash>, GitError> {
        let self_clone = self.clone();
        let prefix = prefix.to_string();
        tokio::task::spawn_blocking(move || self_clone.search_checked_blocking(&prefix))
            .await
            .map_err(|error| GitError::IOError(io::Error::other(error)))?
            .map_err(GitError::IOError)
    }
}

impl LocalStorage {
    /// Lists all object hashes contained in a pack index file. This is used for searching objects by prefix in packs.
    fn list_idx_objects(idx_file: &Path) -> Result<Vec<ObjectHash>, io::Error> {
        let (version, fanout) = Self::read_idx_fanout(idx_file)?;
        let mut idx_file = fs::File::open(idx_file)?;
        let object_count = fanout[255] as u64;
        let hash_size = git_internal::hash::get_hash_kind().size() as u64;

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

    #[tokio::test]
    #[serial_test::serial(hash_kind)]
    async fn search_dedupes_identical_loose_and_pack_candidates() {
        let _kind = git_internal::hash::set_hash_kind_for_test(HashKind::Sha1);
        let dir = tempfile::tempdir().expect("temporary object store");
        let storage = LocalStorage::try_new(dir.path().to_path_buf()).expect("storage");

        // One object that exists both as a loose object and in a pack index
        // (the post-`repack` shape that used to be reported as ambiguous).
        let hash = ObjectHash::Sha1([0x1a; 20]);
        storage
            .put(&hash, b"payload", ObjectType::Blob)
            .await
            .expect("write loose object");

        // Pack-index copy: a minimal v1 idx listing the same OID. `search` only
        // reads the idx (not the pack body), so a placeholder pack is enough.
        let pack_dir = dir.path().join("pack");
        fs::create_dir_all(&pack_dir).expect("create pack dir");
        fs::write(pack_dir.join("dummy.pack"), b"placeholder pack").expect("write pack");

        let mut idx = Vec::new();
        let first_byte = hash.as_ref()[0] as u32;
        for slot in 0..256u32 {
            let count = u32::from(slot >= first_byte);
            idx.extend_from_slice(&count.to_be_bytes());
        }
        idx.extend_from_slice(&12u32.to_be_bytes());
        idx.extend_from_slice(hash.as_ref());
        fs::write(pack_dir.join("dummy.idx"), idx).expect("write v1 idx");

        let matches = <LocalStorage as Storage>::search(&storage, &hash.to_string()[..4]).await;
        assert_eq!(
            matches,
            vec![hash],
            "identical loose and pack candidates must collapse to one"
        );
    }

    #[tokio::test]
    #[serial_test::serial(hash_kind)]
    async fn type_probe_reads_only_large_loose_blob_and_tree_headers() {
        let _kind = git_internal::hash::set_hash_kind_for_test(HashKind::Sha1);
        let dir = tempfile::tempdir().expect("temporary object store");
        let storage = LocalStorage::try_new(dir.path().to_path_buf()).expect("storage");
        for (byte, kind) in [(0x31, ObjectType::Blob), (0x32, ObjectType::Tree)] {
            let hash = ObjectHash::Sha1([byte; 20]);
            let path = storage.get_obj_path(&hash);
            fs::create_dir_all(path.parent().expect("object shard")).expect("create shard");
            let file = fs::File::create(&path).expect("create loose fixture");
            let mut encoder = ZlibEncoder::new(file, Compression::default());
            encoder
                .write_all(format!("{kind} {}\0", 1u64 << 35).as_bytes())
                .expect("write large declared header");
            encoder.finish().expect("finish header-only fixture");

            assert_eq!(
                <LocalStorage as Storage>::object_type_bounded_probe(&storage, &hash)
                    .await
                    .expect("type probe reads only the header"),
                kind
            );
            assert!(
                storage.get_with_limit(&hash, 1024).await.is_err(),
                "the full object is intentionally absent"
            );
        }
    }

    #[tokio::test]
    #[serial_test::serial(hash_kind)]
    async fn type_probe_reads_only_large_packed_blob_and_tree_headers() {
        let _kind = git_internal::hash::set_hash_kind_for_test(HashKind::Sha1);
        let dir = tempfile::tempdir().expect("temporary pack directory");
        let storage = LocalStorage::try_new(dir.path().to_path_buf()).expect("storage");
        for (number, kind) in [(3u8, ObjectType::Blob), (2u8, ObjectType::Tree)] {
            let pack = dir.path().join(format!("header-{number}.pack"));
            let mut bytes = vec![0; 12];
            let mut size = 1u64 << 35;
            let mut first = (number << 4) | (size as u8 & 0x0f);
            size >>= 4;
            if size != 0 {
                first |= 0x80;
            }
            bytes.push(first);
            while size != 0 {
                let mut byte = (size & 0x7f) as u8;
                size >>= 7;
                if size != 0 {
                    byte |= 0x80;
                }
                bytes.push(byte);
            }
            fs::write(&pack, bytes).expect("write header-only pack fixture");
            assert_eq!(
                LocalStorage::object_type_at_pack_offset(
                    &pack,
                    12,
                    0,
                    &mut TypeProbeState::default(),
                    &storage,
                    None,
                )
                .expect("type probe reads only the pack header"),
                kind
            );
        }

        let zstd_delta_pack = dir.path().join("offset-zstdelta.pack");
        let mut bytes = vec![0; 12];
        bytes.extend_from_slice(&[0x30, 0x50, 0x01]);
        fs::write(&zstd_delta_pack, bytes).expect("write zstd delta header fixture");
        assert_eq!(
            LocalStorage::object_type_at_pack_offset(
                &zstd_delta_pack,
                13,
                0,
                &mut TypeProbeState::default(),
                &storage,
                None,
            )
            .expect("resolve OffsetZstdelta base without decoding its body"),
            ObjectType::Blob
        );
    }

    #[tokio::test]
    #[serial_test::serial(hash_kind)]
    async fn type_probe_resolves_ofs_and_ref_delta_bases_without_decoding_payloads() {
        let _kind = git_internal::hash::set_hash_kind_for_test(HashKind::Sha1);
        let dir = tempfile::tempdir().expect("temporary object store");
        let pack_dir = dir.path().join("pack");
        fs::create_dir(&pack_dir).expect("create pack directory");
        let fixture =
            Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/data/packs/ofs-delta-sha1.pack");
        let pack = pack_dir.join("ofs-delta-sha1.pack");
        fs::copy(&fixture, &pack).expect("copy OFS delta fixture");
        let index = pack.with_extension("idx");
        command::index_pack::build_index_v1(
            pack.to_str().expect("UTF-8 pack path"),
            index.to_str().expect("UTF-8 index path"),
        )
        .expect("index OFS delta fixture");
        let storage = LocalStorage::try_new(dir.path().to_path_buf()).expect("storage");
        let ofs_delta = crate::internal::object_format::parse_repo_oid(
            "1b59abc09609574e73330d56815f04ebb4d9bd72",
        )
        .expect("parse fixture delta hash");
        assert_eq!(
            storage
                .object_type_bounded_probe(&ofs_delta)
                .await
                .expect("resolve OFS delta base type"),
            ObjectType::Blob
        );

        let ref_base = ObjectHash::from_type_and_data(ObjectType::Tree, b"base tree");
        storage
            .put(&ref_base, b"base tree", ObjectType::Tree)
            .await
            .expect("store REF delta base");
        let ref_pack = dir.path().join("ref-delta.pack");
        let mut bytes = vec![0; 12];
        bytes.push(0x70); // REF_DELTA, zero encoded bytes; payload is intentionally absent.
        bytes.extend_from_slice(ref_base.as_ref());
        fs::write(&ref_pack, bytes).expect("write REF delta header fixture");
        assert_eq!(
            LocalStorage::object_type_at_pack_offset(
                &ref_pack,
                12,
                0,
                &mut TypeProbeState::default(),
                &storage,
                None,
            )
            .expect("resolve REF delta base type"),
            ObjectType::Tree
        );
    }

    #[cfg(unix)]
    #[test]
    #[serial_test::serial(hash_kind)]
    fn ref_delta_cross_pack_type_probe_ignores_unrelated_orphan_or_installer() {
        use std::os::fd::AsRawFd;

        let _kind = git_internal::hash::set_hash_kind_for_test(HashKind::Sha1);
        let dir = tempfile::tempdir().expect("temporary object store");
        let pack_dir = dir.path().join("pack");
        fs::create_dir(&pack_dir).expect("create pack directory");
        let healthy_pack = pack_dir.join("pack-base.pack");
        fs::copy(
            Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/data/packs/ofs-delta-sha1.pack"),
            &healthy_pack,
        )
        .expect("copy base pack");
        let healthy_idx = healthy_pack.with_extension("idx");
        command::index_pack::build_index_v1(
            healthy_pack.to_str().expect("UTF-8 pack path"),
            healthy_idx.to_str().expect("UTF-8 index path"),
        )
        .expect("index base pack");
        let unrelated = pack_dir.join("pack-unrelated.pack");
        fs::copy(
            Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/data/packs/small-sha1.pack"),
            &unrelated,
        )
        .expect("copy unindexed unrelated pack");
        let storage = LocalStorage::try_new(dir.path().to_path_buf()).expect("storage");
        let base = crate::internal::object_format::parse_repo_oid(
            "b1a36d7748643b07e2bd006211e9e6a492f6bb8b",
        )
        .expect("parse packed base OID");
        let ref_pack = dir.path().join("reference.pack");
        let mut bytes = vec![0; 12];
        bytes.push(0x70);
        bytes.extend_from_slice(base.as_ref());
        fs::write(&ref_pack, bytes).expect("write REF delta header");
        let probe = || {
            LocalStorage::object_type_at_pack_offset(
                &ref_pack,
                12,
                0,
                &mut TypeProbeState::default(),
                &storage,
                None,
            )
        };
        assert_eq!(
            probe().expect("healthy cross-pack base ignores orphan"),
            ObjectType::Blob
        );

        let lock = fs::File::create(unrelated.with_extension("install.lock"))
            .expect("create unrelated install lock");
        // SAFETY: this owned descriptor remains alive until after the probe.
        assert_eq!(unsafe { libc::flock(lock.as_raw_fd(), libc::LOCK_EX) }, 0);
        assert_eq!(
            probe().expect("healthy cross-pack base ignores active unrelated installer"),
            ObjectType::Blob
        );
        drop(lock);

        let missing = ObjectHash::Sha1([0xc3; 20]);
        let mut missing_bytes = vec![0; 12];
        missing_bytes.push(0x70);
        missing_bytes.extend_from_slice(missing.as_ref());
        fs::write(&ref_pack, missing_bytes).expect("write missing REF delta base");
        let error = probe().expect_err("unresolved base with orphan must fail closed");
        assert!(error.to_string().contains("pack-unrelated.pack"));
    }

    #[tokio::test]
    #[serial_test::serial(hash_kind)]
    async fn type_probe_batch_checks_many_packs_with_one_result_per_oid() {
        let _kind = git_internal::hash::set_hash_kind_for_test(HashKind::Sha1);
        let dir = tempfile::tempdir().expect("temporary object store");
        let pack_dir = dir.path().join("pack");
        fs::create_dir(&pack_dir).expect("create pack directory");
        let fixture =
            Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/data/packs/ofs-delta-sha1.pack");
        let seed_pack = pack_dir.join("fixture-00.pack");
        fs::copy(&fixture, &seed_pack).expect("copy fixture pack");
        let seed_index = seed_pack.with_extension("idx");
        command::index_pack::build_index_v1(
            seed_pack.to_str().expect("UTF-8 pack path"),
            seed_index.to_str().expect("UTF-8 index path"),
        )
        .expect("index fixture pack");
        for number in 1..27 {
            let pack = pack_dir.join(format!("fixture-{number:02}.pack"));
            fs::copy(&seed_pack, &pack).expect("copy another pack");
            fs::copy(&seed_index, pack.with_extension("idx")).expect("copy another index");
        }

        let storage = LocalStorage::try_new(dir.path().to_path_buf()).expect("storage");
        let loose = ObjectHash::from_type_and_data(ObjectType::Tree, b"small tree");
        storage
            .put(&loose, b"small tree", ObjectType::Tree)
            .await
            .expect("store loose tree");
        let packed_delta = crate::internal::object_format::parse_repo_oid(
            "1b59abc09609574e73330d56815f04ebb4d9bd72",
        )
        .expect("parse packed delta hash");
        let packed_base = crate::internal::object_format::parse_repo_oid(
            "b1a36d7748643b07e2bd006211e9e6a492f6bb8b",
        )
        .expect("parse packed base hash");
        let missing = ObjectHash::Sha1([0xf4; 20]);
        let found = <LocalStorage as Storage>::object_types_bounded_probe(
            &storage,
            &[packed_delta, loose, missing, packed_base, packed_delta],
        )
        .await
        .expect("probe OIDs across more than 24 packs");
        assert_eq!(found.len(), 3);
        assert_eq!(found.get(&packed_delta), Some(&ObjectType::Blob));
        assert_eq!(found.get(&packed_base), Some(&ObjectType::Blob));
        assert_eq!(found.get(&loose), Some(&ObjectType::Tree));
        assert!(!found.contains_key(&missing));
    }

    #[tokio::test]
    #[serial_test::serial(hash_kind)]
    async fn batch_probes_ignore_unrelated_orphan_but_reject_unresolved_oid() {
        let _kind = git_internal::hash::set_hash_kind_for_test(HashKind::Sha1);
        let dir = tempfile::tempdir().expect("temporary object store");
        let pack_dir = dir.path().join("pack");
        fs::create_dir(&pack_dir).expect("create pack directory");
        let indexed_pack = pack_dir.join("pack-indexed.pack");
        fs::copy(
            Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/data/packs/ofs-delta-sha1.pack"),
            &indexed_pack,
        )
        .expect("copy indexed pack");
        let indexed_idx = indexed_pack.with_extension("idx");
        command::index_pack::build_index_v1(
            indexed_pack.to_str().expect("UTF-8 pack path"),
            indexed_idx.to_str().expect("UTF-8 index path"),
        )
        .expect("index healthy pack");
        fs::copy(
            Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/data/packs/small-sha1.pack"),
            pack_dir.join("pack-orphan.pack"),
        )
        .expect("copy orphan pack");
        let storage = LocalStorage::try_new(dir.path().to_path_buf()).expect("storage");
        let healthy = crate::internal::object_format::parse_repo_oid(
            "1b59abc09609574e73330d56815f04ebb4d9bd72",
        )
        .expect("parse healthy OID");
        assert_eq!(
            storage
                .exist_checked_batch(&[healthy])
                .await
                .expect("healthy indexed object ignores unrelated orphan")
                .get(&healthy),
            Some(&true)
        );
        assert_eq!(
            storage
                .object_types_bounded_probe(&[healthy])
                .await
                .expect("healthy typed object ignores unrelated orphan")
                .get(&healthy),
            Some(&ObjectType::Blob)
        );

        let missing = ObjectHash::Sha1([0xc7; 20]);
        let presence_error = storage
            .exist_checked_batch(&[healthy, missing])
            .await
            .expect_err("unresolved object must not become a confirmed miss");
        assert!(presence_error.to_string().contains("pack-orphan.pack"));
        let type_error = storage
            .object_types_bounded_probe(&[healthy, missing])
            .await
            .expect_err("unresolved type must not bypass orphan pack");
        assert!(type_error.to_string().contains("pack-orphan.pack"));
    }

    #[cfg(unix)]
    #[tokio::test]
    #[serial_test::serial(hash_kind)]
    async fn batch_probes_wait_only_for_requested_oid_in_other_installing_pack() {
        use std::os::fd::AsRawFd;

        let _kind = git_internal::hash::set_hash_kind_for_test(HashKind::Sha1);
        let dir = tempfile::tempdir().expect("temporary object store");
        let pack_dir = dir.path().join("pack");
        fs::create_dir(&pack_dir).expect("create pack directory");
        let first_pack = pack_dir.join("pack-first.pack");
        fs::copy(
            Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/data/packs/ofs-delta-sha1.pack"),
            &first_pack,
        )
        .expect("copy first pack");
        let first_idx = first_pack.with_extension("idx");
        command::index_pack::build_index_v1(
            first_pack.to_str().expect("UTF-8 pack path"),
            first_idx.to_str().expect("UTF-8 index path"),
        )
        .expect("index first pack");
        let second_pack = pack_dir.join("pack-second.pack");
        let lock = fs::File::create(second_pack.with_extension("install.lock"))
            .expect("create second pack install lock");
        // SAFETY: the lock's descriptor stays alive until after the second
        // index is fully published below.
        assert_eq!(unsafe { libc::flock(lock.as_raw_fd(), libc::LOCK_EX) }, 0);
        fs::copy(
            Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/data/packs/small-sha1.pack"),
            &second_pack,
        )
        .expect("publish second pack before its index");
        let storage = LocalStorage::try_new(dir.path().to_path_buf()).expect("storage");
        let first = crate::internal::object_format::parse_repo_oid(
            "1b59abc09609574e73330d56815f04ebb4d9bd72",
        )
        .expect("parse first pack OID");
        let second = crate::internal::object_format::parse_repo_oid(
            "035f9b742ebf552ed87f003d4944480bfea6ba99",
        )
        .expect("parse second pack OID");
        tokio::time::timeout(
            Duration::from_secs(2),
            storage.exist_checked_batch(&[first]),
        )
        .await
        .expect("unrelated active install must not block presence")
        .expect("probe healthy pack");
        tokio::time::timeout(
            Duration::from_secs(2),
            storage.object_types_bounded_probe(&[first]),
        )
        .await
        .expect("unrelated active install must not block type")
        .expect("probe healthy type");

        let presence_storage = storage.clone();
        let presence =
            tokio::spawn(async move { presence_storage.exist_checked_batch(&[second]).await });
        let type_storage = storage.clone();
        let types =
            tokio::spawn(async move { type_storage.object_types_bounded_probe(&[second]).await });
        tokio::time::sleep(Duration::from_millis(100)).await;
        assert!(
            !presence.is_finished(),
            "presence must wait for the second index"
        );
        assert!(
            !types.is_finished(),
            "type probe must wait for the second index"
        );
        let second_idx = second_pack.with_extension("idx");
        command::index_pack::build_index_v1(
            second_pack.to_str().expect("UTF-8 pack path"),
            second_idx.to_str().expect("UTF-8 index path"),
        )
        .expect("publish second index while install lock is held");
        drop(lock);

        let found = tokio::time::timeout(Duration::from_secs(3), presence)
            .await
            .expect("presence completes after install")
            .expect("presence task")
            .expect("presence succeeds");
        assert_eq!(found.get(&second), Some(&true));
        let found_types = tokio::time::timeout(Duration::from_secs(3), types)
            .await
            .expect("type probe completes after install")
            .expect("type task")
            .expect("type probe succeeds");
        assert_eq!(found_types.get(&second), Some(&ObjectType::Blob));
    }

    #[cfg(unix)]
    #[tokio::test]
    #[serial_test::serial(hash_kind)]
    async fn batch_probes_use_healthy_alternate_before_waiting_for_primary_install() {
        use std::os::fd::AsRawFd;

        let _kind = git_internal::hash::set_hash_kind_for_test(HashKind::Sha1);
        let dir = tempfile::tempdir().expect("temporary stores");
        let primary_path = dir.path().join("primary");
        let alternate_path = dir.path().join("alternate");
        let mut primary = LocalStorage::try_new(primary_path.clone()).expect("storage");
        let alternate = LocalStorage::try_new(alternate_path).expect("storage");
        let payload = b"alternate-only object";
        let hash = ObjectHash::from_type_and_data(ObjectType::Blob, payload);
        alternate
            .put(&hash, payload, ObjectType::Blob)
            .await
            .expect("write alternate blob");
        primary.alternates.push(Arc::new(alternate));

        let pack_dir = primary_path.join("pack");
        fs::create_dir(&pack_dir).expect("create primary pack directory");
        let installing = pack_dir.join("pack-installing.pack");
        let lock = fs::File::create(installing.with_extension("install.lock"))
            .expect("create primary install lock");
        // SAFETY: this descriptor remains owned through both probes.
        assert_eq!(unsafe { libc::flock(lock.as_raw_fd(), libc::LOCK_EX) }, 0);
        fs::copy(
            Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/data/packs/small-sha1.pack"),
            &installing,
        )
        .expect("publish primary pack without its index");

        let present =
            tokio::time::timeout(Duration::from_secs(2), primary.exist_checked_batch(&[hash]))
                .await
                .expect("healthy alternate bypasses unrelated primary install")
                .expect("alternate presence probe succeeds");
        assert_eq!(present.get(&hash), Some(&true));
        let types = tokio::time::timeout(
            Duration::from_secs(2),
            primary.object_types_bounded_probe(&[hash]),
        )
        .await
        .expect("alternate type bypasses unrelated primary install")
        .expect("alternate type probe succeeds");
        assert_eq!(types.get(&hash), Some(&ObjectType::Blob));
        drop(lock);
    }

    /// `put` writes loose objects through `write_atomic` (lore.md §7.7): the
    /// object round-trips, and the shard directory holds only the final object
    /// with no leftover temp file.
    #[tokio::test]
    #[serial_test::serial(hash_kind)]
    async fn put_writes_loose_object_atomically() {
        use git_internal::{
            hash::{HashKind, ObjectHash, set_hash_kind_for_test},
            internal::object::types::ObjectType,
        };

        let _kind = set_hash_kind_for_test(HashKind::Sha1);
        let dir = tempfile::tempdir().unwrap();
        let storage = LocalStorage::try_new(dir.path().to_path_buf()).expect("storage");
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

    #[tokio::test]
    #[serial_test::serial(hash_kind)]
    async fn bounded_get_rejects_oversized_loose_declaration_before_payload_decode() {
        use std::io::Write as _;

        let _kind = git_internal::hash::set_hash_kind_for_test(HashKind::Sha1);
        let dir = tempfile::tempdir().expect("create bounded-get fixture");
        let storage = LocalStorage::try_new(dir.path().to_path_buf()).expect("storage");
        let hash = crate::internal::object_format::parse_repo_oid(
            "1111111111111111111111111111111111111111",
        )
        .expect("parse fixture object ID");
        let path = storage.get_obj_path(&hash);
        std::fs::create_dir_all(path.parent().expect("object shard parent"))
            .expect("create object shard");
        let file = std::fs::File::create(path).expect("create loose fixture");
        let mut encoder = flate2::write::ZlibEncoder::new(file, Compression::default());
        let declared = crate::utils::preview_object::MAX_OBJECT_BYTES + 1;
        write!(encoder, "blob {declared}\0").expect("write oversized declaration");
        encoder.finish().expect("finish loose fixture");

        let error = storage
            .get_with_limit(&hash, crate::utils::preview_object::MAX_OBJECT_BYTES)
            .await
            .expect_err("bounded read must reject oversized declaration");
        assert!(
            error.to_string().contains("exceeds preview limit"),
            "{error}"
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
    #[serial_test::serial(hash_kind)]
    fn read_pack_obj_resolves_ofs_delta_base() {
        set_hash_kind(HashKind::Sha1);

        let dir = tempfile::tempdir().expect("tempdir");
        let pack = dir.path().join("ofs-delta-sha1.pack");
        let fixture =
            Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/data/packs/ofs-delta-sha1.pack");
        std::fs::copy(&fixture, &pack).expect("copy fixture pack");

        let idx = dir.path().join("ofs-delta-sha1.idx");
        command::index_pack::build_index_v1(pack.to_str().unwrap(), idx.to_str().unwrap())
            .expect("build v1 index for fixture");

        let ofs_delta = crate::internal::object_format::parse_repo_oid(
            "1b59abc09609574e73330d56815f04ebb4d9bd72",
        )
        .unwrap();
        let obj = LocalStorage::read_pack_by_idx(&idx, &ofs_delta)
            .expect("reading the OFS_DELTA object must resolve its base offset correctly")
            .expect("object must be present in the pack");

        assert_eq!(obj.object_type(), ObjectType::Blob);
        let expected = "libra ofs-delta base line\n".repeat(400).into_bytes();
        let load_cost = crate::utils::storage::load_cost::pack_costs(dir.path(), &[ofs_delta])
            .expect("probe OFS_DELTA load cost")[0]
            .expect("OFS_DELTA cost must be available");
        assert!(
            load_cost > expected.len() as u64,
            "load cost must include the delta base and instruction stream"
        );
        assert_eq!(
            obj.data_decompressed, expected,
            "OFS_DELTA object must reconstruct to the correct blob contents"
        );
    }

    #[test]
    #[serial_test::serial(hash_kind)]
    fn object_size_probe_does_not_build_a_missing_pack_index() {
        set_hash_kind(HashKind::Sha1);
        let dir = tempfile::tempdir().expect("tempdir");
        let pack_dir = dir.path().join("pack");
        std::fs::create_dir(&pack_dir).expect("create pack directory");
        let pack = pack_dir.join("ofs-delta-sha1.pack");
        let fixture =
            Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/data/packs/ofs-delta-sha1.pack");
        std::fs::copy(&fixture, &pack).expect("copy fixture pack");
        let storage = LocalStorage::try_new(dir.path().to_path_buf()).expect("storage");
        let object = crate::internal::object_format::parse_repo_oid(
            "1b59abc09609574e73330d56815f04ebb4d9bd72",
        )
        .expect("parse fixture object ID");

        assert_eq!(
            storage
                .object_sizes_here(&[object])
                .expect("probe object size without index")[0],
            None
        );
        assert!(
            !pack.with_extension("idx").exists(),
            "read-only size probe must not build a pack index"
        );
        let error = storage
            .exist_checked_here(&object)
            .expect_err("a pack without an index is not a confirmed absence");
        assert!(
            error.to_string().contains("has no complete index"),
            "{error}"
        );
        assert!(error.to_string().contains("libra index-pack"), "{error}");
    }

    #[tokio::test]
    #[serial_test::serial(hash_kind)]
    async fn bounded_pack_read_does_not_build_an_unrelated_missing_index() {
        set_hash_kind(HashKind::Sha1);
        let dir = tempfile::tempdir().expect("tempdir");
        let pack_dir = dir.path().join("pack");
        std::fs::create_dir(&pack_dir).expect("create pack directory");
        let fixture =
            Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/data/packs/ofs-delta-sha1.pack");

        let indexed_pack = pack_dir.join("indexed.pack");
        let indexed_idx = indexed_pack.with_extension("idx");
        std::fs::copy(&fixture, &indexed_pack).expect("copy indexed pack fixture");
        command::index_pack::build_index_v1(
            indexed_pack.to_str().expect("UTF-8 indexed pack path"),
            indexed_idx.to_str().expect("UTF-8 indexed idx path"),
        )
        .expect("build indexed fixture index");

        let unrelated_pack = pack_dir.join("unrelated.pack");
        let unrelated_idx = unrelated_pack.with_extension("idx");
        std::fs::copy(&fixture, &unrelated_pack).expect("copy unrelated pack fixture");
        let storage = LocalStorage::try_new(dir.path().to_path_buf()).expect("storage");
        let object = crate::internal::object_format::parse_repo_oid(
            "1b59abc09609574e73330d56815f04ebb4d9bd72",
        )
        .expect("parse fixture object ID");

        storage
            .get_with_limit(&object, crate::utils::preview_object::MAX_OBJECT_BYTES)
            .await
            .expect("bounded read from existing indexed pack");
        assert!(
            !unrelated_idx.exists(),
            "bounded preview read must not build an unrelated missing pack index"
        );
    }

    #[tokio::test]
    #[serial_test::serial(hash_kind)]
    async fn bounded_delta_read_does_not_populate_the_global_pack_cache() {
        set_hash_kind(HashKind::Sha1);
        let dir = tempfile::tempdir().expect("tempdir");
        let pack_dir = dir.path().join("pack");
        std::fs::create_dir(&pack_dir).expect("create pack directory");
        let unique = dir
            .path()
            .file_name()
            .and_then(|name| name.to_str())
            .expect("UTF-8 temp directory name");
        let pack = pack_dir.join(format!("bounded-{unique}.pack"));
        let idx = pack.with_extension("idx");
        let fixture =
            Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/data/packs/ofs-delta-sha1.pack");
        std::fs::copy(&fixture, &pack).expect("copy pack fixture");
        command::index_pack::build_index_v1(
            pack.to_str().expect("UTF-8 pack path"),
            idx.to_str().expect("UTF-8 idx path"),
        )
        .expect("build fixture index");

        let object = crate::internal::object_format::parse_repo_oid(
            "1b59abc09609574e73330d56815f04ebb4d9bd72",
        )
        .expect("parse delta object ID");
        let file_name = pack
            .file_name()
            .and_then(|name| name.to_str())
            .expect("UTF-8 pack file name");
        let delta_key = format!("{file_name:?}-420");
        let base_key = format!("{file_name:?}-241");
        {
            let mut cache = PACK_OBJ_CACHE.lock().expect("pack cache lock");
            cache.remove(&delta_key);
            cache.remove(&base_key);
        }

        let storage = LocalStorage::try_new(dir.path().to_path_buf()).expect("storage");
        let (payload, object_type) = storage
            .get_with_limit(&object, crate::utils::preview_object::MAX_OBJECT_BYTES)
            .await
            .expect("bounded delta read");
        assert_eq!(object_type, ObjectType::Blob);
        assert_eq!(
            payload,
            "libra ofs-delta base line\n".repeat(400).into_bytes()
        );
        let load_cost = crate::utils::storage::load_cost::pack_costs(&pack_dir, &[object])
            .expect("probe bounded delta load cost")[0]
            .expect("delta load cost");
        assert!(
            load_cost > payload.len() as u64,
            "charged peak must include delta base/instruction/result coexistence"
        );
        let cache = PACK_OBJ_CACHE.lock().expect("pack cache lock");
        assert!(
            !cache.contains(&delta_key) && !cache.contains(&base_key),
            "bounded reads must not retain the delta or its base in the 200 MiB global pack cache"
        );
    }

    #[tokio::test]
    async fn checked_batch_reports_loose_pack_and_missing_objects() {
        let _kind = git_internal::hash::set_hash_kind_for_test(HashKind::Sha1);
        let dir = tempfile::tempdir().expect("create object directory");
        let storage = LocalStorage::try_new(dir.path().to_path_buf()).expect("storage");
        let pack_dir = dir.path().join("pack");
        fs::create_dir(&pack_dir).expect("create pack directory");
        let pack = pack_dir.join("fixture.pack");
        let fixture =
            Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/data/packs/ofs-delta-sha1.pack");
        fs::copy(&fixture, &pack).expect("copy pack fixture");
        let index = pack.with_extension("idx");
        command::index_pack::build_index_v1(
            pack.to_str().expect("UTF-8 pack path"),
            index.to_str().expect("UTF-8 index path"),
        )
        .expect("build fixture index");

        let loose = ObjectHash::Sha1([0x22; 20]);
        storage
            .put(&loose, b"loose", ObjectType::Blob)
            .await
            .expect("store loose object");
        let packed_delta = crate::internal::object_format::parse_repo_oid(
            "1b59abc09609574e73330d56815f04ebb4d9bd72",
        )
        .expect("parse packed delta hash");
        let packed_base = crate::internal::object_format::parse_repo_oid(
            "b1a36d7748643b07e2bd006211e9e6a492f6bb8b",
        )
        .expect("parse packed base hash");
        let missing = ObjectHash::Sha1([0x33; 20]);
        let result = storage
            .exist_checked_batch(&[loose, packed_delta, missing, packed_base, packed_delta])
            .await
            .expect("probe local batch");
        assert_eq!(result.len(), 4, "duplicate OID should have one result");
        assert_eq!(result.get(&loose), Some(&true));
        assert_eq!(result.get(&packed_delta), Some(&true));
        assert_eq!(result.get(&packed_base), Some(&true));
        assert_eq!(result.get(&missing), Some(&false));
    }

    #[tokio::test]
    async fn checked_batch_handles_many_packs_and_unresolved_hashes() {
        let _kind = git_internal::hash::set_hash_kind_for_test(HashKind::Sha1);
        let dir = tempfile::tempdir().expect("create object directory");
        let storage = LocalStorage::try_new(dir.path().to_path_buf()).expect("storage");
        let pack_dir = dir.path().join("pack");
        fs::create_dir(&pack_dir).expect("create pack directory");
        let seed_pack = pack_dir.join("fixture-00.pack");
        let fixture =
            Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/data/packs/ofs-delta-sha1.pack");
        fs::copy(&fixture, &seed_pack).expect("copy pack fixture");
        let seed_index = seed_pack.with_extension("idx");
        command::index_pack::build_index_v1(
            seed_pack.to_str().expect("UTF-8 pack path"),
            seed_index.to_str().expect("UTF-8 index path"),
        )
        .expect("build fixture index");
        for number in 1..24 {
            let pack = pack_dir.join(format!("fixture-{number:02}.pack"));
            fs::copy(&seed_pack, &pack).expect("copy another pack");
            fs::copy(&seed_index, pack.with_extension("idx")).expect("copy another index");
        }

        let packed = crate::internal::object_format::parse_repo_oid(
            "1b59abc09609574e73330d56815f04ebb4d9bd72",
        )
        .expect("parse packed object ID");
        let missing_a = ObjectHash::Sha1([0x33; 20]);
        let missing_b = ObjectHash::Sha1([0x44; 20]);
        let result = storage
            .exist_checked_batch(&[missing_a, packed, missing_b, missing_a])
            .await
            .expect("probe a batch across more than 16 pack indexes");
        assert_eq!(result.len(), 3);
        assert_eq!(result.get(&packed), Some(&true));
        assert_eq!(result.get(&missing_a), Some(&false));
        assert_eq!(result.get(&missing_b), Some(&false));
    }

    #[test]
    fn checked_batch_binary_index_finds_bucket_edges_and_misses() {
        fn oid(kind: HashKind, first: u8, second: u8) -> ObjectHash {
            match kind {
                HashKind::Sha1 => {
                    let mut bytes = [0; 20];
                    bytes[0] = first;
                    bytes[1] = second;
                    ObjectHash::Sha1(bytes)
                }
                HashKind::Sha256 => {
                    let mut bytes = [0; 32];
                    bytes[0] = first;
                    bytes[1] = second;
                    ObjectHash::Sha256(bytes)
                }
                HashKind::Blake3 => {
                    let mut bytes = [0; 32];
                    bytes[0] = first;
                    bytes[1] = second;
                    ObjectHash::Blake3(bytes)
                }
            }
        }

        for (kind, version) in [
            (HashKind::Sha1, IdxVersion::V1),
            (HashKind::Sha1, IdxVersion::V2),
            (HashKind::Sha256, IdxVersion::V2),
            (HashKind::Blake3, IdxVersion::V2),
        ] {
            let _kind = git_internal::hash::set_hash_kind_for_test(kind);
            let dir = tempfile::tempdir().expect("temporary index directory");
            let index_path = dir.path().join("fixture.idx");
            let hashes = [
                oid(kind, 0x2a, 0x00),
                oid(kind, 0x2a, 0x80),
                oid(kind, 0x2a, 0xff),
            ];
            let mut bytes = Vec::new();
            if version == IdxVersion::V2 {
                bytes.extend_from_slice(&IDX_MAGIC);
                bytes.extend_from_slice(&2u32.to_be_bytes());
            }
            for bucket in 0..=u8::MAX {
                let count = if bucket < 0x2a { 0u32 } else { 3u32 };
                bytes.extend_from_slice(&count.to_be_bytes());
            }
            match version {
                IdxVersion::V1 => {
                    for (index, hash) in hashes.iter().enumerate() {
                        bytes.extend_from_slice(&(index as u32 + 1).to_be_bytes());
                        bytes.extend_from_slice(hash.as_ref());
                    }
                }
                IdxVersion::V2 => {
                    for hash in &hashes {
                        bytes.extend_from_slice(hash.as_ref());
                    }
                    bytes.extend_from_slice(&[0; 12]); // CRC table
                    for index in 0..hashes.len() {
                        bytes.extend_from_slice(&(index as u32 + 1).to_be_bytes());
                    }
                }
            }
            fs::write(&index_path, bytes).expect("write synthetic pack index");
            let mut index_file = fs::File::open(&index_path).expect("open index");
            let (parsed_version, fanout) =
                LocalStorage::read_idx_fanout_from_open(&mut index_file).expect("read fanout");
            assert_eq!(parsed_version, version);
            for (index, hash) in hashes.iter().enumerate() {
                assert_eq!(
                    LocalStorage::read_idx_from_open_binary(
                        &mut index_file,
                        version,
                        &fanout,
                        hash,
                    )
                    .expect("find indexed object"),
                    Some(index as u64 + 1)
                );
                assert_eq!(
                    LocalStorage::read_idx(&index_path, hash).expect("single-object lookup"),
                    Some(index as u64 + 1)
                );
            }
            for missing in [
                oid(kind, 0x2a, 0x40),
                oid(kind, 0x2a, 0xfe),
                oid(kind, 0x29, 0xff),
                oid(kind, 0x2b, 0x00),
            ] {
                assert_eq!(
                    LocalStorage::read_idx_from_open_binary(
                        &mut index_file,
                        version,
                        &fanout,
                        &missing,
                    )
                    .expect("search missing object"),
                    None
                );
                assert_eq!(
                    LocalStorage::read_idx(&index_path, &missing).expect("single-object miss"),
                    None
                );
            }
        }
    }

    pub(super) fn status_pack_lookup_rejects_truncated_and_corrupt_indexes() {
        let _kind = git_internal::hash::set_hash_kind_for_test(HashKind::Sha1);
        let dir = tempfile::tempdir().expect("temporary index directory");
        let path = dir.path().join("fixture.idx");
        let hash = ObjectHash::Sha1([0x2a; 20]);
        for len in [0, 4, 100, FANOUT as usize - 1] {
            fs::write(&path, vec![0; len]).expect("write truncated index");
            assert!(LocalStorage::read_idx(&path, &hash).is_err(), "len={len}");
        }

        let mut fanout = vec![0; FANOUT as usize];
        fanout[0..4].copy_from_slice(&2u32.to_be_bytes());
        fs::write(&path, &fanout).expect("write nonmonotonic fanout");
        assert_eq!(
            LocalStorage::read_idx(&path, &hash)
                .expect_err("corrupt fanout must fail")
                .kind(),
            io::ErrorKind::InvalidData
        );

        fanout[0..4].copy_from_slice(&0u32.to_be_bytes());
        for count in fanout.as_chunks_mut::<4>().0.iter_mut().skip(0x2a) {
            count.copy_from_slice(&1u32.to_be_bytes());
        }
        fs::write(&path, &fanout).expect("write truncated object table");
        assert_eq!(
            LocalStorage::read_idx(&path, &hash)
                .expect_err("missing object table must fail")
                .kind(),
            io::ErrorKind::UnexpectedEof
        );

        let mut bad_large = Vec::new();
        bad_large.extend_from_slice(&IDX_MAGIC);
        bad_large.extend_from_slice(&2u32.to_be_bytes());
        bad_large.extend_from_slice(&fanout);
        bad_large.extend_from_slice(hash.as_ref());
        bad_large.extend_from_slice(&[0; 4]);
        bad_large.extend_from_slice(&0x8000_0000u32.to_be_bytes());
        bad_large.extend_from_slice(&[0x55; 40]); // Checksums, not a large-offset table.
        fs::write(&path, bad_large).expect("write index with missing large offset");
        assert_eq!(
            LocalStorage::read_idx(&path, &hash)
                .expect_err("checksum bytes cannot be interpreted as an offset")
                .kind(),
            io::ErrorKind::InvalidData
        );

        let mut replacement = vec![0; FANOUT as usize];
        replacement.extend_from_slice(&7u32.to_be_bytes());
        replacement.extend_from_slice(hash.as_ref());
        fs::write(&path, &replacement).expect("replace index with empty index");
        assert_eq!(
            LocalStorage::read_idx(&path, &hash).expect("new index"),
            None
        );
        for count in replacement[..FANOUT as usize]
            .as_chunks_mut::<4>()
            .0
            .iter_mut()
            .skip(0x2a)
        {
            count.copy_from_slice(&1u32.to_be_bytes());
        }
        fs::write(&path, replacement).expect("replace index with populated index");
        assert_eq!(
            LocalStorage::read_idx(&path, &hash).expect("new index"),
            Some(7)
        );
    }

    pub(super) fn status_pack_lookup_matches_v1_v2_and_hash_kinds() {
        for (kind, version) in [
            (HashKind::Sha1, IdxVersion::V1),
            (HashKind::Sha1, IdxVersion::V2),
            (HashKind::Sha256, IdxVersion::V2),
        ] {
            let _kind = git_internal::hash::set_hash_kind_for_test(kind);
            let dir = tempfile::tempdir().expect("temporary index directory");
            let path = dir.path().join("fixture.idx");
            let hash = match kind {
                HashKind::Sha1 => ObjectHash::Sha1([0x2a; 20]),
                HashKind::Sha256 => ObjectHash::Sha256([0x2a; 32]),
                HashKind::Blake3 => unreachable!("fixture kinds are fixed"),
            };
            let mut bytes = Vec::new();
            if version == IdxVersion::V2 {
                bytes.extend_from_slice(&IDX_MAGIC);
                bytes.extend_from_slice(&2u32.to_be_bytes());
            }
            for bucket in 0..=u8::MAX {
                bytes.extend_from_slice(&(u32::from(bucket >= 0x2a)).to_be_bytes());
            }
            if version == IdxVersion::V1 {
                bytes.extend_from_slice(&17u32.to_be_bytes());
            }
            bytes.extend_from_slice(hash.as_ref());
            if version == IdxVersion::V2 {
                bytes.extend_from_slice(&[0; 4]);
                bytes.extend_from_slice(&0x8000_0000u32.to_be_bytes());
                bytes.extend_from_slice(&0x1_0000_0001u64.to_be_bytes());
                bytes.extend_from_slice(&vec![0; kind.size() * 2]);
            }
            fs::write(&path, bytes).expect("write index");
            assert_eq!(
                LocalStorage::read_idx(&path, &hash).expect("lookup"),
                Some(if version == IdxVersion::V1 {
                    17
                } else {
                    0x1_0000_0001
                })
            );
            let missing = match kind {
                HashKind::Sha1 => ObjectHash::Sha1([0x2b; 20]),
                HashKind::Sha256 => ObjectHash::Sha256([0x2b; 32]),
                HashKind::Blake3 => unreachable!("fixture kinds are fixed"),
            };
            assert_eq!(LocalStorage::read_idx(&path, &missing).expect("miss"), None);
        }
    }

    pub(super) fn status_pack_lookup_reduces_reads_on_fixed_bucket() {
        struct Counted<R> {
            inner: R,
            reads: usize,
            seeks: usize,
            end_seeks: usize,
        }

        impl<R: Read> Read for Counted<R> {
            fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
                self.reads += 1;
                self.inner.read(buf)
            }
        }

        impl<R: Seek> Seek for Counted<R> {
            fn seek(&mut self, pos: io::SeekFrom) -> io::Result<u64> {
                self.seeks += 1;
                self.end_seeks += usize::from(matches!(pos, io::SeekFrom::End(_)));
                self.inner.seek(pos)
            }
        }

        // Reference the pre-SP-07 v2 query for this fixture's 32-bit offsets.
        fn old_lookup(
            reader: &mut (impl Read + Seek),
            target: &ObjectHash,
        ) -> io::Result<Option<u64>> {
            assert_eq!(LocalStorage::read_idx_version(reader)?, IdxVersion::V2);
            reader.seek(io::SeekFrom::Start(8))?;
            let mut fanout = [0u32; 256];
            for count in &mut fanout {
                *count = reader.read_u32::<BigEndian>()?;
            }
            let bucket = usize::from(target.as_ref()[0]);
            let start = if bucket == 0 { 0 } else { fanout[bucket - 1] };
            let end = fanout[bucket];
            let names = FANOUT + 8;
            reader.seek(io::SeekFrom::Start(names + u64::from(start) * 20))?;
            for index in start..end {
                if read_sha(reader)? == *target {
                    let offsets = names + u64::from(fanout[255]) * 24;
                    reader.seek(io::SeekFrom::Start(offsets + u64::from(index) * 4))?;
                    return Ok(Some(u64::from(reader.read_u32::<BigEndian>()?)));
                }
            }
            Ok(None)
        }

        let _kind = git_internal::hash::set_hash_kind_for_test(HashKind::Sha1);
        let mut bytes = Vec::new();
        bytes.extend_from_slice(&IDX_MAGIC);
        bytes.extend_from_slice(&2u32.to_be_bytes());
        for bucket in 0..=u8::MAX {
            bytes.extend_from_slice(&(if bucket < 0x2a { 0u32 } else { 1024 }).to_be_bytes());
        }
        let mut target = ObjectHash::Sha1([0; 20]);
        for index in 0..1024u16 {
            let mut name = [0; 20];
            name[0] = 0x2a;
            name[1..3].copy_from_slice(&index.to_be_bytes());
            bytes.extend_from_slice(&name);
            if index == 1023 {
                target = ObjectHash::Sha1(name);
            }
        }
        bytes.extend_from_slice(&vec![0; 1024 * 4]);
        for index in 0..1024u32 {
            bytes.extend_from_slice(&(index + 1).to_be_bytes());
        }
        let dir = tempfile::tempdir().expect("temporary index directory");
        let path = dir.path().join("fixed.idx");
        fs::write(&path, bytes).expect("write fixed packed index fixture");

        let mut binary_times = Vec::new();
        let mut linear_times = Vec::new();
        for _ in 0..21 {
            let mut reader = Counted {
                inner: fs::File::open(&path).expect("open binary fixture"),
                reads: 0,
                seeks: 0,
                end_seeks: 0,
            };
            let start = Instant::now();
            let offset = LocalStorage::read_idx_from_reader(&mut reader, &target)
                .expect("complete single-object lookup");
            binary_times.push(start.elapsed());
            assert_eq!(offset, Some(1024));
            assert!(reader.reads <= 15, "binary reads: {}", reader.reads);
            assert_eq!(reader.seeks, 13);
            assert_eq!(reader.end_seeks, 1);

            let mut reader = Counted {
                inner: fs::File::open(&path).expect("open linear fixture"),
                reads: 0,
                seeks: 0,
                end_seeks: 0,
            };
            let start = Instant::now();
            let offset = old_lookup(&mut reader, &target).expect("old complete lookup");
            linear_times.push(start.elapsed());
            assert_eq!(offset, Some(1024));
            assert_eq!(reader.reads, 1283);
            assert_eq!(reader.seeks, 3);
        }
        binary_times.sort();
        linear_times.sort();
        eprintln!(
            "fixed 1024-name index, 21 complete file-backed queries (warm cache, excluding open): binary median {:?}, p95 {:?}, reads <=15, seeks 13; old linear median {:?}, p95 {:?}, reads 1283, seeks 3",
            binary_times[10], binary_times[19], linear_times[10], linear_times[19]
        );

        let mut reader = Counted {
            inner: fs::File::open(&path).expect("open batch fixture"),
            reads: 0,
            seeks: 0,
            end_seeks: 0,
        };
        let (version, fanout) =
            LocalStorage::read_idx_fanout_from_open(&mut reader).expect("batch fanout");
        reader.seeks = 0;
        reader.reads = 0;
        for _ in 0..32 {
            assert_eq!(
                LocalStorage::read_idx_from_open_binary(&mut reader, version, &fanout, &target)
                    .expect("batch hit"),
                Some(1024)
            );
            let before = (reader.reads, reader.seeks);
            assert_eq!(
                LocalStorage::read_idx_from_open_binary(
                    &mut reader,
                    version,
                    &fanout,
                    &ObjectHash::Sha1([0xff; 20]),
                )
                .expect("empty bucket miss"),
                None
            );
            assert_eq!((reader.reads, reader.seeks), before);
        }
        assert_eq!(reader.end_seeks, 0);
        assert_eq!(reader.reads, 32 * 11);
        assert_eq!(reader.seeks, 32 * 11);
    }

    #[tokio::test]
    async fn checked_batch_rejects_pack_without_index() {
        let _kind = git_internal::hash::set_hash_kind_for_test(HashKind::Sha1);
        let dir = tempfile::tempdir().expect("create object directory");
        let storage = LocalStorage::try_new(dir.path().to_path_buf()).expect("storage");
        let pack_dir = dir.path().join("pack");
        fs::create_dir(&pack_dir).expect("create pack directory");
        let pack = pack_dir.join("unindexed.pack");
        let fixture =
            Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/data/packs/ofs-delta-sha1.pack");
        fs::copy(&fixture, &pack).expect("copy pack fixture");

        let object = ObjectHash::Sha1([0x44; 20]);
        let error = storage
            .exist_checked_batch(&[object])
            .await
            .expect_err("an unindexed pack is not a confirmed object miss");
        assert!(
            error.to_string().contains("has no complete index"),
            "{error}"
        );
        assert!(error.to_string().contains("libra index-pack"), "{error}");
        assert!(error.to_string().contains(&object.to_string()), "{error}");
        assert!(!pack.with_extension("idx").exists());
    }

    #[tokio::test]
    async fn checked_batch_verifies_alternate_payload_bytes() {
        let _kind = git_internal::hash::set_hash_kind_for_test(HashKind::Sha1);
        let primary_dir = tempfile::tempdir().expect("create primary object directory");
        let alternate_dir = tempfile::tempdir().expect("create alternate object directory");
        let alternate = LocalStorage::try_new(alternate_dir.path().to_path_buf()).expect("storage");
        let payload = b"verified borrowed payload";
        let object = ObjectHash::from_type_and_data(ObjectType::Blob, payload);
        alternate
            .put(&object, payload, ObjectType::Blob)
            .await
            .expect("store alternate object");
        let mut primary = LocalStorage::try_new(primary_dir.path().to_path_buf()).expect("storage");
        primary.alternates.push(Arc::new(alternate.clone()));
        let result = primary
            .exist_checked_batch(&[object])
            .await
            .expect("verify borrowed object");
        assert_eq!(result.get(&object), Some(&true));

        let tampered = LocalStorage::compress_zlib(b"blob 8\0tampered")
            .expect("compress tampered alternate object");
        fs::write(alternate.get_obj_path(&object), tampered)
            .expect("replace alternate payload at original OID");
        let error = primary
            .exist_checked_batch(&[object])
            .await
            .expect_err("borrowed object contents must match requested OID");
        assert!(error.to_string().contains(&object.to_string()), "{error}");
    }

    #[tokio::test]
    async fn decoded_object_sizes_separates_decoded_length_from_delta_load_cost() {
        let _kind = git_internal::hash::set_hash_kind_for_test(HashKind::Sha1);
        let primary_dir = tempfile::tempdir().expect("create primary object directory");
        let alternate_dir = tempfile::tempdir().expect("create alternate object directory");

        let pack_dir = primary_dir.path().join("pack");
        fs::create_dir(&pack_dir).expect("create pack directory");
        let pack = pack_dir.join("ofs-delta-sha1.pack");
        let idx = pack.with_extension("idx");
        let fixture =
            Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/data/packs/ofs-delta-sha1.pack");
        fs::copy(&fixture, &pack).expect("copy fixture pack");
        command::index_pack::build_index_v1(
            pack.to_str().expect("UTF-8 pack path"),
            idx.to_str().expect("UTF-8 idx path"),
        )
        .expect("build v1 index");

        let alternate = LocalStorage::try_new(alternate_dir.path().to_path_buf()).expect("storage");
        let borrowed_payload = b"borrowed from alternate store";
        let borrowed = ObjectHash::from_type_and_data(ObjectType::Blob, borrowed_payload);
        alternate
            .put(&borrowed, borrowed_payload, ObjectType::Blob)
            .await
            .expect("store alternate blob");

        let mut primary = LocalStorage::try_new(primary_dir.path().to_path_buf()).expect("storage");
        primary.hash_kind = Some(HashKind::Sha1);
        primary.alternates.push(Arc::new(alternate));

        let loose_payload = b"primary loose payload";
        let loose = ObjectHash::from_type_and_data(ObjectType::Blob, loose_payload);
        primary
            .put(&loose, loose_payload, ObjectType::Blob)
            .await
            .expect("store loose blob");

        let ofs_delta = crate::internal::object_format::parse_repo_oid(
            "1b59abc09609574e73330d56815f04ebb4d9bd72",
        )
        .expect("parse OFS_DELTA OID");
        let missing = ObjectHash::Sha1([0xee; 20]);

        let decoded = primary
            .decoded_object_sizes(&[loose, ofs_delta, borrowed, missing])
            .await
            .expect("query decoded object sizes");
        assert_eq!(
            decoded,
            vec![
                Some(loose_payload.len() as u64),
                Some(10_400),
                Some(borrowed_payload.len() as u64),
                None,
            ]
        );
        assert_eq!(
            primary
                .decoded_object_size(&ofs_delta)
                .await
                .expect("single decoded size"),
            Some(10_400)
        );

        let load_cost = primary
            .object_size(&ofs_delta)
            .await
            .expect("query conservative load cost")
            .expect("OFS_DELTA load cost");
        assert!(
            load_cost > 10_400,
            "object_size must remain the conservative delta reconstruction budget: {load_cost}"
        );
        let over_budget = primary
            .get_with_limit(&ofs_delta, 10_400)
            .await
            .expect_err("preview limit equal to decoded size must still reject delta peak_bytes");
        assert!(
            over_budget.to_string().contains("exceeds preview limit"),
            "{over_budget}"
        );
    }

    #[test]
    fn try_new_ok() {
        let dir = tempfile::tempdir().expect("dir");
        let objects = dir.path().join("objects");
        let store = LocalStorage::try_new(objects.clone()).expect("try_new");
        assert!(objects.is_dir(), "try_new creates the objects directory");
        assert_eq!(store.base_path, objects);
    }

    #[test]
    fn try_new_with_alternates_ok() {
        let dir = tempfile::tempdir().expect("dir");
        let objects = dir.path().join("objects");
        let store = LocalStorage::try_new_with_alternates(objects.clone()).expect("alternates");
        assert!(
            objects.is_dir(),
            "try_new_with_alternates creates the objects directory"
        );
        assert!(
            store.alternates.is_empty(),
            "a fresh directory has no alternate chain"
        );
        assert_eq!(store.base_path, objects);
    }

    #[test]
    fn try_new_failure_returns_error() {
        let file = tempfile::NamedTempFile::new().expect("file");
        let blocked = file.path().join("objects");
        let error = match LocalStorage::try_new(blocked.clone()) {
            Ok(_) => panic!("mkdir through a file returned storage"),
            Err(error) => error,
        };
        let rendered = error.to_string();
        assert!(
            rendered.contains(&blocked.display().to_string()),
            "mkdir failure must name the path: {rendered}"
        );
        assert!(
            rendered.contains("create_dir_all failed"),
            "mkdir failure must name the operation: {rendered}"
        );
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn test_shared_repository_put_permissions() {
        use std::os::unix::fs::PermissionsExt;

        use crate::utils::shared_perm::{
            SharedMode, clear_cached_repo_shared_mode, set_cached_repo_shared_mode,
        };

        let _kind = git_internal::hash::set_hash_kind_for_test(HashKind::Sha1);
        let temp = tempfile::tempdir().expect("create temp dir");
        let storage_root = temp.path().join(".libra");
        let objects_dir = storage_root.join("objects");
        fs::create_dir_all(&objects_dir).expect("create objects dir");
        fs::write(storage_root.join("libra.db"), b"dummy").expect("write db");

        // Set cached shared mode to Group
        set_cached_repo_shared_mode(storage_root.clone(), Some(SharedMode::Group));

        let storage = LocalStorage::try_new(objects_dir.clone()).expect("storage");
        let payload = b"shared object content";
        let object = ObjectHash::from_type_and_data(ObjectType::Blob, payload);
        let written_path = storage
            .put(&object, payload, ObjectType::Blob)
            .await
            .expect("put object");

        let obj_path = PathBuf::from(written_path);
        assert!(obj_path.exists());
        let obj_mode = fs::metadata(&obj_path).unwrap().permissions().mode() & 0o7777;
        assert_eq!(
            obj_mode, 0o444,
            "loose object mode must be 0444 in shared repo"
        );

        let parent_dir = obj_path.parent().unwrap();
        let dir_mode = fs::metadata(parent_dir).unwrap().permissions().mode() & 0o7777;
        assert_eq!(
            dir_mode, 0o2775,
            "fanout directory mode must be 2775 in shared repo"
        );

        clear_cached_repo_shared_mode(Some(&storage_root));
    }
}

#[cfg(test)]
#[test]
fn status_pack_lookup_rejects_truncated_and_corrupt_indexes() {
    tests::status_pack_lookup_rejects_truncated_and_corrupt_indexes();
}

#[cfg(test)]
#[test]
fn status_pack_lookup_matches_v1_v2_and_hash_kinds() {
    tests::status_pack_lookup_matches_v1_v2_and_hash_kinds();
}

#[cfg(test)]
#[test]
fn status_pack_lookup_reduces_reads_on_fixed_bucket() {
    tests::status_pack_lookup_reduces_reads_on_fixed_bucket();
}
