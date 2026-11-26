use std::{
    collections::{HashMap, HashSet},
    fs,
    io::{self, BufReader, Read, Seek, SeekFrom},
    path::{Path, PathBuf},
};

use git_internal::{errors::GitError, hash::ObjectHash};

const IDX_MAGIC: [u8; 4] = [0xff, 0x74, 0x4f, 0x63];
const FANOUT_BYTES: u64 = 256 * 4;
const MAX_DELTA_DEPTH: usize = 128;
const MAX_VALIDATED_DELTA_BYTES: u64 = crate::utils::preview_object::MAX_OBJECT_BYTES;

#[derive(Debug, Clone, Copy)]
enum IndexVersion {
    V1,
    V2,
}

#[cfg(test)]
#[derive(Debug, Default)]
pub(super) struct ProbeStats {
    pub(super) index_opens: usize,
}

#[derive(Default)]
struct Stats {
    index_opens: usize,
    object_probes: usize,
}

#[derive(Clone, Copy)]
struct LoadCost {
    result_size: u64,
    peak_bytes: u64,
}

struct PackIndex {
    file: fs::File,
    path: PathBuf,
    version: IndexVersion,
    fanout: [u32; 256],
    object_count: u64,
    hash_size: u64,
}

impl PackIndex {
    fn open(path: PathBuf, stats: &mut Stats) -> Result<Self, GitError> {
        let mut file = fs::File::open(&path)?;
        stats.index_opens += 1;
        let mut first = [0u8; 4];
        file.read_exact(&mut first)?;
        let version = if first == IDX_MAGIC {
            let version = read_u32(&mut file)?;
            if version != 2 {
                return Err(invalid(format!(
                    "unsupported pack index version {version} in {}",
                    path.display()
                )));
            }
            IndexVersion::V2
        } else {
            file.seek(SeekFrom::Start(0))?;
            IndexVersion::V1
        };
        let mut fanout = [0u32; 256];
        for value in &mut fanout {
            *value = read_u32(&mut file)?;
        }
        let object_count = u64::from(fanout[255]);
        let hash_size = git_internal::hash::get_hash_kind().size() as u64;
        if matches!(version, IndexVersion::V1) && hash_size != 20 {
            return Err(invalid(format!(
                "pack index v1 at {} only supports SHA-1",
                path.display()
            )));
        }
        Ok(Self {
            file,
            path,
            version,
            fanout,
            object_count,
            hash_size,
        })
    }

    /// Resolve all requested hashes while scanning each relevant fanout bucket
    /// at most once. The index file remains open for REF_DELTA base lookups.
    fn lookup_many(
        &mut self,
        hashes: &HashSet<ObjectHash>,
    ) -> Result<HashMap<ObjectHash, u64>, GitError> {
        let mut by_bucket: HashMap<u8, HashSet<ObjectHash>> = HashMap::new();
        for hash in hashes {
            by_bucket.entry(hash.as_ref()[0]).or_default().insert(*hash);
        }

        let mut indices = HashMap::new();
        for (bucket, wanted) in by_bucket {
            let start = if bucket == 0 {
                0
            } else {
                u64::from(self.fanout[bucket as usize - 1])
            };
            let end = u64::from(self.fanout[bucket as usize]);
            match self.version {
                IndexVersion::V1 => {
                    self.file.seek(SeekFrom::Start(FANOUT_BYTES + 24 * start))?;
                    for index in start..end {
                        let _offset = read_u32(&mut self.file)?;
                        let hash = read_hash(&mut self.file, self.hash_size)?;
                        if wanted.contains(&hash) {
                            indices.insert(hash, index);
                        }
                    }
                }
                IndexVersion::V2 => {
                    let names = FANOUT_BYTES + 8;
                    self.file
                        .seek(SeekFrom::Start(names + self.hash_size * start))?;
                    for index in start..end {
                        let hash = read_hash(&mut self.file, self.hash_size)?;
                        if wanted.contains(&hash) {
                            indices.insert(hash, index);
                        }
                    }
                }
            }
        }

        let mut found = HashMap::with_capacity(indices.len());
        for (hash, index) in indices {
            found.insert(hash, self.read_offset(index)?);
        }
        Ok(found)
    }

    fn lookup_one(&mut self, hash: ObjectHash) -> Result<Option<u64>, GitError> {
        let mut hashes = HashSet::with_capacity(1);
        hashes.insert(hash);
        Ok(self.lookup_many(&hashes)?.remove(&hash))
    }

    fn read_offset(&mut self, index: u64) -> Result<u64, GitError> {
        match self.version {
            IndexVersion::V1 => {
                self.file.seek(SeekFrom::Start(FANOUT_BYTES + 24 * index))?;
                Ok(u64::from(read_u32(&mut self.file)?))
            }
            IndexVersion::V2 => {
                let names = FANOUT_BYTES + 8;
                let crc = checked_add(
                    names,
                    checked_mul(self.object_count, self.hash_size, "index name table")?,
                    "index CRC table",
                )?;
                let offsets = checked_add(
                    crc,
                    checked_mul(self.object_count, 4, "index CRC table")?,
                    "index offset table",
                )?;
                self.file.seek(SeekFrom::Start(checked_add(
                    offsets,
                    checked_mul(index, 4, "index offset entry")?,
                    "index offset entry",
                )?))?;
                let offset = read_u32(&mut self.file)?;
                if offset & 0x8000_0000 == 0 {
                    return Ok(u64::from(offset));
                }
                let large_index = u64::from(offset & 0x7fff_ffff);
                let large_offsets = checked_add(
                    offsets,
                    checked_mul(self.object_count, 4, "index offset table")?,
                    "large index offset table",
                )?;
                self.file.seek(SeekFrom::Start(checked_add(
                    large_offsets,
                    checked_mul(large_index, 8, "large index offset entry")?,
                    "large index offset entry",
                )?))?;
                read_u64(&mut self.file).map_err(GitError::from)
            }
        }
    }
}

enum ProbeFailure {
    Oversized(GitError),
    Fatal(GitError),
}

impl ProbeFailure {
    fn into_git_error(self) -> GitError {
        match self {
            Self::Oversized(error) | Self::Fatal(error) => error,
        }
    }
}

impl From<GitError> for ProbeFailure {
    fn from(error: GitError) -> Self {
        Self::Fatal(error)
    }
}

impl From<io::Error> for ProbeFailure {
    fn from(error: io::Error) -> Self {
        Self::Fatal(GitError::from(error))
    }
}

#[derive(Clone, Copy)]
enum PackQuery {
    LoadCost { aggregate_limit: Option<u64> },
    DecodedSize,
}

struct PackProbe {
    pack: BufReader<fs::File>,
    pack_path: PathBuf,
    index: PackIndex,
    cache: HashMap<u64, LoadCost>,
    oversized: HashMap<u64, String>,
    visiting: HashSet<u64>,
}

impl PackProbe {
    fn new(pack_path: PathBuf, index: PackIndex) -> Result<Self, GitError> {
        let pack = BufReader::new(fs::File::open(&pack_path)?);
        Ok(Self {
            pack,
            pack_path,
            index,
            cache: HashMap::new(),
            oversized: HashMap::new(),
            visiting: HashSet::new(),
        })
    }

    fn cost_at(&mut self, object_offset: u64, depth: usize) -> Result<LoadCost, GitError> {
        self.probe_at(object_offset, depth)
            .map_err(ProbeFailure::into_git_error)
    }

    fn decoded_size_at(
        &mut self,
        object_offset: u64,
        depth: usize,
    ) -> Result<Option<u64>, GitError> {
        match self.probe_at(object_offset, depth) {
            Ok(cost) => Ok(Some(cost.result_size)),
            Err(ProbeFailure::Oversized(_)) => Ok(None),
            Err(ProbeFailure::Fatal(error)) => Err(error),
        }
    }

    fn probe_at(&mut self, object_offset: u64, depth: usize) -> Result<LoadCost, ProbeFailure> {
        if let Some(cost) = self.cache.get(&object_offset) {
            return Ok(*cost);
        }
        if let Some(message) = self.oversized.get(&object_offset) {
            return Err(ProbeFailure::Oversized(invalid(message.clone())));
        }
        if depth >= MAX_DELTA_DEPTH {
            return Err(ProbeFailure::Fatal(invalid(format!(
                "delta chain at offset {object_offset} in {} exceeds depth {MAX_DELTA_DEPTH}",
                self.pack_path.display()
            ))));
        }
        if !self.visiting.insert(object_offset) {
            return Err(ProbeFailure::Fatal(invalid(format!(
                "delta cycle at offset {object_offset} in {}",
                self.pack_path.display()
            ))));
        }
        let result = self.read_cost(object_offset, depth);
        self.visiting.remove(&object_offset);
        match result {
            Ok(cost) => {
                self.cache.insert(object_offset, cost);
                Ok(cost)
            }
            Err(ProbeFailure::Oversized(error)) => {
                if let GitError::InvalidObjectInfo(ref message) = error {
                    self.oversized.insert(object_offset, message.clone());
                }
                Err(ProbeFailure::Oversized(error))
            }
            Err(ProbeFailure::Fatal(error)) => Err(ProbeFailure::Fatal(error)),
        }
    }

    fn read_cost(&mut self, object_offset: u64, depth: usize) -> Result<LoadCost, ProbeFailure> {
        self.pack.seek(SeekFrom::Start(object_offset))?;
        let (kind, encoded_size) = read_pack_header(&mut self.pack)?;
        match kind {
            1..=4 => {
                validate_zlib_payload_probe(
                    &mut self.pack,
                    encoded_size,
                    &self.pack_path,
                    object_offset,
                )?;
                Ok(LoadCost {
                    result_size: encoded_size,
                    peak_bytes: encoded_size,
                })
            }
            6 => {
                let distance = read_ofs_distance(&mut self.pack)?;
                let base_offset = object_offset.checked_sub(distance).ok_or_else(|| {
                    ProbeFailure::Fatal(invalid(format!(
                        "OFS_DELTA at {object_offset} in {} points before the pack",
                        self.pack_path.display()
                    )))
                })?;
                self.read_delta_probe(object_offset, encoded_size, base_offset, depth)
            }
            7 => {
                let base_hash = read_hash(
                    &mut self.pack,
                    git_internal::hash::get_hash_kind().size() as u64,
                )?;
                let base_offset = self.index.lookup_one(base_hash)?.ok_or_else(|| {
                    ProbeFailure::Fatal(invalid(format!(
                        "REF_DELTA base {base_hash} is absent from {}",
                        self.index.path.display()
                    )))
                })?;
                self.read_delta_probe(object_offset, encoded_size, base_offset, depth)
            }
            other => Err(ProbeFailure::Fatal(invalid(format!(
                "unsupported pack object type {other} at offset {object_offset} in {}",
                self.pack_path.display()
            )))),
        }
    }

    #[cfg(test)]
    fn read_delta_cost(
        &mut self,
        object_offset: u64,
        encoded_size: u64,
        base_offset: u64,
        depth: usize,
    ) -> Result<LoadCost, GitError> {
        self.read_delta_probe(object_offset, encoded_size, base_offset, depth)
            .map_err(ProbeFailure::into_git_error)
    }

    fn read_delta_probe(
        &mut self,
        object_offset: u64,
        encoded_size: u64,
        base_offset: u64,
        depth: usize,
    ) -> Result<LoadCost, ProbeFailure> {
        // The pack reader currently points at this delta's zlib stream. Validate
        // the bounded instruction stream before seeking recursively to its base.
        if encoded_size > MAX_VALIDATED_DELTA_BYTES {
            return Err(ProbeFailure::Oversized(invalid(format!(
                "packed delta at offset {object_offset} in {} declares {encoded_size} instruction bytes, which exceeds preview limit of {MAX_VALIDATED_DELTA_BYTES} bytes",
                self.pack_path.display()
            ))));
        }
        let (declared_base, result_size) =
            validate_delta_stream(&mut self.pack, encoded_size, &self.pack_path, object_offset)?;
        let base = self.probe_at(base_offset, depth + 1)?;
        if declared_base != base.result_size {
            return Err(ProbeFailure::Fatal(invalid(format!(
                "delta at offset {object_offset} in {} declares base size {declared_base}, but its base has size {}",
                self.pack_path.display(),
                base.result_size
            ))));
        }
        combine_delta_cost(base, encoded_size, result_size).map_err(ProbeFailure::from)
    }
}

fn combine_delta_cost(
    base: LoadCost,
    encoded_size: u64,
    result_size: u64,
) -> Result<LoadCost, GitError> {
    let chain_peak = checked_add(base.peak_bytes, encoded_size, "delta chain load cost")?;
    let rebuild_peak = checked_add(
        checked_add(base.result_size, encoded_size, "delta rebuild load cost")?,
        result_size,
        "delta rebuild load cost",
    )?;
    Ok(LoadCost {
        result_size,
        peak_bytes: chain_peak.max(rebuild_peak),
    })
}

pub(super) fn load_costs(
    pack_dir: &Path,
    hashes: &[ObjectHash],
) -> Result<Vec<Option<u64>>, GitError> {
    load_costs_inner(pack_dir, hashes, None, &mut Stats::default())
}

pub(super) fn load_costs_with_limit(
    pack_dir: &Path,
    hashes: &[ObjectHash],
    aggregate_limit: u64,
) -> Result<Vec<Option<u64>>, GitError> {
    load_costs_inner(
        pack_dir,
        hashes,
        Some(aggregate_limit),
        &mut Stats::default(),
    )
}

pub(super) fn decoded_sizes(
    pack_dir: &Path,
    hashes: &[ObjectHash],
) -> Result<Vec<Option<u64>>, GitError> {
    probe_packs_inner(
        pack_dir,
        hashes,
        PackQuery::DecodedSize,
        &mut Stats::default(),
    )
}

fn load_costs_inner(
    pack_dir: &Path,
    hashes: &[ObjectHash],
    aggregate_limit: Option<u64>,
    stats: &mut Stats,
) -> Result<Vec<Option<u64>>, GitError> {
    probe_packs_inner(
        pack_dir,
        hashes,
        PackQuery::LoadCost { aggregate_limit },
        stats,
    )
}

fn probe_packs_inner(
    pack_dir: &Path,
    hashes: &[ObjectHash],
    query: PackQuery,
    stats: &mut Stats,
) -> Result<Vec<Option<u64>>, GitError> {
    let mut results = vec![None; hashes.len()];
    let mut positions: HashMap<ObjectHash, Vec<usize>> = HashMap::new();
    for (position, hash) in hashes.iter().enumerate() {
        positions.entry(*hash).or_default().push(position);
    }
    let mut unresolved: HashSet<ObjectHash> = positions.keys().copied().collect();
    let mut packs = list_indexed_packs(pack_dir)?;
    packs.sort();
    let mut aggregate_cost = 0u64;

    for pack_path in packs {
        if unresolved.is_empty() {
            break;
        }
        let idx_path = pack_path.with_extension("idx");
        let mut index = PackIndex::open(idx_path, stats)?;
        let offsets = index.lookup_many(&unresolved)?;
        if offsets.is_empty() {
            continue;
        }
        let mut probe = PackProbe::new(pack_path, index)?;
        let mut offsets: Vec<_> = offsets.into_iter().collect();
        offsets.sort_by_key(|(_, offset)| *offset);
        for (hash, offset) in offsets {
            stats.object_probes += 1;
            let measured = match query {
                PackQuery::LoadCost { aggregate_limit } => {
                    let cost = probe.cost_at(offset, 0)?.peak_bytes;
                    aggregate_cost = checked_add(
                        aggregate_cost,
                        crate::utils::preview_object::charged_bytes(cost),
                        "preview aggregate cache load cost",
                    )?;
                    if let Some(limit) = aggregate_limit
                        && aggregate_cost > limit
                    {
                        return Err(invalid(format!(
                            "preview aggregate cache load cost exceeds {limit} bytes"
                        )));
                    }
                    Some(cost)
                }
                PackQuery::DecodedSize => probe.decoded_size_at(offset, 0)?,
            };
            if let Some(value) = measured
                && let Some(requested_positions) = positions.get(&hash)
            {
                for position in requested_positions {
                    results[*position] = Some(value);
                }
            }
            unresolved.remove(&hash);
        }
    }
    Ok(results)
}

#[cfg(test)]
fn load_costs_inner_with_limit(
    pack_dir: &Path,
    hashes: &[ObjectHash],
    aggregate_limit: u64,
    stats: &mut Stats,
) -> Result<Vec<Option<u64>>, GitError> {
    load_costs_inner(pack_dir, hashes, Some(aggregate_limit), stats)
}

fn list_indexed_packs(pack_dir: &Path) -> Result<Vec<PathBuf>, GitError> {
    if !pack_dir.is_dir() {
        return Ok(Vec::new());
    }
    let mut packs = Vec::new();
    for entry in fs::read_dir(pack_dir)? {
        let path = entry?.path();
        if path.is_file()
            && path
                .extension()
                .is_some_and(|extension| extension == "pack")
            && path.with_extension("idx").is_file()
        {
            packs.push(path);
        }
    }
    Ok(packs)
}

fn read_pack_header(reader: &mut impl Read) -> Result<(u8, u64), GitError> {
    let first = read_byte(reader)?;
    let kind = (first >> 4) & 0x07;
    let mut size = u64::from(first & 0x0f);
    let mut shift = 4u32;
    let mut current = first;
    while current & 0x80 != 0 {
        current = read_byte(reader)?;
        let part = u64::from(current & 0x7f);
        if shift >= 64 || part > (u64::MAX >> shift) {
            return Err(invalid("pack object size exceeds u64".to_string()));
        }
        size |= part << shift;
        shift += 7;
    }
    Ok((kind, size))
}

fn read_ofs_distance(reader: &mut impl Read) -> Result<u64, GitError> {
    let mut current = read_byte(reader)?;
    let mut distance = u64::from(current & 0x7f);
    let mut count = 1usize;
    while current & 0x80 != 0 {
        if count >= 10 {
            return Err(invalid("overlong OFS_DELTA base distance".to_string()));
        }
        current = read_byte(reader)?;
        let next = distance
            .checked_add(1)
            .ok_or_else(|| invalid("OFS_DELTA base distance exceeds u64".to_string()))?;
        if next > (u64::MAX >> 7) {
            return Err(invalid("OFS_DELTA base distance exceeds u64".to_string()));
        }
        distance = (next << 7)
            .checked_add(u64::from(current & 0x7f))
            .ok_or_else(|| invalid("OFS_DELTA base distance exceeds u64".to_string()))?;
        count += 1;
    }
    Ok(distance)
}

#[cfg(test)]
fn validate_zlib_payload(
    reader: &mut impl Read,
    declared: u64,
    pack_path: &Path,
    object_offset: u64,
) -> Result<(), GitError> {
    validate_zlib_payload_probe(reader, declared, pack_path, object_offset)
        .map_err(ProbeFailure::into_git_error)
}

fn validate_zlib_payload_probe(
    reader: &mut impl Read,
    declared: u64,
    pack_path: &Path,
    object_offset: u64,
) -> Result<(), ProbeFailure> {
    if declared > crate::utils::preview_object::MAX_OBJECT_BYTES {
        return Err(ProbeFailure::Oversized(invalid(format!(
            "packed object at offset {object_offset} in {} declares {declared} bytes, which exceeds preview limit of {} bytes",
            pack_path.display(),
            crate::utils::preview_object::MAX_OBJECT_BYTES
        ))));
    }
    let mut decoder = flate2::read::ZlibDecoder::new(reader);
    consume_exact(&mut decoder, declared)?;
    let mut extra = [0u8; 1];
    if decoder.read(&mut extra)? != 0 {
        return Err(ProbeFailure::Fatal(invalid(format!(
            "packed object at offset {object_offset} in {} exceeds its declared size {declared}",
            pack_path.display()
        ))));
    }
    Ok(())
}

fn validate_delta_stream(
    reader: &mut impl Read,
    encoded_size: u64,
    pack_path: &Path,
    object_offset: u64,
) -> Result<(u64, u64), GitError> {
    let mut decoder = flate2::read::ZlibDecoder::new(reader);
    let mut consumed = 0u64;
    let base_size = read_delta_varint(&mut decoder, &mut consumed, encoded_size)?;
    let result_size = read_delta_varint(&mut decoder, &mut consumed, encoded_size)?;
    let mut produced = 0u64;
    while consumed < encoded_size {
        let command = read_delta_byte(&mut decoder, &mut consumed, encoded_size)?;
        if command == 0 {
            return Err(invalid(format!(
                "delta at offset {object_offset} in {} contains reserved instruction 0",
                pack_path.display()
            )));
        }
        if command & 0x80 == 0 {
            let literal = u64::from(command);
            consume_delta_bytes(&mut decoder, &mut consumed, encoded_size, literal)?;
            produced = checked_add(produced, literal, "delta result size")?;
            continue;
        }

        let mut copy_offset = 0u64;
        for shift in 0..4 {
            if command & (1 << shift) != 0 {
                let value = u64::from(read_delta_byte(&mut decoder, &mut consumed, encoded_size)?);
                copy_offset |= value << (shift * 8);
            }
        }
        let mut copy_size = 0u64;
        for shift in 0..3 {
            if command & (1 << (shift + 4)) != 0 {
                let value = u64::from(read_delta_byte(&mut decoder, &mut consumed, encoded_size)?);
                copy_size |= value << (shift * 8);
            }
        }
        if copy_size == 0 {
            copy_size = 0x1_0000;
        }
        let copy_end = checked_add(copy_offset, copy_size, "delta copy range")?;
        if copy_end > base_size {
            return Err(invalid(format!(
                "delta at offset {object_offset} in {} copies beyond its declared {base_size}-byte base",
                pack_path.display()
            )));
        }
        produced = checked_add(produced, copy_size, "delta result size")?;
    }
    let mut extra = [0u8; 1];
    if decoder.read(&mut extra)? != 0 {
        return Err(invalid(format!(
            "delta at offset {object_offset} in {} exceeds its declared instruction size {encoded_size}",
            pack_path.display()
        )));
    }
    if produced != result_size {
        return Err(invalid(format!(
            "delta at offset {object_offset} in {} declares result size {result_size}, but instructions produce {produced}",
            pack_path.display()
        )));
    }
    Ok((base_size, result_size))
}

fn read_delta_varint(
    reader: &mut impl Read,
    consumed: &mut u64,
    limit: u64,
) -> Result<u64, GitError> {
    let mut value = 0u64;
    let mut shift = 0u32;
    loop {
        let byte = read_delta_byte(reader, consumed, limit)?;
        let part = u64::from(byte & 0x7f);
        if shift >= 64 || part > (u64::MAX >> shift) {
            return Err(invalid("delta size varint exceeds u64".to_string()));
        }
        value |= part << shift;
        if byte & 0x80 == 0 {
            return Ok(value);
        }
        shift += 7;
    }
}

fn read_delta_byte(reader: &mut impl Read, consumed: &mut u64, limit: u64) -> Result<u8, GitError> {
    if *consumed >= limit {
        return Err(invalid("delta instruction stream is truncated".to_string()));
    }
    let byte = read_byte(reader)?;
    *consumed += 1;
    Ok(byte)
}

fn consume_delta_bytes(
    reader: &mut impl Read,
    consumed: &mut u64,
    limit: u64,
    count: u64,
) -> Result<(), GitError> {
    let end = checked_add(*consumed, count, "delta instruction length")?;
    if end > limit {
        return Err(invalid(
            "delta literal exceeds its instruction stream".to_string(),
        ));
    }
    consume_exact(reader, count)?;
    *consumed = end;
    Ok(())
}

fn consume_exact(reader: &mut impl Read, count: u64) -> Result<(), GitError> {
    let mut remaining = count;
    let mut buffer = [0u8; 64 * 1024];
    while remaining != 0 {
        let wanted = remaining.min(buffer.len() as u64) as usize;
        let read = reader.read(&mut buffer[..wanted])?;
        if read == 0 {
            return Err(invalid(format!(
                "compressed object ended with {remaining} bytes still expected"
            )));
        }
        remaining -= read as u64;
    }
    Ok(())
}

fn read_hash(reader: &mut impl Read, hash_size: u64) -> Result<ObjectHash, GitError> {
    let size = usize::try_from(hash_size)
        .map_err(|error| invalid(format!("invalid object hash size: {error}")))?;
    let mut bytes = vec![0u8; size];
    reader.read_exact(&mut bytes)?;
    ObjectHash::from_bytes_for_kind(git_internal::hash::get_hash_kind(), &bytes)
        .map_err(|error| invalid(format!("invalid object hash in pack index: {error}")))
}

fn read_byte(reader: &mut impl Read) -> Result<u8, GitError> {
    let mut byte = [0u8; 1];
    reader.read_exact(&mut byte)?;
    Ok(byte[0])
}

fn read_u32(reader: &mut impl Read) -> io::Result<u32> {
    let mut bytes = [0u8; 4];
    reader.read_exact(&mut bytes)?;
    Ok(u32::from_be_bytes(bytes))
}

fn read_u64(reader: &mut impl Read) -> io::Result<u64> {
    let mut bytes = [0u8; 8];
    reader.read_exact(&mut bytes)?;
    Ok(u64::from_be_bytes(bytes))
}

fn checked_add(left: u64, right: u64, context: &str) -> Result<u64, GitError> {
    left.checked_add(right)
        .ok_or_else(|| invalid(format!("{context} exceeds u64")))
}

fn checked_mul(left: u64, right: u64, context: &str) -> Result<u64, GitError> {
    left.checked_mul(right)
        .ok_or_else(|| invalid(format!("{context} exceeds u64")))
}

fn invalid(message: String) -> GitError {
    GitError::InvalidObjectInfo(message)
}

#[cfg(test)]
pub(super) fn load_costs_with_stats(
    pack_dir: &Path,
    hashes: &[ObjectHash],
) -> Result<(Vec<Option<u64>>, ProbeStats), GitError> {
    let mut stats = Stats::default();
    let costs = load_costs_inner(pack_dir, hashes, None, &mut stats)?;
    Ok((
        costs,
        ProbeStats {
            index_opens: stats.index_opens,
        },
    ))
}

#[cfg(test)]
mod tests {

    use git_internal::hash::{HashKind, set_hash_kind};

    use super::*;

    #[test]
    #[serial_test::serial(hash_kind)]
    fn batch_probe_opens_one_index_and_charges_the_delta_chain() {
        set_hash_kind(HashKind::Sha1);
        let temp = tempfile::tempdir().expect("create pack fixture");
        let pack_dir = temp.path().join("pack");
        std::fs::create_dir(&pack_dir).expect("create pack directory");
        let pack = pack_dir.join("ofs-delta-sha1.pack");
        let idx = pack.with_extension("idx");
        let fixture =
            Path::new(env!("LIBRA_WORKSPACE_ROOT")).join("tests/data/packs/ofs-delta-sha1.pack");
        std::fs::copy(&fixture, &pack).expect("copy pack fixture");
        crate::internal::index_pack::build_index_v1(
            pack.to_str().expect("UTF-8 pack path"),
            idx.to_str().expect("UTF-8 idx path"),
        )
        .expect("build fixture index");
        let delta = crate::internal::object_format::parse_repo_oid(
            "1b59abc09609574e73330d56815f04ebb4d9bd72",
        )
        .expect("parse delta OID");

        let (costs, stats) =
            load_costs_with_stats(&pack_dir, &[delta, delta]).expect("batch probe packed delta");
        assert_eq!(stats.index_opens, 1);
        assert_eq!(costs[0], costs[1]);
        assert!(
            costs[0].is_some_and(|cost| cost > 10_400),
            "delta cost must include its base and instruction stream: {costs:?}"
        );
    }

    #[test]
    #[serial_test::serial(hash_kind)]
    fn aggregate_limit_stops_before_probing_later_pack_objects() {
        set_hash_kind(HashKind::Sha1);
        let temp = tempfile::tempdir().expect("create pack fixture");
        let pack_dir = temp.path().join("pack");
        std::fs::create_dir(&pack_dir).expect("create pack directory");
        let pack = pack_dir.join("aggregate-limit.pack");
        let idx = pack.with_extension("idx");
        let fixture =
            Path::new(env!("LIBRA_WORKSPACE_ROOT")).join("tests/data/packs/small-sha1.pack");
        std::fs::copy(&fixture, &pack).expect("copy pack fixture");
        crate::internal::index_pack::build_index_v1(
            pack.to_str().expect("UTF-8 pack path"),
            idx.to_str().expect("UTF-8 idx path"),
        )
        .expect("build fixture index");

        let mut index = PackIndex::open(idx, &mut Stats::default()).expect("open fixture index");
        index
            .file
            .seek(SeekFrom::Start(FANOUT_BYTES))
            .expect("seek v1 object table");
        let mut hashes = Vec::new();
        for _ in 0..index.object_count {
            read_u32(&mut index.file).expect("read v1 pack offset");
            hashes.push(read_hash(&mut index.file, index.hash_size).expect("read object hash"));
        }
        assert!(hashes.len() > 1, "fixture must contain multiple objects");
        let first_cost = load_costs(&pack_dir, &hashes[..1]).expect("probe first packed object")[0]
            .expect("first packed object must be present");
        assert!(
            first_cost < 4_096,
            "fixture must prove the minimum-charge boundary: {first_cost}"
        );

        let mut stats = Stats::default();
        let error = load_costs_inner_with_limit(&pack_dir, &hashes, 4_095, &mut stats)
            .expect_err("the first tiny object must exceed the charged aggregate budget");
        assert!(error.to_string().contains("aggregate"), "{error}");
        assert_eq!(
            stats.object_probes, 1,
            "objects after the aggregate limit is crossed must not be decompressed"
        );
    }

    #[test]
    #[serial_test::serial(hash_kind)]
    fn malformed_delta_instruction_is_rejected_without_reconstruction() {
        let mut encoded = Vec::new();
        {
            use std::io::Write as _;
            let mut encoder =
                flate2::write::ZlibEncoder::new(&mut encoded, flate2::Compression::default());
            encoder
                .write_all(&[1, 1, 0])
                .expect("write invalid delta stream");
            encoder.finish().expect("finish invalid delta stream");
        }
        let error =
            validate_delta_stream(&mut encoded.as_slice(), 3, Path::new("malformed.pack"), 12)
                .expect_err("reserved instruction must fail closed");
        assert!(error.to_string().contains("instruction 0"), "{error}");
    }

    #[test]
    #[serial_test::serial(hash_kind)]
    fn metadata_varints_reject_overflow_without_panicking() {
        let mut pack_header =
            io::Cursor::new([0xb0, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0x7f]);
        assert!(read_pack_header(&mut pack_header).is_err());

        let mut delta_size = io::Cursor::new([0xff; 11]);
        let mut consumed = 0;
        assert!(read_delta_varint(&mut delta_size, &mut consumed, 11).is_err());
    }

    #[test]
    #[serial_test::serial(hash_kind)]
    fn delta_cost_charges_oversized_base_and_instruction_payloads() {
        let oversized_base = LoadCost {
            result_size: 40 * 1024 * 1024,
            peak_bytes: 40 * 1024 * 1024,
        };
        let base_cost = combine_delta_cost(oversized_base, 3, 1).expect("combine base cost");
        assert!(base_cost.peak_bytes > MAX_VALIDATED_DELTA_BYTES);

        let small_base = LoadCost {
            result_size: 1,
            peak_bytes: 1,
        };
        let instruction_cost = combine_delta_cost(small_base, MAX_VALIDATED_DELTA_BYTES + 1, 1)
            .expect("combine instruction cost");
        assert!(instruction_cost.peak_bytes > MAX_VALIDATED_DELTA_BYTES);
    }

    #[test]
    #[serial_test::serial(hash_kind)]
    fn oversized_delta_declaration_is_rejected_before_reading_its_base() {
        set_hash_kind(HashKind::Sha1);
        let temp = tempfile::tempdir().expect("create pack fixture");
        let pack_dir = temp.path().join("pack");
        std::fs::create_dir(&pack_dir).expect("create pack directory");
        let pack = pack_dir.join("ofs-delta-sha1.pack");
        let idx = pack.with_extension("idx");
        let fixture =
            Path::new(env!("LIBRA_WORKSPACE_ROOT")).join("tests/data/packs/ofs-delta-sha1.pack");
        std::fs::copy(&fixture, &pack).expect("copy pack fixture");
        crate::internal::index_pack::build_index_v1(
            pack.to_str().expect("UTF-8 pack path"),
            idx.to_str().expect("UTF-8 idx path"),
        )
        .expect("build fixture index");
        let index = PackIndex::open(idx, &mut Stats::default()).expect("open fixture index");
        let mut probe = PackProbe::new(pack, index).expect("open fixture probe");

        let error = match probe.read_delta_cost(12, MAX_VALIDATED_DELTA_BYTES + 1, u64::MAX / 2, 0)
        {
            Ok(_) => panic!("oversized delta must fail before its invalid base is read"),
            Err(error) => error,
        };
        assert!(
            error.to_string().contains("exceeds preview limit"),
            "oversized declaration should be the first error: {error}"
        );
    }

    #[test]
    #[serial_test::serial(hash_kind)]
    fn non_delta_probe_rejects_oversized_declaration_before_decoding_payload() {
        let declared = crate::utils::preview_object::MAX_OBJECT_BYTES + 1;
        let error = validate_zlib_payload(
            &mut io::Cursor::new(Vec::<u8>::new()),
            declared,
            Path::new("oversized.pack"),
            12,
        )
        .expect_err("oversized packed preview object must fail closed");
        assert!(
            error.to_string().contains("exceeds preview limit"),
            "the declaration must be rejected before the payload is decoded: {error}"
        );
    }

    #[test]
    fn decoded_sizes_returns_result_size_for_plain_and_ofs_delta_objects() {
        let _kind = git_internal::hash::set_hash_kind_for_test(HashKind::Sha1);
        let temp = tempfile::tempdir().expect("create pack fixture");
        let pack_dir = temp.path().join("pack");
        std::fs::create_dir(&pack_dir).expect("create pack directory");
        let pack = pack_dir.join("ofs-delta-sha1.pack");
        let idx = pack.with_extension("idx");
        let fixture =
            Path::new(env!("LIBRA_WORKSPACE_ROOT")).join("tests/data/packs/ofs-delta-sha1.pack");
        std::fs::copy(&fixture, &pack).expect("copy pack fixture");
        crate::internal::index_pack::build_index_v1(
            pack.to_str().expect("UTF-8 pack path"),
            idx.to_str().expect("UTF-8 idx path"),
        )
        .expect("build fixture index");

        let base = crate::internal::object_format::parse_repo_oid(
            "b1a36d7748643b07e2bd006211e9e6a492f6bb8b",
        )
        .expect("parse base OID");
        let ofs_delta = crate::internal::object_format::parse_repo_oid(
            "1b59abc09609574e73330d56815f04ebb4d9bd72",
        )
        .expect("parse OFS_DELTA OID");
        let missing = crate::internal::object_format::parse_repo_oid(
            "0000000000000000000000000000000000000001",
        )
        .expect("parse missing OID");

        let decoded =
            decoded_sizes(&pack_dir, &[base, ofs_delta, missing]).expect("probe decoded sizes");
        assert_eq!(decoded, vec![Some(10_420), Some(10_400), None]);

        let costs = load_costs(&pack_dir, &[base, ofs_delta]).expect("probe load costs");
        assert_eq!(costs[0], Some(10_420));
        assert!(
            costs[1].is_some_and(|cost| cost > 10_400),
            "load_costs must keep returning peak_bytes for OFS_DELTA: {costs:?}"
        );
    }

    #[tokio::test]
    async fn decoded_sizes_returns_result_size_for_multilayer_ref_delta_chain() {
        use git_internal::internal::{object::blob::Blob, pack::entry::Entry};

        let _kind = git_internal::hash::set_hash_kind_for_test(HashKind::Sha1);
        let temp = tempfile::tempdir().expect("create pack fixture");
        let pack_dir = temp.path().join("pack");
        std::fs::create_dir(&pack_dir).expect("create pack directory");

        let mut lines: Vec<String> = (0..500)
            .map(|index| format!("chain-line-{index:04}-payload-0123456789abcdef"))
            .collect();
        let mut blobs = Vec::new();
        for step in 0..4 {
            if step > 0 {
                lines.insert(step * 80, format!("step-{step}-addition"));
            }
            blobs.push(Blob::from_content(&format!("{}\n", lines.join("\n"))));
        }
        let entries: Vec<Entry> = blobs.iter().cloned().map(Entry::from).collect();
        let pack_bytes = crate::internal::pack_writer::encode_pack_bytes_with_window(
            entries,
            HashKind::Sha1,
            10,
        )
        .await
        .expect("encode delta-compressed pack");
        let pack = pack_dir.join("ref-chain.pack");
        let idx = pack.with_extension("idx");
        std::fs::write(&pack, &pack_bytes).expect("write pack");
        crate::internal::index_pack::build_index_v1(
            pack.to_str().expect("UTF-8 pack path"),
            idx.to_str().expect("UTF-8 idx path"),
        )
        .expect("build pack index");

        let hashes: Vec<ObjectHash> = blobs.iter().map(|blob| blob.id).collect();
        let expected_decoded: Vec<Option<u64>> = blobs
            .iter()
            .map(|blob| Some(blob.data.len() as u64))
            .collect();
        let decoded = decoded_sizes(&pack_dir, &hashes).expect("probe decoded sizes");
        assert_eq!(decoded, expected_decoded);

        let costs = load_costs(&pack_dir, &hashes).expect("probe load costs");
        let delta_entries = costs
            .iter()
            .zip(&blobs)
            .filter(|(cost, blob)| cost.is_some_and(|peak| peak > blob.data.len() as u64))
            .count();
        assert!(
            delta_entries >= 3,
            "at least three versions must be REF_DELTA chains with peak_bytes > decoded size: {costs:?}"
        );
    }

    #[test]
    fn decoded_size_probe_treats_oversized_delta_as_miss_without_decoding_base() {
        let _kind = git_internal::hash::set_hash_kind_for_test(HashKind::Sha1);
        let temp = tempfile::tempdir().expect("create pack fixture");
        let pack_dir = temp.path().join("pack");
        std::fs::create_dir(&pack_dir).expect("create pack directory");
        let pack = pack_dir.join("ofs-delta-sha1.pack");
        let idx = pack.with_extension("idx");
        let fixture =
            Path::new(env!("LIBRA_WORKSPACE_ROOT")).join("tests/data/packs/ofs-delta-sha1.pack");
        std::fs::copy(&fixture, &pack).expect("copy pack fixture");
        crate::internal::index_pack::build_index_v1(
            pack.to_str().expect("UTF-8 pack path"),
            idx.to_str().expect("UTF-8 idx path"),
        )
        .expect("build fixture index");
        let index = PackIndex::open(idx, &mut Stats::default()).expect("open fixture index");
        let mut probe = PackProbe::new(pack, index).expect("open fixture probe");

        let outcome = probe.read_delta_probe(12, MAX_VALIDATED_DELTA_BYTES + 1, u64::MAX / 2, 0);
        assert!(matches!(outcome, Err(ProbeFailure::Oversized(_))));
    }
}
