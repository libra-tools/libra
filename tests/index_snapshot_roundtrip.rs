//! OL-07 focused coverage: raw index bytes are restored byte-for-byte.

use libra::internal::operation::{FacetCaptureCtx, FacetRestoreCtx, RawIndexFacet, StateFacet};
use libra::internal::worktree_scope::{RequestScope, WorktreeScope};
use libra::utils::client_storage::ClientStorage;

#[test]
fn raw_index_facet_preserves_exact_bytes() {
    let root = tempfile::tempdir().expect("temporary worktree");
    let gitdir = root.path().join(".libra");
    std::fs::create_dir_all(&gitdir).unwrap();
    let bytes = b"index-with-intent-add-skip-worktree-assume-unchanged-stat\n";
    std::fs::write(gitdir.join("index"), bytes).unwrap();
    let storage = ClientStorage::init_local(root.path().join("objects"));
    let scope = RequestScope {
        scope: WorktreeScope::Main,
        workdir: root.path().to_path_buf(),
        gitdir: gitdir.clone(),
        storage: root.path().to_path_buf(),
        worktree_root: root.path().to_path_buf(),
    };
    let facet = RawIndexFacet::new(scope, storage);
    let capture = facet.capture(&FacetCaptureCtx::default()).unwrap();
    std::fs::write(gitdir.join("index"), b"changed\n").unwrap();
    facet.restore(&capture, &mut FacetRestoreCtx::default()).unwrap();
    assert_eq!(std::fs::read(gitdir.join("index")).unwrap(), bytes);
    assert_eq!(capture.meta["byte_exact"], true);
}
