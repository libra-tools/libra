//! Filesystem aliases must not bypass an indexed gitlink boundary.

use git_internal::internal::index::IndexEntry;

use super::{
    Completeness, Index, TreeItemMode, UntrackedManifest, fs,
    support::{NESTED, TRACKED, UNTRACKED, blob_oid, fixture, gitlink_index, tree},
};
use crate::utils::path_case::same_file_entry;

#[tokio::test]
async fn capture_prunes_gitlinks_reached_through_physical_case_aliases() {
    for linked in [false, true] {
        // Given: both parent and leaf names differ, but the index spelling
        // resolves to the same physical directory as the visible spelling.
        let mut fixture = fixture(linked).await;
        let scope = fixture.snapshotter.scope.clone();
        let visible = scope.worktree_root.join("Vendor/Sub");
        let indexed = scope.worktree_root.join("vendor/sub");
        fs::create_dir_all(visible.join("deep")).expect("aliased gitlink directory");
        if !indexed.try_exists().expect("index spelling lookup") {
            eprintln!("case-alias capture requires a case-insensitive filesystem; not exercised");
            return;
        }
        assert!(same_file_entry(&visible, &indexed), "physical case alias");
        fs::write(visible.join("deep/inner.txt"), NESTED).expect("opaque nested bytes");
        fs::write(scope.worktree_root.join("Vendor/sub2.txt"), UNTRACKED)
            .expect("ordinary prefix sibling");
        fs::write(scope.worktree_root.join("tracked.txt"), TRACKED).expect("tracked sibling");
        let (mut index, gitlink_oid) = gitlink_index(0);
        index.add(IndexEntry::new_from_blob(
            "tracked.txt".into(),
            blob_oid(TRACKED),
            0,
        ));
        index.save(scope.gitdir.join("index")).expect("index");
        let raw_index = fs::read(scope.gitdir.join("index")).expect("raw index");
        assert!(!fixture.storage.exist(&blob_oid(NESTED)));
        assert!(!fixture.storage.exist(&gitlink_oid));

        // When: capture follows the actual directory entries rather than
        // assuming that their byte spelling matches the index.
        let outcome = fixture.snapshotter.capture().await.expect("capture");

        // Then: aliased child bytes never enter parent objects; ordinary
        // siblings and the pinned index remain fully captured.
        assert!(
            !fixture.storage.exist(&blob_oid(NESTED)),
            "capture persisted opaque bytes through a physical case alias"
        );
        assert!(fixture.storage.exist(&blob_oid(TRACKED)));
        assert!(fixture.storage.exist(&blob_oid(UNTRACKED)));
        assert!(!fixture.storage.exist(&gitlink_oid));
        assert_eq!(outcome.snapshot.completeness, Completeness::Full);
        assert_eq!(
            fixture
                .storage
                .get(&outcome.snapshot.raw_index_blob_oid)
                .expect("raw index blob"),
            raw_index
        );
        assert_eq!(
            fs::read(scope.gitdir.join("index")).expect("preserved index"),
            raw_index
        );
        let index = Index::load(scope.gitdir.join("index")).expect("preserved index loads");
        let gitlink = index.get("vendor/sub", 0).expect("gitlink entry");
        assert_eq!((gitlink.mode, gitlink.hash), (0o160000, gitlink_oid));
        let root = tree(&fixture.storage, outcome.snapshot.index_tree_oid);
        let vendor = root
            .tree_items
            .iter()
            .find(|entry| entry.name == "vendor")
            .expect("vendor tree");
        let vendor_tree = tree(&fixture.storage, vendor.id);
        let gitlink = vendor_tree
            .tree_items
            .iter()
            .find(|entry| entry.name == "sub")
            .expect("gitlink tree entry");
        assert_eq!(
            (gitlink.mode, gitlink.id),
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
            std::collections::BTreeMap::from([("Vendor/sub2.txt".into(), blob_oid(UNTRACKED))])
        );
    }
}

#[tokio::test]
async fn capture_preserves_physically_distinct_case_sensitive_siblings() {
    // Given: the differently cased paths are separate directories, not
    // aliases, on a filesystem that supports both names simultaneously.
    let mut fixture = fixture(false).await;
    let scope = fixture.snapshotter.scope.clone();
    let indexed = scope.worktree_root.join("vendor/sub");
    let sibling = scope.worktree_root.join("vendor/Sub");
    fs::create_dir_all(&indexed).expect("gitlink directory");
    if sibling.try_exists().expect("sibling spelling lookup") {
        assert!(same_file_entry(&indexed, &sibling), "case-insensitive host");
        eprintln!("distinct-case capture requires a case-sensitive filesystem; not exercised");
        return;
    }
    fs::create_dir(&sibling).expect("distinct sibling directory");
    assert!(
        !same_file_entry(&indexed, &sibling),
        "distinct physical directories"
    );
    fs::write(indexed.join("inner.txt"), NESTED).expect("opaque nested bytes");
    fs::write(sibling.join("visible.txt"), UNTRACKED).expect("ordinary sibling bytes");
    gitlink_index(0)
        .0
        .save(scope.gitdir.join("index"))
        .expect("index");

    // When: capture checks the indexed boundary against physical paths.
    let outcome = fixture.snapshotter.capture().await.expect("capture");

    // Then: the real sibling is not discarded merely because its case folds
    // to an indexed gitlink name.
    assert!(!fixture.storage.exist(&blob_oid(NESTED)));
    assert!(fixture.storage.exist(&blob_oid(UNTRACKED)));
    assert_eq!(outcome.snapshot.completeness, Completeness::Full);
    let manifest: UntrackedManifest = serde_json::from_slice(
        &fixture
            .storage
            .get(&outcome.snapshot.untracked_manifest_oid)
            .expect("untracked manifest"),
    )
    .expect("manifest decodes");
    assert_eq!(
        manifest.files,
        std::collections::BTreeMap::from([("vendor/Sub/visible.txt".into(), blob_oid(UNTRACKED))])
    );
}

#[tokio::test]
async fn capture_prunes_gitlinks_reached_through_physical_unicode_normalization_aliases() {
    // Given: NFC and NFD spellings are different strings but resolve to one
    // directory on normalization-insensitive filesystems, including APFS.
    let mut fixture = fixture(false).await;
    let scope = fixture.snapshotter.scope.clone();
    let indexed_name = "vendor/caf\u{e9}";
    let visible_name = "vendor/cafe\u{301}";
    let visible = scope.worktree_root.join(visible_name);
    let indexed = scope.worktree_root.join(indexed_name);
    assert_ne!(indexed_name, visible_name);
    fs::create_dir_all(&visible).expect("NFD gitlink directory");
    if !indexed.try_exists().expect("NFC spelling lookup") {
        eprintln!(
            "normalization-alias capture requires a normalization-insensitive filesystem; not exercised"
        );
        return;
    }
    assert!(
        same_file_entry(&visible, &indexed),
        "physical NFC/NFD alias"
    );
    let listed_name = fs::read_dir(scope.worktree_root.join("vendor"))
        .expect("actual directory listing")
        .next()
        .expect("visible child")
        .expect("child entry")
        .file_name();
    assert_ne!(
        listed_name.to_str().expect("fixture name is UTF-8"),
        "caf\u{e9}",
        "fixture must exercise a nonliteral directory entry"
    );
    fs::write(visible.join("inner.txt"), NESTED).expect("opaque nested bytes");
    fs::write(scope.worktree_root.join("vendor/cafe2.txt"), UNTRACKED).expect("ordinary sibling");
    let gitlink_oid = gitlink_index(0).1;
    let mut entry = IndexEntry::new_from_blob(indexed_name.into(), gitlink_oid, 0);
    entry.mode = 0o160000;
    let mut index = Index::new();
    index.add(entry);
    index.save(scope.gitdir.join("index")).expect("NFC index");
    let raw_index = fs::read(scope.gitdir.join("index")).expect("raw index");
    assert!(!fixture.storage.exist(&blob_oid(NESTED)));

    // When: capture enumerates the physically aliased directory spelling.
    let outcome = fixture.snapshotter.capture().await.expect("capture");

    // Then: normalization cannot expose nested bytes, and the opaque NFC
    // index entry and ordinary sibling remain intact.
    assert!(
        !fixture.storage.exist(&blob_oid(NESTED)),
        "capture persisted opaque bytes through a physical normalization alias"
    );
    assert!(fixture.storage.exist(&blob_oid(UNTRACKED)));
    assert!(!fixture.storage.exist(&gitlink_oid));
    assert_eq!(outcome.snapshot.completeness, Completeness::Full);
    assert_eq!(
        fixture
            .storage
            .get(&outcome.snapshot.raw_index_blob_oid)
            .expect("raw index blob"),
        raw_index
    );
    let preserved = Index::load(scope.gitdir.join("index")).expect("preserved index");
    let entry = preserved.get(indexed_name, 0).expect("NFC gitlink");
    assert_eq!((entry.mode, entry.hash), (0o160000, gitlink_oid));
    let manifest: UntrackedManifest = serde_json::from_slice(
        &fixture
            .storage
            .get(&outcome.snapshot.untracked_manifest_oid)
            .expect("untracked manifest"),
    )
    .expect("manifest decodes");
    assert_eq!(
        manifest.files,
        std::collections::BTreeMap::from([("vendor/cafe2.txt".into(), blob_oid(UNTRACKED))])
    );
}
