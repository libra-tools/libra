//! Thin-pack writer (lore.md §2.10): PACK v2 encoding with REF_DELTA
//! entries whose bases the SERVER already has — `git receive-pack` completes
//! them via `index-pack --fix-thin` (or resolves them in `unpack-objects`
//! below the unpack limit).
//!
//! The delta encoder is SELF-CONTAINED: git-internal's `delta` module
//! is PRIVATE (all items are exercised through the push thin-pack path),
//! so Libra implements the standard Git delta wire format directly, with
//! git's own conventions: copy ops carry at most 64 KiB (0x10000) per op —
//! which also keeps every op far under the 16 MiB copy-length wire ceiling —
//! and literal inserts at most 127 bytes per op. Correctness is arbitrated
//! by REAL `git index-pack`/`unpack-objects` in the L1 interop tests, not by
//! a mirror decoder.

use std::io::Write;

use flate2::{Compression, write::ZlibEncoder};
use git_internal::{hash::ObjectHash, internal::object::types::ObjectType};

/// One pack entry: full object, or a delta against a server-known base.
pub struct ThinPackEntry {
    pub hash: ObjectHash,
    pub obj_type: ObjectType,
    pub data: Vec<u8>,
    /// `Some(base)` ⇒ `data` is a git delta stream against `base` (REF_DELTA).
    pub delta_base: Option<ObjectHash>,
}

/// Git delta wire format: `[varint base_size][varint result_size][ops...]`.
/// Block-based greedy matcher: index every 16-byte-aligned block of `base`
/// by a rolling key, extend matches bidirectionally, emit Copy ops (≤ 64 KiB
/// each) and literal Insert ops (≤ 127 bytes each).
pub fn delta_encode(base: &[u8], new: &[u8]) -> Vec<u8> {
    const BLOCK: usize = 16;
    const MAX_COPY: usize = 0x10000;
    let mut out = Vec::with_capacity(64 + new.len() / 8);
    write_size(&mut out, base.len());
    write_size(&mut out, new.len());

    // Index base blocks (first occurrence wins — deterministic).
    let mut index: std::collections::HashMap<&[u8], usize> = std::collections::HashMap::new();
    if base.len() >= BLOCK {
        let mut pos = 0;
        while pos + BLOCK <= base.len() {
            index.entry(&base[pos..pos + BLOCK]).or_insert(pos);
            pos += BLOCK;
        }
    }

    let mut literal_start = 0usize;
    let mut cursor = 0usize;
    while cursor + BLOCK <= new.len() {
        if let Some(&base_pos) = index.get(&new[cursor..cursor + BLOCK]) {
            // Extend the match forward.
            let mut len = BLOCK;
            while base_pos + len < base.len()
                && cursor + len < new.len()
                && base[base_pos + len] == new[cursor + len]
            {
                len += 1;
            }
            // Extend backward into pending literals.
            let mut back = 0usize;
            while back < cursor - literal_start
                && back < base_pos
                && base[base_pos - back - 1] == new[cursor - back - 1]
            {
                back += 1;
            }
            let copy_from = base_pos - back;
            let copy_at = cursor - back;
            let copy_len = len + back;
            flush_literals(&mut out, &new[literal_start..copy_at]);
            let mut emitted = 0usize;
            while emitted < copy_len {
                let chunk = (copy_len - emitted).min(MAX_COPY);
                write_copy_op(&mut out, copy_from + emitted, chunk);
                emitted += chunk;
            }
            cursor = copy_at + copy_len;
            literal_start = cursor;
        } else {
            cursor += 1;
        }
    }
    flush_literals(&mut out, &new[literal_start..]);
    out
}

/// Git varint size encoding (7 bits per byte, MSB = continuation).
fn write_size(out: &mut Vec<u8>, mut size: usize) {
    loop {
        let mut byte = (size & 0x7f) as u8;
        size >>= 7;
        if size > 0 {
            byte |= 0x80;
        }
        out.push(byte);
        if size == 0 {
            break;
        }
    }
}

fn flush_literals(out: &mut Vec<u8>, mut literals: &[u8]) {
    while !literals.is_empty() {
        let chunk = literals.len().min(0x7f);
        out.push(chunk as u8);
        out.extend_from_slice(&literals[..chunk]);
        literals = &literals[chunk..];
    }
}

/// Copy op: MSB set; flag bits 0-3 select present offset bytes, 4-6 present
/// size bytes. A size of exactly 0x10000 is encoded as zero size bytes
/// (git's convention).
fn write_copy_op(out: &mut Vec<u8>, offset: usize, size: usize) {
    debug_assert!(size <= 0x10000);
    let mut instruction = 0x80u8;
    let mut payload = Vec::with_capacity(7);
    for shift in 0..4 {
        let byte = ((offset >> (8 * shift)) & 0xff) as u8;
        if byte != 0 {
            instruction |= 1 << shift;
            payload.push(byte);
        }
    }
    let encoded_size = if size == 0x10000 { 0 } else { size };
    for shift in 0..3 {
        let byte = ((encoded_size >> (8 * shift)) & 0xff) as u8;
        if byte != 0 {
            instruction |= 1 << (4 + shift);
            payload.push(byte);
        }
    }
    out.push(instruction);
    out.extend_from_slice(&payload);
}

fn pack_object_type_code(obj_type: ObjectType) -> u8 {
    match obj_type {
        ObjectType::Commit => 1,
        ObjectType::Tree => 2,
        ObjectType::Blob => 3,
        ObjectType::Tag => 4,
        _ => 3,
    }
}

/// Entry header: type in bits 4-6 of the first byte, size as 4+7·n bit varint.
fn write_entry_header(out: &mut Vec<u8>, type_code: u8, mut size: usize) {
    let mut byte = ((type_code & 0x7) << 4) | (size & 0x0f) as u8;
    size >>= 4;
    while size > 0 {
        out.push(byte | 0x80);
        byte = (size & 0x7f) as u8;
        size >>= 7;
    }
    out.push(byte);
}

fn zlib(data: &[u8]) -> std::io::Result<Vec<u8>> {
    let mut encoder = ZlibEncoder::new(Vec::new(), Compression::default());
    encoder.write_all(data)?;
    encoder.finish()
}

/// Encode a complete (possibly thin) PACK v2 stream: header, entries (full
/// objects with git type/size headers; deltas as REF_DELTA/type-7 with the
/// raw base hash), and the wire-hash trailer over all preceding bytes.
pub fn encode_thin_pack(entries: &[ThinPackEntry]) -> std::io::Result<Vec<u8>> {
    let mut pack = Vec::new();
    pack.extend_from_slice(b"PACK");
    pack.extend_from_slice(&2u32.to_be_bytes());
    pack.extend_from_slice(&(entries.len() as u32).to_be_bytes());
    for entry in entries {
        match &entry.delta_base {
            None => {
                write_entry_header(
                    &mut pack,
                    pack_object_type_code(entry.obj_type),
                    entry.data.len(),
                );
                pack.extend_from_slice(&zlib(&entry.data)?);
            }
            Some(base) => {
                // REF_DELTA = type 7; size = the DELTA payload length.
                write_entry_header(&mut pack, 7, entry.data.len());
                pack.extend_from_slice(&base.to_data());
                pack.extend_from_slice(&zlib(&entry.data)?);
            }
        }
    }
    // Trailer: content hash of everything so far, in the active wire kind.
    let digest = ObjectHash::new(&pack);
    pack.extend_from_slice(&digest.to_data());
    Ok(pack)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Reference decoder FOR TESTS ONLY (the real arbiter is git itself in
    /// the push interop tests): replays a delta stream against a base.
    fn delta_apply(base: &[u8], delta: &[u8]) -> Vec<u8> {
        fn read_size(data: &[u8], cursor: &mut usize) -> usize {
            let mut size = 0usize;
            let mut shift = 0;
            loop {
                let byte = data[*cursor];
                *cursor += 1;
                size |= ((byte & 0x7f) as usize) << shift;
                if byte & 0x80 == 0 {
                    break;
                }
                shift += 7;
            }
            size
        }
        let mut cursor = 0usize;
        let base_size = read_size(delta, &mut cursor);
        assert_eq!(base_size, base.len());
        let result_size = read_size(delta, &mut cursor);
        let mut out = Vec::with_capacity(result_size);
        while cursor < delta.len() {
            let instruction = delta[cursor];
            cursor += 1;
            if instruction & 0x80 != 0 {
                let mut offset = 0usize;
                for shift in 0..4 {
                    if instruction & (1 << shift) != 0 {
                        offset |= (delta[cursor] as usize) << (8 * shift);
                        cursor += 1;
                    }
                }
                let mut size = 0usize;
                for shift in 0..3 {
                    if instruction & (1 << (4 + shift)) != 0 {
                        size |= (delta[cursor] as usize) << (8 * shift);
                        cursor += 1;
                    }
                }
                if size == 0 {
                    size = 0x10000;
                }
                out.extend_from_slice(&base[offset..offset + size]);
            } else {
                let len = instruction as usize;
                out.extend_from_slice(&delta[cursor..cursor + len]);
                cursor += len;
            }
        }
        assert_eq!(out.len(), result_size);
        out
    }

    #[test]
    fn delta_roundtrips_typical_edits() {
        let base = b"The quick brown fox jumps over the lazy dog. ".repeat(100);
        // Small edit in the middle of a large blob.
        let mut new = base.clone();
        new.splice(1000..1010, b"EDITED-HERE".iter().copied());
        let delta = delta_encode(&base, &new);
        assert_eq!(delta_apply(&base, &delta), new);
        assert!(
            delta.len() < new.len() / 4,
            "large win on a small edit: {} vs {}",
            delta.len(),
            new.len()
        );
        // Append-only change.
        let mut appended = base.clone();
        appended.extend_from_slice(b"tail content");
        let delta = delta_encode(&base, &appended);
        assert_eq!(delta_apply(&base, &delta), appended);
        // Disjoint content (no matches) still round-trips as literals.
        let unrelated = b"completely different bytes".to_vec();
        let delta = delta_encode(&base, &unrelated);
        assert_eq!(delta_apply(&base, &delta), unrelated);
        // Empty edges.
        assert_eq!(delta_apply(b"", &delta_encode(b"", b"abc")), b"abc");
        assert_eq!(delta_apply(&base, &delta_encode(&base, b"")), b"");
    }

    #[test]
    fn copy_ops_split_at_64k_and_encode_the_exact_boundary() {
        // A 200 KiB identical prefix must split into 64 KiB copy ops
        // (including the zero-size-bytes encoding for exactly 0x10000).
        let base = vec![7u8; 200 * 1024];
        let mut new = base.clone();
        new.extend_from_slice(b"suffix");
        let delta = delta_encode(&base, &new);
        assert_eq!(delta_apply(&base, &delta), new);
    }
}
