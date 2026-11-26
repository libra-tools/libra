//! Snapshot capture must treat every indexed gitlink as an opaque boundary.

use git_internal::internal::index::IndexEntry;
use support::{MAIN_ONLY, NESTED, TRACKED, UNTRACKED, blob_oid, fixture, gitlink_index, tree};

use super::{Completeness, HeadState, Index, TreeItemMode, UntrackedManifest, fs};

#[path = "tests/gitlink_tests/case_aliases.rs"]
mod case_aliases;
#[path = "tests/gitlink_tests/identity_errors.rs"]
mod identity_errors;
#[path = "tests/gitlink_tests/identity_placeholders.rs"]
mod identity_placeholders;
#[path = "tests/gitlink_tests/index_failures.rs"]
mod index_failures;
#[path = "tests/gitlink_tests/listing_drift.rs"]
mod listing_drift;
#[path = "tests/gitlink_tests/replacement_race.rs"]
mod replacement_race;
#[path = "tests/gitlink_tests/support.rs"]
mod support;
#[path = "tests/gitlink_tests/unmaterialized.rs"]
mod unmaterialized;

#[tokio::test]
async fn capture_prunes_populated_gitlinks_and_preserves_pinned_index_and_siblings() {
    for linked in [false, true] {
        // Given: a real index names a gitlink whose directory has no nested .git marker.
        let mut fixture = fixture(linked).await;
        let scope = fixture.snapshotter.scope.clone();
        fs::create_dir_all(scope.worktree_root.join("vendor/sub/deep"))
            .expect("nested directories");
        fs::write(
            scope.worktree_root.join("vendor/sub/deep/inner.txt"),
            NESTED,
        )
        .expect("nested bytes");
        fs::write(scope.worktree_root.join("vendor/tracked.txt"), TRACKED)
            .expect("tracked sibling");
        fs::write(scope.worktree_root.join("vendor/sub2.txt"), UNTRACKED).expect("prefix sibling");
        let (mut index, gitlink_oid) = gitlink_index(0);
        index.add(IndexEntry::new_from_blob(
            "vendor/tracked.txt".into(),
            blob_oid(TRACKED),
            0,
        ));
        index
            .save(scope.gitdir.join("index"))
            .expect("pinned index");
        let raw_index = fs::read(scope.gitdir.join("index")).expect("raw index before capture");
        assert!(!fixture.storage.exist(&blob_oid(NESTED)));
        assert!(!fixture.storage.exist(&gitlink_oid));

        // When: capture scans, hashes and persists the actual pinned worktree.
        let outcome = fixture.snapshotter.capture().await.expect("capture");

        // Then: nested bytes never become parent objects; ordinary siblings do.
        assert!(
            !fixture.storage.exist(&blob_oid(NESTED)),
            "capture persisted opaque gitlink bytes"
        );
        assert!(fixture.storage.exist(&blob_oid(TRACKED)));
        assert!(fixture.storage.exist(&blob_oid(UNTRACKED)));
        assert!(!fixture.storage.exist(&blob_oid(MAIN_ONLY)));
        assert!(!fixture.storage.exist(&gitlink_oid));
        assert_eq!(outcome.snapshot.completeness, Completeness::Full);
        assert_eq!(
            outcome.snapshot.workspace_id,
            if linked { "linked-test" } else { "main" }
        );
        assert_eq!(
            fixture
                .storage
                .get(&outcome.snapshot.raw_index_blob_oid)
                .expect("raw index blob"),
            raw_index
        );
        assert_eq!(
            fs::read(scope.gitdir.join("index")).expect("index after capture"),
            raw_index
        );
        let root = tree(&fixture.storage, outcome.snapshot.index_tree_oid);
        let vendor = root
            .tree_items
            .iter()
            .find(|entry| entry.name == "vendor")
            .expect("vendor tree");
        let subtree = tree(&fixture.storage, vendor.id);
        let submodule = subtree
            .tree_items
            .iter()
            .find(|entry| entry.name == "sub")
            .expect("gitlink");
        assert_eq!(
            (submodule.mode, submodule.id),
            (TreeItemMode::Commit, gitlink_oid)
        );
        let manifest: UntrackedManifest = serde_json::from_slice(
            &fixture
                .storage
                .get(&outcome.snapshot.untracked_manifest_oid)
                .expect("untracked manifest"),
        )
        .expect("manifest decodes");
        assert_eq!(
            manifest.files,
            std::collections::BTreeMap::from([("vendor/sub2.txt".into(), blob_oid(UNTRACKED))])
        );
    }
}

#[tokio::test]
async fn scan_prunes_gitlink_roots_from_every_index_stage() {
    for stage in 0..=3 {
        // Given: even an unmerged gitlink remains an opaque boundary.
        let fixture = fixture(false).await;
        let scope = &fixture.snapshotter.scope;
        fs::create_dir_all(scope.worktree_root.join("vendor/sub")).expect("gitlink directory");
        fs::write(scope.worktree_root.join("vendor/sub/inner.txt"), NESTED).expect("nested bytes");
        fs::write(scope.worktree_root.join("vendor/sub2.txt"), UNTRACKED).expect("sibling");
        gitlink_index(stage)
            .0
            .save(scope.gitdir.join("index"))
            .expect("staged gitlink");

        // When: the scanner classifies files before persistence.
        let scan = fixture.snapshotter.scan_working_copy().await.expect("scan");

        // Then: no root or descendant enters either capture map.
        assert!(scan.tracked.is_empty());
        assert_eq!(
            scan.untracked,
            std::collections::BTreeMap::from([("vendor/sub2.txt".into(), blob_oid(UNTRACKED))])
        );
        assert_eq!(scan.completeness, Completeness::Full);
    }
}

#[tokio::test]
async fn capture_with_corrupt_index_keeps_raw_bytes_without_scanning_unknown_boundaries() {
    // Given: index parsing fails, so the scanner cannot identify gitlink boundaries.
    let mut fixture = fixture(false).await;
    let scope = fixture.snapshotter.scope.clone();
    let raw = b"invalid index bytes preserved for the command's own diagnostic\n";
    fs::write(scope.gitdir.join("index"), raw).expect("corrupt index");
    fs::create_dir_all(scope.worktree_root.join("vendor/sub")).expect("nested directory");
    fs::write(scope.worktree_root.join("vendor/sub/inner.txt"), NESTED).expect("nested bytes");
    fs::write(scope.worktree_root.join("visible.txt"), UNTRACKED).expect("visible bytes");
    assert!(Index::load(scope.gitdir.join("index")).is_err());
    assert!(!fixture.storage.exist(&blob_oid(NESTED)));
    assert!(!fixture.storage.exist(&blob_oid(UNTRACKED)));

    // When: the middleware attempts a partial capture before the command's error.
    let outcome = fixture
        .snapshotter
        .capture()
        .await
        .expect("partial raw-index capture");

    // Then: only known metadata is captured; opaque user content is not persisted.
    assert!(
        !fixture.storage.exist(&blob_oid(NESTED)),
        "corrupt-index fallback persisted unknown worktree content"
    );
    assert_eq!(outcome.snapshot.completeness, Completeness::Partial);
    assert!(!fixture.storage.exist(&blob_oid(UNTRACKED)));
    assert_eq!(
        fixture
            .storage
            .get(&outcome.snapshot.raw_index_blob_oid)
            .expect("raw bytes"),
        raw
    );
    let scan = fixture
        .snapshotter
        .scan_working_copy()
        .await
        .expect("partial scan");
    assert!(scan.tracked.is_empty() && scan.untracked.is_empty());
    assert_eq!(scan.completeness, Completeness::Partial);
    assert_eq!(scan.bytes, 0);
}

#[tokio::test]
async fn capture_with_a_valid_empty_or_missing_index_preserves_unborn_files() {
    for empty_index in [false, true] {
        // Given: empty and missing indexes are valid, unlike corrupt bytes.
        let mut fixture = fixture(false).await;
        let scope = fixture.snapshotter.scope.clone();
        fs::remove_file(scope.gitdir.join("HEAD")).expect("unborn HEAD");
        let raw = if empty_index {
            Index::new()
                .save(scope.gitdir.join("index"))
                .expect("empty index");
            fs::read(scope.gitdir.join("index")).expect("empty index bytes")
        } else {
            assert!(!scope.gitdir.join("index").exists());
            Vec::new()
        };
        fs::write(scope.worktree_root.join("untracked.txt"), UNTRACKED).expect("untracked file");
        fs::create_dir(scope.worktree_root.join("ordinary")).expect("ordinary directory");
        fs::write(scope.worktree_root.join("ordinary/file.txt"), TRACKED)
            .expect("nested ordinary file");

        // When: capture observes the valid empty index view.
        let outcome = fixture.snapshotter.capture().await.expect("unborn capture");

        // Then: ordinary root and nested files are captured, without creating an index.
        assert_eq!(outcome.snapshot.completeness, Completeness::Full);
        assert_eq!(
            outcome.snapshot.head,
            HeadState::Symbolic {
                reference: "refs/heads/main".into(),
            }
        );
        assert!(fixture.storage.exist(&blob_oid(UNTRACKED)));
        assert!(fixture.storage.exist(&blob_oid(TRACKED)));
        assert_eq!(
            fixture
                .storage
                .get(&outcome.snapshot.raw_index_blob_oid)
                .expect("raw index"),
            raw
        );
        let manifest: UntrackedManifest = serde_json::from_slice(
            &fixture
                .storage
                .get(&outcome.snapshot.untracked_manifest_oid)
                .expect("manifest bytes"),
        )
        .expect("untracked manifest");
        assert_eq!(
            manifest.files,
            std::collections::BTreeMap::from([
                ("ordinary/file.txt".into(), blob_oid(TRACKED)),
                ("untracked.txt".into(), blob_oid(UNTRACKED)),
            ])
        );
        assert_eq!(scope.gitdir.join("index").exists(), empty_index);
    }
}
