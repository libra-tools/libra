//! OL-11 append-only transition contracts.

use std::{fs, sync::OnceLock, thread, time::Duration};

use clap::Parser;
use git_internal::{hash::ObjectHash, internal::object::types::ObjectType};
use libra::internal::{
    config::ConfigKv,
    db::{self, get_db_conn_instance_for_path},
    operation::{
        DoctorEngine, JournalEntry, JournalPhase, OperationKind, OperationMetaV2,
        OperationStatusV2, OperationStoreV2, OperationV2, RepoViewV2, RestoreEngine,
        RestoreReceipt, RestoreWhat, UndoEngine, WorkspaceStatePointer,
    },
    worktree_scope::{RequestScope, WorktreeScope},
};
use libra::utils::client_storage::ClientStorage;
use libra::utils::util;
use std::process::Command;
use tempfile::tempdir;

fn oid(label: &[u8]) -> ObjectHash {
    ObjectHash::from_type_and_data(ObjectType::Blob, label)
}

fn scope(root: &std::path::Path) -> RequestScope {
    let gitdir = root.join(".libra");
    fs::create_dir_all(&gitdir).expect("gitdir");
    RequestScope {
        scope: WorktreeScope::Main,
        workdir: root.to_path_buf(),
        gitdir,
        storage: root.to_path_buf(),
        worktree_root: root.to_path_buf(),
    }
}

static CLI_REPOSITORY_LOCK: OnceLock<tokio::sync::Mutex<()>> = OnceLock::new();

async fn lock_cli_repository_tests() -> tokio::sync::MutexGuard<'static, ()> {
    CLI_REPOSITORY_LOCK
        .get_or_init(|| tokio::sync::Mutex::new(()))
        .lock()
        .await
}

#[test]
fn transition_kinds_are_stable_machine_values() {
    for (kind, expected) in [
        (OperationKind::Undo, "undo"),
        (OperationKind::Redo, "redo"),
        (OperationKind::Revert, "revert"),
    ] {
        assert_eq!(kind.to_string(), expected);
        let parsed = expected.parse::<OperationKind>().expect("kind parses");
        assert_eq!(parsed, kind);
    }
}

#[test]
fn transition_receipt_is_machine_stable_and_dry_run_safe() {
    let receipt = RestoreReceipt {
        target_op_id: "source-op".to_string(),
        target_view_oid: oid(b"target"),
        new_op_id: None,
        workspace_id: "main".to_string(),
        what: RestoreWhat::All,
        dry_run: true,
        restored_facets: vec!["working_copy".to_string(), "head".to_string()],
        changed_paths: 2,
    };
    let value = serde_json::to_value(receipt).expect("receipt serializes");
    assert_eq!(value["dry_run"], true);
    assert_eq!(value["new_op_id"], serde_json::Value::Null);
    assert_eq!(value["restored_facets"][0], "working_copy");
}

#[tokio::test]
async fn revert_parent_lineage_is_persisted_with_the_operation() {
    let directory = tempdir().expect("temporary repository");
    let database = db::create_database(directory.path().join("repo.db").to_str().unwrap())
        .await
        .expect("database");
    let storage = ClientStorage::init_local(directory.path().join("objects"));
    let refs = oid(b"refs");
    storage
        .put(&refs, b"refs", ObjectType::Blob)
        .expect("refs object");
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
            op_id: "parent".to_string(),
            parent_op_ids: Vec::new(),
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
        .expect("parent");
    store
        .write_operation(&OperationV2 {
            op_id: "revert".to_string(),
            parent_op_ids: vec!["parent".to_string()],
            pre_view_oid: view_oid,
            post_view_oid: view_oid,
            kind: OperationKind::Revert,
            status: OperationStatusV2::Success,
            metadata: OperationMetaV2::default(),
            restores_op_id: Some("target".to_string()),
            reverts_op_id: Some("parent".to_string()),
            predecessor_map_oid: None,
        })
        .await
        .expect("revert");
    let loaded = store
        .load_operation("revert")
        .await
        .expect("load")
        .expect("operation");
    assert_eq!(loaded.reverts_op_id.as_deref(), Some("parent"));
}

#[tokio::test]
async fn undo_refuses_a_non_current_head_without_touching_state() {
    let directory = tempdir().expect("temporary repository");
    let database = db::create_database(directory.path().join("repo.db").to_str().unwrap())
        .await
        .expect("database");
    let storage = ClientStorage::init_local(directory.path().join("objects"));
    let refs = oid(b"refs");
    storage.put(&refs, b"refs", ObjectType::Blob).expect("refs");
    let store = OperationStoreV2::new_for_repo("repo", database.clone(), storage.clone());
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
            op_id: "not-current".to_string(),
            parent_op_ids: Vec::new(),
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
    let restore = RestoreEngine::new(scope(directory.path()), "repo", database, storage);
    let error = UndoEngine::new(restore)
        .undo("not-current", true, false)
        .await
        .expect_err("non-head undo must fail closed");
    assert!(error.to_string().contains("unique current operation head"));
}

#[test]
fn transition_cli_exposes_force_for_dirty_state_override() {
    let args = libra::command::op::OpArgs::try_parse_from([
        "libra",
        "undo",
        "op-1",
        "--force",
        "--dry-run",
    ])
    .expect("undo args");
    let libra::command::op::OpCommand::Undo { force, dry_run, .. } = args.command else {
        panic!("expected undo");
    };
    assert!(force && dry_run);
}

#[tokio::test]
async fn doctor_reports_missing_object_and_unfinished_journal_read_only() {
    let directory = tempdir().expect("temporary repository");
    let database = db::create_database(directory.path().join("repo.db").to_str().unwrap())
        .await
        .expect("database");
    let storage = ClientStorage::init_local(directory.path().join("objects"));
    let store = OperationStoreV2::new_for_repo("repo", database, storage);
    let view_oid = oid(b"missing-view");
    // The operation points at a missing view object, exercising doctor’s
    // fail-closed read-only diagnosis path without enabling --fix.
    store
        .write_operation(&OperationV2 {
            op_id: "broken".to_string(),
            parent_op_ids: Vec::new(),
            pre_view_oid: view_oid,
            post_view_oid: view_oid,
            kind: OperationKind::Command,
            status: OperationStatusV2::Running,
            metadata: OperationMetaV2::default(),
            restores_op_id: None,
            reverts_op_id: None,
            predecessor_map_oid: None,
        })
        .await
        .expect("operation");
    store
        .append_journal(&JournalEntry {
            journal_id: "broken-journal".to_string(),
            op_id: "broken".to_string(),
            phase: JournalPhase::Mutation,
            pre_view_oid: Some(view_oid),
            target_view_oid: Some(view_oid),
            owner: "test".to_string(),
            updated_at: 1,
            recovery_payload: None,
        })
        .await
        .expect("journal");
    let engine = DoctorEngine::new(scope(directory.path()), "repo", store);
    let report = engine.inspect(true, false).await.expect("doctor report");
    assert!(
        report
            .issues
            .iter()
            .any(|issue| issue.code == "missing-view")
    );
    assert!(
        report
            .issues
            .iter()
            .any(|issue| issue.code == "unfinished-journal")
    );
    assert!(report.fixed.is_empty());
}

#[tokio::test]
async fn real_cli_operation_can_undo_and_redo_without_rewriting_the_target() {
    let _test_lock = lock_cli_repository_tests().await;
    let repository = tempdir().expect("repository");
    libra::utils::test::setup_with_new_libra_in(repository.path()).await;
    fs::write(repository.path().join("a.txt"), "one\n").expect("file");

    fn run_output(repository: &std::path::Path, args: &[&str]) -> std::process::Output {
        let output = Command::new(env!("CARGO_BIN_EXE_libra"))
            .env("LIBRA_SKIP_WEB_BUILD", "1")
            .args(args)
            .current_dir(repository)
            .output()
            .expect("libra command");
        thread::sleep(Duration::from_millis(250));
        output
    }

    fn run(repository: &std::path::Path, args: &[&str]) {
        let output = run_output(repository, args);
        assert!(
            output.status.success(),
            "libra {args:?} failed:\n{}",
            String::from_utf8_lossy(&output.stderr)
        );
    }

    run(repository.path(), &["add", "a.txt"]);
    run(repository.path(), &["commit", "-m", "first", "--no-verify"]);
    fs::write(repository.path().join("a.txt"), "two\n").expect("file update");
    run(repository.path(), &["add", "a.txt"]);
    run(
        repository.path(),
        &["commit", "-m", "second", "--no-verify"],
    );

    let pinned = RequestScope::resolve(repository.path().to_path_buf()).expect("scope");
    let database_path = pinned.storage.join(util::DATABASE);
    let database = get_db_conn_instance_for_path(&database_path)
        .await
        .expect("database");
    let repo_id = ConfigKv::get_with_conn(&database, "libra.repoid")
        .await
        .expect("repo id")
        .expect("repo id entry")
        .value;
    let storage = ClientStorage::init_local(pinned.storage.join("objects"));
    let store = OperationStoreV2::new_for_repo(&repo_id, database.clone(), storage.clone());
    let operations = store.list_operations().await.expect("operations");
    let target = operations
        .iter()
        .rev()
        .find(|operation| {
            operation.status == OperationStatusV2::Success
                && operation.parent_op_ids.len() == 1
                && operation.kind == OperationKind::Command
        })
        .expect("a v2 commit operation")
        .clone();
    let parent_id = target.parent_op_ids.first().expect("target parent").clone();
    run(
        repository.path(),
        &[
            "op",
            "revert",
            &target.op_id,
            "--parent",
            &parent_id,
            "--force",
        ],
    );
    let revert = store
        .list_operations()
        .await
        .expect("operations after revert")
        .into_iter()
        .rev()
        .find(|operation| operation.kind == OperationKind::Revert)
        .expect("revert operation");
    assert_eq!(revert.reverts_op_id.as_deref(), Some(parent_id.as_str()));

    run(repository.path(), &["op", "undo", &revert.op_id, "--force"]);
    let undo = store
        .list_operations()
        .await
        .expect("operations after undo")
        .into_iter()
        .rev()
        .find(|operation| operation.kind == OperationKind::Undo)
        .expect("undo operation");
    assert_eq!(undo.restores_op_id.as_deref(), Some(revert.op_id.as_str()));
    let target_post = store
        .load_view(&target.post_view_oid)
        .expect("target post view");
    let undo_post = store
        .load_view(&undo.post_view_oid)
        .expect("undo post view");
    let target_head = store
        .load_snapshot(
            target_post
                .workspaces
                .values()
                .next()
                .expect("target snapshot"),
        )
        .expect("target snapshot")
        .head;
    let undo_head = store
        .load_snapshot(undo_post.workspaces.values().next().expect("undo snapshot"))
        .expect("undo snapshot")
        .head;
    assert_eq!(
        undo_head, target_head,
        "undo must restore the target commit HEAD"
    );

    run(repository.path(), &["op", "redo", &undo.op_id, "--force"]);
    let redo = store
        .list_operations()
        .await
        .expect("operations after redo")
        .into_iter()
        .rev()
        .find(|operation| operation.kind == OperationKind::Redo)
        .expect("redo operation");
    assert_eq!(redo.restores_op_id.as_deref(), Some(revert.op_id.as_str()));
    assert_eq!(redo.metadata.command_name.as_deref(), Some("op redo"));
}

#[tokio::test]
async fn undo_crash_matrix_recovers_each_journal_phase() {
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
    let store = OperationStoreV2::new_for_repo(&repo_id, database.clone(), storage.clone());
    let expected_heads = store
        .read_heads(&repo_id, pinned.scope.storage_key())
        .await
        .expect("heads");
    assert_eq!(
        expected_heads.len(),
        1,
        "crash matrix needs one baseline head"
    );
    let baseline = store
        .load_operation(&expected_heads[0])
        .await
        .expect("baseline")
        .expect("baseline operation");
    let phases = [
        JournalPhase::Reserved,
        JournalPhase::PreView,
        JournalPhase::Mutation,
        JournalPhase::PostView,
        JournalPhase::Publish,
    ];
    for (index, phase) in phases.into_iter().enumerate() {
        let op_id = format!("crash-undo-{index}");
        store
            .write_operation(&OperationV2 {
                op_id: op_id.clone(),
                parent_op_ids: expected_heads.clone(),
                pre_view_oid: baseline.post_view_oid,
                post_view_oid: baseline.post_view_oid,
                kind: OperationKind::Undo,
                status: OperationStatusV2::Running,
                metadata: OperationMetaV2::default(),
                restores_op_id: Some(baseline.op_id.clone()),
                reverts_op_id: None,
                predecessor_map_oid: None,
            })
            .await
            .expect("crash operation");
        store
            .append_journal(&JournalEntry {
                journal_id: format!("journal-{op_id}"),
                op_id: op_id.clone(),
                phase,
                pre_view_oid: Some(baseline.post_view_oid),
                target_view_oid: Some(baseline.post_view_oid),
                owner: "test".to_string(),
                updated_at: index as i64 + 1,
                recovery_payload: Some(
                    serde_json::json!({
                        "kind": "restore",
                        "restore_refs": false,
                        "workspace_id": "main"
                    })
                    .to_string(),
                ),
            })
            .await
            .expect("crash journal");
        if phase == JournalPhase::Publish {
            store
                .cas_update_op_heads(
                    &repo_id,
                    pinned.scope.storage_key(),
                    &expected_heads,
                    &[op_id],
                )
                .await
                .expect("publish crash head");
        }
        let engine =
            RestoreEngine::new(pinned.clone(), &repo_id, database.clone(), storage.clone());
        let recovery = engine
            .recover_interrupted_operations(pinned.scope.storage_key())
            .await;
        assert!(
            recovery.is_err(),
            "recovery reports the interrupted operation"
        );
        assert_eq!(
            store
                .read_heads(&repo_id, pinned.scope.storage_key())
                .await
                .expect("recovered heads"),
            expected_heads,
            "phase {phase} must roll back to the baseline head"
        );
    }
}

#[tokio::test]
async fn doctor_dry_run_does_not_repair_and_fix_rebuilds_a_missing_pointer() {
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
    let store = OperationStoreV2::new_for_repo(&repo_id, database, storage);
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
    let bad_id = "doctor-bad-journal";
    store
        .write_operation(&OperationV2 {
            op_id: bad_id.to_string(),
            parent_op_ids: vec![baseline_id],
            pre_view_oid: baseline.post_view_oid,
            post_view_oid: baseline.post_view_oid,
            kind: OperationKind::Undo,
            status: OperationStatusV2::Running,
            metadata: OperationMetaV2::default(),
            restores_op_id: Some(baseline.op_id),
            reverts_op_id: None,
            predecessor_map_oid: None,
        })
        .await
        .expect("bad operation");
    store
        .append_journal(&JournalEntry {
            journal_id: "doctor-bad-journal-row".to_string(),
            op_id: bad_id.to_string(),
            phase: JournalPhase::Mutation,
            pre_view_oid: Some(baseline.post_view_oid),
            target_view_oid: Some(baseline.post_view_oid),
            owner: "test".to_string(),
            updated_at: 1,
            recovery_payload: Some(
                serde_json::json!({
                    "kind": "restore",
                    "restore_refs": false,
                    "workspace_id": "main"
                })
                .to_string(),
            ),
        })
        .await
        .expect("bad journal");
    fs::remove_file(WorkspaceStatePointer::path(&pinned)).expect("remove pointer");
    let engine = DoctorEngine::new(pinned.clone(), repo_id, store.clone());
    let dry_run = engine.inspect(true, true).await.expect("dry-run report");
    assert!(
        dry_run
            .issues
            .iter()
            .any(|issue| issue.code == "missing-pointer")
    );
    assert!(dry_run.fixed.is_empty(), "dry-run must not repair");
    assert!(!WorkspaceStatePointer::path(&pinned).exists());
    let fixed = engine.inspect(false, true).await.expect("fix report");
    assert!(fixed.fixed.iter().any(|item| item == "workspace-pointer"));
    assert!(fixed.fixed.iter().any(|item| item == "unfinished-journals"));
    assert_eq!(
        store
            .load_operation(bad_id)
            .await
            .expect("recovered bad operation")
            .expect("bad operation row")
            .status,
        OperationStatusV2::Failed
    );
    assert!(WorkspaceStatePointer::load(&pinned).await.is_ok());
}
