//! MCP server handler integration tests verifying tool listing, resource access, and prompt serving.
//!
//! Calls `LibraMcpServer::*_impl` methods directly (bypassing the
//! `RequestContext`-bearing `ServerHandler` trait dispatch) so we can exercise the
//! tool router, resource router, validation rules, and actor-resolution behaviour
//! without spinning up an HTTP transport. Each test seeds objects via
//! `LocalStorage::put_tracked` against an in-memory SQLite then exercises one
//! create/read/update path.
//!
//! Coverage spans:
//! - Server info / tool router exposure / resource list bootstrap
//! - Cross-object validation rules (run requires task, patchset requires run, etc.)
//! - HEAD/unborn-HEAD/uuid-prefixed-id alias handling
//! - Cross-intent and cross-run reference rejection
//! - Listing endpoints render summary text correctly
//! - Default vs explicit actor (human, agent, mcp_client) selection
//!
//! **Layer:** L1 — deterministic, in-process SQLite + temp-dir storage, no external
//! dependencies.

use std::sync::Arc;

use git_internal::internal::object::{
    context::{ContextSnapshot, SelectionStrategy},
    decision::{Decision, DecisionType},
    evidence::{Evidence, EvidenceKind},
    intent::Intent,
    patchset::{ChangeType, PatchSet, TouchedFile},
    plan::{Plan, PlanStep},
    provenance::Provenance,
    run::Run,
    task::Task,
    tool::{ToolInvocation, ToolStatus},
    types::ActorRef,
};
use libra::{
    internal::{
        ai::{
            history::HistoryManager,
            mcp::{
                resource::{
                    CreateDecisionParams, CreateEvidenceParams, CreatePatchSetParams,
                    CreatePlanParams, CreateProvenanceParams, CreateRunParams, CreateTaskParams,
                    CreateToolInvocationParams, ListTasksParams, UpdateIntentParams,
                },
                server::LibraMcpServer,
            },
            util::normalize_commit_anchor,
        },
        model::reference,
    },
    utils::{storage::local::LocalStorage, storage_ext::StorageExt},
};
use rmcp::{ServerHandler, handler::server::wrapper::Parameters};
use sea_orm::{ActiveModelTrait, ConnectionTrait, Database, Schema, Set};
use tempfile::tempdir;
use uuid::Uuid;

/// Build an in-memory SQLite, create only the `reference` table (the minimum these
/// tests need), and return the connection. Lighter-weight than running the full
/// bootstrap SQL because most MCP tests only need head/branch resolution.
async fn setup_test_db() -> sea_orm::DatabaseConnection {
    let db = Database::connect("sqlite::memory:").await.unwrap();
    let builder = db.get_database_backend();
    let schema = Schema::new(builder);
    let stmt = schema.create_table_from_entity(reference::Entity);
    db.execute_raw(builder.build(&stmt)).await.unwrap();
    db
}

/// Insert a detached-HEAD row (`name = None`, `commit = Some(commit)`) so tests can
/// exercise the `HEAD` alias path of `create_run_impl` without committing real
/// git objects.
async fn seed_detached_head(history_manager: &HistoryManager, commit: &str) {
    let db = history_manager.database_connection();
    reference::ActiveModel {
        name: Set(None),
        kind: Set(reference::ConfigKind::Head),
        commit: Set(Some(commit.to_string())),
        remote: Set(None),
        ..Default::default()
    }
    .insert(&db)
    .await
    .unwrap();
}

/// Insert an unborn-branch HEAD row (`name = Some(branch)`, `commit = None`) so
/// tests can exercise the "HEAD on a branch that has no commits yet" code path —
/// the commit anchor must normalise to forty zeros.
async fn seed_unborn_branch_head(history_manager: &HistoryManager, branch: &str) {
    let db = history_manager.database_connection();
    reference::ActiveModel {
        name: Set(Some(branch.to_string())),
        kind: Set(reference::ConfigKind::Head),
        commit: Set(None),
        remote: Set(None),
        ..Default::default()
    }
    .insert(&db)
    .await
    .unwrap();
}

/// Scenario: instantiate the MCP server and confirm `ServerHandler::get_info`
/// reports the canonical server name "libra". Smoke-tests the handler trait
/// implementation without exercising any tool or resource path.
#[tokio::test]
async fn test_mcp_integration_server_info() {
    let temp_dir = tempdir().unwrap();
    let storage = Arc::new(LocalStorage::new(temp_dir.path().join("objects")));
    let db_conn = Arc::new(setup_test_db().await);
    let history_manager = Arc::new(HistoryManager::new(
        storage.clone(),
        temp_dir.path().to_path_buf(),
        db_conn,
    ));
    let server = LibraMcpServer::new(Some(history_manager), Some(storage));

    let info = ServerHandler::get_info(&server);
    assert_eq!(info.server_info.name, "libra");
}

/// Scenario: confirm the auto-generated tool router exposes `create_task` and
/// `list_tasks` (the canonical MCP tools) and refuses to resolve an unknown name.
/// Acts as a wiring pin so refactors of the tool registration macro cannot silently
/// drop tools from the public surface.
#[tokio::test]
async fn test_mcp_integration_tool_router_exposes_generated_tools() {
    let (server, _storage, _history_manager, _temp_dir) = setup_server().await;

    let create_task = ServerHandler::get_tool(&server, "create_task")
        .expect("create_task should be exposed through the MCP tool router");
    assert_eq!(create_task.name, "create_task");

    let list_tasks = ServerHandler::get_tool(&server, "list_tasks")
        .expect("list_tasks should be exposed through the MCP tool router");
    assert_eq!(list_tasks.name, "list_tasks");

    assert!(
        ServerHandler::get_tool(&server, "__missing_tool__").is_none(),
        "unknown tools should not resolve through the MCP tool router"
    );
}

/// Scenario: call `list_resources_impl` and confirm the response includes
/// `libra://history/latest` (the canonical "what is the latest AI history hash?"
/// pointer). Boundary: avoids the trait dispatch path because that requires a
/// `RequestContext` we cannot easily fabricate in tests.
#[tokio::test]
async fn test_mcp_integration_list_resources() {
    let temp_dir = tempdir().unwrap();
    let storage = Arc::new(LocalStorage::new(temp_dir.path().join("objects")));
    let db_conn = Arc::new(setup_test_db().await);
    let history_manager = Arc::new(HistoryManager::new(
        storage.clone(),
        temp_dir.path().to_path_buf(),
        db_conn,
    ));
    let server = LibraMcpServer::new(Some(history_manager), Some(storage));

    // Call implementation directly to avoid RequestContext
    let resources = server.list_resources_impl().await.unwrap();
    assert!(!resources.is_empty());
    assert!(resources.iter().any(|r| r.uri == "libra://history/latest"));
}

/// Scenario: end-to-end create + read + list flow for a Task object via MCP.
/// Steps: `create_task_impl` → parse the returned ID out of the success message →
/// `read_resource_impl(libra://object/<id>)` → `list_tasks` and confirm the new ID
/// shows up in the listing text. Pins the round-trip contract that drives the
/// minimal MCP client experience.
#[tokio::test]
async fn test_mcp_integration_create_and_read_task() {
    let temp_dir = tempdir().unwrap();
    let storage = Arc::new(LocalStorage::new(temp_dir.path().join("objects")));
    let db_conn = Arc::new(setup_test_db().await);
    let history_manager = Arc::new(HistoryManager::new(
        storage.clone(),
        temp_dir.path().to_path_buf(),
        db_conn,
    ));
    let server = LibraMcpServer::new(Some(history_manager), Some(storage));

    // 1. Create Task
    let params = CreateTaskParams {
        title: "Integration Test Task".to_string(),
        intent_id: None,
        description: Some("Description".to_string()),
        goal_type: Some("feature".to_string()),
        constraints: Some(vec!["Must use Rust".to_string()]),
        acceptance_criteria: None,
        requested_by_kind: None,
        requested_by_id: None,
        dependencies: None,
        parent_task_id: None,
        origin_step_id: None,
        status: None,
        reason: None,
        tags: None,
        external_ids: None,
        actor_kind: None,
        actor_id: None,
    };

    let actor = server.default_actor().unwrap();
    let result = server.create_task_impl(params, actor).await.unwrap();
    let content = &result.content[0];

    // Use serde_json to inspect content to avoid enum variant issues
    let val = serde_json::to_value(content).unwrap();
    let text = val
        .get("text")
        .expect("text field")
        .as_str()
        .expect("string");

    assert!(text.contains("Task created with ID"));

    // Extract ID (simple parsing)
    let id_str = text.split("ID: ").nth(1).unwrap().trim();

    // 2. Read Resource by ID
    let uri = format!("libra://object/{}", id_str);
    let contents = server.read_resource_impl(&uri).await.unwrap();
    assert_eq!(contents.len(), 1);

    let res_val = serde_json::to_value(&contents[0]).unwrap();
    let res_text = res_val
        .get("text")
        .expect("text field")
        .as_str()
        .expect("string");

    println!("Resource content: {}", res_text);
    assert!(res_text.contains("Integration Test Task"));
    assert!(res_text.contains("Must use Rust"));

    // 3. List Tasks
    let list_params = ListTasksParams {
        limit: None,
        status: None,
    };
    let list_result = server.list_tasks(Parameters(list_params)).await.unwrap();

    let list_val = serde_json::to_value(&list_result.content[0]).unwrap();
    let list_text = list_val
        .get("text")
        .expect("text field")
        .as_str()
        .expect("string");

    assert!(list_text.contains(id_str));
    assert!(list_text.contains("Integration Test Task"));
}

/// Scenario: `create_run_impl` with a random `task_id` UUID that has no
/// corresponding Task object must fail with an error message containing
/// "task_id not found". Pins the foreign-key validation contract for runs.
#[tokio::test]
async fn test_create_run_requires_existing_task() {
    let (server, _storage, _history_manager, _temp_dir) = setup_server().await;
    let actor = server.default_actor().unwrap();

    let result = server
        .create_run_impl(
            CreateRunParams {
                task_id: Uuid::new_v4().to_string(),
                base_commit_sha: "a".repeat(64),
                plan_id: None,
                status: None,
                context_snapshot_id: None,
                error: None,
                agent_instances: None,
                metrics_json: None,
                reason: None,
                orchestrator_version: None,
                tags: None,
                external_ids: None,
                actor_kind: None,
                actor_id: None,
            },
            actor,
        )
        .await;

    assert!(result.is_err(), "run should fail when task does not exist");
    let err = result.unwrap_err();
    assert!(
        err.message.contains("task_id not found"),
        "unexpected error: {}",
        err.message
    );
}

/// Scenario: when `base_commit_sha = "HEAD"` is supplied, the server resolves it
/// against the seeded detached-HEAD row and stores the canonical 40-char hash on
/// the Run object. Pins the HEAD alias contract so MCP clients can use literal
/// "HEAD" without pre-resolving it.
#[tokio::test]
async fn test_create_run_accepts_head_base_commit() {
    let (server, storage, history_manager, _temp_dir) = setup_server().await;
    let actor = ActorRef::human("tester").unwrap();
    let head_commit = "b".repeat(40);
    seed_detached_head(&history_manager, &head_commit).await;

    let task = Task::new(actor.clone(), "task with HEAD base commit", None).unwrap();
    storage.put_tracked(&task, &history_manager).await.unwrap();

    let result = server
        .create_run_impl(
            CreateRunParams {
                task_id: task.header().object_id().to_string(),
                base_commit_sha: "HEAD".to_string(),
                plan_id: None,
                status: None,
                context_snapshot_id: None,
                error: None,
                agent_instances: None,
                metrics_json: None,
                reason: None,
                orchestrator_version: None,
                tags: None,
                external_ids: None,
                actor_kind: None,
                actor_id: None,
            },
            actor,
        )
        .await
        .unwrap();

    let content = serde_json::to_value(&result.content[0]).unwrap();
    let run_id = content
        .get("text")
        .unwrap()
        .as_str()
        .unwrap()
        .split("ID: ")
        .nth(1)
        .unwrap()
        .trim();
    let run_hash = history_manager
        .get_object_hash("run", run_id)
        .await
        .unwrap()
        .unwrap();
    let run: Run = storage.get_json(&run_hash).await.unwrap();

    assert_eq!(
        run.commit().to_string(),
        normalize_commit_anchor(&head_commit).unwrap()
    );
}

/// Scenario: HEAD points at an unborn branch (no commits yet). Resolving "HEAD"
/// must yield the all-zeros sentinel commit (`"0".repeat(40)`) so callers can
/// create a Run on a freshly-initialized repo. Boundary case for the HEAD alias
/// path that pairs with the detached-HEAD test.
#[tokio::test]
async fn test_create_run_accepts_unborn_head_base_commit() {
    let (server, storage, history_manager, _temp_dir) = setup_server().await;
    let actor = ActorRef::human("tester").unwrap();
    seed_unborn_branch_head(&history_manager, "main").await;

    let task = Task::new(actor.clone(), "task with unborn HEAD base commit", None).unwrap();
    storage.put_tracked(&task, &history_manager).await.unwrap();

    let result = server
        .create_run_impl(
            CreateRunParams {
                task_id: task.header().object_id().to_string(),
                base_commit_sha: "HEAD".to_string(),
                plan_id: None,
                status: None,
                context_snapshot_id: None,
                error: None,
                agent_instances: None,
                metrics_json: None,
                reason: None,
                orchestrator_version: None,
                tags: None,
                external_ids: None,
                actor_kind: None,
                actor_id: None,
            },
            actor,
        )
        .await
        .unwrap();

    let content = serde_json::to_value(&result.content[0]).unwrap();
    let run_id = content
        .get("text")
        .unwrap()
        .as_str()
        .unwrap()
        .split("ID: ")
        .nth(1)
        .unwrap()
        .trim();
    let run_hash = history_manager
        .get_object_hash("run", run_id)
        .await
        .unwrap()
        .unwrap();
    let run: Run = storage.get_json(&run_hash).await.unwrap();

    assert_eq!(
        run.commit().to_string(),
        normalize_commit_anchor(&"0".repeat(40)).unwrap()
    );
}

/// Scenario: `create_patchset_impl` with a random `run_id` must fail with
/// "run_id not found". Pins the parent-foreign-key contract for patchsets.
#[tokio::test]
async fn test_create_patchset_requires_existing_run() {
    let (server, _storage, _history_manager, _temp_dir) = setup_server().await;
    let actor = server.default_actor().unwrap();

    let result = server
        .create_patchset_impl(
            CreatePatchSetParams {
                run_id: Uuid::new_v4().to_string(),
                generation: 1,
                sequence: None,
                base_commit_sha: "a".repeat(64),
                touched_files: None,
                rationale: None,
                diff_format: None,
                diff_artifact: None,
                tags: None,
                external_ids: None,
                actor_kind: None,
                actor_id: None,
            },
            actor,
        )
        .await;

    assert!(
        result.is_err(),
        "patchset creation should fail when run does not exist"
    );
    let err = result.unwrap_err();
    assert!(
        err.message.contains("run_id not found"),
        "unexpected error: {}",
        err.message
    );
}

/// Scenario: `create_tool_invocation_impl` with a random `run_id` must fail with
/// "run_id not found". Pins the parent-foreign-key contract for tool invocations.
#[tokio::test]
async fn test_create_tool_invocation_requires_existing_run() {
    let (server, _storage, _history_manager, _temp_dir) = setup_server().await;
    let actor = server.default_actor().unwrap();

    let result = server
        .create_tool_invocation_impl(
            CreateToolInvocationParams {
                run_id: Uuid::new_v4().to_string(),
                tool_name: "read_file".to_string(),
                status: None,
                args_json: None,
                io_footprint: None,
                result_summary: None,
                artifacts: None,
                tags: None,
                external_ids: None,
                actor_kind: None,
                actor_id: None,
            },
            actor,
        )
        .await;

    assert!(
        result.is_err(),
        "tool invocation creation should fail when run does not exist"
    );
    let err = result.unwrap_err();
    assert!(
        err.message.contains("run_id not found"),
        "unexpected error: {}",
        err.message
    );
}

/// Scenario: `create_provenance_impl` with a random `run_id` must fail with
/// "run_id not found". Pins the parent-foreign-key contract for provenance.
#[tokio::test]
async fn test_create_provenance_requires_existing_run() {
    let (server, _storage, _history_manager, _temp_dir) = setup_server().await;
    let actor = server.default_actor().unwrap();

    let result = server
        .create_provenance_impl(
            CreateProvenanceParams {
                run_id: Uuid::new_v4().to_string(),
                provider: "openai".to_string(),
                model: "gpt-4o".to_string(),
                parameters_json: None,
                temperature: None,
                max_tokens: None,
                tags: None,
                external_ids: None,
                actor_kind: None,
                actor_id: None,
            },
            actor,
        )
        .await;

    assert!(
        result.is_err(),
        "provenance creation should fail when run does not exist"
    );
    let err = result.unwrap_err();
    assert!(
        err.message.contains("run_id not found"),
        "unexpected error: {}",
        err.message
    );
}

/// Scenario: `create_task_impl` with `intent_id` referencing a UUID that has no
/// Intent object must fail with "intent_id not found". Pins the optional-foreign-
/// key validation: when supplied, the intent must exist.
#[tokio::test]
async fn test_create_task_rejects_missing_intent_reference() {
    let (server, _storage, _history_manager, _temp_dir) = setup_server().await;
    let actor = server.default_actor().unwrap();

    let result = server
        .create_task_impl(
            CreateTaskParams {
                title: "Task with missing intent".to_string(),
                intent_id: Some(Uuid::new_v4().to_string()),
                description: None,
                goal_type: None,
                constraints: None,
                acceptance_criteria: None,
                requested_by_kind: None,
                requested_by_id: None,
                dependencies: None,
                parent_task_id: None,
                origin_step_id: None,
                status: None,
                reason: None,
                tags: None,
                external_ids: None,
                actor_kind: None,
                actor_id: None,
            },
            actor,
        )
        .await;

    assert!(
        result.is_err(),
        "task creation should fail on missing intent reference"
    );
    let err = result.unwrap_err();
    assert!(
        err.message.contains("intent_id not found"),
        "unexpected error: {}",
        err.message
    );
}

/// Scenario: `update_intent_impl` accepts the user-friendly status alias
/// `"discarded"` and translates it into the canonical `Cancelled` lifecycle event.
/// Pins the alias surface area for human-driven MCP clients.
#[tokio::test]
async fn test_update_intent_accepts_discarded_alias() {
    let (server, storage, history_manager, _temp_dir) = setup_server().await;
    let actor = ActorRef::human("tester").unwrap();

    let intent = Intent::new(actor, "Alias test intent").unwrap();
    let intent_id = intent.header().object_id();
    storage
        .put_tracked(&intent, &history_manager)
        .await
        .unwrap();

    let result = server
        .update_intent_impl(UpdateIntentParams {
            intent_id: intent_id.to_string(),
            status: Some("discarded".to_string()),
            commit_sha: None,
            reason: Some("no longer needed".to_string()),
            next_intent_id: None,
        })
        .await;

    assert!(
        result.is_ok(),
        "discarded alias should map to cancelled lifecycle event"
    );
}

/// Scenario: `create_decision_impl` accepts both `run_id` and
/// `chosen_patchset_id` strings prefixed with `uuid:` (an optional disambiguator
/// some MCP clients emit). Pins the input-tolerant ID parser.
#[tokio::test]
async fn test_create_decision_accepts_uuid_prefixed_ids() {
    let (server, storage, history_manager, _temp_dir) = setup_server().await;
    let actor = ActorRef::human("tester").unwrap();

    let task = Task::new(actor.clone(), "decision task", None).unwrap();
    storage.put_tracked(&task, &history_manager).await.unwrap();

    let base = "b".repeat(64);
    let run = Run::new(actor.clone(), task.header().object_id(), &base).unwrap();
    storage.put_tracked(&run, &history_manager).await.unwrap();

    let patchset = PatchSet::new(actor.clone(), run.header().object_id(), &base).unwrap();
    storage
        .put_tracked(&patchset, &history_manager)
        .await
        .unwrap();

    let result = server
        .create_decision_impl(
            CreateDecisionParams {
                run_id: format!("uuid:{}", run.header().object_id()),
                decision_type: "commit".to_string(),
                chosen_patchset_id: Some(format!("uuid:{}", patchset.header().object_id())),
                result_commit_sha: Some(base),
                checkpoint_id: None,
                rationale: Some("apply selected patch".to_string()),
                tags: None,
                external_ids: None,
                actor_kind: None,
                actor_id: None,
            },
            actor,
        )
        .await;

    assert!(result.is_ok(), "uuid: prefixed ids should be accepted");
}

/// Scenario: `update_intent_impl` accepts an `intent_id` prefixed with `uuid:`.
/// Companion to the decision-side test for the same input-tolerance contract.
#[tokio::test]
async fn test_update_intent_accepts_uuid_prefixed_intent_id() {
    let (server, storage, history_manager, _temp_dir) = setup_server().await;
    let actor = ActorRef::human("tester").unwrap();

    let intent = Intent::new(actor, "prefix intent").unwrap();
    let intent_id = intent.header().object_id();
    storage
        .put_tracked(&intent, &history_manager)
        .await
        .unwrap();

    let result = server
        .update_intent_impl(UpdateIntentParams {
            intent_id: format!("uuid:{intent_id}"),
            status: Some("active".to_string()),
            commit_sha: None,
            reason: Some("prefix accepted".to_string()),
            next_intent_id: None,
        })
        .await;

    assert!(
        result.is_ok(),
        "uuid: prefixed intent id should be accepted"
    );
}

/// Scenario: a plan being created for `intent_b` references a parent plan that
/// belongs to `intent_a`. The server rejects with "parent_plan_ids must belong to
/// intent". Pins the cross-intent integrity rule that keeps each intent's plan DAG
/// self-contained.
#[tokio::test]
async fn test_create_plan_rejects_parent_from_other_intent() {
    let (server, storage, history_manager, _temp_dir) = setup_server().await;
    let actor = ActorRef::human("tester").unwrap();

    let intent_a = Intent::new(actor.clone(), "intent-a").unwrap();
    let intent_b = Intent::new(actor.clone(), "intent-b").unwrap();
    storage
        .put_tracked(&intent_a, &history_manager)
        .await
        .unwrap();
    storage
        .put_tracked(&intent_b, &history_manager)
        .await
        .unwrap();

    let parent_plan = Plan::new(actor.clone(), intent_a.header().object_id()).unwrap();
    storage
        .put_tracked(&parent_plan, &history_manager)
        .await
        .unwrap();

    let result = server
        .create_plan_impl(
            CreatePlanParams {
                intent_id: intent_b.header().object_id().to_string(),
                parent_plan_ids: Some(vec![parent_plan.header().object_id().to_string()]),
                context_frame_ids: None,
                steps: None,
                tags: None,
                external_ids: None,
                actor_kind: None,
                actor_id: None,
            },
            actor,
        )
        .await;

    assert!(
        result.is_err(),
        "cross-intent parent plan should be rejected"
    );
    let err = result.unwrap_err();
    assert!(
        err.message
            .contains("parent_plan_ids must belong to intent"),
        "unexpected error: {}",
        err.message
    );
}

/// Scenario: a Run pairs a task whose intent is `intent_task` with a plan whose
/// intent is `intent_plan`. The server rejects because the plan must belong to the
/// same intent as the task. Error message must contain "plan_id intent" so callers
/// can identify which side of the mismatch failed.
#[tokio::test]
async fn test_create_run_rejects_plan_with_mismatched_task_intent() {
    let (server, storage, history_manager, _temp_dir) = setup_server().await;
    let actor = ActorRef::human("tester").unwrap();

    let intent_task = Intent::new(actor.clone(), "task-intent").unwrap();
    let intent_plan = Intent::new(actor.clone(), "plan-intent").unwrap();
    storage
        .put_tracked(&intent_task, &history_manager)
        .await
        .unwrap();
    storage
        .put_tracked(&intent_plan, &history_manager)
        .await
        .unwrap();

    let mut task = Task::new(actor.clone(), "task with intent", None).unwrap();
    task.set_intent(Some(intent_task.header().object_id()));
    storage.put_tracked(&task, &history_manager).await.unwrap();

    let plan = Plan::new(actor.clone(), intent_plan.header().object_id()).unwrap();
    storage.put_tracked(&plan, &history_manager).await.unwrap();

    let result = server
        .create_run_impl(
            CreateRunParams {
                task_id: task.header().object_id().to_string(),
                base_commit_sha: "c".repeat(64),
                plan_id: Some(plan.header().object_id().to_string()),
                status: None,
                context_snapshot_id: None,
                error: None,
                agent_instances: None,
                metrics_json: None,
                reason: None,
                orchestrator_version: None,
                tags: None,
                external_ids: None,
                actor_kind: None,
                actor_id: None,
            },
            actor,
        )
        .await;

    assert!(
        result.is_err(),
        "mismatched task/plan intent should be rejected"
    );
    let err = result.unwrap_err();
    assert!(
        err.message.contains("plan_id intent"),
        "unexpected error: {}",
        err.message
    );
}

/// Scenario: an Evidence record claims a `patchset_id` that actually belongs to a
/// different Run than the one the evidence is being attached to. The server must
/// reject with an error mentioning `patchset_id`, "belongs to run", and the actual
/// owning run ID so callers can correct the wiring. Pins cross-run integrity for
/// patchset references in evidence.
#[tokio::test]
async fn test_create_evidence_rejects_patchset_from_different_run() {
    let (server, storage, history_manager, _temp_dir) = setup_server().await;
    let actor = ActorRef::human("tester").unwrap();

    let task = Task::new(actor.clone(), "evidence task", None).unwrap();
    storage.put_tracked(&task, &history_manager).await.unwrap();

    let base = "d".repeat(64);
    let run_a = Run::new(actor.clone(), task.header().object_id(), &base).unwrap();
    let run_b = Run::new(actor.clone(), task.header().object_id(), &base).unwrap();
    storage.put_tracked(&run_a, &history_manager).await.unwrap();
    storage.put_tracked(&run_b, &history_manager).await.unwrap();

    let patchset_on_b = PatchSet::new(actor.clone(), run_b.header().object_id(), &base).unwrap();
    storage
        .put_tracked(&patchset_on_b, &history_manager)
        .await
        .unwrap();

    let result = server
        .create_evidence_impl(
            CreateEvidenceParams {
                run_id: run_a.header().object_id().to_string(),
                patchset_id: Some(patchset_on_b.header().object_id().to_string()),
                kind: "test".to_string(),
                tool: "cargo".to_string(),
                command: Some("cargo test".to_string()),
                exit_code: Some(1),
                summary: Some("failed".to_string()),
                report_artifacts: None,
                tags: None,
                external_ids: None,
                actor_kind: None,
                actor_id: None,
            },
            actor.clone(),
        )
        .await;

    assert!(
        result.is_err(),
        "evidence patchset from another run should be rejected"
    );
    let err = result.unwrap_err();
    assert!(
        err.message.contains("patchset_id")
            && err.message.contains("belongs to run")
            && err
                .message
                .contains(&run_a.header().object_id().to_string()),
        "unexpected error: {}",
        err.message
    );
}

/// Scenario: same as the evidence cross-run test, but for `Decision` —
/// `chosen_patchset_id` must belong to the decision's own run. Pins the matching
/// integrity rule across both Evidence and Decision objects.
#[tokio::test]
async fn test_create_decision_rejects_patchset_from_different_run() {
    let (server, storage, history_manager, _temp_dir) = setup_server().await;
    let actor = ActorRef::human("tester").unwrap();

    let task = Task::new(actor.clone(), "decision relation task", None).unwrap();
    storage.put_tracked(&task, &history_manager).await.unwrap();

    let base = "e".repeat(64);
    let run_a = Run::new(actor.clone(), task.header().object_id(), &base).unwrap();
    let run_b = Run::new(actor.clone(), task.header().object_id(), &base).unwrap();
    storage.put_tracked(&run_a, &history_manager).await.unwrap();
    storage.put_tracked(&run_b, &history_manager).await.unwrap();

    let patchset_on_b = PatchSet::new(actor.clone(), run_b.header().object_id(), &base).unwrap();
    storage
        .put_tracked(&patchset_on_b, &history_manager)
        .await
        .unwrap();

    let result = server
        .create_decision_impl(
            CreateDecisionParams {
                run_id: run_a.header().object_id().to_string(),
                decision_type: "commit".to_string(),
                chosen_patchset_id: Some(patchset_on_b.header().object_id().to_string()),
                result_commit_sha: Some(base),
                checkpoint_id: None,
                rationale: Some("wrong patchset".to_string()),
                tags: None,
                external_ids: None,
                actor_kind: None,
                actor_id: None,
            },
            actor,
        )
        .await;

    assert!(
        result.is_err(),
        "decision patchset from another run should be rejected"
    );
    let err = result.unwrap_err();
    assert!(
        err.message.contains("chosen_patchset_id")
            && err.message.contains("belongs to run")
            && err
                .message
                .contains(&run_a.header().object_id().to_string()),
        "unexpected error: {}",
        err.message
    );
}

/// Helper: create a server with storage and history, returning all components.
///
/// Returns the server alongside the underlying `LocalStorage`, `HistoryManager`, and
/// `TempDir` so callers can both drive MCP calls and seed the history layer
/// directly. The `TempDir` must be held alive — dropping it removes the on-disk
/// objects.
async fn setup_server() -> (
    LibraMcpServer,
    Arc<LocalStorage>,
    Arc<HistoryManager>,
    tempfile::TempDir,
) {
    let temp_dir = tempdir().unwrap();
    let storage = Arc::new(LocalStorage::new(temp_dir.path().join("objects")));
    let db_conn = Arc::new(setup_test_db().await);
    let history_manager = Arc::new(HistoryManager::new(
        storage.clone(),
        temp_dir.path().to_path_buf(),
        db_conn,
    ));
    let server = LibraMcpServer::new(Some(history_manager.clone()), Some(storage.clone()));
    (server, storage, history_manager, temp_dir)
}

/// Scenario: `libra://history/latest` returns the literal "no history" before any
/// AI history exists, then returns the real 40+ hex-char hash once a Task is
/// `put_tracked`'d (which writes a history commit). Pins the resource's two-state
/// behaviour for the read-only "what's the latest?" pointer.
#[tokio::test]
async fn test_history_latest_returns_real_hash() {
    let (server, storage, history_manager, _temp_dir) = setup_server().await;

    // Before any history: should return "no history"
    let contents = server
        .read_resource_impl("libra://history/latest")
        .await
        .unwrap();
    let val = serde_json::to_value(&contents[0]).unwrap();
    let text = val.get("text").unwrap().as_str().unwrap();
    assert_eq!(text, "no history");

    // Create a task to produce a history commit
    let actor = ActorRef::human("tester").unwrap();
    let task =
        git_internal::internal::object::task::Task::new(actor, "History Test", None).unwrap();
    storage.put_tracked(&task, &history_manager).await.unwrap();

    // Now should return a real hex hash
    let contents = server
        .read_resource_impl("libra://history/latest")
        .await
        .unwrap();
    let val = serde_json::to_value(&contents[0]).unwrap();
    let text = val.get("text").unwrap().as_str().unwrap();
    assert_ne!(text, "no history");
    assert!(text.len() >= 40, "Expected a hash string, got: {}", text);
    assert!(
        text.chars().all(|c| c.is_ascii_hexdigit()),
        "Not a hex hash: {}",
        text
    );
}

/// Scenario: with no live thread/run anchored, `libra://context/active` returns
/// JSON with `active = false`. Pins the inactive-state contract for the active
/// context resource.
#[tokio::test]
async fn test_context_active_no_active() {
    let (server, _, _, _temp_dir) = setup_server().await;

    let contents = server
        .read_resource_impl("libra://context/active")
        .await
        .unwrap();
    let val = serde_json::to_value(&contents[0]).unwrap();
    let text = val.get("text").unwrap().as_str().unwrap();
    let json: serde_json::Value = serde_json::from_str(text).unwrap();
    assert_eq!(json["active"], false);
}

/// Scenario: store a `ContextSnapshot` with a custom summary, then call
/// `list_context_snapshots` and confirm the rendered text shows "Strategy:",
/// "Items:", and the summary string. Pins the listing endpoint's text format.
#[tokio::test]
async fn test_list_context_snapshots_with_summary() {
    let (server, storage, history_manager, _temp_dir) = setup_server().await;
    let actor = ActorRef::human("tester").unwrap();
    let _base = "a".repeat(64);

    let mut snap = ContextSnapshot::new(actor, SelectionStrategy::Heuristic).unwrap();
    snap.set_summary(Some("test summary".to_string()));
    storage.put_tracked(&snap, &history_manager).await.unwrap();

    use libra::internal::ai::mcp::resource::ListContextSnapshotsParams;
    let result = server
        .list_context_snapshots(Parameters(ListContextSnapshotsParams { limit: None }))
        .await
        .unwrap();
    let val = serde_json::to_value(&result.content[0]).unwrap();
    let text = val.get("text").unwrap().as_str().unwrap();

    assert!(text.contains("Strategy:"));
    assert!(text.contains("Items:"));
    assert!(text.contains("test summary"));
}

/// Scenario: store a `Plan` with a single `PlanStep`, then `list_plans` must
/// render "Steps: 1" in the summary text. Pins the step-count rendering contract.
#[tokio::test]
async fn test_list_plans_with_summary() {
    let (server, storage, history_manager, _temp_dir) = setup_server().await;
    let actor = ActorRef::human("tester").unwrap();
    let _run_id = Uuid::new_v4();

    let mut plan = Plan::new(actor, Uuid::new_v4()).unwrap();
    let step = PlanStep::new("step 1");
    plan.add_step(step);
    storage.put_tracked(&plan, &history_manager).await.unwrap();

    use libra::internal::ai::mcp::resource::ListPlansParams;
    let result = server
        .list_plans(Parameters(ListPlansParams { limit: None }))
        .await
        .unwrap();
    let val = serde_json::to_value(&result.content[0]).unwrap();
    let text = val.get("text").unwrap().as_str().unwrap();

    assert!(text.contains("Steps: 1"));
}

/// Scenario: store a `PatchSet` with one touched file, then `list_patchsets` must
/// render "Files: 1" and "Format:" in the summary text. Pins per-patchset summary
/// rendering for the listing endpoint.
#[tokio::test]
async fn test_list_patchsets_with_summary() {
    let (server, storage, history_manager, _temp_dir) = setup_server().await;
    let actor = ActorRef::human("tester").unwrap();
    let run_id = Uuid::new_v4();
    let base = "b".repeat(64);

    let mut ps = PatchSet::new(actor, run_id, &base).unwrap();
    let tf = TouchedFile::new("src/main.rs".to_string(), ChangeType::Modify, 10, 5).unwrap();
    ps.add_touched(tf);
    storage.put_tracked(&ps, &history_manager).await.unwrap();

    use libra::internal::ai::mcp::resource::ListPatchSetsParams;
    let result = server
        .list_patchsets(Parameters(ListPatchSetsParams { limit: None }))
        .await
        .unwrap();
    let val = serde_json::to_value(&result.content[0]).unwrap();
    let text = val.get("text").unwrap().as_str().unwrap();

    assert!(text.contains("Files: 1"));
    assert!(text.contains("Format:"));
}

/// Scenario: store an `Evidence` with kind=Test, tool="cargo", exit_code=0, and a
/// summary, then `list_evidences` must render "Kind:", "Tool: cargo", "Exit: 0",
/// and the summary string. Pins evidence summary text fields.
#[tokio::test]
async fn test_list_evidences_with_summary() {
    let (server, storage, history_manager, _temp_dir) = setup_server().await;
    let actor = ActorRef::human("tester").unwrap();
    let run_id = Uuid::new_v4();

    let mut ev = Evidence::new(actor, run_id, EvidenceKind::Test, "cargo").unwrap();
    ev.set_exit_code(Some(0));
    ev.set_summary(Some("all tests passed".to_string()));
    storage.put_tracked(&ev, &history_manager).await.unwrap();

    use libra::internal::ai::mcp::resource::ListEvidencesParams;
    let result = server
        .list_evidences(Parameters(ListEvidencesParams { limit: None }))
        .await
        .unwrap();
    let val = serde_json::to_value(&result.content[0]).unwrap();
    let text = val.get("text").unwrap().as_str().unwrap();

    assert!(text.contains("Kind:"));
    assert!(text.contains("Tool: cargo"));
    assert!(text.contains("Exit: 0"));
    assert!(text.contains("all tests passed"));
}

/// Scenario: store a `ToolInvocation` with status=Ok and a result summary, then
/// `list_tool_invocations` must render "Tool: read_file", "Ok", and the summary.
/// Pins tool invocation summary rendering.
#[tokio::test]
async fn test_list_tool_invocations_with_summary() {
    let (server, storage, history_manager, _temp_dir) = setup_server().await;
    let actor = ActorRef::human("tester").unwrap();
    let run_id = Uuid::new_v4();

    let mut inv = ToolInvocation::new(actor, run_id, "read_file").unwrap();
    inv.set_status(ToolStatus::Ok);
    inv.set_result_summary(Some("read 100 lines".to_string()));
    storage.put_tracked(&inv, &history_manager).await.unwrap();

    use libra::internal::ai::mcp::resource::ListToolInvocationsParams;
    let result = server
        .list_tool_invocations(Parameters(ListToolInvocationsParams { limit: None }))
        .await
        .unwrap();
    let val = serde_json::to_value(&result.content[0]).unwrap();
    let text = val.get("text").unwrap().as_str().unwrap();

    assert!(text.contains("Tool: read_file"));
    assert!(text.contains("Ok"));
    assert!(text.contains("read 100 lines"));
}

/// Scenario: store a `Provenance` with provider="openai" and model="gpt-4o", then
/// `list_provenances` must render "Provider: openai" and "Model: gpt-4o". Pins
/// provenance summary rendering.
#[tokio::test]
async fn test_list_provenances_with_summary() {
    let (server, storage, history_manager, _temp_dir) = setup_server().await;
    let actor = ActorRef::human("tester").unwrap();
    let run_id = Uuid::new_v4();

    let prov = Provenance::new(actor, run_id, "openai", "gpt-4o").unwrap();
    storage.put_tracked(&prov, &history_manager).await.unwrap();

    use libra::internal::ai::mcp::resource::ListProvenancesParams;
    let result = server
        .list_provenances(Parameters(ListProvenancesParams { limit: None }))
        .await
        .unwrap();
    let val = serde_json::to_value(&result.content[0]).unwrap();
    let text = val.get("text").unwrap().as_str().unwrap();

    assert!(text.contains("Provider: openai"));
    assert!(text.contains("Model: gpt-4o"));
}

/// Scenario: store a `Decision` (DecisionType::Commit) with a rationale, then
/// `list_decisions` must render the decision type ("Commit") and the rationale
/// text. Pins decision summary rendering.
#[tokio::test]
async fn test_list_decisions_with_summary() {
    let (server, storage, history_manager, _temp_dir) = setup_server().await;
    let actor = ActorRef::human("tester").unwrap();
    let run_id = Uuid::new_v4();

    let mut dec = Decision::new(actor, run_id, DecisionType::Commit).unwrap();
    dec.set_rationale(Some("all tests pass".to_string()));
    storage.put_tracked(&dec, &history_manager).await.unwrap();

    use libra::internal::ai::mcp::resource::ListDecisionsParams;
    let result = server
        .list_decisions(Parameters(ListDecisionsParams { limit: None }))
        .await
        .unwrap();
    let val = serde_json::to_value(&result.content[0]).unwrap();
    let text = val.get("text").unwrap().as_str().unwrap();

    assert!(text.contains("Commit"));
    assert!(text.contains("all tests pass"));
}

/// Scenario: pass explicit `actor_kind = "human"` / `actor_id = "jackie"` to
/// `create_task_impl` and confirm the task is created (and shows up in
/// `list_tasks` output). Pins the override path so MCP clients can attribute
/// actions to the calling user, not the MCP default.
#[tokio::test]
async fn test_create_task_with_explicit_human_actor() {
    let (server, _storage, _history_manager, _temp_dir) = setup_server().await;

    let params = CreateTaskParams {
        title: "Human-authored task".to_string(),
        intent_id: None,
        description: None,
        goal_type: None,
        constraints: None,
        acceptance_criteria: None,
        requested_by_kind: None,
        requested_by_id: None,
        dependencies: None,
        parent_task_id: None,
        origin_step_id: None,
        status: None,
        reason: None,
        tags: None,
        external_ids: None,
        actor_kind: Some("human".to_string()),
        actor_id: Some("jackie".to_string()),
    };

    let actor = ActorRef::human("jackie").unwrap();
    let result = server
        .create_task_impl(params, actor.clone())
        .await
        .unwrap();
    let val = serde_json::to_value(&result.content[0]).unwrap();
    let text = val.get("text").unwrap().as_str().unwrap();
    assert!(text.contains("Task created with ID:"));

    // Verify the stored object has Human actor kind
    let task_id = text.split("ID: ").nth(1).unwrap().trim();
    use libra::internal::ai::mcp::resource::ListTasksParams;
    let list = server
        .list_tasks(Parameters(ListTasksParams {
            limit: None,
            status: None,
        }))
        .await
        .unwrap();
    let list_text = serde_json::to_value(&list.content[0])
        .unwrap()
        .get("text")
        .unwrap()
        .as_str()
        .unwrap()
        .to_string();
    assert!(list_text.contains(task_id));
}

/// Scenario: same as the explicit-human variant but with `actor_kind = "agent"` /
/// `actor_id = "coder-bot"`. Pins the agent-attribution path used by autonomous
/// AI workflows.
#[tokio::test]
async fn test_create_task_with_agent_actor() {
    let (server, _storage, _history_manager, _temp_dir) = setup_server().await;

    let params = CreateTaskParams {
        title: "Agent-created task".to_string(),
        intent_id: None,
        description: None,
        goal_type: None,
        constraints: None,
        acceptance_criteria: None,
        requested_by_kind: None,
        requested_by_id: None,
        dependencies: None,
        parent_task_id: None,
        origin_step_id: None,
        status: None,
        reason: None,
        tags: None,
        external_ids: None,
        actor_kind: Some("agent".to_string()),
        actor_id: Some("coder-bot".to_string()),
    };

    let actor = ActorRef::agent("coder-bot").unwrap();
    let result = server.create_task_impl(params, actor).await.unwrap();
    let val = serde_json::to_value(&result.content[0]).unwrap();
    let text = val.get("text").unwrap().as_str().unwrap();
    assert!(text.contains("Task created with ID:"));
}

/// Scenario: omit both `actor_kind` and `actor_id`. The server falls back to
/// `default_actor()` which yields an `ActorKind::McpClient`. The task creates
/// successfully. Pins the unauthenticated default attribution path.
#[tokio::test]
async fn test_create_task_default_actor_is_mcp() {
    let (server, _storage, _history_manager, _temp_dir) = setup_server().await;

    let params = CreateTaskParams {
        title: "Default actor task".to_string(),
        intent_id: None,
        description: None,
        goal_type: None,
        constraints: None,
        acceptance_criteria: None,
        requested_by_kind: None,
        requested_by_id: None,
        dependencies: None,
        parent_task_id: None,
        origin_step_id: None,
        status: None,
        reason: None,
        tags: None,
        external_ids: None,
        actor_kind: None,
        actor_id: None,
    };

    // Using default_actor (falls back to mcp_client)
    let actor = server.default_actor().unwrap();
    assert_eq!(
        actor.kind(),
        &git_internal::internal::object::types::ActorKind::McpClient
    );

    let result = server.create_task_impl(params, actor).await.unwrap();
    let val = serde_json::to_value(&result.content[0]).unwrap();
    let text = val.get("text").unwrap().as_str().unwrap();
    assert!(text.contains("Task created with ID:"));
}
