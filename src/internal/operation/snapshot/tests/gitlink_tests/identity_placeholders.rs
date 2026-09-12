//! Ambiguous non-directory gitlink identities must not produce silent omissions.

use super::{
    Completeness, UntrackedManifest, fs,
    support::{NESTED, UNTRACKED, blob_oid, fixture, gitlink_index},
};

#[tokio::test]
async fn capture_marks_ambiguous_gitlink_file_hardlinks_partial() {
    // Given: an indexed gitlink has a file placeholder, and an ordinary
    // sibling is a different directory entry for the same physical file.
    let mut fixture = fixture(false).await;
    let scope = fixture.snapshotter.scope.clone();
    fs::create_dir(scope.worktree_root.join("vendor")).expect("vendor");
    let placeholder = scope.worktree_root.join("vendor/sub");
    let sibling = scope.worktree_root.join("visible-hardlink.txt");
    fs::write(&placeholder, NESTED).expect("opaque file placeholder");
    fs::hard_link(&placeholder, &sibling).expect("ordinary hardlink sibling");
    #[cfg(unix)]
    assert!(
        crate::utils::path_case::same_file_entry(&placeholder, &sibling),
        "shared file identity"
    );
    fs::write(scope.worktree_root.join("independent.txt"), UNTRACKED)
        .expect("independent ordinary file");
    let (index, gitlink_oid) = gitlink_index(0);
    index.save(scope.gitdir.join("index")).expect("index");
    let raw_index = fs::read(scope.gitdir.join("index")).expect("raw index");
    assert!(!fixture.storage.exist(&blob_oid(NESTED)));

    // When: identity alone cannot distinguish a nonliteral file alias from
    // an ordinary hardlink, capture must choose a conservative partial view.
    let outcome = fixture
        .snapshotter
        .capture()
        .await
        .expect("partial capture");

    // Then: omitting the ambiguous sibling is explicit, never silently Full;
    // known-independent contents and raw index metadata are still captured.
    assert_eq!(outcome.snapshot.completeness, Completeness::Partial);
    assert!(!fixture.storage.exist(&blob_oid(NESTED)));
    assert!(fixture.storage.exist(&blob_oid(UNTRACKED)));
    assert!(!fixture.storage.exist(&gitlink_oid));
    assert_eq!(
        fixture
            .storage
            .get(&outcome.snapshot.raw_index_blob_oid)
            .expect("raw index blob"),
        raw_index
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
        std::collections::BTreeMap::from([("independent.txt".into(), blob_oid(UNTRACKED))])
    );
}

#[cfg(unix)]
#[tokio::test]
async fn capture_preserves_literal_gitlink_symlink_placeholders_without_following_them() {
    // Given: a gitlink's literal path is a symlink to opaque data outside
    // this worktree; the symlink itself is an opaque index placeholder.
    let mut fixture = fixture(false).await;
    let scope = fixture.snapshotter.scope.clone();
    let outside = scope
        .worktree_root
        .parent()
        .expect("fixture root")
        .join("outside-gitlink");
    fs::create_dir(&outside).expect("external child directory");
    fs::write(outside.join("inner.txt"), NESTED).expect("external child bytes");
    fs::create_dir(scope.worktree_root.join("vendor")).expect("vendor");
    let target = "../../outside-gitlink";
    std::os::unix::fs::symlink(target, scope.worktree_root.join("vendor/sub"))
        .expect("gitlink symlink placeholder");
    fs::write(scope.worktree_root.join("vendor/sub2.txt"), UNTRACKED).expect("ordinary sibling");
    let (index, gitlink_oid) = gitlink_index(0);
    index.save(scope.gitdir.join("index")).expect("index");
    let raw_index = fs::read(scope.gitdir.join("index")).expect("raw index");

    // When: capture records the parent worktree without following the leaf.
    let outcome = fixture.snapshotter.capture().await.expect("capture");

    // Then: neither the symlink bytes nor its target payload enter objects;
    // a known literal boundary does not make the otherwise complete scan partial.
    assert_eq!(outcome.snapshot.completeness, Completeness::Full);
    assert!(!fixture.storage.exist(&blob_oid(NESTED)));
    assert!(!fixture.storage.exist(&blob_oid(target.as_bytes())));
    assert!(!fixture.storage.exist(&gitlink_oid));
    assert!(fixture.storage.exist(&blob_oid(UNTRACKED)));
    assert_eq!(
        fixture
            .storage
            .get(&outcome.snapshot.raw_index_blob_oid)
            .expect("raw index blob"),
        raw_index
    );
    assert!(
        fs::symlink_metadata(scope.worktree_root.join("vendor/sub"))
            .expect("unchanged placeholder")
            .file_type()
            .is_symlink()
    );
}
