//! OL-11 append-only transition contracts.

use std::{fs, process::Command, sync::OnceLock, thread, time::Duration};

use clap::Parser;
use git_internal::{hash::ObjectHash, internal::object::types::ObjectType};
use libra::{
    internal::{
        config::ConfigKv,
        db::{self, get_db_conn_instance_for_path},
        model::reference,
        operation::{
            DoctorEngine, JournalEntry, JournalPhase, MutationClass, OperationError, OperationKind,
            OperationMetaV2, OperationStatusV2, OperationStoreV2, OperationV2, RepoViewV2,
            RestoreEngine, RestoreReceipt, RestoreWhat, Staleness, UndoEngine,
            WorkspaceStatePointer, run_with_operation,
        },
        worktree_scope::{RequestScope, WorktreeScope},
    },
    utils::{client_storage::ClientStorage, util},
};
use sea_orm::EntityTrait;
use tempfile::tempdir;

fn oid(label: &[u8]) -> ObjectHash {
    ObjectHash::from_type_and_data(ObjectType::Blob, label)
}

async fn branch_tip(database: &sea_orm::DatabaseConnection, branch: &str) -> Option<String> {
    use sea_orm::{ConnectionTrait, DbBackend, Statement};
    let row = database
        .query_one_raw(Statement::from_sql_and_values(
            DbBackend::Sqlite,
            "SELECT `commit` FROM reference WHERE kind = 'Branch' AND name = ? \
             AND remote IS NULL",
            [branch.to_string().into()],
        ))
        .await
        .expect("branch tip query");
    row.and_then(|row| row.try_get::<Option<String>>("", "commit").ok().flatten())
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
async fn doctor_recovers_a_running_publish_journal_once() {
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
    let scope_key = pinned.scope.storage_key();
    let baseline_heads = store
        .read_heads(&repo_id, scope_key)
        .await
        .expect("baseline heads");
    let baseline_id = baseline_heads
        .first()
        .cloned()
        .expect("baseline operation head");
    let baseline = store
        .load_operation(&baseline_id)
        .await
        .expect("load baseline")
        .expect("baseline operation");

    let interrupted_id = "doctor-publish-interrupted";
    store
        .write_operation(&OperationV2 {
            op_id: interrupted_id.to_string(),
            parent_op_ids: baseline_heads.clone(),
            pre_view_oid: baseline.post_view_oid,
            post_view_oid: baseline.post_view_oid,
            kind: OperationKind::Undo,
            status: OperationStatusV2::Running,
            metadata: OperationMetaV2::default(),
            restores_op_id: Some(baseline_id.clone()),
            reverts_op_id: None,
            predecessor_map_oid: None,
        })
        .await
        .expect("interrupted operation");
    store
        .append_journal(&JournalEntry {
            journal_id: "doctor-publish-interrupted-journal".to_string(),
            op_id: interrupted_id.to_string(),
            phase: JournalPhase::Publish,
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
        .expect("publish journal");
    store
        .cas_update_op_heads(
            &repo_id,
            scope_key,
            &baseline_heads,
            &[interrupted_id.to_string()],
        )
        .await
        .expect("published interrupted head");
    // Model the physical restore having installed its target snapshot before
    // the process crashed after publishing the operation head.
    fs::write(repository.path().join("a.txt"), "two\n").expect("interrupted target state");

    // This terminal operation retains an earlier-phase journal. Doctor must
    // leave it alone after the active interrupted operation is recovered.
    let terminal_id = "doctor-terminal-operation";
    store
        .write_operation(&OperationV2 {
            op_id: terminal_id.to_string(),
            parent_op_ids: baseline_heads.clone(),
            pre_view_oid: baseline.post_view_oid,
            post_view_oid: baseline.post_view_oid,
            kind: OperationKind::Undo,
            status: OperationStatusV2::Failed,
            metadata: OperationMetaV2::default(),
            restores_op_id: Some(baseline_id),
            reverts_op_id: None,
            predecessor_map_oid: None,
        })
        .await
        .expect("terminal operation");
    store
        .append_journal(&JournalEntry {
            journal_id: "doctor-terminal-operation-journal".to_string(),
            op_id: terminal_id.to_string(),
            phase: JournalPhase::Mutation,
            pre_view_oid: Some(baseline.post_view_oid),
            target_view_oid: Some(baseline.post_view_oid),
            owner: "test".to_string(),
            updated_at: 2,
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
        .expect("terminal journal");

    let engine = DoctorEngine::new(pinned.clone(), repo_id.clone(), store.clone());
    let dry_run = engine.inspect(true, true).await.expect("doctor dry run");
    assert!(
        dry_run
            .issues
            .iter()
            .any(|issue| issue.code == "unfinished-journal"),
        "a running operation at Publish still needs recovery"
    );
    assert!(dry_run.fixed.is_empty(), "dry-run must not repair");
    assert_eq!(
        store
            .read_heads(&repo_id, scope_key)
            .await
            .expect("heads after dry run"),
        vec![interrupted_id.to_string()],
        "dry-run must preserve the published head"
    );
    assert_eq!(
        fs::read(repository.path().join("a.txt")).expect("file after dry run"),
        b"two\n",
        "dry-run must preserve the interrupted physical state"
    );

    let fixed = engine.inspect(false, true).await.expect("doctor fix");
    assert!(fixed.fixed.iter().any(|item| item == "unfinished-journals"));
    assert_eq!(
        store
            .load_operation(interrupted_id)
            .await
            .expect("load recovered operation")
            .expect("recovered operation")
            .status,
        OperationStatusV2::Failed,
        "doctor rolls back the interrupted publish"
    );
    assert_eq!(
        store
            .read_heads(&repo_id, scope_key)
            .await
            .expect("recovered heads"),
        baseline_heads,
        "doctor restores the prior head"
    );
    assert_eq!(
        fs::read(repository.path().join("a.txt")).expect("file after recovery"),
        b"one\n",
        "doctor must restore the physical pre-view as well as the operation head"
    );

    let second = engine
        .inspect(false, true)
        .await
        .expect("second doctor fix");
    assert!(
        !second
            .issues
            .iter()
            .any(|issue| issue.code == "unfinished-journal"),
        "terminal and recovered operations must not be re-diagnosed as unfinished"
    );
    assert!(
        !second
            .fixed
            .iter()
            .any(|item| item == "unfinished-journals"),
        "terminal journals must not trigger another recovery pass"
    );
}

/// A caller may hold a pinned main-worktree request while its process cwd is
/// another linked worktree of the same repository. Keep the mismatched cwd in
/// a child process so this regression cannot perturb parallel tests.
#[tokio::test]
async fn doctor_recovery_uses_pinned_main_scope_from_linked_cwd() {
    let _test_lock = lock_cli_repository_tests().await;
    let directory = tempdir().expect("repository fixture");
    let main = directory.path().join("main");
    let linked = directory.path().join("linked");
    fs::create_dir(&main).expect("main worktree directory");
    libra::utils::test::setup_with_new_libra_in(&main).await;
    fs::write(main.join("a.txt"), b"one\n").expect("baseline file");
    for args in [
        &["add", "a.txt"][..],
        &["commit", "-m", "first", "--no-verify"][..],
    ] {
        let output = Command::new(env!("CARGO_BIN_EXE_libra"))
            .args(args)
            .current_dir(&main)
            .output()
            .expect("baseline CLI command");
        assert!(
            output.status.success(),
            "libra {args:?}: {}",
            String::from_utf8_lossy(&output.stderr)
        );
    }
    let output = Command::new(env!("CARGO_BIN_EXE_libra"))
        .args(["worktree", "add", linked.to_str().expect("linked path")])
        .current_dir(&main)
        .output()
        .expect("create linked worktree");
    assert!(
        output.status.success(),
        "libra worktree add: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let linked_file = linked.join("a.txt");
    assert_eq!(
        fs::read(&linked_file).expect("linked checkout file"),
        b"one\n"
    );
    fs::write(&linked_file, b"linked-only-state\n").expect("linked-only sentinel");

    let child = Command::new(std::env::current_exe().expect("test binary"))
        .arg("doctor_recovery_linked_cwd_child")
        .arg("--exact")
        .arg("--nocapture")
        .env("LIBRA_FIX_CM01_MAIN", &main)
        .current_dir(&linked)
        .output()
        .expect("linked-cwd recovery child");
    let stdout = String::from_utf8_lossy(&child.stdout);
    let stderr = String::from_utf8_lossy(&child.stderr);
    assert!(
        stdout.contains("running 1 test")
            && stdout.contains("test doctor_recovery_linked_cwd_child"),
        "child test selection must execute exactly once; stdout:\n{stdout}\nstderr:\n{stderr}"
    );
    assert!(
        child.status.success(),
        "pinned main recovery from linked cwd failed; stdout:\n{stdout}\nstderr:\n{stderr}"
    );
}

#[tokio::test]
async fn doctor_recovery_linked_cwd_child() {
    let Some(main) = std::env::var_os("LIBRA_FIX_CM01_MAIN") else {
        return;
    };
    let main = std::path::PathBuf::from(main);
    let cwd = std::env::current_dir().expect("child cwd");
    assert!(matches!(WorktreeScope::current(), WorktreeScope::Linked(_)));
    let pinned = RequestScope::resolve(main.clone()).expect("pinned main scope");
    assert_eq!(pinned.scope, WorktreeScope::Main);
    println!("linked child cwd: {}", cwd.display());

    let database = get_db_conn_instance_for_path(&pinned.storage.join(util::DATABASE))
        .await
        .expect("main database");
    let repo_id = ConfigKv::get_with_conn(&database, "libra.repoid")
        .await
        .expect("repo id query")
        .expect("repo id")
        .value;
    let storage = ClientStorage::init_local(pinned.storage.join("objects"));
    let store = OperationStoreV2::new_for_repo(&repo_id, database, storage);
    let scope_key = pinned.scope.storage_key();
    let baseline_heads = store
        .read_heads(&repo_id, scope_key)
        .await
        .expect("baseline heads");
    let baseline_id = baseline_heads.first().cloned().expect("baseline head");
    let baseline = store
        .load_operation(&baseline_id)
        .await
        .expect("baseline query")
        .expect("baseline operation");
    let interrupted_id = "linked-cwd-doctor-publish-interrupted";
    store
        .write_operation(&OperationV2 {
            op_id: interrupted_id.to_string(),
            parent_op_ids: baseline_heads.clone(),
            pre_view_oid: baseline.post_view_oid,
            post_view_oid: baseline.post_view_oid,
            kind: OperationKind::Undo,
            status: OperationStatusV2::Running,
            metadata: OperationMetaV2::default(),
            restores_op_id: Some(baseline_id),
            reverts_op_id: None,
            predecessor_map_oid: None,
        })
        .await
        .expect("interrupted operation");
    store
        .append_journal(&JournalEntry {
            journal_id: format!("journal-{interrupted_id}"),
            op_id: interrupted_id.to_string(),
            phase: JournalPhase::Publish,
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
        .expect("publish journal");
    store
        .cas_update_op_heads(
            &repo_id,
            scope_key,
            &baseline_heads,
            &[interrupted_id.to_string()],
        )
        .await
        .expect("published interrupted head");
    fs::write(main.join("a.txt"), b"two\n").expect("interrupted physical state");

    let mut references_before = reference::Entity::find()
        .all(store.db())
        .await
        .expect("reference rows before recovery");
    references_before.sort_by_key(|row| row.id);
    let head_rows = references_before
        .iter()
        .filter(|row| row.kind == reference::ConfigKind::Head && row.remote.is_none())
        .collect::<Vec<_>>();
    assert_eq!(
        head_rows.len(),
        2,
        "main and linked must each own a HEAD row"
    );
    assert!(head_rows.iter().any(|row| row.worktree_id.is_none()));
    let WorktreeScope::Linked(linked_id) = WorktreeScope::current() else {
        panic!("child must remain in the linked worktree");
    };
    assert!(
        head_rows
            .iter()
            .any(|row| row.worktree_id.as_deref() == Some(linked_id.as_str())),
        "the linked HEAD row must exist before recovery"
    );
    let linked_file_before = fs::read(cwd.join("a.txt")).expect("linked file before recovery");
    assert_eq!(linked_file_before, b"linked-only-state\n");

    let engine = DoctorEngine::new(pinned.clone(), repo_id.clone(), store.clone());
    engine.inspect(false, true).await.expect("doctor recovery");
    assert_eq!(
        fs::read(main.join("a.txt")).expect("recovered file"),
        b"one\n",
        "doctor must restore pinned main pre-view, independent of child cwd"
    );
    assert_eq!(
        store
            .read_heads(&repo_id, scope_key)
            .await
            .expect("recovered heads"),
        baseline_heads,
        "doctor must restore the pinned main operation head"
    );
    assert_eq!(
        store
            .load_operation(interrupted_id)
            .await
            .expect("recovered operation query")
            .expect("recovered operation")
            .status,
        OperationStatusV2::Failed,
        "doctor must mark the interrupted publish as failed"
    );
    let mut references_after = reference::Entity::find()
        .all(store.db())
        .await
        .expect("reference rows after recovery");
    references_after.sort_by_key(|row| row.id);
    assert_eq!(
        references_after, references_before,
        "doctor must preserve complete main/linked HEAD rows and shared refs without adding rows"
    );
    assert_eq!(
        fs::read(cwd.join("a.txt")).expect("linked file after recovery"),
        linked_file_before,
        "doctor must preserve linked worktree bytes"
    );
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

/// The P1 crash window: a normal `commit`-class command crashes between the
/// head compare-and-swap and the workspace pointer save. The head is the new
/// Running operation while the pointer still references the parent, so every
/// subsequent mutation refuses with a stale pointer. `op doctor --fix` must
/// recover in one pass: advance the interrupted head to Success and rebuild
/// the pointer to its post-view snapshot.
#[tokio::test]
async fn doctor_recovers_crash_between_head_cas_and_pointer_save() {
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
    let crashed_id = "crash-between-cas-and-pointer";
    store
        .write_operation(&OperationV2 {
            op_id: crashed_id.to_string(),
            parent_op_ids: vec![baseline_id.clone()],
            pre_view_oid: baseline.post_view_oid,
            post_view_oid: baseline.post_view_oid,
            kind: OperationKind::Command,
            status: OperationStatusV2::Running,
            metadata: OperationMetaV2 {
                command_name: Some("commit".to_string()),
                ..OperationMetaV2::default()
            },
            restores_op_id: None,
            reverts_op_id: None,
            predecessor_map_oid: None,
        })
        .await
        .expect("crashed operation");
    store
        .append_journal(&JournalEntry {
            journal_id: format!("journal-{crashed_id}"),
            op_id: crashed_id.to_string(),
            phase: JournalPhase::Publish,
            pre_view_oid: Some(baseline.post_view_oid),
            target_view_oid: Some(baseline.post_view_oid),
            owner: "test".to_string(),
            updated_at: 1,
            recovery_payload: None,
        })
        .await
        .expect("crashed journal");
    store
        .cas_update_op_heads(
            &repo_id,
            pinned.scope.storage_key(),
            std::slice::from_ref(&baseline_id),
            &[crashed_id.to_string()],
        )
        .await
        .expect("advance head to the crashed operation");

    // The pointer still references the baseline (parent) operation, so it is
    // stale against the crashed head. A normal mutation must refuse.
    let heads_view = store
        .read_heads_view(&repo_id, pinned.scope.storage_key())
        .await
        .expect("heads view");
    let pointer = WorkspaceStatePointer::load(&pinned).await.expect("pointer");
    assert_eq!(pointer.staleness(&heads_view), Staleness::Stale);
    let mutation = run_with_operation(
        &pinned,
        OperationMetaV2 {
            command_name: Some("commit".to_string()),
            ..OperationMetaV2::default()
        },
        MutationClass::RepoMutation,
        |_| async { Ok::<_, _>(()) },
    )
    .await;
    assert!(
        matches!(mutation, Err(OperationError::Stale(ref text)) if text.contains("op doctor --fix")),
        "stale mutation must point at the doctor recovery path: {mutation:?}"
    );

    // `op doctor --fix` recovers the interrupted head and rebuilds the pointer.
    let engine = DoctorEngine::new(pinned.clone(), repo_id.clone(), store.clone());
    let fixed = engine.inspect(false, true).await.expect("fix report");
    assert!(
        fixed.fixed.iter().any(|item| item == "unfinished-journals"),
        "doctor must report the recovered interrupted operation: {fixed:?}"
    );
    assert_eq!(
        store
            .load_operation(crashed_id)
            .await
            .expect("recovered")
            .expect("crashed op row")
            .status,
        OperationStatusV2::Success,
        "a published command head that completed its mutation is advanced to Success"
    );
    let heads_view = store
        .read_heads_view(&repo_id, pinned.scope.storage_key())
        .await
        .expect("heads view");
    let pointer = WorkspaceStatePointer::load(&pinned).await.expect("pointer");
    assert_eq!(
        pointer.staleness(&heads_view),
        Staleness::Fresh,
        "doctor must rebuild the pointer to the recovered head"
    );

    // A normal mutation now succeeds without manual intervention.
    let mutation = run_with_operation(
        &pinned,
        OperationMetaV2 {
            command_name: Some("commit".to_string()),
            ..OperationMetaV2::default()
        },
        MutationClass::RepoMutation,
        |_| async { Ok::<_, _>(()) },
    )
    .await;
    assert!(mutation.is_ok(), "post-recovery mutation must succeed");
}

/// An interrupted command that never reached publication (crash before the
/// head CAS) is a global orphan and must be failed closed by doctor, while an
/// unpublished running operation still referenced as a head by another scope
/// is left untouched.
#[tokio::test]
async fn doctor_fails_orphaned_running_command_but_not_foreign_scope_head() {
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

    // Orphaned running command: crash happened before the head CAS.
    let orphan_id = "crash-before-publish-orphan";
    store
        .write_operation(&OperationV2 {
            op_id: orphan_id.to_string(),
            parent_op_ids: vec![baseline_id.clone()],
            pre_view_oid: baseline.post_view_oid,
            post_view_oid: baseline.post_view_oid,
            kind: OperationKind::Command,
            status: OperationStatusV2::Running,
            metadata: OperationMetaV2::default(),
            restores_op_id: None,
            reverts_op_id: None,
            predecessor_map_oid: None,
        })
        .await
        .expect("orphan operation");

    // Running head owned by a foreign scope (e.g. a linked worktree).
    let foreign_id = "crash-foreign-scope-head";
    store
        .write_operation(&OperationV2 {
            op_id: foreign_id.to_string(),
            parent_op_ids: vec![baseline_id.clone()],
            pre_view_oid: baseline.post_view_oid,
            post_view_oid: baseline.post_view_oid,
            kind: OperationKind::Command,
            status: OperationStatusV2::Running,
            metadata: OperationMetaV2::default(),
            restores_op_id: None,
            reverts_op_id: None,
            predecessor_map_oid: None,
        })
        .await
        .expect("foreign operation");
    store
        .cas_update_op_heads(
            &repo_id,
            "foreign-scope",
            &[],
            std::slice::from_ref(&baseline_id),
        )
        .await
        .expect("foreign baseline");
    store
        .cas_update_op_heads(
            &repo_id,
            "foreign-scope",
            std::slice::from_ref(&baseline_id),
            &[foreign_id.to_string()],
        )
        .await
        .expect("foreign head");

    let engine = DoctorEngine::new(pinned.clone(), repo_id, store.clone());
    let fixed = engine.inspect(false, true).await.expect("fix report");
    assert_eq!(
        store
            .load_operation(orphan_id)
            .await
            .expect("orphan")
            .expect("orphan row")
            .status,
        OperationStatusV2::Failed,
        "a globally orphaned running command is failed closed"
    );
    assert_eq!(
        store
            .load_operation(foreign_id)
            .await
            .expect("foreign")
            .expect("foreign row")
            .status,
        OperationStatusV2::Running,
        "a running head owned by another scope must not be touched"
    );
    assert!(
        fixed.fixed.iter().any(|item| item == "unfinished-journals"),
        "doctor must report the orphaned operation as fixed: {fixed:?}"
    );
}

#[tokio::test]
async fn undo_restores_symbolic_head_branch_tip_to_exact_commit() {
    // Review P1: a default undo/restore must restore the commit OID that the
    // symbolic HEAD branch points at, not just the symbolic reference name.
    // Assert against the actual `reference` table commit, never the branch
    // name alone.
    let _test_lock = lock_cli_repository_tests().await;
    let repository = tempdir().expect("repository");
    libra::utils::test::setup_with_new_libra_in(repository.path()).await;
    fs::write(repository.path().join("a.txt"), "one\n").expect("file");

    fn run(repository: &std::path::Path, args: &[&str]) {
        let output = Command::new(env!("CARGO_BIN_EXE_libra"))
            .env("LIBRA_SKIP_WEB_BUILD", "1")
            .args(args)
            .current_dir(repository)
            .output()
            .expect("libra command");
        assert!(
            output.status.success(),
            "libra {args:?} failed:\n{}",
            String::from_utf8_lossy(&output.stderr)
        );
        thread::sleep(Duration::from_millis(250));
    }

    run(repository.path(), &["add", "a.txt"]);
    run(repository.path(), &["commit", "-m", "first", "--no-verify"]);
    fs::write(repository.path().join("a.txt"), "two\n").expect("file update");
    run(repository.path(), &["add", "a.txt"]);
    run(
        repository.path(),
        &["commit", "-m", "second", "--no-verify"],
    );

    use sea_orm::{ConnectionTrait, DbBackend, Statement};

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
    // The branch tip the snapshot captured for the target's parent state:
    // undo must move the live branch row back to exactly this commit.
    let pre_view = store.load_view(&target.pre_view_oid).expect("pre view");
    let pre_snapshot = store
        .load_snapshot(pre_view.workspaces.values().next().expect("pre snapshot"))
        .expect("pre snapshot");
    let branch = match &pre_snapshot.head {
        libra::internal::operation::HeadState::Symbolic { reference, .. } => reference
            .strip_prefix("refs/heads/")
            .unwrap_or(reference)
            .to_string(),
        libra::internal::operation::HeadState::Detached { .. } => {
            panic!("expected a symbolic HEAD for the commit test")
        }
    };
    let pre_bytes = store
        .load_object(&pre_view.refs_facet_oid)
        .expect("pre refs facet");
    let pre_value: serde_json::Value = serde_json::from_slice(&pre_bytes).expect("pre refs json");
    let pre_commit = pre_value["references"]
        .as_array()
        .expect("refs array")
        .iter()
        .find(|entry| {
            entry["kind"] == "Branch"
                && entry["name"] == branch.as_str()
                && entry["remote"].is_null()
        })
        .and_then(|entry| entry["commit"].as_str())
        .map(str::to_string)
        .expect("pre-view branch tip commit");

    // The branch tip the target state carried: undo must move the live
    // branch row from the target's commit back to the pre-view commit.
    let post_view = store.load_view(&target.post_view_oid).expect("post view");
    let post_bytes = store
        .load_object(&post_view.refs_facet_oid)
        .expect("post refs facet");
    let post_value: serde_json::Value =
        serde_json::from_slice(&post_bytes).expect("post refs json");
    let post_commit = post_value["references"]
        .as_array()
        .expect("refs array")
        .iter()
        .find(|entry| {
            entry["kind"] == "Branch"
                && entry["name"] == branch.as_str()
                && entry["remote"].is_null()
        })
        .and_then(|entry| entry["commit"].as_str())
        .map(str::to_string)
        .expect("post-view branch tip commit");

    // HEAD is symbolic on the branch; the current branch tip is the target
    // commit (post-view) before undo.
    assert_eq!(
        branch_tip(&database, &branch).await.as_deref(),
        Some(post_commit.as_str()),
        "branch tip must be the target commit before undo"
    );

    run(repository.path(), &["op", "undo", &target.op_id, "--force"]);

    // The live branch row must now point at the commit from the target's
    // pre-view (the parent commit), while HEAD stays symbolic on the branch.
    assert_eq!(
        branch_tip(&database, &branch).await.as_deref(),
        Some(pre_commit.as_str()),
        "undo must restore the symbolic HEAD branch tip to the exact parent commit OID"
    );
    let head_row = database
        .query_one_raw(Statement::from_sql_and_values(
            DbBackend::Sqlite,
            "SELECT name FROM reference WHERE kind = 'Head' AND name = ?",
            [branch.clone().into()],
        ))
        .await
        .expect("head query");
    assert!(
        head_row.is_some(),
        "HEAD must remain symbolic on branch '{branch}' after undo"
    );
}
