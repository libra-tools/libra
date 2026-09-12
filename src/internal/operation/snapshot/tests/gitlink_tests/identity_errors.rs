//! Unknown physical boundaries fail closed without reading nested payloads.

use std::{io, path::Path, sync::Arc};

use git_internal::internal::index::IndexEntry;

use super::{
    Completeness, UntrackedManifest, fs,
    support::{NESTED, TRACKED, UNTRACKED, blob_oid, fixture, gitlink_index},
};
use crate::internal::worktree_io::{
    executor::WorktreeIo,
    handler::handle_request_to_buffer,
    protocol::{IoEvent, IoRequest, bytes_to_path, wire_result, write_frame},
};

#[tokio::test]
async fn capture_with_an_unreadable_gitlink_parent_is_partial_without_worktree_blobs() {
    // Given: a gitlink's parent is a regular file, so the real beneath walk
    // cannot identify that boundary and returns NotADirectory, not NotFound.
    let mut fixture = fixture(false).await;
    let scope = fixture.snapshotter.scope.clone();
    fs::write(scope.worktree_root.join("vendor"), NESTED).expect("non-directory parent");
    fs::write(scope.worktree_root.join("independent.txt"), UNTRACKED).expect("ordinary file");
    let (index, gitlink_oid) = gitlink_index(0);
    index.save(scope.gitdir.join("index")).expect("index");
    let raw_index = fs::read(scope.gitdir.join("index")).expect("raw index");

    // When: capture cannot establish all opaque roots.
    let outcome = fixture
        .snapshotter
        .capture()
        .await
        .expect("partial capture");

    // Then: the entire scan is suppressed, but metadata remains recoverable.
    assert_eq!(outcome.snapshot.completeness, Completeness::Partial);
    assert!(!fixture.storage.exist(&blob_oid(NESTED)));
    assert!(!fixture.storage.exist(&blob_oid(UNTRACKED)));
    assert!(!fixture.storage.exist(&gitlink_oid));
    assert_eq!(
        fixture
            .storage
            .get(&outcome.snapshot.raw_index_blob_oid)
            .expect("raw blob"),
        raw_index
    );
    let manifest: UntrackedManifest = serde_json::from_slice(
        &fixture
            .storage
            .get(&outcome.snapshot.untracked_manifest_oid)
            .expect("manifest"),
    )
    .expect("manifest decodes");
    assert!(manifest.files.is_empty());
}

fn deny_blocked_identity(request: IoRequest, output: &mut Vec<u8>) -> io::Result<bool> {
    match &request {
        IoRequest::EntryIdentity { path, .. } if bytes_to_path(path) == Path::new("blocked") => {
            write_frame(output, &IoEvent::Begin)?;
            write_frame(
                output,
                &IoEvent::DoneEntryIdentity {
                    result: wire_result(Err(io::Error::new(
                        io::ErrorKind::PermissionDenied,
                        "controlled identity error",
                    ))),
                },
            )?;
            return Ok(true);
        }
        IoRequest::ReadDir { path, .. } | IoRequest::FileBlobHash { path, .. } => {
            assert!(
                !bytes_to_path(path).starts_with("blocked"),
                "unknown boundary was traversed or hashed"
            );
        }
        _ => {}
    }
    handle_request_to_buffer(request, output)
}

#[tokio::test]
async fn capture_skips_unknown_candidates_without_traversing_or_hashing_them() {
    // Given: one narrow injected identity error, with all other requests
    // using the real bounded I/O handler and actual files.
    let mut fixture = fixture(false).await;
    let scope = fixture.snapshotter.scope.clone();
    fs::create_dir_all(scope.worktree_root.join("vendor/sub")).expect("gitlink");
    fs::create_dir(scope.worktree_root.join("blocked")).expect("unknown directory");
    fs::write(scope.worktree_root.join("blocked/inner.txt"), NESTED).expect("opaque payload");
    fs::write(scope.worktree_root.join("independent.txt"), UNTRACKED).expect("ordinary file");
    fs::write(scope.worktree_root.join("tracked.txt"), TRACKED).expect("tracked sibling");
    let (mut index, gitlink_oid) = gitlink_index(0);
    index.add(IndexEntry::new_from_blob(
        "tracked.txt".into(),
        blob_oid(TRACKED),
        0,
    ));
    index.save(scope.gitdir.join("index")).expect("index");
    let raw_index = fs::read(scope.gitdir.join("index")).expect("raw index");
    fixture.snapshotter = fixture
        .snapshotter
        .with_io(Arc::new(WorktreeIo::with_test_handler(
            deny_blocked_identity,
        )));

    // When: capture classifies an entry with unknown physical identity.
    let outcome = fixture
        .snapshotter
        .capture()
        .await
        .expect("partial capture");

    // Then: it never sends a directory or hash read beneath that entry;
    // independent files and raw metadata are retained in an explicit Partial.
    assert_eq!(outcome.snapshot.completeness, Completeness::Partial);
    assert!(!fixture.storage.exist(&blob_oid(NESTED)));
    assert!(fixture.storage.exist(&blob_oid(UNTRACKED)));
    assert!(fixture.storage.exist(&blob_oid(TRACKED)));
    assert!(!fixture.storage.exist(&gitlink_oid));
    assert_eq!(
        fixture
            .storage
            .get(&outcome.snapshot.raw_index_blob_oid)
            .expect("raw blob"),
        raw_index
    );
    let manifest: UntrackedManifest = serde_json::from_slice(
        &fixture
            .storage
            .get(&outcome.snapshot.untracked_manifest_oid)
            .expect("manifest"),
    )
    .expect("manifest decodes");
    assert_eq!(
        manifest.files,
        std::collections::BTreeMap::from([("independent.txt".into(), blob_oid(UNTRACKED)),])
    );
}
