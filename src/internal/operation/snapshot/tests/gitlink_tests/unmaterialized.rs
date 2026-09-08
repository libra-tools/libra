//! Gitlinks remain index entries without requiring a materialized child repository.

use super::{
    Completeness, Index, TreeItemMode, UntrackedManifest, fs,
    support::{NESTED, UNTRACKED, blob_oid, fixture, gitlink_index, tree},
};

#[tokio::test]
async fn capture_preserves_unmaterialized_gitlinks_and_ignores_file_placeholders() {
    for placeholder in ["absent", "directory", "file"] {
        // Given: the index owns a gitlink even if its child is unavailable or
        // a user has put an ordinary file at that path.
        let mut fixture = fixture(false).await;
        let scope = fixture.snapshotter.scope.clone();
        fs::create_dir(scope.worktree_root.join("vendor")).expect("vendor");
        match placeholder {
            "absent" => {}
            "directory" => fs::create_dir(scope.worktree_root.join("vendor/sub"))
                .expect("placeholder directory"),
            "file" => fs::write(scope.worktree_root.join("vendor/sub"), NESTED)
                .expect("opaque placeholder file"),
            other => panic!("unknown fixture {other}"),
        }
        fs::write(scope.worktree_root.join("vendor/sub2.txt"), UNTRACKED).expect("sibling");
        let (index, gitlink_oid) = gitlink_index(0);
        index.save(scope.gitdir.join("index")).expect("index");
        let raw = fs::read(scope.gitdir.join("index")).expect("raw index");
        assert!(!fixture.storage.exist(&gitlink_oid));
        assert!(!fixture.storage.exist(&blob_oid(NESTED)));

        // When: capture records the parent repository's own state.
        let outcome = fixture.snapshotter.capture().await.expect("capture");

        // Then: no gitlink payload is read or stored, but its index OID and
        // ordinary siblings survive without an artificial partial snapshot.
        assert_eq!(outcome.snapshot.completeness, Completeness::Full);
        assert!(!fixture.storage.exist(&gitlink_oid));
        assert!(!fixture.storage.exist(&blob_oid(NESTED)));
        assert!(fixture.storage.exist(&blob_oid(UNTRACKED)));
        assert_eq!(
            fixture
                .storage
                .get(&outcome.snapshot.raw_index_blob_oid)
                .expect("raw blob"),
            raw
        );
        let captured = Index::load(scope.gitdir.join("index")).expect("preserved index");
        let entry = captured.get("vendor/sub", 0).expect("gitlink index entry");
        assert_eq!((entry.mode, entry.hash), (0o160000, gitlink_oid));
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
                .expect("manifest"),
        )
        .expect("manifest decodes");
        assert_eq!(
            manifest.files,
            std::collections::BTreeMap::from([("vendor/sub2.txt".into(), blob_oid(UNTRACKED))])
        );
    }
}
