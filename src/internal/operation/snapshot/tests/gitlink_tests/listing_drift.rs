//! End-of-listing guards detect replacement and missing-to-present drift.

use std::{
    io::{self, Write},
    path::Path,
    sync::Arc,
};

use super::{
    Completeness, fs,
    support::{NESTED, UNTRACKED, blob_oid, fixture, gitlink_index},
};
use crate::{
    internal::worktree_io::{
        executor::WorktreeIo,
        handler::handle_request_to_buffer,
        protocol::{IoRequest, bytes_to_path},
    },
    utils::path_case::same_file_entry,
};

enum Drift {
    Replacement,
    Materialization,
}

fn replace_during_listing(request: IoRequest, output: &mut Vec<u8>) -> io::Result<bool> {
    handle_drift(request, output, Drift::Replacement)
}

fn materialize_during_listing(request: IoRequest, output: &mut Vec<u8>) -> io::Result<bool> {
    handle_drift(request, output, Drift::Materialization)
}

fn handle_drift(request: IoRequest, output: &mut Vec<u8>, drift: Drift) -> io::Result<bool> {
    match &request {
        IoRequest::EntryIdentity { path, root }
            if bytes_to_path(path) == Path::new("vendor/sub") =>
        {
            fs::OpenOptions::new()
                .create(true)
                .append(true)
                .open(bytes_to_path(root).join(".libra/identity-observations"))?
                .write_all(b"x")?;
        }
        IoRequest::FileBlobHash { root, .. } => {
            fs::write(
                bytes_to_path(root).join(".libra/file-hash-requested"),
                b"requested",
            )?;
        }
        _ => {}
    }
    let parent = match drift {
        Drift::Replacement => "Vendor",
        Drift::Materialization => "vendor",
    };
    let changed_root = match &request {
        IoRequest::ReadDir { path, root, .. } if bytes_to_path(path) == Path::new(parent) => {
            let root = bytes_to_path(root);
            assert!(
                fs::read(root.join(".libra/identity-observations"))?.len() >= 2,
                "initial identity and pre-enumeration check must both precede the drift"
            );
            Some(root)
        }
        _ => None,
    };
    if let (Drift::Materialization, Some(root)) = (&drift, &changed_root) {
        let visible = root.join("vendor/sub");
        if !visible.try_exists()? {
            fs::create_dir(&visible)?;
            fs::write(visible.join("inner.txt"), NESTED)?;
            fs::write(root.join(".libra/drift-triggered"), b"materialized")?;
        }
    }
    let keep_running = handle_request_to_buffer(request, output)?;
    if let (Drift::Replacement, Some(root)) = (drift, changed_root) {
        let parked = root.join(".libra/parked-gitlink");
        if !parked.try_exists()? {
            let visible = root.join("Vendor/Sub");
            fs::rename(&visible, parked)?;
            fs::create_dir(&visible)?;
            fs::write(visible.join("inner.txt"), NESTED)?;
            fs::write(root.join(".libra/drift-triggered"), b"replaced")?;
        }
    }
    Ok(keep_running)
}

#[tokio::test]
async fn capture_discards_listing_when_an_aliased_root_is_replaced_during_enumeration() {
    // Given: directory A survives both initial and pre-enumeration identity checks.
    let mut fixture = fixture(false).await;
    let scope = fixture.snapshotter.scope.clone();
    let visible = scope.worktree_root.join("Vendor/Sub");
    let indexed = scope.worktree_root.join("vendor/sub");
    fs::create_dir_all(&visible).expect("initial gitlink");
    if !indexed.try_exists().expect("index spelling lookup") {
        eprintln!(
            "during-listing alias replacement requires a case-insensitive filesystem; not exercised"
        );
        return;
    }
    assert!(same_file_entry(&visible, &indexed), "physical case alias");
    fs::write(scope.worktree_root.join("independent.txt"), UNTRACKED).expect("ordinary sibling");
    let (index, gitlink_oid) = gitlink_index(0);
    index.save(scope.gitdir.join("index")).expect("index");
    let raw = fs::read(scope.gitdir.join("index")).expect("raw index");
    fixture.snapshotter = fixture
        .snapshotter
        .with_io(Arc::new(WorktreeIo::with_test_handler(
            replace_during_listing,
        )));

    // When: the real parent-directory handler lists A, then replaces it by B
    // before the scanner handles the returned child entry.
    let outcome = fixture
        .snapshotter
        .capture()
        .await
        .expect("partial capture");

    // Then: the end guard discards every candidate before any file hash or
    // persistence. Directory enumeration itself is not an atomic snapshot.
    assert!(scope.gitdir.join("drift-triggered").exists());
    assert!(
        fs::read(scope.gitdir.join("identity-observations"))
            .expect("identity calls")
            .len()
            >= 3
    );
    assert_eq!(outcome.snapshot.completeness, Completeness::Partial);
    assert!(!scope.gitdir.join("file-hash-requested").exists());
    assert!(!fixture.storage.exist(&blob_oid(NESTED)));
    assert!(!fixture.storage.exist(&blob_oid(UNTRACKED)));
    assert!(!fixture.storage.exist(&gitlink_oid));
    assert_eq!(
        fixture
            .storage
            .get(&outcome.snapshot.raw_index_blob_oid)
            .expect("raw blob"),
        raw
    );
}

#[tokio::test]
async fn capture_discards_listing_when_a_missing_gitlink_materializes_during_enumeration() {
    // Given: the indexed gitlink is genuinely absent at both identity checks.
    let mut fixture = fixture(false).await;
    let scope = fixture.snapshotter.scope.clone();
    fs::create_dir(scope.worktree_root.join("vendor")).expect("parent directory");
    assert!(!scope.worktree_root.join("vendor/sub").exists());
    fs::write(scope.worktree_root.join("independent.txt"), UNTRACKED).expect("ordinary sibling");
    let (index, gitlink_oid) = gitlink_index(0);
    index.save(scope.gitdir.join("index")).expect("index");
    let raw = fs::read(scope.gitdir.join("index")).expect("raw index");
    fixture.snapshotter = fixture
        .snapshotter
        .with_io(Arc::new(WorktreeIo::with_test_handler(
            materialize_during_listing,
        )));

    // When: the missing child appears only after enumeration has started.
    let outcome = fixture
        .snapshotter
        .capture()
        .await
        .expect("partial capture");

    // Then: None-to-Some is drift too, even though literal pruning kept the
    // new directory opaque; the unstable listing is not reported Full.
    assert!(scope.gitdir.join("drift-triggered").exists());
    assert!(scope.worktree_root.join("vendor/sub").is_dir());
    assert!(
        fs::read(scope.gitdir.join("identity-observations"))
            .expect("identity calls")
            .len()
            >= 3
    );
    assert_eq!(outcome.snapshot.completeness, Completeness::Partial);
    assert!(!scope.gitdir.join("file-hash-requested").exists());
    assert!(!fixture.storage.exist(&blob_oid(NESTED)));
    assert!(!fixture.storage.exist(&blob_oid(UNTRACKED)));
    assert!(!fixture.storage.exist(&gitlink_oid));
    assert_eq!(
        fixture
            .storage
            .get(&outcome.snapshot.raw_index_blob_oid)
            .expect("raw blob"),
        raw
    );
}
