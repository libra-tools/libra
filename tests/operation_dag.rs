//! OL-04 focused coverage for operation objects, journal rows, and head CAS.

use git_internal::{hash::ObjectHash, internal::object::types::ObjectType};
use libra::internal::{
    db,
    operation::{
        JournalEntry, JournalPhase, OperationKind, OperationMetaV2, OperationStatusV2,
        OperationStoreV2, OperationV2, RepoViewV2,
    },
};
use tempfile::TempDir;

fn oid(label: &[u8]) -> ObjectHash {
    ObjectHash::from_type_and_data(ObjectType::Blob, label)
}

fn view() -> RepoViewV2 {
    RepoViewV2 {
        schema_version: 2,
        repo_id: "repo-1".to_string(),
        refs_facet_oid: oid(b"refs"),
        workspaces: Default::default(),
        change_roots: Vec::new(),
        extension_facets: Default::default(),
    }
}

#[tokio::test]
async fn operation_store_round_trips_objects_journal_and_head_cas() {
    let dir = TempDir::new().expect("temporary operation store directory");
    let db_path = dir.path().join("repo.db");
    let object_path = dir.path().join("objects");
    let reopened_object_path = object_path.clone();
    let db = db::create_database(db_path.to_str().expect("UTF-8 database path"))
        .await
        .expect("database initializes");
    let storage = libra::utils::client_storage::ClientStorage::init_local(object_path);
    storage
        .put(&view().refs_facet_oid, b"refs", ObjectType::Blob)
        .expect("refs facet object writes");
    let store = OperationStoreV2::new_for_repo("repo-1", db, storage);

    let view_oid = store.write_view_manifest(&view()).expect("manifest writes");
    assert_eq!(store.load_view(&view_oid).expect("manifest loads"), view());

    let operation = OperationV2 {
        op_id: "op-1".to_string(),
        parent_op_ids: vec!["parent-a".to_string(), "parent-b".to_string()],
        pre_view_oid: view_oid,
        post_view_oid: view_oid,
        kind: OperationKind::Command,
        status: OperationStatusV2::Success,
        metadata: OperationMetaV2 {
            command_name: Some("test".to_string()),
            ..Default::default()
        },
        restores_op_id: None,
        reverts_op_id: None,
        predecessor_map_oid: None,
    };
    store
        .write_operation(&operation)
        .await
        .expect("operation writes");

    let generation = store
        .cas_update_op_heads("repo-1", "main", &[], &["op-1".to_string()])
        .await
        .expect("initial head publish");
    assert_eq!(generation, 1);
    assert_eq!(
        store
            .read_heads("repo-1", "main")
            .await
            .expect("heads read"),
        ["op-1"]
    );
    let heads_view = store
        .read_heads_view("repo-1", "main")
        .await
        .expect("heads view reads");
    assert!(heads_view.is_ancestor("parent-a", "op-1"));
    assert!(heads_view.is_ancestor("parent-b", "op-1"));

    let second_operation = OperationV2 {
        op_id: "op-2".to_string(),
        parent_op_ids: vec!["op-1".to_string()],
        pre_view_oid: view_oid,
        post_view_oid: view_oid,
        kind: OperationKind::Command,
        status: OperationStatusV2::Success,
        metadata: OperationMetaV2::default(),
        restores_op_id: None,
        reverts_op_id: None,
        predecessor_map_oid: None,
    };
    store
        .write_operation(&second_operation)
        .await
        .expect("second operation writes");
    assert_eq!(
        store
            .cas_update_op_heads(
                "repo-1",
                "main",
                &["op-1".to_string()],
                &["op-1".to_string(), "op-2".to_string()],
            )
            .await
            .expect("multi-head publish"),
        2
    );
    assert_eq!(
        store
            .read_heads("repo-1", "main")
            .await
            .expect("multi-head read"),
        ["op-1", "op-2"]
    );

    let conflict = store
        .cas_update_op_heads("repo-1", "main", &[], &["op-2".to_string()])
        .await;
    assert!(matches!(
        conflict,
        Err(libra::internal::operation::StoreError::CasConflict { .. })
    ));
    assert_eq!(
        store
            .read_heads("repo-1", "main")
            .await
            .expect("heads remain after CAS conflict"),
        ["op-1", "op-2"]
    );

    store
        .append_journal(&JournalEntry {
            journal_id: "journal-1".to_string(),
            op_id: "op-1".to_string(),
            phase: JournalPhase::Reserved,
            pre_view_oid: Some(view_oid),
            target_view_oid: None,
            owner: "test".to_string(),
            updated_at: 1,
            recovery_payload: None,
        })
        .await
        .expect("journal writes");
    store
        .append_journal(&JournalEntry {
            journal_id: "journal-1".to_string(),
            op_id: "op-1".to_string(),
            phase: JournalPhase::Publish,
            pre_view_oid: Some(view_oid),
            target_view_oid: Some(view_oid),
            owner: "test".to_string(),
            updated_at: 2,
            recovery_payload: Some("published".to_string()),
        })
        .await
        .expect("journal phase advances");
    let phase_regression = store
        .append_journal(&JournalEntry {
            journal_id: "journal-1".to_string(),
            op_id: "op-1".to_string(),
            phase: JournalPhase::Mutation,
            pre_view_oid: Some(view_oid),
            target_view_oid: Some(view_oid),
            owner: "test".to_string(),
            updated_at: 3,
            recovery_payload: None,
        })
        .await;
    assert!(matches!(
        phase_regression,
        Err(libra::internal::operation::StoreError::Validation(message))
            if message.contains("cannot move backwards")
    ));
    assert_eq!(
        store
            .read_journal("op-1")
            .await
            .expect("journal reads")
            .len(),
        1
    );
    assert_eq!(
        store
            .read_journal("op-1")
            .await
            .expect("journal phase reads")
            .first()
            .expect("journal entry")
            .phase,
        JournalPhase::Publish
    );

    let generation = store
        .cas_update_op_heads("repo-1", "generation", &[], &["op-1".to_string()])
        .await
        .expect("generation scope initializes");
    assert_eq!(generation, 1);
    assert_eq!(
        store
            .cas_update_op_heads_at_generation(
                "repo-1",
                "generation",
                generation,
                &["op-1".to_string()],
                &["op-2".to_string()],
            )
            .await
            .expect("generation CAS advances"),
        2
    );
    let stale_generation = store
        .cas_update_op_heads_at_generation(
            "repo-1",
            "generation",
            generation,
            &["op-2".to_string()],
            &["op-1".to_string()],
        )
        .await;
    assert!(matches!(
        stale_generation,
        Err(libra::internal::operation::StoreError::CasConflict { .. })
    ));
    assert_eq!(
        store
            .read_heads("repo-1", "generation")
            .await
            .expect("stale generation leaves head unchanged"),
        ["op-2"]
    );
    assert_eq!(
        store
            .merge_op_heads(
                "repo-1",
                "generation",
                &["op-1".to_string()],
                &["op-1".to_string()],
            )
            .await
            .expect("stale candidate is retained as a sibling"),
        3
    );
    assert_eq!(
        store
            .read_heads("repo-1", "generation")
            .await
            .expect("merged sibling heads read"),
        ["op-1", "op-2"]
    );
    assert_eq!(
        store
            .read_head_generation("repo-1", "generation")
            .await
            .expect("generation reads"),
        3
    );

    drop(store);
    let reopened_db = db::establish_connection(db_path.to_str().expect("UTF-8 database path"))
        .await
        .expect("database reopens");
    let reopened = OperationStoreV2::new_for_repo(
        "repo-1",
        reopened_db,
        libra::utils::client_storage::ClientStorage::init_local(reopened_object_path),
    );
    assert_eq!(
        reopened
            .read_journal("op-1")
            .await
            .expect("journal remains after store reopen")
            .len(),
        1
    );
}
