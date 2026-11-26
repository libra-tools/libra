//! Frozen `fastcdc-v2020-32k` chunker (shared Libra ↔ mega2 Media contract).
//!
//! Recipe: `fastcdc = "=3.2.1"`, `v2020`, `Normalization::Level1`, seed=`0`,
//! min/avg/max = 32768/65536/262144 bytes. Changing any of these requires a
//! new algorithm name and synchronized dual-repo revision.
//!
//! Chunk hashes and `media_oid` remain SHA-256 (application digest domain);
//! they never follow repository `core.objectformat` (ADR-B3-04 / GC-B3-02).

use std::io::{self, Read, Seek, SeekFrom};

use fastcdc::v2020::{ChunkData, FastCDC, Normalization, StreamCDC};

use super::sha256_hex;

/// Algorithm name recorded by Media manifests (C-01 / shared table).
pub const ALGORITHM: &str = "fastcdc-v2020-32k";

/// Minimum chunk size (non-tail chunks must be ≥ this).
pub const MIN_SIZE: usize = 32 * 1024;
/// Target average chunk size.
pub const AVG_SIZE: usize = 64 * 1024;
/// Maximum chunk size (non-tail and tail upper bound).
pub const MAX_SIZE: usize = 256 * 1024;

const MIN_U32: u32 = MIN_SIZE as u32;
const AVG_U32: u32 = AVG_SIZE as u32;
const MAX_U32: u32 = MAX_SIZE as u32;
const SEED: u64 = 0;

/// One content-defined chunk: byte range in the media object and lowercase-hex
/// SHA-256 of its raw (uncompressed) bytes. LFS digest domain, not Git HashKind.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Chunk {
    pub offset: u64,
    pub length: u64,
    pub chunk_hash: String,
}

/// Visit chunks one at a time. The callback borrows the raw chunk bytes for the
/// duration of the call; callers that need the bytes later must copy them.
/// Empty input yields no visits. EOF always emits a trailing chunk covering
/// remaining bytes (may be shorter than [`MIN_SIZE`]).
pub fn visit_chunks<R, E, F>(reader: R, mut visit: F) -> Result<(), E>
where
    R: Read,
    E: From<io::Error>,
    F: FnMut(&Chunk, &[u8]) -> Result<(), E>,
{
    let chunker = StreamCDC::with_level_and_seed(
        reader,
        MIN_U32,
        AVG_U32,
        MAX_U32,
        Normalization::Level1,
        SEED,
    );
    let mut offset: u64 = 0;
    for item in chunker {
        let ChunkData { length, data, .. } = item.map_err(|err| E::from(io::Error::other(err)))?;
        let chunk = Chunk {
            offset,
            length: length as u64,
            chunk_hash: sha256_hex(&data),
        };
        visit(&chunk, &data)?;
        offset = offset
            .checked_add(length as u64)
            .ok_or_else(|| E::from(io::Error::other("chunk offset overflow")))?;
    }
    Ok(())
}

/// Chunk a byte stream into a `Vec`. Prefer [`visit_chunks`] on the production
/// path so a large object is not retained as one vector.
pub fn chunk_reader<R: Read>(reader: R) -> io::Result<Vec<Chunk>> {
    let mut out = Vec::new();
    visit_chunks(reader, |chunk, _data| -> io::Result<()> {
        out.push(chunk.clone());
        Ok(())
    })?;
    Ok(out)
}

/// In-memory convenience over [`chunk_reader`]; must agree with the streaming path.
pub fn chunk_bytes(data: &[u8]) -> Vec<Chunk> {
    let chunker =
        FastCDC::with_level_and_seed(data, MIN_U32, AVG_U32, MAX_U32, Normalization::Level1, SEED);
    chunker
        .map(|c| {
            let slice = &data[c.offset..c.offset + c.length];
            Chunk {
                offset: c.offset as u64,
                length: c.length as u64,
                chunk_hash: sha256_hex(slice),
            }
        })
        .collect()
}

/// Prior layout span used by ADR-FL-04 coherence planning (metadata only).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PriorSpan {
    pub offset: u64,
    pub length: u64,
    pub chunk_hash: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum Segment {
    Reuse(Chunk),
    Dirty { start: u64, end: u64 },
}

/// Plan a same-length prior re-chunk against `reader`.
///
/// Returns `Ok(None)` when the caller should fall back to a full-file cold cut
/// (absorb failed, or an empty prior with non-zero size). Size mismatches are
/// the caller's responsibility — pass same-size priors only. Prior spans must
/// already be structurally valid and cover `media_size`.
pub fn rechunk_with_prior<R: Read + Seek>(
    reader: &mut R,
    media_size: u64,
    prior: &[PriorSpan],
) -> io::Result<Option<Vec<Chunk>>> {
    if media_size == 0 {
        return Ok(Some(Vec::new()));
    }
    if prior.is_empty() {
        return Ok(None);
    }
    let mut segments: Vec<Segment> = Vec::new();
    for span in prior {
        let end = span
            .offset
            .checked_add(span.length)
            .ok_or_else(|| io::Error::other("prior chunk offset overflow"))?;
        if end > media_size {
            return Err(io::Error::other("prior chunk exceeds media size"));
        }
        let actual = hash_span(reader, span.offset, span.length)?;
        if actual == span.chunk_hash {
            push_reuse(
                &mut segments,
                Chunk {
                    offset: span.offset,
                    length: span.length,
                    chunk_hash: span.chunk_hash.clone(),
                },
            );
        } else {
            push_dirty(&mut segments, span.offset, end);
        }
    }
    materialize_segments(reader, media_size, segments)
}

fn push_reuse(segments: &mut Vec<Segment>, chunk: Chunk) {
    segments.push(Segment::Reuse(chunk));
}

fn push_dirty(segments: &mut Vec<Segment>, start: u64, end: u64) {
    if let Some(Segment::Dirty { end: e, .. }) = segments.last_mut()
        && *e == start
    {
        *e = end;
        return;
    }
    segments.push(Segment::Dirty { start, end });
}

fn materialize_segments<R: Read + Seek>(
    reader: &mut R,
    media_size: u64,
    segments: Vec<Segment>,
) -> io::Result<Option<Vec<Chunk>>> {
    let mut out: Vec<Chunk> = Vec::new();
    let mut i = 0usize;
    while i < segments.len() {
        match &segments[i] {
            Segment::Reuse(chunk) => {
                out.push(chunk.clone());
                i += 1;
            }
            Segment::Dirty { start, end } => {
                let mut lo = *start;
                let mut hi = *end;
                let mut left = i;
                let mut right = i;
                loop {
                    let planned = cdc_span(reader, lo, hi - lo)?;
                    if chunks_are_legal(&planned, media_size) {
                        out.extend(planned);
                        i = right + 1;
                        break;
                    }
                    let mut expanded = false;
                    if left > 0
                        && let Segment::Reuse(prev) = &segments[left - 1]
                    {
                        lo = prev.offset;
                        left -= 1;
                        while out.last().is_some_and(|c| c.offset >= lo) {
                            out.pop();
                        }
                        expanded = true;
                    }
                    if right + 1 < segments.len()
                        && let Segment::Reuse(next) = &segments[right + 1]
                    {
                        hi = next
                            .offset
                            .checked_add(next.length)
                            .ok_or_else(|| io::Error::other("chunk offset overflow"))?;
                        right += 1;
                        expanded = true;
                    }
                    if !expanded {
                        return Ok(None);
                    }
                    if lo == 0 && hi == media_size {
                        return Ok(None);
                    }
                }
            }
        }
    }
    Ok(Some(out))
}

fn chunks_are_legal(chunks: &[Chunk], media_size: u64) -> bool {
    if chunks.is_empty() {
        return media_size == 0;
    }
    for chunk in chunks {
        if chunk.length == 0 || chunk.length > MAX_SIZE as u64 {
            return false;
        }
        let ends_at_eof = chunk
            .offset
            .checked_add(chunk.length)
            .is_some_and(|end| end == media_size);
        if !ends_at_eof && chunk.length < MIN_SIZE as u64 {
            return false;
        }
    }
    true
}

/// SHA-256 of `length` bytes starting at `offset`.
pub fn hash_span<R: Read + Seek>(reader: &mut R, offset: u64, length: u64) -> io::Result<String> {
    let bytes = read_span(reader, offset, length)?;
    Ok(sha256_hex(&bytes))
}

/// Read `length` bytes starting at `offset`.
pub fn read_span<R: Read + Seek>(reader: &mut R, offset: u64, length: u64) -> io::Result<Vec<u8>> {
    reader.seek(SeekFrom::Start(offset))?;
    let mut buf = vec![0u8; length as usize];
    reader.read_exact(&mut buf)?;
    Ok(buf)
}

fn cdc_span<R: Read + Seek>(reader: &mut R, offset: u64, length: u64) -> io::Result<Vec<Chunk>> {
    let data = read_span(reader, offset, length)?;
    let mut out = Vec::new();
    for mut chunk in chunk_bytes(&data) {
        chunk.offset = chunk
            .offset
            .checked_add(offset)
            .ok_or_else(|| io::Error::other("chunk offset overflow"))?;
        out.push(chunk);
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use serde::Deserialize;

    use super::*;

    #[derive(Debug, Deserialize)]
    struct GoldenFile {
        algorithm: String,
        vectors: std::collections::BTreeMap<String, GoldenVector>,
    }

    #[derive(Debug, Deserialize)]
    struct GoldenVector {
        input_kind: String,
        input_len: usize,
        input_sha256: String,
        chunks: Vec<GoldenChunk>,
    }

    #[derive(Debug, Deserialize, PartialEq, Eq)]
    struct GoldenChunk {
        offset: u64,
        length: u64,
        chunk_hash: String,
    }

    /// Short-read reader: at most `n` bytes per `read` (C-01 short-read equivalence).
    struct ShortRead<'a> {
        inner: io::Cursor<&'a [u8]>,
        n: usize,
    }

    impl Read for ShortRead<'_> {
        fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
            let cap = self.n.min(buf.len());
            self.inner.read(&mut buf[..cap])
        }
    }

    fn fixture_bytes(kind: &str, len: usize) -> Vec<u8> {
        match kind {
            "empty" => Vec::new(),
            "zeros" => vec![0u8; len],
            "repeat_a" => vec![b'A'; len],
            "fixed_seq" => {
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
            other => panic!("unknown golden input_kind: {other}"),
        }
    }

    fn load_golden() -> GoldenFile {
        let path =
            PathBuf::from(env!("LIBRA_WORKSPACE_ROOT")).join("tests/fixtures/fastcdc/golden.json");
        let text = std::fs::read_to_string(&path)
            .unwrap_or_else(|e| panic!("read {}: {e}", path.display()));
        serde_json::from_str(&text).expect("parse shared golden.json")
    }

    #[test]
    fn algorithm_constants_match_shared_recipe() {
        assert_eq!(ALGORITHM, "fastcdc-v2020-32k");
        assert_eq!(MIN_SIZE, 32 * 1024);
        assert_eq!(AVG_SIZE, 64 * 1024);
        assert_eq!(MAX_SIZE, 256 * 1024);
    }

    #[test]
    fn empty_input_yields_zero_chunks() {
        assert!(chunk_bytes(&[]).is_empty());
        assert!(chunk_reader(io::Cursor::new(&[][..])).unwrap().is_empty());
    }

    #[test]
    fn shared_golden_slice_stream_and_short_read_agree() {
        let golden = load_golden();
        assert_eq!(golden.algorithm, ALGORITHM);
        assert!(!golden.vectors.is_empty());
        for (name, vec) in &golden.vectors {
            let data = fixture_bytes(&vec.input_kind, vec.input_len);
            assert_eq!(data.len(), vec.input_len, "{name} length");
            assert_eq!(sha256_hex(&data), vec.input_sha256, "{name} input hash");

            let sliced = chunk_bytes(&data);
            let expected: Vec<Chunk> = vec
                .chunks
                .iter()
                .map(|c| Chunk {
                    offset: c.offset,
                    length: c.length,
                    chunk_hash: c.chunk_hash.clone(),
                })
                .collect();
            assert_eq!(sliced, expected, "{name} slice vs golden");

            let streamed = chunk_reader(io::Cursor::new(&data[..])).unwrap();
            assert_eq!(streamed, expected, "{name} stream vs golden");

            if !data.is_empty() {
                let short = chunk_reader(ShortRead {
                    inner: io::Cursor::new(&data[..]),
                    n: 7,
                })
                .unwrap();
                assert_eq!(short, expected, "{name} short-read vs golden");
            }

            let mut off = 0u64;
            for (i, c) in sliced.iter().enumerate() {
                assert_eq!(c.offset, off, "{name}[{i}] offset");
                assert!(
                    c.length >= 1 && c.length <= MAX_SIZE as u64,
                    "{name}[{i}] len"
                );
                if i + 1 < sliced.len() {
                    assert!(
                        c.length >= MIN_SIZE as u64,
                        "{name}[{i}] non-tail below MIN"
                    );
                }
                let start = c.offset as usize;
                let end = start + c.length as usize;
                assert_eq!(c.chunk_hash, sha256_hex(&data[start..end]));
                off += c.length;
            }
            assert_eq!(off as usize, data.len(), "{name} coverage");
        }
    }

    #[test]
    fn prior_same_bytes_reuses_every_span() {
        let data = fixture_bytes("fixed_seq", 1_048_576);
        let cold = chunk_bytes(&data);
        let prior: Vec<PriorSpan> = cold
            .iter()
            .map(|c| PriorSpan {
                offset: c.offset,
                length: c.length,
                chunk_hash: c.chunk_hash.clone(),
            })
            .collect();
        let mut cursor = io::Cursor::new(&data[..]);
        let planned = rechunk_with_prior(&mut cursor, data.len() as u64, &prior)
            .unwrap()
            .expect("same bytes stay coherent");
        assert_eq!(planned, cold);
    }

    #[test]
    fn prior_same_length_edit_reuses_only_matching_hashes() {
        let mut data = fixture_bytes("fixed_seq", 1_048_576);
        let cold = chunk_bytes(&data);
        assert!(cold.len() >= 3, "fixture must yield multiple chunks");
        let prior: Vec<PriorSpan> = cold
            .iter()
            .map(|c| PriorSpan {
                offset: c.offset,
                length: c.length,
                chunk_hash: c.chunk_hash.clone(),
            })
            .collect();
        // Flip one byte inside the middle chunk so only that region is dirty.
        let mid = &cold[cold.len() / 2];
        let flip_at = mid.offset as usize + (mid.length as usize / 2);
        data[flip_at] ^= 0xff;
        let mut cursor = io::Cursor::new(&data[..]);
        let planned = rechunk_with_prior(&mut cursor, data.len() as u64, &prior)
            .unwrap()
            .expect("same-length edit stays coherent");
        let mut reused = 0usize;
        for chunk in &planned {
            if let Some(old) = prior
                .iter()
                .find(|p| p.offset == chunk.offset && p.length == chunk.length)
                && old.chunk_hash == chunk.chunk_hash
            {
                reused += 1;
                let start = chunk.offset as usize;
                let end = start + chunk.length as usize;
                assert_eq!(chunk.chunk_hash, sha256_hex(&data[start..end]));
            }
        }
        assert!(reused >= 1, "at least one unchanged prior chunk is reused");
        let covered: u64 = planned.iter().map(|c| c.length).sum();
        assert_eq!(covered, data.len() as u64);
        assert!(chunks_are_legal(&planned, data.len() as u64));
    }

    #[test]
    fn prior_empty_with_nonzero_size_requests_cold_cut() {
        let data = fixture_bytes("fixed_seq", 65_536);
        let mut cursor = io::Cursor::new(&data[..]);
        let planned = rechunk_with_prior(&mut cursor, data.len() as u64, &[]).unwrap();
        assert!(planned.is_none());
    }
}
