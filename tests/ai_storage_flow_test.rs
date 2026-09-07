//! Integration tests for AI object storage (Task, Run, Plan, Artifact) on local and R2 backends.
//!
//! Walks through the full create/store/load cycle for the AI object types — Task,
//! ContextSnapshot, Run, Plan, and an Artifact blob — confirming both local and
//! Cloudflare R2 backends preserve identity (object hash, body fields). The local
//! test additionally asserts that `put_tracked` records into the AI history ref
//! stored in the SQLite DB rather than on disk under `refs/libra/intent`.
//!
//! **Layer:** `test_ai_flow_local` is L1 (in-memory). `test_ai_flow_r2` is L3 —
//! requires `R2_ENDPOINT`. Both tests are `#[serial]` because they mutate the
//! process CWD.

use std::{path::Path, str::FromStr, sync::Arc};

use git_internal::{
    hash::ObjectHash,
    internal::object::{
        context::SelectionStrategy,
        plan::{Plan, PlanStep},
        run::Run,
        task::{GoalType, Task},
        types::{ActorRef, ObjectType},
    },
};
use libra::{
    command::commit::CommitArgs,
    internal::{
        ai::{
            history::HistoryManager,
            intentspec::{
                IntentSpec, ResolveContext, RiskLevel,
                draft::{DraftAcceptance, DraftIntent, DraftRisk, IntentDraft},
                resolve_intentspec,
                types::{ChangeType, Objective, ObjectiveKind},
            },
            mcp::server::LibraMcpServer,
            orchestrator::{
                persistence::ExecutionAuditSession,
                types::{
                    ExecutionPlanSpec, GateStage, PersistedPlanReviewBundle, TaskContract,
                    TaskKind, TaskSpec,
                },
            },
            runtime::{
                contracts::TaskExecutionStatus,
                phase0::{ContextSnapshotRequest, write_context_snapshot_if_needed, write_intent},
                phase1::write_plan_set,
                phase2::{write_attempt_finish_with_session, write_attempt_start_with_session},
            },
        },
        head::Head,
    },
    utils::{
        storage::{Storage, local::LocalStorage, remote::RemoteStorage},
        storage_ext::StorageExt,
        test,
    },
};
use serial_test::serial;
use tempfile::tempdir;
use uuid::Uuid;

/// Integration test for the full AI storage flow using LocalStorage.
///
/// Scenario: in a fresh temp-dir Libra repo, create a Task, a ContextSnapshot, a
/// Run, and a Plan via `put_tracked` (which both writes to storage and updates the
/// AI history). The test asserts:
/// - The legacy on-disk `refs/libra/intent` file is NOT created (the ref now lives
///   in the SQLite DB).
/// - Each object round-trips through `get_json` with identical IDs and bodies.
/// - An artifact blob committed via `put_artifact` is retrievable both as a generic
///   blob and via its key.
/// - Plain blob storage via `put`/`get` continues to work side-by-side.
///
/// `#[serial]` because `ChangeDirGuard` mutates process CWD.
#[tokio::test]
#[serial(cwd)]
async fn test_ai_flow_local() {
    // 1. Setup Storage and Repo Environment
    let dir = tempdir().unwrap();
    // Change directory so try_get_storage_path finds the repo
    let _guard = test::ChangeDirGuard::new(dir.path());

    test::setup_with_new_libra_in(dir.path()).await;

    let libra_dir = dir.path().join(".libra");
    let objects_dir = libra_dir.join("objects");

    let storage = Arc::new(LocalStorage::new(objects_dir));
    let db_conn = Arc::new(libra::internal::db::get_db_conn_instance().await);
    let history_manager = HistoryManager::new(storage.clone(), libra_dir.clone(), db_conn);

    // 2. User creates a Task
    // let _repo_id = Uuid::new_v4();
    let actor = ActorRef::human("jackie").unwrap();
    let mut task = Task::new(actor.clone(), "Refactor Storage", Some(GoalType::Refactor)).unwrap();
    task.add_constraint("Must use StorageExt");

    // Use put_tracked to ensure History Log is updated (Orphan Branch)
    let task_hash = storage.put_tracked(&task, &history_manager).await.unwrap();
    println!("Stored Task: {}", task_hash);

    // Verify History Log Creation
    // The AI ref should exist and point to a commit
    let history_ref_path = libra_dir.join("refs/libra/intent");
    assert!(
        !history_ref_path.exists(),
        "AI history ref file should NOT be created at {:?}, it should be in DB",
        history_ref_path
    );
    assert!(
        history_manager
            .resolve_history_head()
            .await
            .unwrap()
            .is_some(),
        "AI ref should exist in DB"
    );

    libra::command::commit::execute(CommitArgs {
        message: Some("initial commit".to_string()),
        allow_empty: true,
        disable_pre: true,
        no_verify: true,
        ..Default::default()
    })
    .await;

    let head_commit = Head::current_commit().await.unwrap().to_string();
    let base_commit_sha = libra::internal::ai::util::normalize_commit_anchor(&head_commit).unwrap();

    let snapshot = git_internal::internal::object::context::ContextSnapshot::new(
        actor.clone(),
        git_internal::internal::object::context::SelectionStrategy::Heuristic,
    )
    .unwrap();

    let snapshot_hash = storage
        .put_tracked(&snapshot, &history_manager)
        .await
        .unwrap();
    println!("Stored Snapshot: {}", snapshot_hash);

    // 2.6. User creates a Run
    let mut run = Run::new(actor.clone(), task.header().object_id(), &base_commit_sha).unwrap();
    run.set_snapshot(Some(snapshot.header().object_id()));

    let run_hash = storage.put_tracked(&run, &history_manager).await.unwrap();
    println!("Stored Run: {}", run_hash);

    // 2.7. User creates a Plan
    let plan = Plan::new(actor.clone(), Uuid::new_v4()).unwrap();
    let plan_hash = storage.put_tracked(&plan, &history_manager).await.unwrap();
    println!("Stored Plan: {}", plan_hash);

    // Verify Run Retrieval
    let loaded_run: Run = storage.get_json(&run_hash).await.unwrap();
    assert_eq!(run.header().object_id(), loaded_run.header().object_id());

    // Verify Plan Retrieval
    let loaded_plan: Plan = storage.get_json(&plan_hash).await.unwrap();
    assert_eq!(plan.header().object_id(), loaded_plan.header().object_id());

    // 3. Verify Task Retrieval
    let loaded_task: Task = storage.get_json(&task_hash).await.unwrap();
    assert_eq!(task.title(), loaded_task.title());
    assert_eq!(task.constraints(), loaded_task.constraints());

    // 4. Create an Artifact (simulating a Plan or Patch)
    let patch_content = b"diff --git a/src/main.rs b/src/main.rs\n...";
    let artifact = storage.put_artifact(patch_content).await.unwrap();
    println!("Stored Artifact: {}", artifact.key());

    assert_eq!(artifact.store(), "libra");

    // 5. Verify Artifact Retrieval (via StorageExt or underlying Storage)
    let artifact_hash = ObjectHash::from_str(artifact.key()).unwrap();
    let (data, obj_type) = storage.get(&artifact_hash).await.unwrap();
    assert_eq!(obj_type, ObjectType::Blob);
    assert_eq!(data, patch_content);

    // 6. Verify Normal Blob Storage works alongside
    let blob_content = b"Standard Blob Content";
    let blob_hash = ObjectHash::from_type_and_data(ObjectType::Blob, blob_content);
    storage
        .put(&blob_hash, blob_content, ObjectType::Blob)
        .await
        .unwrap();

    let (loaded_blob, _) = storage.get(&blob_hash).await.unwrap();
    assert_eq!(loaded_blob, blob_content);
}

/// Runtime formal writes should compose without duplicating the core AI object
/// graph: one Intent, two role-split Plans, one root Task plus compiled tasks,
/// and one root Run plus the per-task attempt Run.
#[tokio::test]
#[serial(cwd)]
async fn runtime_formal_writes_preserve_order_and_minimal_object_set() {
    let dir = tempdir().unwrap();
    let _guard = test::ChangeDirGuard::new(dir.path());

    test::setup_with_new_libra_in(dir.path()).await;

    let libra_dir = dir.path().join(".libra");
    let objects_dir = libra_dir.join("objects");
    let storage = Arc::new(LocalStorage::new(objects_dir));
    let db_path = libra_dir.join("libra.db");
    let db_conn = Arc::new(
        libra::internal::db::establish_connection(db_path.to_str().unwrap())
            .await
            .unwrap(),
    );
    let ai_history = Arc::new(HistoryManager::new(storage.clone(), libra_dir, db_conn));
    let mcp_server = Arc::new(LibraMcpServer::new(
        Some(ai_history.clone()),
        Some(storage.clone()),
    ));

    let spec = sample_runtime_storage_spec();
    let intent = write_intent(&spec, &mcp_server)
        .await
        .expect("Phase 0 write should persist the Intent exactly once");
    assert_history_count(&ai_history, "intent", 1).await;
    assert_history_count(&ai_history, "snapshot", 0).await;
    assert_history_count(&ai_history, "plan", 0).await;
    assert_history_count(&ai_history, "task", 0).await;
    assert_history_count(&ai_history, "run", 0).await;

    let skipped_snapshot = write_context_snapshot_if_needed(
        ContextSnapshotRequest {
            items: Vec::new(),
            selection_strategy: SelectionStrategy::Explicit,
            summary: None,
            actor: ActorRef::system("runtime-storage-test").unwrap(),
        },
        &mcp_server,
    )
    .await
    .expect("empty snapshot request should not fail");
    assert!(skipped_snapshot.is_none());
    assert_history_count(&ai_history, "snapshot", 0).await;

    let plan_spec = sample_runtime_plan_spec(&intent.intent_id);
    let implementation_task_id = plan_spec.tasks[0].id();
    let gate_task_id = plan_spec.tasks[1].id();
    let plan_outcome = write_plan_set(&mcp_server, &intent.intent_id, None, None, &plan_spec)
        .await
        .expect("Phase 1 plan-set write should persist the execution/test pair");
    assert_ne!(plan_outcome.execution_plan_id, plan_outcome.test_plan_id);
    assert_eq!(
        plan_outcome
            .plan_id_by_task_id
            .get(&implementation_task_id)
            .map(String::as_str),
        Some(plan_outcome.execution_plan_id.as_str())
    );
    assert_eq!(
        plan_outcome
            .plan_id_by_task_id
            .get(&gate_task_id)
            .map(String::as_str),
        Some(plan_outcome.test_plan_id.as_str())
    );
    assert_history_count(&ai_history, "intent", 1).await;
    assert_history_count(&ai_history, "plan", 2).await;
    assert_history_count(&ai_history, "task", 0).await;
    assert_history_count(&ai_history, "run", 0).await;

    let preview_bundle = PersistedPlanReviewBundle {
        plan_id: plan_outcome.execution_plan_id.clone(),
        test_plan_id: plan_outcome.test_plan_id.clone(),
        step_ids: Default::default(),
        task_ids: Default::default(),
        plan_id_by_task_id: plan_outcome.plan_id_by_task_id.clone(),
    };
    let session = ExecutionAuditSession::start(
        mcp_server.clone(),
        &spec,
        Path::new("."),
        Some(&intent.intent_id),
        Some(preview_bundle),
        None,
    )
    .await
    .expect("Runtime session should reuse the persisted Intent and Plan set");
    assert_history_count(&ai_history, "intent", 1).await;
    assert_history_count(&ai_history, "plan", 2).await;
    assert_history_count(&ai_history, "task", 1).await;
    assert_history_count(&ai_history, "run", 1).await;

    session
        .record_plan_compiled(&plan_spec)
        .await
        .expect("compiled plan should bind the existing execution/test plans");
    assert_history_count(&ai_history, "intent", 1).await;
    assert_history_count(&ai_history, "plan", 2).await;
    assert_history_count(&ai_history, "task", 3).await;
    assert_history_count(&ai_history, "run", 1).await;

    let implementation_task = &plan_spec.tasks[0];
    let start = write_attempt_start_with_session(
        &session,
        implementation_task,
        "runtime-storage-test-model",
        Some("starting implementation attempt".to_string()),
    )
    .await
    .expect("Phase 2 attempt start should create the task run");
    assert_eq!(start.task_id, implementation_task_id);
    assert_eq!(start.status, TaskExecutionStatus::Interrupted);
    assert!(!start.is_terminal());

    let finish = write_attempt_finish_with_session(
        &session,
        implementation_task,
        TaskExecutionStatus::Completed,
        Some("implementation attempt completed".to_string()),
    )
    .await
    .expect("Phase 2 attempt finish should update the same task run");
    assert_eq!(finish.task_id, implementation_task_id);
    assert_eq!(finish.run_id, start.run_id);
    assert_eq!(finish.status, TaskExecutionStatus::Completed);
    assert!(finish.is_terminal());
    assert!(!finish.is_failure());
    assert_history_count(&ai_history, "intent", 1).await;
    assert_history_count(&ai_history, "plan", 2).await;
    assert_history_count(&ai_history, "task", 3).await;
    assert_history_count(&ai_history, "run", 2).await;
}

/// Integration test for AI storage flow using Cloudflare R2 (S3-compatible).
///
/// Scenario: with a live R2 (or any S3-compatible) endpoint configured via env, write
/// a Task and an Artifact through `RemoteStorage`, then read both back to confirm the
/// remote backend round-trips correctly. Acts as the cloud counterpart to
/// `test_ai_flow_local` and is the only smoke test we have for R2 connectivity at
/// this layer.
///
/// Boundary: silently skipped when `R2_ENDPOINT` is unset. `#[serial]` because the
/// test changes nothing CWD-related but stays grouped with the `local` flow for
/// determinism.
///
/// To run this test manually:
/// 1. Set the following environment variables:
///    - R2_ENDPOINT: Your R2 endpoint URL
///    - R2_ACCESS_KEY: Your Access Key ID
///    - R2_SECRET_KEY: Your Secret Access Key
///    - R2_BUCKET: Target bucket name
///    - R2_REGION: Region (usually "auto" for R2)
/// 2. Run: `cargo test --test ai_storage_flow_test -- --ignored`
///
/// This test verifies that:
/// - Objects can be stored and retrieved from R2
/// - Artifacts are correctly stored in R2
/// - Connectivity to the remote storage provider works as expected
#[tokio::test]
#[serial(cloud_live, cwd, env, hash_kind, workspace_failpoints)]
async fn test_ai_flow_r2() {
    if std::env::var("R2_ENDPOINT").map_or(true, |v| v.is_empty()) {
        eprintln!("skipped (R2_ENDPOINT not set)");
        return;
    }

    // 1. Load Config from Env
    let endpoint = std::env::var("R2_ENDPOINT").expect("R2_ENDPOINT not set");
    let access_key = std::env::var("R2_ACCESS_KEY").expect("R2_ACCESS_KEY not set");
    let secret_key = std::env::var("R2_SECRET_KEY").expect("R2_SECRET_KEY not set");
    let bucket = std::env::var("R2_BUCKET").expect("R2_BUCKET not set");
    let region = std::env::var("R2_REGION").unwrap_or_else(|_| "auto".to_string());

    // 2. Setup Remote Storage (Using object_store directly to avoid coupling RemoteStorage to specific backends)
    let s3 = object_store::aws::AmazonS3Builder::new()
        .with_bucket_name(&bucket)
        .with_region(&region)
        .with_endpoint(&endpoint)
        .with_access_key_id(&access_key)
        .with_secret_access_key(&secret_key)
        .with_virtual_hosted_style_request(false)
        .build()
        .expect("Failed to build S3 client");

    let storage = Arc::new(RemoteStorage::new(Arc::new(s3)));

    // 3. User creates a Task
    let _repo_id = Uuid::new_v4();
    let actor = ActorRef::human("jackie-r2").unwrap();
    let task = Task::new(actor, "Test R2 Storage", Some(GoalType::Chore)).unwrap();

    let task_hash = storage.put_json(&task).await.unwrap();
    println!("Stored Task to R2: {}", task_hash);

    // 4. Verify Task Retrieval from R2
    let loaded_task: Task = storage.get_json(&task_hash).await.unwrap();
    assert_eq!(task.title(), loaded_task.title());

    // 5. Create Artifact
    let artifact_content = b"Cloud Content";
    let artifact = storage.put_artifact(artifact_content).await.unwrap();
    println!("Stored Artifact to R2: {}", artifact.key());

    // 6. Verify Artifact Retrieval
    let artifact_hash = ObjectHash::from_str(artifact.key()).unwrap();
    let (data, _) = storage.get(&artifact_hash).await.unwrap();
    assert_eq!(data, artifact_content);
}

async fn assert_history_count(history: &HistoryManager, object_type: &str, expected: usize) {
    let actual = history.list_objects(object_type).await.unwrap().len();
    assert_eq!(
        actual, expected,
        "expected {expected} {object_type} objects in AI history, got {actual}"
    );
}

fn sample_runtime_storage_spec() -> IntentSpec {
    resolve_intentspec(
        IntentDraft {
            intent: DraftIntent {
                summary: "Exercise Runtime storage writes".to_string(),
                problem_statement: "Runtime formal writes must preserve object identity"
                    .to_string(),
                change_type: ChangeType::Feature,
                objectives: vec![Objective {
                    title: "Persist formal writes without duplicates".to_string(),
                    kind: ObjectiveKind::Implementation,
                }],
                in_scope: vec!["src/internal/ai/runtime".to_string()],
                out_of_scope: vec![],
                touch_hints: None,
            },
            acceptance: DraftAcceptance {
                success_criteria: vec!["Object counts remain minimal across phases".to_string()],
                fast_checks: vec![],
                integration_checks: vec![],
                security_checks: vec![],
                release_checks: vec![],
            },
            risk: DraftRisk {
                rationale: "storage-flow regression test".to_string(),
                factors: vec![],
                level: Some(RiskLevel::Low),
            },
        },
        RiskLevel::Low,
        ResolveContext {
            working_dir: ".".to_string(),
            base_ref: "HEAD".to_string(),
            created_by_id: "ai-storage-flow-test".to_string(),
        },
    )
}

fn sample_runtime_plan_spec(intent_id: &str) -> ExecutionPlanSpec {
    let implementation_task = Task::new(
        ActorRef::agent("runtime-storage-test").unwrap(),
        "Edit runtime storage path",
        None,
    )
    .unwrap();
    let implementation_task_id = implementation_task.header().object_id();
    let mut gate_task = Task::new(
        ActorRef::agent("runtime-storage-test").unwrap(),
        "Run runtime storage verification",
        None,
    )
    .unwrap();
    gate_task.add_dependency(implementation_task_id);

    ExecutionPlanSpec {
        intent_spec_id: intent_id.to_string(),
        revision: 1,
        parent_revision: None,
        replan_reason: None,
        tasks: vec![
            TaskSpec {
                step: PlanStep::new("Edit runtime storage path"),
                task: implementation_task,
                objective: "Persist the implementation-side formal writes".to_string(),
                kind: TaskKind::Implementation,
                gate_stage: None,
                owner_role: Some("coder".to_string()),
                scope_in: vec!["src/internal/ai/runtime".to_string()],
                scope_out: vec![],
                checks: vec![],
                contract: TaskContract::default(),
            },
            TaskSpec {
                step: PlanStep::new("Run runtime storage verification"),
                task: gate_task,
                objective: "Verify the storage object graph remains minimal".to_string(),
                kind: TaskKind::Gate,
                gate_stage: Some(GateStage::Fast),
                owner_role: Some("tester".to_string()),
                scope_in: vec!["tests/ai_storage_flow_test.rs".to_string()],
                scope_out: vec![],
                checks: vec![],
                contract: TaskContract::default(),
            },
        ],
        max_parallel: 1,
        checkpoints: vec![],
    }
}
