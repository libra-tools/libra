//! OL-13 multi-worktree operation heads and reconcile contracts.

use std::{collections::BTreeMap, fs, process::Command, sync::OnceLock};

use git_internal::{
    hash::ObjectHash,
    internal::object::{
        ObjectTrait,
        tree::{Tree, TreeItem, TreeItemMode},
        types::ObjectType,
    },
};
use libra::{
    internal::{
        config::ConfigKv,
        db::{self, get_db_conn_instance_for_path},
        operation::{
            CapturePolicy, Completeness, HeadState, OperationKind, OperationMetaV2,
            OperationStatusV2, OperationStoreV2, OperationV2, ReconcileEngine, ReconcileOutcome,
            RepoViewV2, WorkspaceSnapshotV2,
        },
        worktree_scope::{RequestScope, WorktreeScope},
    },
    utils::{client_storage::ClientStorage, util},
};
use tempfile::tempdir;

static CLI_REPOSITORY_LOCK: OnceLock<tokio::sync::Mutex<()>> = OnceLock::new();

async fn lock_cli_repository_tests() -> tokio::sync::MutexGuard<'static, ()> {
    CLI_REPOSITORY_LOCK
        .get_or_init(|| tokio::sync::Mutex::new(()))
        .lock()
        .await
}

fn scope(worktree_root: &std::path::Path, repo_root: &std::path::Path) -> RequestScope {
    let gitdir = worktree_root.join(".libra");
    fs::create_dir_all(gitdir.join("info")).expect("gitdir");
    RequestScope {
        scope: WorktreeScope::Main,
        workdir: worktree_root.to_path_buf(),
        gitdir,
        storage: repo_root.to_path_buf(),
        worktree_root: worktree_root.to_path_buf(),
    }
}

/// A minimal but complete workspace snapshot whose referenced objects all
/// exist in the shared storage, so views referencing it satisfy the
/// recursive object closure that restore enforces.
fn full_snapshot(storage: &ClientStorage, workspace_id: &str) -> (ObjectHash, WorkspaceSnapshotV2) {
    let blob = |label: &[u8]| -> ObjectHash {
        let oid = ObjectHash::from_type_and_data(ObjectType::Blob, label);
        storage
            .put(&oid, label, ObjectType::Blob)
            .expect("blob object");
        oid
    };
    let tree_item = TreeItem::new(TreeItemMode::Blob, blob(b"file"), "a.txt".to_string());
    let tree = Tree::from_tree_items(vec![tree_item]).expect("tree");
    let tree_bytes = tree.to_data().expect("tree bytes");
    let tree_oid = ObjectHash::from_type_and_data(ObjectType::Tree, &tree_bytes);
    storage
        .put(&tree_oid, &tree_bytes, ObjectType::Tree)
        .expect("tree object");
    let snapshot = WorkspaceSnapshotV2 {
        schema_version: 2,
        workspace_id: workspace_id.to_string(),
        head: HeadState::Symbolic {
            reference: "refs/heads/main".to_string(),
        },
        index_tree_oid: tree_oid,
        raw_index_blob_oid: blob(b"raw-index"),
        working_copy_tree_oid: tree_oid,
        untracked_manifest_oid: blob(b"untracked"),
        sparse_facet_oid: None,
        sequencer_facet_oid: None,
        worktree_generation: 1,
        capture_policy: CapturePolicy::TrackedAndUntracked,
        completeness: Completeness::Full,
        facet_restore_policies: BTreeMap::new(),
    };
    let snapshot_oid = ObjectHash::from_type_and_data(
        ObjectType::Blob,
        &snapshot.to_canonical_bytes().expect("snapshot bytes"),
    );
    storage
        .put(
            &snapshot_oid,
            &snapshot.to_canonical_bytes().expect("snapshot bytes"),
            ObjectType::Blob,
        )
        .expect("snapshot object");
    (snapshot_oid, snapshot)
}

fn refs_facet(storage: &ClientStorage, references: serde_json::Value) -> ObjectHash {
    let bytes = serde_json::to_vec(&serde_json::json!({
        "schema_version": 1,
        "references": references,
    }))
    .expect("refs bytes");
    let oid = ObjectHash::from_type_and_data(ObjectType::Blob, &bytes);
    storage
        .put(&oid, &bytes, ObjectType::Blob)
        .expect("refs object");
    oid
}

fn branch_ref(name: &str, commit: &str) -> serde_json::Value {
    serde_json::json!({
        "id": 1,
        "name": name,
        "kind": "Branch",
        "commit": commit,
        "remote": null,
        "worktree_id": null,
    })
}

/// Publish one operation whose post view references `snapshot_oid` and the
/// given refs facet.
async fn publish_head(
    store: &OperationStoreV2,
    repo_id: &str,
    workspace_id: &str,
    op_id: &str,
    parents: Vec<String>,
    refs: ObjectHash,
    snapshot_oid: ObjectHash,
) {
    let view = RepoViewV2 {
        schema_version: 2,
        repo_id: repo_id.to_string(),
        refs_facet_oid: refs,
        workspaces: [(workspace_id.to_string(), snapshot_oid)]
            .into_iter()
            .collect(),
        change_roots: Vec::new(),
        extension_facets: Default::default(),
    };
    let view_oid = store.write_view_manifest(&view).expect("view manifest");
    store
        .write_operation(&OperationV2 {
            op_id: op_id.to_string(),
            parent_op_ids: parents,
            pre_view_oid: view_oid,
            post_view_oid: view_oid,
            kind: OperationKind::Command,
            status: OperationStatusV2::Success,
            metadata: OperationMetaV2::default(),
            restores_op_id: None,
            reverts_op_id: None,
            predecessor_map_oid: None,
        })
        .await
        .expect("operation");
}

#[tokio::test]
async fn concurrent_publications_preserve_sibling_heads() {
    let directory = tempdir().expect("repository");
    let database = db::create_database(directory.path().join("repo.db").to_str().unwrap())
        .await
        .expect("database");
    let storage = ClientStorage::init_local(directory.path().join("objects"));
    let store = OperationStoreV2::new_for_repo("repo", database, storage.clone());
    let scope_key = ""; // WorktreeScope::Main storage key

    store
        .cas_update_op_heads_at_generation("repo", scope_key, 0, &[], &["base".to_string()])
        .await
        .expect("baseline head");

    // Publisher A reads heads=[base], publishes op-a on top of it.
    store
        .cas_update_op_heads_at_generation(
            "repo",
            scope_key,
            store.read_head_generation("repo", scope_key).await.unwrap(),
            &["base".to_string()],
            &["op-a".to_string()],
        )
        .await
        .expect("first publisher wins");

    // Publisher B still expects heads=[base]: the strict CAS rejects it and
    // the middleware path retains both candidates as sibling heads
    // (ADR-OL-06) instead of overwriting the first publisher.
    let conflict = store
        .cas_update_op_heads(
            "repo",
            scope_key,
            &["base".to_string()],
            &["op-b".to_string()],
        )
        .await
        .expect_err("stale publisher must be rejected");
    assert!(conflict.to_string().contains("op-a"));
    store
        .merge_op_heads(
            "repo",
            scope_key,
            &["base".to_string()],
            &["op-b".to_string()],
        )
        .await
        .expect("sibling merge");
    let mut heads = store.read_heads("repo", scope_key).await.expect("heads");
    heads.sort();
    assert_eq!(heads, vec!["op-a".to_string(), "op-b".to_string()]);

    // A writer holding the pre-sibling generation must never publish again.
    let stale = store
        .cas_update_op_heads(
            "repo",
            scope_key,
            &["base".to_string()],
            &["op-c".to_string()],
        )
        .await;
    assert!(
        stale.is_err(),
        "the old writer must be fenced by generation"
    );
}

#[tokio::test]
async fn reconcile_converges_unambiguous_heads() {
    let directory = tempdir().expect("repository");
    let database = db::create_database(directory.path().join("repo.db").to_str().unwrap())
        .await
        .expect("database");
    let storage = ClientStorage::init_local(directory.path().join("objects"));
    let store = OperationStoreV2::new_for_repo("repo", database, storage.clone());
    let scope_key = ""; // WorktreeScope::Main storage key
    let refs = refs_facet(&storage, serde_json::json!([branch_ref("main", "1111")]));
    let (snapshot_a, _) = full_snapshot(&storage, "workspace-a");
    let (snapshot_b, _) = full_snapshot(&storage, "workspace-b");

    store
        .cas_update_op_heads_at_generation("repo", scope_key, 0, &[], &["base".to_string()])
        .await
        .expect("baseline head");
    publish_head(
        &store,
        "repo",
        "workspace-a",
        "op-a",
        vec!["base".to_string()],
        refs,
        snapshot_a,
    )
    .await;
    store
        .cas_update_op_heads(
            "repo",
            scope_key,
            &["base".to_string()],
            &["op-a".to_string()],
        )
        .await
        .expect("publisher a wins");
    publish_head(
        &store,
        "repo",
        "workspace-b",
        "op-b",
        vec!["base".to_string()],
        refs,
        snapshot_b,
    )
    .await;
    // The concurrent publisher B retains its candidate as a sibling head.
    store
        .merge_op_heads(
            "repo",
            scope_key,
            &["base".to_string()],
            &["op-b".to_string()],
        )
        .await
        .expect("retain both heads");

    let pinned = scope(directory.path(), directory.path());
    let engine = ReconcileEngine::new(pinned, "repo", store.clone());
    let outcome = engine.reconcile(false).await.expect("reconcile");
    match &outcome {
        ReconcileOutcome::Converged {
            reconcile_op_id,
            parents,
            ..
        } => {
            let mut actual = parents.clone();
            actual.sort();
            assert_eq!(actual, vec!["op-a".to_string(), "op-b".to_string()]);
            let reconcile = store
                .load_operation(reconcile_op_id)
                .await
                .expect("load")
                .expect("reconcile operation");
            assert_eq!(reconcile.kind, OperationKind::Reconcile);
            assert_eq!(reconcile.status, OperationStatusV2::Success);
            let view = store
                .load_view(&reconcile.post_view_oid)
                .expect("converged view");
            assert!(view.workspaces.contains_key("workspace-a"));
            assert!(view.workspaces.contains_key("workspace-b"));
        }
        other => panic!("expected convergence, got {other:?}"),
    }
    let heads = store.read_heads("repo", scope_key).await.expect("heads");
    assert_eq!(heads.len(), 1, "reconcile must converge to a single head");

    // A second pass has nothing left to do.
    let again = engine.reconcile(false).await.expect("second reconcile");
    assert_eq!(again, ReconcileOutcome::NothingToReconcile);
}

#[tokio::test]
async fn reconcile_reports_conflicting_refs_without_guessing() {
    let directory = tempdir().expect("repository");
    let database = db::create_database(directory.path().join("repo.db").to_str().unwrap())
        .await
        .expect("database");
    let storage = ClientStorage::init_local(directory.path().join("objects"));
    let store = OperationStoreV2::new_for_repo("repo", database, storage.clone());
    let scope_key = ""; // WorktreeScope::Main storage key
    let refs_a = refs_facet(&storage, serde_json::json!([branch_ref("main", "aaaa")]));
    let refs_b = refs_facet(&storage, serde_json::json!([branch_ref("main", "bbbb")]));
    let (snapshot, _) = full_snapshot(&storage, "main");
    store
        .cas_update_op_heads_at_generation("repo", scope_key, 0, &[], &["base".to_string()])
        .await
        .expect("baseline head");
    publish_head(
        &store,
        "repo",
        "main",
        "op-a",
        vec!["base".to_string()],
        refs_a,
        snapshot,
    )
    .await;
    store
        .cas_update_op_heads(
            "repo",
            scope_key,
            &["base".to_string()],
            &["op-a".to_string()],
        )
        .await
        .expect("publisher a wins");
    publish_head(
        &store,
        "repo",
        "main",
        "op-b",
        vec!["base".to_string()],
        refs_b,
        snapshot,
    )
    .await;
    store
        .merge_op_heads(
            "repo",
            scope_key,
            &["base".to_string()],
            &["op-b".to_string()],
        )
        .await
        .expect("two sibling heads");

    let pinned = scope(directory.path(), directory.path());
    let engine = ReconcileEngine::new(pinned, "repo", store.clone());
    let outcome = engine.reconcile(false).await.expect("reconcile");
    let ReconcileOutcome::Conflicted { conflicts } = outcome else {
        panic!("a diverging ref target must be reported as a conflict");
    };
    assert_eq!(conflicts.len(), 1);
    assert_eq!(conflicts[0].name, "main");
    let mut targets: Vec<&String> = conflicts[0].targets.values().collect();
    targets.sort();
    assert_eq!(targets, vec!["aaaa", "bbbb"], "both sides are reported");

    // The head set is preserved: reconciliation never guesses a winner.
    let mut heads = store.read_heads("repo", scope_key).await.expect("heads");
    heads.sort();
    assert_eq!(heads, vec!["op-a".to_string(), "op-b".to_string()]);
}

#[tokio::test]
async fn reconcile_dry_run_does_not_write_objects() {
    let directory = tempdir().expect("repository");
    let database = db::create_database(directory.path().join("repo.db").to_str().unwrap())
        .await
        .expect("database");
    let storage = ClientStorage::init_local(directory.path().join("objects"));
    let store = OperationStoreV2::new_for_repo("repo", database, storage.clone());
    let scope_key = ""; // WorktreeScope::Main storage key
    let refs = refs_facet(&storage, serde_json::json!([branch_ref("main", "1111")]));
    let (snapshot_a, _) = full_snapshot(&storage, "workspace-a");
    let (snapshot_b, _) = full_snapshot(&storage, "workspace-b");

    store
        .cas_update_op_heads_at_generation("repo", scope_key, 0, &[], &["base".to_string()])
        .await
        .expect("baseline head");
    publish_head(
        &store,
        "repo",
        "workspace-a",
        "op-a",
        vec!["base".to_string()],
        refs,
        snapshot_a,
    )
    .await;
    store
        .cas_update_op_heads(
            "repo",
            scope_key,
            &["base".to_string()],
            &["op-a".to_string()],
        )
        .await
        .expect("publisher a wins");
    publish_head(
        &store,
        "repo",
        "workspace-b",
        "op-b",
        vec!["base".to_string()],
        refs,
        snapshot_b,
    )
    .await;
    store
        .merge_op_heads(
            "repo",
            scope_key,
            &["base".to_string()],
            &["op-b".to_string()],
        )
        .await
        .expect("retain both heads");

    let objects_before = fs::read_dir(directory.path().join("objects"))
        .expect("objects dir")
        .filter_map(Result::ok)
        .map(|entry| entry.file_name())
        .collect::<std::collections::BTreeSet<_>>();

    let pinned = scope(directory.path(), directory.path());
    let engine = ReconcileEngine::new(pinned, "repo", store.clone());
    let outcome = engine.reconcile(true).await.expect("dry-run reconcile");
    assert!(matches!(outcome, ReconcileOutcome::DryRunConverged { .. }));

    let objects_after = fs::read_dir(directory.path().join("objects"))
        .expect("objects dir")
        .filter_map(Result::ok)
        .map(|entry| entry.file_name())
        .collect::<std::collections::BTreeSet<_>>();
    assert_eq!(
        objects_before, objects_after,
        "dry-run must not write any object"
    );

    // The head set is untouched by a dry run.
    let mut heads = store.read_heads("repo", scope_key).await.expect("heads");
    heads.sort();
    assert_eq!(heads, vec!["op-a".to_string(), "op-b".to_string()]);
}

#[tokio::test]
async fn reconcile_conflict_exits_non_zero_with_stable_code() {
    let _test_lock = lock_cli_repository_tests().await;
    let repository = tempdir().expect("repository");
    libra::utils::test::setup_with_new_libra_in(repository.path()).await;
    fs::write(repository.path().join("a.txt"), "one\n").expect("file");
    for args in [
        &["add", "a.txt"][..],
        &["commit", "-m", "first", "--no-verify"][..],
    ] {
        let output = Command::new(env!("CARGO_BIN_EXE_libra"))
            .env("LIBRA_SKIP_WEB_BUILD", "1")
            .args(args)
            .current_dir(repository.path())
            .output()
            .expect("libra command");
        assert!(output.status.success());
    }
    let pinned = RequestScope::resolve(repository.path().to_path_buf()).expect("scope");
    let database = get_db_conn_instance_for_path(&pinned.storage.join(util::DATABASE))
        .await
        .expect("database");
    let repo_id = ConfigKv::get_with_conn(&database, "libra.repoid")
        .await
        .expect("repo id")
        .expect("repo id entry")
        .value;
    let storage = ClientStorage::init_local(pinned.storage.join("objects"));
    let store = OperationStoreV2::new_for_repo(&repo_id, database, storage.clone());
    let baseline_id = store
        .read_heads(&repo_id, pinned.scope.storage_key())
        .await
        .expect("heads")
        .first()
        .cloned()
        .expect("baseline head");
    let baseline = store
        .load_operation(&baseline_id)
        .await
        .expect("baseline")
        .expect("baseline operation");

    // A diverging branch ref between two sibling heads must be reported as a
    // conflict with a non-zero exit code (LBR-CONFLICT-002).
    let baseline_view = store
        .load_view(&baseline.post_view_oid)
        .expect("baseline view");
    let main_snapshot = baseline_view
        .workspaces
        .get("main")
        .copied()
        .expect("baseline main workspace snapshot");
    let branch_ref = |commit: &str| {
        serde_json::json!({
            "id": 1,
            "name": "main",
            "kind": "Branch",
            "commit": commit,
            "remote": null,
            "worktree_id": null,
        })
    };
    for (op, refs) in [
        ("reconcile-conflict-a", branch_ref("aaaa")),
        ("reconcile-conflict-b", branch_ref("bbbb")),
    ] {
        let view = RepoViewV2 {
            schema_version: 2,
            repo_id: repo_id.clone(),
            refs_facet_oid: refs_facet(&storage, serde_json::json!([refs])),
            workspaces: [("main".to_string(), main_snapshot)].into_iter().collect(),
            change_roots: Vec::new(),
            extension_facets: Default::default(),
        };
        let view_oid = store.write_view_manifest(&view).expect("view manifest");
        store
            .write_operation(&OperationV2 {
                op_id: op.to_string(),
                parent_op_ids: vec![baseline_id.clone()],
                pre_view_oid: view_oid,
                post_view_oid: view_oid,
                kind: OperationKind::Command,
                status: OperationStatusV2::Success,
                metadata: OperationMetaV2::default(),
                restores_op_id: None,
                reverts_op_id: None,
                predecessor_map_oid: None,
            })
            .await
            .expect("operation");
    }
    store
        .cas_update_op_heads(
            &repo_id,
            pinned.scope.storage_key(),
            std::slice::from_ref(&baseline_id),
            &["reconcile-conflict-a".to_string()],
        )
        .await
        .expect("publish conflict a");
    store
        .merge_op_heads(
            &repo_id,
            pinned.scope.storage_key(),
            std::slice::from_ref(&baseline_id),
            &["reconcile-conflict-b".to_string()],
        )
        .await
        .expect("retain both conflict heads");

    let output = Command::new(env!("CARGO_BIN_EXE_libra"))
        .env("LIBRA_SKIP_WEB_BUILD", "1")
        .args(["op", "reconcile", "--json"])
        .current_dir(repository.path())
        .output()
        .expect("reconcile");
    assert!(
        !output.status.success(),
        "reconcile conflict must exit non-zero: {:?}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("LBR-CONFLICT-002"),
        "reconcile conflict must carry the stable conflict code: {stderr}"
    );

    // The head set is preserved on conflict.
    let mut heads = store
        .read_heads(&repo_id, pinned.scope.storage_key())
        .await
        .expect("heads");
    heads.sort();
    assert_eq!(
        heads,
        vec![
            "reconcile-conflict-a".to_string(),
            "reconcile-conflict-b".to_string()
        ]
    );
}

#[tokio::test]
async fn reconcile_nothing_to_do_on_single_head() {
    let _test_lock = lock_cli_repository_tests().await;
    let repository = tempdir().expect("repository");
    libra::utils::test::setup_with_new_libra_in(repository.path()).await;
    fs::write(repository.path().join("a.txt"), "one\n").expect("file");
    let output = Command::new(env!("CARGO_BIN_EXE_libra"))
        .env("LIBRA_SKIP_WEB_BUILD", "1")
        .args(["add", "a.txt"])
        .current_dir(repository.path())
        .output()
        .expect("add");
    assert!(output.status.success());
    let output = Command::new(env!("CARGO_BIN_EXE_libra"))
        .env("LIBRA_SKIP_WEB_BUILD", "1")
        .args(["commit", "-m", "first", "--no-verify"])
        .current_dir(repository.path())
        .output()
        .expect("commit");
    assert!(output.status.success());

    let pinned = RequestScope::resolve(repository.path().to_path_buf()).expect("scope");
    let database = get_db_conn_instance_for_path(&pinned.storage.join(util::DATABASE))
        .await
        .expect("database");
    let repo_id = ConfigKv::get_with_conn(&database, "libra.repoid")
        .await
        .expect("repo id")
        .expect("repo id entry")
        .value;
    let storage = ClientStorage::init_local(pinned.storage.join("objects"));
    let store = OperationStoreV2::new_for_repo(&repo_id, database, storage);

    // A single-head repository has nothing to reconcile.
    let scope = RequestScope::resolve(repository.path().to_path_buf()).expect("scope");
    let engine = ReconcileEngine::new(scope, &repo_id, store);
    let outcome = engine.reconcile(false).await.expect("reconcile");
    assert_eq!(outcome, ReconcileOutcome::NothingToReconcile);
}

/// After reconcile converges the head set, the transition commands regain
/// their single-head precondition: a dry-run `op undo` of the converged head
/// succeeds where it was rejected while two sibling heads existed.
#[tokio::test]
async fn restore_is_available_after_reconcile_converges() {
    let _test_lock = lock_cli_repository_tests().await;
    let repository = tempdir().expect("repository");
    libra::utils::test::setup_with_new_libra_in(repository.path()).await;
    fs::write(repository.path().join("a.txt"), "one\n").expect("file");
    for args in [
        &["add", "a.txt"][..],
        &["commit", "-m", "first", "--no-verify"][..],
    ] {
        let output = Command::new(env!("CARGO_BIN_EXE_libra"))
            .env("LIBRA_SKIP_WEB_BUILD", "1")
            .args(args)
            .current_dir(repository.path())
            .output()
            .expect("libra command");
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
    }
    let pinned = RequestScope::resolve(repository.path().to_path_buf()).expect("scope");
    let database = get_db_conn_instance_for_path(&pinned.storage.join(util::DATABASE))
        .await
        .expect("database");
    let repo_id = ConfigKv::get_with_conn(&database, "libra.repoid")
        .await
        .expect("repo id")
        .expect("repo id entry")
        .value;
    let storage = ClientStorage::init_local(pinned.storage.join("objects"));
    let store = OperationStoreV2::new_for_repo(&repo_id, database.clone(), storage.clone());
    let baseline_id = store
        .read_heads(&repo_id, pinned.scope.storage_key())
        .await
        .expect("heads")
        .first()
        .cloned()
        .expect("baseline head");
    let baseline = store
        .load_operation(&baseline_id)
        .await
        .expect("baseline")
        .expect("baseline operation");
    let baseline_view = store
        .load_view(&baseline.post_view_oid)
        .expect("baseline view");
    let main_snapshot = baseline_view
        .workspaces
        .get("main")
        .copied()
        .expect("baseline main workspace snapshot");

    // Two sibling operations sharing a provably identical refs facet.
    let refs = baseline_view.refs_facet_oid;
    for op in ["reconcile-a", "reconcile-b"] {
        let view = RepoViewV2 {
            schema_version: 2,
            repo_id: repo_id.clone(),
            refs_facet_oid: refs,
            workspaces: [("main".to_string(), main_snapshot)].into_iter().collect(),
            change_roots: Vec::new(),
            extension_facets: Default::default(),
        };
        let view_oid = store.write_view_manifest(&view).expect("view manifest");
        store
            .write_operation(&OperationV2 {
                op_id: op.to_string(),
                parent_op_ids: vec![baseline_id.clone()],
                pre_view_oid: view_oid,
                post_view_oid: view_oid,
                kind: OperationKind::Command,
                status: OperationStatusV2::Success,
                metadata: OperationMetaV2::default(),
                restores_op_id: None,
                reverts_op_id: None,
                predecessor_map_oid: None,
            })
            .await
            .expect("operation");
    }
    store
        .cas_update_op_heads(
            &repo_id,
            pinned.scope.storage_key(),
            std::slice::from_ref(&baseline_id),
            &["reconcile-a".to_string()],
        )
        .await
        .expect("publish a");
    store
        .merge_op_heads(
            &repo_id,
            pinned.scope.storage_key(),
            std::slice::from_ref(&baseline_id),
            &["reconcile-b".to_string()],
        )
        .await
        .expect("retain both heads");

    // While the head set is forked, the transition refuses to guess.
    let output = Command::new(env!("CARGO_BIN_EXE_libra"))
        .env("LIBRA_SKIP_WEB_BUILD", "1")
        .args(["op", "undo", "reconcile-a", "--dry-run", "--force"])
        .current_dir(repository.path())
        .output()
        .expect("undo before reconcile");
    assert!(
        !output.status.success(),
        "a transition must be refused while sibling heads exist"
    );
    assert!(
        String::from_utf8_lossy(&output.stderr).contains("unique current operation head"),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );

    // Reconcile converges the head set.
    let output = Command::new(env!("CARGO_BIN_EXE_libra"))
        .env("LIBRA_SKIP_WEB_BUILD", "1")
        .args(["op", "reconcile", "--json"])
        .current_dir(repository.path())
        .output()
        .expect("reconcile");
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let heads = store
        .read_heads(&repo_id, pinned.scope.storage_key())
        .await
        .expect("heads");
    assert_eq!(heads.len(), 1, "reconcile converges to a single head");

    // A dry-run undo of the converged head is available again.
    let output = Command::new(env!("CARGO_BIN_EXE_libra"))
        .env("LIBRA_SKIP_WEB_BUILD", "1")
        .args(["op", "undo", &heads[0], "--dry-run", "--force"])
        .current_dir(repository.path())
        .output()
        .expect("undo after reconcile");
    assert!(
        output.status.success(),
        "restore/undo must be available after reconcile: {}",
        String::from_utf8_lossy(&output.stderr)
    );
}
