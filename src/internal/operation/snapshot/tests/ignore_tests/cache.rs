//! Repeated ignore-cache failures remain unknown until the source is repaired.

use super::{
    Completeness, Fixture, SAFE, SECRET, VISIBLE, fs, listing, ready, source_path, write_payloads,
};
use crate::{internal::operation::snapshot::UntrackedManifest, utils::util};

pub(super) async fn invalid_utf8(configured: bool) {
    // Given: epoch-zero prewarming must not permanently cache a failed parse.
    let mut fixture = Fixture::open();
    write_payloads(&fixture);
    let source = source_path(&fixture, configured).await;
    fs::write(&source, [0xff, b'\n']).expect("invalid UTF-8 ignore source");
    let root = fixture.snapshotter.scope.worktree_root.clone();
    assert_eq!(util::secure_ignore_walk_epoch(), 0);
    let _ = util::check_gitignore_as_dir(&root, &root.join("blocked/secret.txt"), false);
    let index_before = fs::read(fixture.snapshotter.scope.gitdir.join("index")).expect("index");
    ready();

    // When: two independent captures encounter the unchanged invalid bytes.
    for attempt in 1..=2 {
        let outcome = fixture
            .snapshotter
            .capture()
            .await
            .expect("partial capture");

        // Then: every epoch remains Partial and discards even earlier candidates.
        assert_eq!(
            outcome.snapshot.completeness,
            Completeness::Partial,
            "attempt {attempt}"
        );
        let manifest: UntrackedManifest = serde_json::from_slice(
            &fixture
                .storage
                .get(&outcome.snapshot.untracked_manifest_oid)
                .expect("manifest bytes"),
        )
        .expect("untracked manifest");
        assert!(
            manifest.files.is_empty(),
            "attempt {attempt}: {:?}",
            manifest.files
        );
        assert!(
            !root.join(".libra/hash-requested").exists(),
            "attempt {attempt} hashed payload"
        );
        for bytes in [SAFE, SECRET, VISIBLE] {
            assert!(
                !fixture.storage.exist(&listing::blob_oid(bytes)),
                "attempt {attempt}"
            );
        }
        assert_eq!(
            fixture
                .storage
                .get(&outcome.snapshot.raw_index_blob_oid)
                .expect("raw index"),
            index_before,
        );
    }

    // A legal repair must recover without manually clearing caches or epochs.
    fs::write(source, b"secret.txt\n").expect("repair ignore source");
    let repaired = fixture
        .snapshotter
        .capture()
        .await
        .expect("repaired capture");
    assert_eq!(repaired.snapshot.completeness, Completeness::Full);
    let manifest: UntrackedManifest = serde_json::from_slice(
        &fixture
            .storage
            .get(&repaired.snapshot.untracked_manifest_oid)
            .expect("repaired manifest"),
    )
    .expect("repaired manifest parses");
    assert_eq!(
        manifest.files,
        std::collections::BTreeMap::from([
            ("safe.txt".into(), listing::blob_oid(SAFE)),
            ("blocked/visible.txt".into(), listing::blob_oid(VISIBLE)),
        ])
    );
    assert!(!fixture.storage.exist(&listing::blob_oid(SECRET)));
    assert_eq!(
        fs::read(root.join(".libra/index")).expect("index after"),
        index_before
    );
}
