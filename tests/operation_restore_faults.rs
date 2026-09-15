//! OL-10 receipt and fail-closed contract tests.

use std::fs;

use git_internal::{hash::ObjectHash, internal::object::types::ObjectType};
use libra::{
    internal::{
        db,
        operation::{
            OperationKind, OperationMetaV2, OperationStatusV2, OperationStoreV2, OperationV2,
            RepoViewV2, RestoreReceipt, RestoreWhat, recover_restore_transactions,
        },
    },
    utils::client_storage::ClientStorage,
};
use tempfile::tempdir;

fn oid(label: &[u8]) -> ObjectHash {
    ObjectHash::from_type_and_data(ObjectType::Blob, label)
}

#[test]
fn interrupted_swap_manifest_recovers_the_worktree_idempotently() {
    let root = tempdir().expect("worktree");
    let transaction = root.path().join(".libra/operation-restore-interrupted");
    fs::create_dir_all(transaction.join("backup")).expect("backup");
    fs::create_dir_all(transaction.join("stage")).expect("stage");
    fs::write(root.path().join("old.txt"), b"new").expect("installed target");
    fs::write(transaction.join("backup/old.txt"), b"old").expect("old backup");
    let manifest = serde_json::json!({
        "schema_version": 1,
        "backup_paths": ["old.txt"],
        "install_entries": [{
            "path": "old.txt",
            "object_oid": oid(b"new").to_string(),
            "mode": "Blob"
        }]
    });
    fs::write(
        transaction.join("manifest.json"),
        serde_json::to_vec(&manifest).expect("serialize manifest"),
    )
    .expect("manifest");
    fs::write(transaction.join("phase-installing"), b"").expect("installing marker");

    assert_eq!(
        recover_restore_transactions(root.path()).expect("recover"),
        1
    );
    assert_eq!(
        fs::read(root.path().join("old.txt")).expect("restored old"),
        b"old"
    );
    assert!(!transaction.exists());

    assert_eq!(
        recover_restore_transactions(root.path()).expect("repeat recovery"),
        0
    );
    assert_eq!(
        fs::read(root.path().join("old.txt")).expect("idempotently preserved old"),
        b"old"
    );
}

#[test]
fn crash_during_staging_must_preserve_unmoved_original() {
    // Review-reproduced failure: a crash while top-level entries are being
    // moved into `backup` leaves some originals in `backup` and the rest still
    // in the worktree root. Recovery must restore the moved ones and MUST NOT
    // delete the ones that were never moved (their only copy is in the root).
    let root = tempdir().expect("worktree");
    let transaction = root.path().join(".libra/operation-restore-crash-staging");
    fs::create_dir_all(transaction.join("backup")).expect("backup");
    fs::create_dir_all(transaction.join("stage")).expect("stage");
    // `a.txt` was already moved into backup (original preserved there).
    fs::write(transaction.join("backup/a.txt"), b"old-a").expect("backup a");
    // `b.txt` is the original that was never moved; it still lives in root.
    fs::write(root.path().join("b.txt"), b"old-b").expect("original b");
    // Staged replacements for both exist but were never installed.
    fs::write(transaction.join("stage/a.txt"), b"new-a").expect("stage a");
    fs::write(transaction.join("stage/b.txt"), b"new-b").expect("stage b");
    let manifest = serde_json::json!({
        "schema_version": 1,
        "backup_paths": ["a.txt", "b.txt"],
        "install_entries": [
            {"path": "a.txt", "object_oid": oid(b"new-a").to_string(), "mode": "Blob"},
            {"path": "b.txt", "object_oid": oid(b"new-b").to_string(), "mode": "Blob"}
        ]
    });
    fs::write(
        transaction.join("manifest.json"),
        serde_json::to_vec(&manifest).expect("serialize manifest"),
    )
    .expect("manifest");
    // The move phase started but never finished: no installing marker.
    fs::write(transaction.join("phase-backing-up"), b"").expect("backing-up marker");

    assert_eq!(
        recover_restore_transactions(root.path()).expect("recover"),
        1
    );
    assert_eq!(
        fs::read(root.path().join("a.txt")).expect("a restored"),
        b"old-a"
    );
    assert_eq!(
        fs::read(root.path().join("b.txt")).expect("b preserved"),
        b"old-b"
    );
    assert!(!transaction.exists());
}

#[test]
fn crash_during_recovery_must_preserve_already_restored_original() {
    // Review-reproduced failure: a crash inside recovery itself. Some
    // originals were already restored from `backup` into the worktree root,
    // others are still in `backup`. Re-running recovery must not delete the
    // already-restored originals and must finish restoring the remainder.
    let root = tempdir().expect("worktree");
    let transaction = root.path().join(".libra/operation-restore-crash-recovery");
    fs::create_dir_all(transaction.join("backup")).expect("backup");
    fs::create_dir_all(transaction.join("stage")).expect("stage");
    // `a.txt` was already restored on the first (interrupted) recovery pass:
    // the original is back in root and gone from backup.
    fs::write(root.path().join("a.txt"), b"old-a").expect("restored a");
    // `b.txt` is still in backup, waiting for this pass.
    fs::write(transaction.join("backup/b.txt"), b"old-b").expect("backup b");
    // Staged replacements remain; they must not be installed.
    fs::write(transaction.join("stage/a.txt"), b"new-a").expect("stage a");
    fs::write(transaction.join("stage/b.txt"), b"new-b").expect("stage b");
    let manifest = serde_json::json!({
        "schema_version": 1,
        "backup_paths": ["a.txt", "b.txt"],
        "install_entries": [
            {"path": "a.txt", "object_oid": oid(b"new-a").to_string(), "mode": "Blob"},
            {"path": "b.txt", "object_oid": oid(b"new-b").to_string(), "mode": "Blob"}
        ]
    });
    fs::write(
        transaction.join("manifest.json"),
        serde_json::to_vec(&manifest).expect("serialize manifest"),
    )
    .expect("manifest");
    fs::write(transaction.join("phase-installing"), b"").expect("installing marker");

    assert_eq!(
        recover_restore_transactions(root.path()).expect("recover"),
        1
    );
    assert_eq!(
        fs::read(root.path().join("a.txt")).expect("a preserved"),
        b"old-a"
    );
    assert_eq!(
        fs::read(root.path().join("b.txt")).expect("b restored"),
        b"old-b"
    );
    assert!(!transaction.exists());
}

#[test]
fn dry_run_receipt_is_machine_stable() {
    let receipt = RestoreReceipt {
        target_op_id: "target-op".to_string(),
        target_view_oid: oid(b"target-view"),
        new_op_id: None,
        workspace_id: "main".to_string(),
        what: RestoreWhat::WorkingCopy,
        dry_run: true,
        restored_facets: vec!["working_copy".to_string()],
        changed_paths: 3,
    };
    let value = serde_json::to_value(&receipt).expect("receipt serializes");
    assert_eq!(value["target_op_id"], "target-op");
    assert_eq!(value["what"], "working_copy");
    assert_eq!(value["dry_run"], true);
    assert_eq!(value["changed_paths"], 3);
    assert!(value.get("new_op_id").is_some());
}

#[test]
fn restore_selection_is_explicit_and_round_trips() {
    for what in [
        RestoreWhat::All,
        RestoreWhat::WorkingCopy,
        RestoreWhat::Index,
        RestoreWhat::Sequencer,
        RestoreWhat::Sparse,
        RestoreWhat::Head,
    ] {
        let bytes = serde_json::to_vec(&what).expect("selection serializes");
        assert_eq!(
            serde_json::from_slice::<RestoreWhat>(&bytes).expect("selection parses"),
            what
        );
    }
}

#[test]
fn receipt_does_not_embed_worktree_paths_or_secrets() {
    let receipt = RestoreReceipt {
        target_op_id: "target-op".to_string(),
        target_view_oid: oid(b"target-view"),
        new_op_id: Some("new-op".to_string()),
        workspace_id: "main".to_string(),
        what: RestoreWhat::All,
        dry_run: false,
        restored_facets: vec!["working_copy".to_string()],
        changed_paths: 1,
    };
    let encoded = serde_json::to_string(&receipt).expect("receipt serializes");
    assert!(!encoded.contains(".libra"));
    assert!(!encoded.contains("/root"));
    assert!(!encoded.contains("secret"));
}

#[test]
fn target_view_object_closure_fails_closed() {
    let missing = oid(b"missing-workspace");
    let view = RepoViewV2 {
        schema_version: 2,
        repo_id: "repo".to_string(),
        refs_facet_oid: oid(b"refs"),
        workspaces: [("main".to_string(), missing)].into_iter().collect(),
        change_roots: Vec::new(),
        extension_facets: Default::default(),
    };
    assert!(
        view.validate_recursive_closure(|candidate| {
            (*candidate == view.refs_facet_oid).then_some(Vec::new())
        })
        .is_err()
    );
}

#[tokio::test]
async fn stale_generation_is_rejected_before_publish() {
    let dir = tempdir().expect("tempdir");
    let database = db::create_database(dir.path().join("repo.db").to_str().unwrap())
        .await
        .expect("database");
    let storage = ClientStorage::init_local(dir.path().join("objects"));
    let refs = oid(b"refs");
    storage.put(&refs, b"refs", ObjectType::Blob).expect("refs");
    let store = OperationStoreV2::new_for_repo("repo", database, storage);
    let view = RepoViewV2 {
        schema_version: 2,
        repo_id: "repo".to_string(),
        refs_facet_oid: refs,
        workspaces: Default::default(),
        change_roots: Vec::new(),
        extension_facets: Default::default(),
    };
    let view_oid = store.write_view_manifest(&view).expect("view");
    store
        .write_operation(&OperationV2 {
            op_id: "op-1".to_string(),
            parent_op_ids: Vec::new(),
            pre_view_oid: view_oid,
            post_view_oid: view_oid,
            kind: OperationKind::Restore,
            status: OperationStatusV2::Success,
            metadata: OperationMetaV2::default(),
            restores_op_id: None,
            reverts_op_id: None,
            predecessor_map_oid: None,
        })
        .await
        .expect("operation");
    store
        .cas_update_op_heads_at_generation("repo", "main", 0, &[], &["op-1".to_string()])
        .await
        .expect("publish first head");
    let conflict = store
        .cas_update_op_heads_at_generation("repo", "main", 0, &[], &["op-2".to_string()])
        .await;
    assert!(matches!(
        conflict,
        Err(libra::internal::operation::StoreError::CasConflict { .. })
    ));
}
