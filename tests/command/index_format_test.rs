//! Index format contract for index v2/v3 extended flags (plan issues/490 SW-01,
//! ADR-SW-02).
//!
//! The reader/writer live in the `git-internal` dependency; these tests pin the
//! Libra-facing contract: the checked-in Git 2.55 v3 fixture reads with its
//! skip-worktree bit, unknown extended bits and v4 fail closed, a v2 index
//! round-trips byte-identically, and an extended bit forces version 3.
//!
//! Layer: L1 — deterministic; fixture checked in, no system Git.

use std::fs;

use git_internal::{
    hash::{HashKind, ObjectHash, set_hash_kind_for_test},
    internal::index::Index,
};
use serial_test::serial;

/// Absolute path of a checked-in fixture.
fn fixture_path(name: &str) -> std::path::PathBuf {
    std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests/data/index-v3")
        .join(name)
}

fn read_fixture_bytes(name: &str) -> Vec<u8> {
    fs::read(fixture_path(name)).expect("read index fixture")
}

/// M-FMT F1/F2/F3: the Git-generated v3 fixture reads with CE_SKIP_WORKTREE,
/// an extended bit forces version 3 on write, and the round trip preserves it.
#[test]
#[serial(hash_kind)]
fn test_index_v3_fixture_roundtrip_matrix() {
    let _guard = set_hash_kind_for_test(HashKind::Sha1);
    let dir = tempfile::tempdir().expect("temp dir");
    let path = dir.path().join("index");
    fs::write(&path, read_fixture_bytes("git-2.55-skip-worktree.index")).expect("write fixture");

    let index = Index::from_file(&path).expect("v3 fixture must load");
    assert_eq!(index.size(), 1, "one entry");
    let entry = index.get("a.txt", 0).expect("fixture entry");
    assert_eq!(entry.mode, 0o100644);
    assert!(
        entry.flags.skip_worktree,
        "CE_SKIP_WORKTREE must be decoded"
    );
    assert!(!entry.flags.intent_to_add);

    // Writing a v3 index keeps the extended word and the header version.
    index.to_file(&path).expect("write v3 index");
    let bytes = fs::read(&path).expect("read written index");
    assert_eq!(&bytes[..4], b"DIRC");
    assert_eq!(
        u32::from_be_bytes([bytes[4], bytes[5], bytes[6], bytes[7]]),
        3,
        "an extended entry forces version 3"
    );
    let reloaded = Index::from_file(&path).expect("reload");
    assert!(
        reloaded
            .get("a.txt", 0)
            .expect("reloaded entry")
            .flags
            .skip_worktree
    );

    // An index without extended flags stays version 2.
    let mut plain = Index::new();
    plain.update(git_internal::internal::index::IndexEntry::new_from_blob(
        "plain.txt".to_string(),
        ObjectHash::from_bytes(&[0x42u8; 20]).expect("oid"),
        3,
    ));
    let v2_path = dir.path().join("index-v2");
    plain.to_file(&v2_path).expect("write v2");
    let v2_bytes = fs::read(&v2_path).expect("read v2");
    assert_eq!(
        u32::from_be_bytes([v2_bytes[4], v2_bytes[5], v2_bytes[6], v2_bytes[7]]),
        2
    );
}

/// M-FMT F2: reading and writing a v2 index leaves the bytes unchanged.
#[test]
#[serial(hash_kind)]
fn test_index_v2_bytes_unchanged() {
    let _guard = set_hash_kind_for_test(HashKind::Sha1);
    let dir = tempfile::tempdir().expect("temp dir");
    let path = dir.path().join("index-v2");

    let mut index = Index::new();
    index.update(git_internal::internal::index::IndexEntry::new_from_blob(
        "file.txt".to_string(),
        ObjectHash::from_bytes(&[0x24u8; 20]).expect("oid"),
        7,
    ));
    index.to_file(&path).expect("first write");
    let first = fs::read(&path).expect("first read");

    let loaded = Index::from_file(&path).expect("load v2");
    loaded.to_file(&path).expect("rewrite v2");
    let second = fs::read(&path).expect("second read");
    assert_eq!(first, second, "v2 output must round-trip byte-identically");
}

/// M-FMT F4: an unknown extended bit fails closed and never rewrites the file.
#[test]
#[serial(hash_kind)]
fn test_index_unknown_extended_bit_fails_closed() {
    let _guard = set_hash_kind_for_test(HashKind::Sha1);
    let dir = tempfile::tempdir().expect("temp dir");
    let path = dir.path().join("index-unknown-bit");

    // Patch the fixture's extended word from 0x4000 to 0x1000 and recompute the
    // SHA-1 trailer so only the unknown bit is the reason for the failure.
    let mut raw = read_fixture_bytes("git-2.55-skip-worktree.index");
    raw[74] = 0x10;
    raw[75] = 0x00;
    let mut hasher = git_internal::utils::HashAlgorithm::new_for_kind(HashKind::Sha1);
    hasher.update(&raw[..raw.len() - 20]);
    let checksum = hasher.finalize_object_hash();
    raw.truncate(raw.len() - 20);
    raw.extend_from_slice(checksum.as_ref());
    fs::write(&path, &raw).expect("write patched fixture");
    let before = fs::read(&path).expect("snapshot");

    let error = match Index::from_file(&path) {
        Ok(_) => panic!("unknown extended bit must fail closed"),
        Err(error) => error,
    };
    let message = error.to_string();
    assert!(
        message.contains("extended") || message.contains("index"),
        "unexpected error: {message}"
    );
    assert_eq!(fs::read(&path).expect("re-read"), before, "file untouched");
}

/// M-FMT F5: index version 4 (and anything else) is rejected.
#[test]
#[serial(hash_kind)]
fn test_index_v4_rejected() {
    let _guard = set_hash_kind_for_test(HashKind::Sha1);
    let dir = tempfile::tempdir().expect("temp dir");
    let path = dir.path().join("index-v4");

    let mut raw = read_fixture_bytes("git-2.55-skip-worktree.index");
    raw[4..8].copy_from_slice(&4u32.to_be_bytes());
    let mut hasher = git_internal::utils::HashAlgorithm::new_for_kind(HashKind::Sha1);
    hasher.update(&raw[..raw.len() - 20]);
    let checksum = hasher.finalize_object_hash();
    raw.truncate(raw.len() - 20);
    raw.extend_from_slice(checksum.as_ref());
    fs::write(&path, &raw).expect("write v4 fixture");

    let error = match Index::from_file(&path) {
        Ok(_) => panic!("version 4 must be rejected"),
        Err(error) => error,
    };
    assert!(
        error.to_string().contains('4'),
        "the error should name the version: {error}"
    );
}
