use super::*;
use crate::utils::beneath::open_root;

#[test]
fn entry_identity_distinguishes_files_and_preserves_hardlink_identity() {
    // Given: two independent files and a second name for the first file.
    let directory = tempfile::tempdir().expect("fixture directory");
    fs::write(directory.path().join("first"), b"same bytes").expect("first file");
    fs::write(directory.path().join("second"), b"same bytes").expect("second file");
    fs::hard_link(
        directory.path().join("first"),
        directory.path().join("hardlink"),
    )
    .expect("hardlink");
    let root = open_root(directory.path()).expect("pinned root");

    // When: identity is queried without reading file payloads.
    let first = entry_identity_beneath(&root, Path::new("first")).expect("first identity");
    let second = entry_identity_beneath(&root, Path::new("second")).expect("second identity");
    let hardlink = entry_identity_beneath(&root, Path::new("hardlink")).expect("hardlink identity");

    // Then: names and content equality do not replace physical identity.
    assert_eq!(first.kind, EntryKind::File);
    assert_eq!(first, hardlink);
    assert_ne!(first.key, second.key);
    assert_eq!(
        entry_identity_beneath(&root, Path::new(""))
            .expect("root identity")
            .kind,
        EntryKind::Directory
    );
}

#[test]
fn entry_identity_retains_missing_and_invalid_path_errors() {
    // Given: an empty pinned root.
    let directory = tempfile::tempdir().expect("fixture directory");
    let root = open_root(directory.path()).expect("pinned root");

    // When: missing and escaping paths are queried.
    let missing = entry_identity_beneath(&root, Path::new("missing")).expect_err("missing entry");
    let escaping =
        entry_identity_beneath(&root, Path::new("../outside")).expect_err("escaping entry");
    let absolute = entry_identity_beneath(&root, directory.path()).expect_err("absolute entry");

    // Then: absence remains distinct from an invalid boundary request.
    assert_eq!(missing.kind(), io::ErrorKind::NotFound);
    assert_eq!(escaping.kind(), io::ErrorKind::InvalidInput);
    assert_eq!(absolute.kind(), io::ErrorKind::InvalidInput);
}

#[cfg(unix)]
#[test]
fn entry_identity_identifies_symlink_leaves_without_following_them() {
    // Given: a symlink leaf points to a real directory outside the root.
    let directory = tempfile::tempdir().expect("fixture directory");
    let outside = tempfile::tempdir().expect("external directory");
    fs::write(outside.path().join("payload"), b"opaque").expect("external payload");
    std::os::unix::fs::symlink(outside.path(), directory.path().join("link")).expect("symlink");
    let root = open_root(directory.path()).expect("pinned root");

    // When: the leaf and a path descending through it are queried.
    let leaf = entry_identity_beneath(&root, Path::new("link")).expect("symlink identity");
    let descent = entry_identity_beneath(&root, Path::new("link/payload"));

    // Then: the link itself can be classified, but no target can be traversed.
    assert_eq!(leaf.kind, EntryKind::Symlink);
    assert!(descent.is_err(), "symlink parent must fail closed");
}

#[test]
fn entry_identity_keys_keep_the_full_file_id_and_volume() {
    // Given: identities with identical low 64 bits but different upper bits
    // or volumes, as required for Windows ReFS file IDs.
    let first = EntryIdentityKey {
        volume: 1,
        file_id: [0; 16],
    };
    let mut high_bits = first;
    high_bits.file_id[15] = 1;
    let other_volume = EntryIdentityKey { volume: 2, ..first };

    // When: the keys are encoded through the worker's serde representation.
    let encoded = serde_json::to_vec(&high_bits).expect("encoded key");
    let decoded: EntryIdentityKey = serde_json::from_slice(&encoded).expect("decoded key");

    // Then: no comparison or round trip truncates either identifier component.
    assert_eq!(decoded, high_bits);
    assert_ne!(first, high_bits);
    assert_ne!(first, other_volume);
}
