//! Deterministic root replacement between identity capture and enumeration.

use std::{io, path::Path, sync::Arc};

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

fn replace_after_root_identity(request: IoRequest, output: &mut Vec<u8>) -> io::Result<bool> {
    if let IoRequest::ReadDir { root, .. } = &request {
        fs::write(
            bytes_to_path(root).join(".libra/readdir-requested"),
            b"requested",
        )?;
    }
    if let IoRequest::FileBlobHash { path, root, .. } = &request
        && bytes_to_path(path).starts_with("Vendor/Sub")
    {
        fs::write(
            bytes_to_path(root).join(".libra/nested-hash-requested"),
            b"requested",
        )?;
    }
    let replacement_root = match &request {
        IoRequest::EntryIdentity { path, root }
            if bytes_to_path(path) == Path::new("vendor/sub") =>
        {
            Some(bytes_to_path(root))
        }
        _ => None,
    };
    // The actual handler samples identity A before the test replaces it.
    let keep_running = handle_request_to_buffer(request, output)?;
    if let Some(root) = replacement_root {
        let parked = root.join(".libra/parked-gitlink");
        if !parked.try_exists()? {
            let visible = root.join("Vendor/Sub");
            fs::rename(&visible, parked)?;
            fs::create_dir(&visible)?;
            fs::write(visible.join("inner.txt"), NESTED)?;
        }
    }
    Ok(keep_running)
}

#[tokio::test]
async fn capture_discards_listing_when_an_aliased_gitlink_root_is_replaced() {
    // Given: a physical case alias initially identifies directory A.
    let mut fixture = fixture(false).await;
    let scope = fixture.snapshotter.scope.clone();
    let visible = scope.worktree_root.join("Vendor/Sub");
    let indexed = scope.worktree_root.join("vendor/sub");
    fs::create_dir_all(&visible).expect("initial gitlink directory");
    if !indexed.try_exists().expect("index spelling lookup") {
        eprintln!(
            "replacement-alias capture requires a case-insensitive filesystem; not exercised"
        );
        return;
    }
    assert!(same_file_entry(&visible, &indexed), "physical case alias");
    fs::write(scope.worktree_root.join("independent.txt"), UNTRACKED).expect("ordinary sibling");
    let (index, gitlink_oid) = gitlink_index(0);
    index.save(scope.gitdir.join("index")).expect("index");
    let raw_index = fs::read(scope.gitdir.join("index")).expect("raw index");
    fixture.snapshotter = fixture
        .snapshotter
        .with_io(Arc::new(WorktreeIo::with_test_handler(
            replace_after_root_identity,
        )));

    // When: the narrow real-handler wrapper replaces A with B after the
    // first identity response but before any directory enumeration.
    let outcome = fixture
        .snapshotter
        .capture()
        .await
        .expect("partial capture");

    // Then: a stale identity set never authorizes persistence of B's bytes;
    // the unstable listing is discarded, not merely labelled Partial later.
    assert!(
        scope.worktree_root.join(".libra/parked-gitlink").is_dir(),
        "replacement ran"
    );
    assert!(
        !fixture.storage.exist(&blob_oid(NESTED)),
        "stale gitlink identity leaked replacement payload"
    );
    assert!(
        !scope.gitdir.join("nested-hash-requested").exists(),
        "replacement payload was submitted for hashing"
    );
    assert!(
        !scope.gitdir.join("readdir-requested").exists(),
        "pre-enumeration identity drift did not stop directory listing"
    );
    assert_eq!(outcome.snapshot.completeness, Completeness::Partial);
    assert!(
        !fixture.storage.exist(&blob_oid(UNTRACKED)),
        "unstable listing was not discarded"
    );
    assert!(!fixture.storage.exist(&gitlink_oid));
    assert_eq!(
        fixture
            .storage
            .get(&outcome.snapshot.raw_index_blob_oid)
            .expect("raw index blob"),
        raw_index
    );
}
