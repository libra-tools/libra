//! Tests pack index generation validating offsets, CRC32, fanout tables, and trailer hashes.
//!
//! **Layer:** L1 — deterministic, no external dependencies.

use std::{
    collections::HashMap,
    fs,
    io::BufReader,
    path::{Path, PathBuf},
    sync::{Arc, Mutex},
};

use git_internal::{
    errors::GitError,
    hash::{HashKind, ObjectHash, get_hash_kind, set_hash_kind, set_hash_kind_for_test},
    internal::{
        metadata::{EntryMeta, MetaAttached},
        object::blob::Blob,
        pack::{Pack, encode::PackEncoder, entry::Entry},
    },
    utils::HashAlgorithm,
};
use libra::command::index_pack::{build_index_v1, build_index_v2};
use serial_test::serial;
use sha1::{Digest, Sha1};
use tempfile::tempdir;
use tokio::sync::mpsc;

use super::{
    assert_cli_success, init_repo_via_cli, parse_cli_error_stderr, parse_json_stdout,
    run_libra_command,
};

/// Expected pack contents for validation
#[derive(Debug)]
struct ExpectedEntry {
    offset: u64,
    crc32: u32,
}

/// Expected pack contents for validation
#[derive(Debug)]
struct ExpectedPack {
    pack_hash: ObjectHash,
    entries: HashMap<ObjectHash, ExpectedEntry>,
}

/// Parsed index version 1 contents
#[derive(Debug)]
struct ParsedIdxEntryV1 {
    hash: ObjectHash,
    offset: u64,
}

/// Parsed index version 1 contents
#[derive(Debug)]
struct ParsedIdxV1 {
    fanout: [u32; 256],
    entries: Vec<ParsedIdxEntryV1>,
    pack_hash: ObjectHash,
    idx_hash: [u8; 20],
}

/// Parsed index version 2 contents
#[derive(Debug)]
struct ParsedIdxEntryV2 {
    hash: ObjectHash,
    crc32: u32,
    offset: u64,
}

/// Parsed index version 2 contents
#[derive(Debug)]
struct ParsedIdxV2 {
    fanout: [u32; 256],
    entries: Vec<ParsedIdxEntryV2>,
    pack_hash: ObjectHash,
    idx_hash: Vec<u8>,
    idx_hash_basis_len: usize,
}

/// Returns the path to the directory containing test pack files
fn packs_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("./tests/data/packs")
}

/// Finds a pack file in the test packs directory by its prefix
fn find_pack(prefix: &str) -> PathBuf {
    let dir = packs_dir();
    let mut matches = Vec::new();
    for entry in fs::read_dir(&dir).expect("read packs dir failed") {
        let entry = entry.expect("dir entry error");
        let name = entry.file_name();
        let name = name.to_string_lossy();
        if name.starts_with(prefix) && name.ends_with(".pack") {
            matches.push(entry.path());
        }
    }
    match matches.len() {
        0 => panic!("pack with prefix `{prefix}` not found in {:?}", dir),
        1 => matches.remove(0),
        _ => panic!("multiple packs with prefix `{prefix}` found in {:?}", dir),
    }
}

/// Copies a pack file to a temporary directory for testing
fn copy_pack_to_temp(prefix: &str) -> std::io::Result<(tempfile::TempDir, PathBuf)> {
    let pack_src = find_pack(prefix);
    let dir = tempdir()?;
    let file_name = pack_src
        .file_name()
        .expect("pack file should have a filename");
    let pack_dst = dir.path().join(file_name);
    fs::copy(&pack_src, &pack_dst)?;
    Ok((dir, pack_dst))
}

/// Decode a pack file to extract expected entries and pack hash
fn decode_pack_expected(pack_path: &Path, kind: HashKind) -> Result<ExpectedPack, GitError> {
    let _guard = set_hash_kind_for_test(kind);
    let file = fs::File::open(pack_path)?;
    let mut reader = BufReader::new(file);
    let entries = Arc::new(Mutex::new(Vec::new()));
    let entries_clone = entries.clone();

    let tmp_path = pack_path
        .parent()
        .unwrap_or_else(|| Path::new("."))
        .to_path_buf();
    let mut pack = Pack::new(Some(2), Some(64 * 1024 * 1024), Some(tmp_path), true);
    pack.decode(
        &mut reader,
        move |entry: MetaAttached<Entry, EntryMeta>| {
            entries_clone.lock().unwrap().push(entry);
        },
        None::<fn(ObjectHash)>,
    )?;

    let entries = Arc::try_unwrap(entries).unwrap().into_inner().unwrap();
    let mut map = HashMap::with_capacity(entries.len());
    for entry in entries {
        let offset = entry.meta.pack_offset.ok_or_else(|| {
            GitError::ConversionError("missing pack offset in entry meta".to_string())
        })?;
        let crc32 = entry
            .meta
            .crc32
            .ok_or_else(|| GitError::ConversionError("missing crc32 in entry meta".to_string()))?;
        map.insert(
            entry.inner.hash,
            ExpectedEntry {
                offset: offset as u64,
                crc32,
            },
        );
    }

    Ok(ExpectedPack {
        pack_hash: pack.signature,
        entries: map,
    })
}

#[test]
fn test_index_pack_cli_missing_file_returns_fatal_128() {
    let repo = tempdir().unwrap();
    init_repo_via_cli(repo.path());

    let missing_pack = repo.path().join("missing.pack");
    let output = run_libra_command(&["index-pack", missing_pack.to_str().unwrap()], repo.path());
    let (stderr, report) = parse_cli_error_stderr(&output.stderr);

    assert_eq!(output.status.code(), Some(128));
    assert_eq!(report.error_code, "LBR-IO-001");
    assert_eq!(
        stderr,
        format!(
            "fatal: could not open '{}' for reading: No such file or directory\nError-Code: LBR-IO-001",
            missing_pack.display()
        )
    );
}

#[test]
#[serial(cwd)]
fn test_index_pack_json_output_reports_generated_index() {
    let repo = tempdir().unwrap();
    init_repo_via_cli(repo.path());

    let (_pack_dir, pack_path) = copy_pack_to_temp("small-sha1").expect("failed to stage pack");
    let output = run_libra_command(
        &["index-pack", pack_path.to_str().unwrap(), "--json"],
        repo.path(),
    );

    assert_cli_success(&output, "index-pack --json should succeed");
    let json = parse_json_stdout(&output);
    let index_file = json["data"]["index_file"]
        .as_str()
        .expect("index_file should be present");

    assert_eq!(json["command"], "index-pack");
    assert_eq!(
        json["data"]["pack_file"],
        pack_path.to_string_lossy().as_ref()
    );
    assert_eq!(json["data"]["index_version"], 1);
    assert!(
        Path::new(index_file).exists(),
        "generated index file should exist"
    );
}

/// Compute fanout table from sorted hashes
fn compute_fanout<'a, I>(hashes: I) -> [u32; 256]
where
    I: IntoIterator<Item = &'a ObjectHash>,
{
    let mut fanout = [0u32; 256];
    for hash in hashes {
        fanout[hash.as_ref()[0] as usize] += 1;
    }
    for i in 1..fanout.len() {
        fanout[i] += fanout[i - 1];
    }
    fanout
}

/// Assert that hashes are sorted in ascending order
fn assert_sorted_hashes(hashes: &[ObjectHash]) {
    for pair in hashes.windows(2) {
        assert!(
            pair[0] <= pair[1],
            "hashes are not sorted: {} > {}",
            pair[0],
            pair[1]
        );
    }
}

/// Parse index version 1 from bytes
fn parse_idx_v1(bytes: &[u8]) -> ParsedIdxV1 {
    const ENTRY_SIZE: usize = 4 + 20;
    const TRAILER_SIZE: usize = 20 + 20;
    let mut cursor = 0usize;
    assert!(bytes.len() >= 256 * 4 + TRAILER_SIZE, "idx v1 is too short");

    let mut fanout = [0u32; 256];
    for (i, bucket) in fanout.iter_mut().enumerate() {
        let start = cursor + i * 4;
        let end = start + 4;
        *bucket = u32::from_be_bytes(bytes[start..end].try_into().unwrap());
    }
    cursor += 256 * 4;

    let object_count = fanout[255] as usize;
    let entries_end = cursor + object_count * ENTRY_SIZE;
    let trailer_end = entries_end + TRAILER_SIZE;
    assert_eq!(
        trailer_end,
        bytes.len(),
        "idx v1 length does not match object count"
    );

    let mut entries = Vec::with_capacity(object_count);
    for i in 0..object_count {
        let entry_start = cursor + i * ENTRY_SIZE;
        let offset = u32::from_be_bytes(bytes[entry_start..entry_start + 4].try_into().unwrap());
        let hash_start = entry_start + 4;
        let hash_end = hash_start + 20;
        let hash = ObjectHash::from_bytes(&bytes[hash_start..hash_end])
            .expect("failed to parse v1 entry hash");
        entries.push(ParsedIdxEntryV1 {
            hash,
            offset: offset as u64,
        });
    }

    let pack_hash_start = entries_end;
    let pack_hash_end = pack_hash_start + 20;
    let idx_hash_start = pack_hash_end;
    let idx_hash_end = idx_hash_start + 20;
    let pack_hash = ObjectHash::from_bytes(&bytes[pack_hash_start..pack_hash_end])
        .expect("failed to parse v1 pack hash");
    let idx_hash: [u8; 20] = bytes[idx_hash_start..idx_hash_end]
        .try_into()
        .expect("failed to parse v1 idx hash");

    ParsedIdxV1 {
        fanout,
        entries,
        pack_hash,
        idx_hash,
    }
}

/// Parse index version 2 from bytes
fn parse_idx_v2(bytes: &[u8], kind: HashKind) -> ParsedIdxV2 {
    let hash_len = kind.size();
    assert!(
        bytes.len() >= 8 + 256 * 4 + hash_len * 2,
        "idx v2 is too short"
    );
    assert_eq!(&bytes[0..4], &[0xFF, 0x74, 0x4F, 0x63], "idx magic");
    let version = u32::from_be_bytes(bytes[4..8].try_into().unwrap());
    assert_eq!(version, 2, "idx version must be 2");

    let mut cursor = 8usize;
    let mut fanout = [0u32; 256];
    for (i, bucket) in fanout.iter_mut().enumerate() {
        let start = cursor + i * 4;
        let end = start + 4;
        *bucket = u32::from_be_bytes(bytes[start..end].try_into().unwrap());
    }
    cursor += 256 * 4;

    let object_count = fanout[255] as usize;
    let names_end = cursor + object_count * hash_len;
    assert!(names_end <= bytes.len(), "idx v2 names are truncated");
    let names = &bytes[cursor..names_end];
    cursor = names_end;

    let crc_end = cursor + object_count * 4;
    assert!(crc_end <= bytes.len(), "idx v2 crc32 are truncated");
    let crcs = &bytes[cursor..crc_end];
    cursor = crc_end;

    let offsets_end = cursor + object_count * 4;
    assert!(offsets_end <= bytes.len(), "idx v2 offsets are truncated");
    let offsets = &bytes[cursor..offsets_end];
    cursor = offsets_end;

    let (offset_chunks, offset_remainder) = offsets.as_chunks::<4>();
    assert!(offset_remainder.is_empty(), "idx v2 offsets are truncated");
    let large_count = offset_chunks
        .iter()
        .filter(|raw| u32::from_be_bytes(**raw) & 0x8000_0000 != 0)
        .count();
    let large_offsets_end = cursor + large_count * 8;
    assert!(
        large_offsets_end + hash_len * 2 <= bytes.len(),
        "idx v2 large offsets or trailer are truncated"
    );
    let mut large_offsets = Vec::with_capacity(large_count);
    let (large_offset_chunks, large_offset_remainder) =
        bytes[cursor..large_offsets_end].as_chunks::<8>();
    assert!(
        large_offset_remainder.is_empty(),
        "idx v2 large offsets are truncated"
    );
    for chunk in large_offset_chunks {
        large_offsets.push(u64::from_be_bytes(*chunk));
    }
    cursor = large_offsets_end;

    let pack_hash_start = cursor;
    let pack_hash_end = pack_hash_start + hash_len;
    let idx_hash_end = pack_hash_end + hash_len;
    assert_eq!(
        idx_hash_end,
        bytes.len(),
        "idx v2 has unexpected trailing bytes"
    );
    let pack_hash = ObjectHash::from_bytes(&bytes[pack_hash_start..pack_hash_end])
        .expect("failed to parse v2 pack hash");
    let idx_hash = bytes[pack_hash_end..idx_hash_end].to_vec();
    assert_eq!(idx_hash.len(), hash_len, "idx v2 hash length mismatch");

    let mut entries = Vec::with_capacity(object_count);
    for i in 0..object_count {
        let hash_start = i * hash_len;
        let hash_end = hash_start + hash_len;
        let hash = ObjectHash::from_bytes(&names[hash_start..hash_end])
            .expect("failed to parse v2 entry hash");
        let crc_start = i * 4;
        let crc32 = u32::from_be_bytes(crcs[crc_start..crc_start + 4].try_into().unwrap());
        let offset_start = i * 4;
        let raw = u32::from_be_bytes(offsets[offset_start..offset_start + 4].try_into().unwrap());
        let offset = if raw & 0x8000_0000 == 0 {
            raw as u64
        } else {
            let idx = (raw & 0x7FFF_FFFF) as usize;
            large_offsets[idx]
        };
        entries.push(ParsedIdxEntryV2 {
            hash,
            crc32,
            offset,
        });
    }

    ParsedIdxV2 {
        fanout,
        entries,
        pack_hash,
        idx_hash,
        idx_hash_basis_len: pack_hash_start,
    }
}

/// Returns the index v1 hash from the given bytes
fn compute_idx_v1_hash(bytes: &[u8]) -> [u8; 20] {
    let mut hasher = Sha1::new();
    let end = bytes.len().checked_sub(20).expect("idx v1 too short");
    hasher.update(&bytes[..end]);
    hasher.finalize().into()
}

/// Returns the index v2 hash from the given bytes
fn compute_idx_v2_hash(bytes: &[u8], basis_len: usize) -> Vec<u8> {
    assert!(basis_len <= bytes.len(), "idx v2 hash basis out of range");
    let mut hasher = HashAlgorithm::new();
    hasher.update(&bytes[..basis_len]);
    hasher.finalize()
}
/// Encode entries to pack bytes
async fn encode_entries_to_pack_bytes(entries: Vec<Entry>) -> Result<Vec<u8>, GitError> {
    assert!(!entries.is_empty(), "encode requires at least one entry");
    let (pack_tx, mut pack_rx) = mpsc::channel::<Vec<u8>>(128);
    let (entry_tx, entry_rx) = mpsc::channel::<MetaAttached<Entry, EntryMeta>>(entries.len());
    let mut encoder = PackEncoder::new(entries.len(), 0, pack_tx);
    let kind = get_hash_kind();
    let encode_handle = tokio::spawn(async move {
        set_hash_kind(kind);
        encoder.encode(entry_rx).await
    });

    for entry in entries {
        entry_tx
            .send(MetaAttached {
                inner: entry,
                meta: EntryMeta::new(),
            })
            .await
            .map_err(|e| GitError::PackEncodeError(format!("send entry failed: {e}")))?;
    }
    drop(entry_tx);

    let mut pack_bytes = Vec::new();
    while let Some(chunk) = pack_rx.recv().await {
        pack_bytes.extend_from_slice(&chunk);
    }

    let encode_result = encode_handle
        .await
        .map_err(|e| GitError::PackEncodeError(format!("pack encoder task join error: {e}")))?;
    encode_result?;
    Ok(pack_bytes)
}

/// Assert index v1 matches expected pack contents
fn assert_index_v1_for_pack(prefix: &str) -> Result<(), GitError> {
    let _guard = set_hash_kind_for_test(HashKind::Sha1);
    let (tmp_dir, pack_path) = copy_pack_to_temp(prefix)?;
    let index_path = tmp_dir.path().join("out.idx");
    build_index_v1(
        pack_path.to_str().expect("pack path should be valid"),
        index_path.to_str().expect("idx path should be valid"),
    )?;

    let expected = decode_pack_expected(&pack_path, HashKind::Sha1)?;
    let idx_bytes = fs::read(&index_path)?;
    let parsed = parse_idx_v1(&idx_bytes);

    assert_eq!(
        parsed.pack_hash, expected.pack_hash,
        "v1 pack hash mismatch for {prefix}"
    );
    assert_eq!(
        parsed.entries.len(),
        expected.entries.len(),
        "v1 entry count mismatch for {prefix}"
    );

    let parsed_hashes: Vec<ObjectHash> = parsed.entries.iter().map(|entry| entry.hash).collect();
    assert_sorted_hashes(&parsed_hashes);
    assert_eq!(
        parsed.fanout,
        compute_fanout(parsed_hashes.iter()),
        "v1 fanout mismatch for {prefix}"
    );

    for entry in &parsed.entries {
        let expected_entry = expected
            .entries
            .get(&entry.hash)
            .unwrap_or_else(|| panic!("v1 missing hash in idx: {}", entry.hash));
        assert_eq!(
            entry.offset, expected_entry.offset,
            "v1 offset mismatch for {prefix} hash {}",
            entry.hash
        );
    }

    let idx_hash = compute_idx_v1_hash(&idx_bytes);
    assert_eq!(
        parsed.idx_hash, idx_hash,
        "v1 idx hash mismatch for {prefix}"
    );
    Ok(())
}

/// Assert index v2 matches expected pack contents
fn assert_index_v2_matches_pack(
    pack_path: &Path,
    index_path: &Path,
    kind: HashKind,
    label: &str,
) -> Result<(), GitError> {
    let _guard = set_hash_kind_for_test(kind);
    let expected = decode_pack_expected(pack_path, kind)?;
    let idx_bytes = fs::read(index_path)?;
    let parsed = parse_idx_v2(&idx_bytes, kind);

    assert_eq!(
        parsed.pack_hash, expected.pack_hash,
        "v2 pack hash mismatch for {label}"
    );
    assert_eq!(
        parsed.entries.len(),
        expected.entries.len(),
        "v2 entry count mismatch for {label}"
    );

    let parsed_hashes: Vec<ObjectHash> = parsed.entries.iter().map(|entry| entry.hash).collect();
    assert_sorted_hashes(&parsed_hashes);
    assert_eq!(
        parsed.fanout,
        compute_fanout(parsed_hashes.iter()),
        "v2 fanout mismatch for {label}"
    );

    for entry in &parsed.entries {
        let expected_entry = expected
            .entries
            .get(&entry.hash)
            .unwrap_or_else(|| panic!("v2 missing hash in idx: {}", entry.hash));
        assert_eq!(
            entry.offset, expected_entry.offset,
            "v2 offset mismatch for {label} hash {}",
            entry.hash
        );
        assert_eq!(
            entry.crc32, expected_entry.crc32,
            "v2 crc32 mismatch for {label} hash {}",
            entry.hash
        );
    }

    let idx_hash = compute_idx_v2_hash(&idx_bytes, parsed.idx_hash_basis_len);
    assert_eq!(
        parsed.idx_hash, idx_hash,
        "v2 idx hash mismatch for {label}"
    );
    Ok(())
}

/// Assert index v2 matches expected pack contents
fn assert_index_v2_for_pack(prefix: &str, kind: HashKind) -> Result<(), GitError> {
    let _guard = set_hash_kind_for_test(kind);
    let (tmp_dir, pack_path) = copy_pack_to_temp(prefix)?;
    let index_path = tmp_dir.path().join("out.idx");
    build_index_v2(
        pack_path.to_str().expect("pack path should be valid"),
        index_path.to_str().expect("idx path should be valid"),
    )?;

    assert_index_v2_matches_pack(&pack_path, &index_path, kind, prefix)
}

#[test]
fn build_index_v1_small_pack_roundtrip() -> Result<(), GitError> {
    assert_index_v1_for_pack("small-sha1")
}

#[test]
fn build_index_v1_delta_pack_roundtrip() -> Result<(), GitError> {
    assert_index_v1_for_pack("ref-delta-sha1")
}

#[test]
fn build_index_v2_sha1_roundtrip() -> Result<(), GitError> {
    for prefix in ["small-sha1", "ref-delta-sha1"] {
        assert_index_v2_for_pack(prefix, HashKind::Sha1)?;
    }
    Ok(())
}

#[test]
fn build_index_v2_sha256_roundtrip() -> Result<(), GitError> {
    for prefix in ["small-sha256", "ref-delta-sha256"] {
        assert_index_v2_for_pack(prefix, HashKind::Sha256)?;
    }
    Ok(())
}

#[test]
fn build_index_v2_sha256_encode_roundtrip() -> Result<(), GitError> {
    let _guard = set_hash_kind_for_test(HashKind::Sha256);
    let entries = vec![
        Entry::from(Blob::from_content("alpha")),
        Entry::from(Blob::from_content("beta")),
        Entry::from(Blob::from_content("gamma")),
    ];

    let pack_bytes = {
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()?;
        rt.block_on(encode_entries_to_pack_bytes(entries))?
    };
    assert!(!pack_bytes.is_empty(), "encoded pack is empty");

    let tmp_dir = tempdir()?;
    let pack_path = tmp_dir.path().join("encode-sha256-small.pack");
    fs::write(&pack_path, &pack_bytes)?;
    let index_path = tmp_dir.path().join("encode-sha256-small.idx");
    build_index_v2(
        pack_path.to_str().expect("pack path should be valid"),
        index_path.to_str().expect("idx path should be valid"),
    )?;

    assert_index_v2_matches_pack(
        &pack_path,
        &index_path,
        HashKind::Sha256,
        "encode-sha256-small",
    )
}

#[test]
fn build_index_v1_rejects_sha256_hash_kind() {
    let _guard = set_hash_kind_for_test(HashKind::Sha256);
    let (tmp_dir, pack_path) = copy_pack_to_temp("small-sha256").unwrap();
    let index_path = tmp_dir.path().join("out.idx");
    let err = build_index_v1(
        pack_path.to_str().expect("pack path should be valid"),
        index_path.to_str().expect("idx path should be valid"),
    )
    .expect_err("build_index_v1 should reject sha256 hash kind");
    match err {
        GitError::InvalidPackFile(msg) => {
            assert!(
                msg.contains("Index version 1 only supports SHA-1"),
                "unexpected error message: {msg}"
            );
        }
        other => panic!("unexpected error: {other:?}"),
    }
}
