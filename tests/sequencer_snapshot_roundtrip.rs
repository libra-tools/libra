//! OL-07 focused coverage: all mutable-state facets are registered together.

use libra::internal::operation::{registry_for_scope, RestorePolicy};
use libra::internal::worktree_scope::{RequestScope, WorktreeScope};
use libra::utils::client_storage::ClientStorage;

#[test]
fn registry_contains_index_sequencer_and_sparse_facets() {
    let root = tempfile::tempdir().expect("temporary worktree");
    let gitdir = root.path().join(".libra");
    std::fs::create_dir_all(&gitdir).unwrap();
    std::fs::write(gitdir.join("index"), b"empty").unwrap();
    let scope = RequestScope { scope: WorktreeScope::Main, workdir: root.path().to_path_buf(), gitdir, storage: root.path().to_path_buf(), worktree_root: root.path().to_path_buf() };
    let registry = registry_for_scope(scope, ClientStorage::init_local(root.path().join("objects"))).unwrap();
    assert_eq!(registry.len(), 3);
    assert_eq!(registry.get(&"index".into()).unwrap().restore_policy(), RestorePolicy::AutoRestore);
    assert_eq!(registry.get(&"sequencer".into()).unwrap().restore_policy(), RestorePolicy::AutoRestore);
    assert_eq!(registry.get(&"sparse".into()).unwrap().restore_policy(), RestorePolicy::Rebuild);
}
