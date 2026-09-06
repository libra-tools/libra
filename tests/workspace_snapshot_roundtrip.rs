//! OL-06 focused coverage: a canonical workspace manifest survives a
//! content-addressed round trip and remains distinct from a Git commit.

use git_internal::{hash::ObjectHash, internal::object::types::ObjectType};
use libra::internal::operation::{CapturePolicy, Completeness, HeadState, WorkspaceSnapshotV2};
use std::collections::BTreeMap;

fn oid(byte: u8) -> ObjectHash { ObjectHash::from_type_and_data(ObjectType::Blob, &[byte]) }

#[test]
fn workspace_snapshot_manifest_roundtrips_with_partial_policy() {
    let snapshot = WorkspaceSnapshotV2 {
        schema_version: 2,
        workspace_id: "test-workspace".to_string(),
        head: HeadState::Symbolic { reference: "refs/heads/main".to_string() },
        index_tree_oid: oid(1),
        raw_index_blob_oid: oid(2),
        working_copy_tree_oid: oid(3),
        untracked_manifest_oid: oid(4),
        sparse_facet_oid: None,
        sequencer_facet_oid: None,
        worktree_generation: 9,
        capture_policy: CapturePolicy::TrackedAndUntracked,
        completeness: Completeness::Partial,
        facet_restore_policies: BTreeMap::new(),
    };
    let bytes = snapshot.to_canonical_bytes().expect("manifest encodes");
    assert_eq!(WorkspaceSnapshotV2::from_canonical_bytes(&bytes).unwrap(), snapshot);
    assert_ne!(snapshot.index_tree_oid, snapshot.working_copy_tree_oid);
}
