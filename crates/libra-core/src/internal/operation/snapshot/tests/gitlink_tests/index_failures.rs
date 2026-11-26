//! Unknown index state must not be mistaken for a valid unborn index.

#[cfg(unix)]
#[tokio::test]
async fn scan_with_a_dangling_index_symlink_skips_unknown_worktree_boundaries() {
    use super::{
        Completeness, fs,
        support::{NESTED, fixture},
    };

    // Given: an index entry exists but its symlink target is unavailable;
    // this is not the legitimate absence of an index in an unborn repository.
    let fixture = fixture(false).await;
    let scope = &fixture.snapshotter.scope;
    let index = scope.gitdir.join("index");
    std::os::unix::fs::symlink("missing-index-target", &index).expect("dangling index symlink");
    assert!(
        fs::symlink_metadata(&index)
            .expect("index entry")
            .file_type()
            .is_symlink()
    );
    assert!(!index.try_exists().expect("missing target"));
    fs::create_dir_all(scope.worktree_root.join("vendor/sub")).expect("unknown gitlink directory");
    fs::write(scope.worktree_root.join("vendor/sub/inner.txt"), NESTED)
        .expect("unknown nested bytes");

    // When: the scanner attempts to establish opaque boundaries from the index.
    let scan = fixture
        .snapshotter
        .scan_working_copy()
        .await
        .expect("partial scan");

    // Then: it does not claim a full empty-index view or read any user files.
    assert_eq!(scan.completeness, Completeness::Partial);
    assert!(scan.tracked.is_empty() && scan.untracked.is_empty());
    assert_eq!(scan.bytes, 0);
}

#[cfg(unix)]
#[tokio::test]
async fn scan_with_a_regular_index_symlink_preserves_normal_capture() {
    use super::{
        Completeness, Index, fs,
        support::{UNTRACKED, blob_oid, fixture},
    };

    // Given: the index symlink resolves to a valid, regular index file.
    let fixture = fixture(false).await;
    let scope = &fixture.snapshotter.scope;
    Index::new()
        .save(scope.gitdir.join("actual-index"))
        .expect("regular index target");
    std::os::unix::fs::symlink("actual-index", scope.gitdir.join("index"))
        .expect("valid index symlink");
    fs::write(scope.worktree_root.join("untracked.txt"), UNTRACKED).expect("ordinary file");

    // When: the scanner reads a usable index through the symlink.
    let scan = fixture.snapshotter.scan_working_copy().await.expect("scan");

    // Then: rejecting unknown index boundaries does not ban valid symlinks.
    assert_eq!(scan.completeness, Completeness::Full);
    assert_eq!(
        scan.untracked,
        std::collections::BTreeMap::from([("untracked.txt".into(), blob_oid(UNTRACKED)),])
    );
}
