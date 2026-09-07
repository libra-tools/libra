//! Intent specification flow tests covering draft, resolve, validate, and repair stages.
//!
//! Pins the v0.7 contract that Intent, IntentEvent, and Task objects all live on the
//! single AI ref `refs/libra/intent` (now stored in SQLite), share a `HistoryManager`,
//! and round-trip through `LocalStorage::put_tracked` / `get_json`. Covers the parent
//! linkage between root and child intents, the `IntentEventKind::Analyzed` lifecycle
//! marker, and confirms Task and Intent coexist on the same branch.
//!
//! **Layer:** L1 — deterministic, no external dependencies. `#[serial]` because
//! `ChangeDirGuard` mutates the process CWD.

use std::sync::Arc;

use git_internal::internal::object::{
    context::SelectionStrategy,
    intent::Intent,
    intent_event::{IntentEvent, IntentEventKind},
    task::Task,
    types::ActorRef,
};
use libra::{
    internal::ai::{
        history::HistoryManager,
        intentspec::{
            ResolveContext, RiskLevel,
            draft::{DraftAcceptance, DraftIntent, DraftRisk, IntentDraft},
            resolve_intentspec,
            types::{ChangeType, Objective, ObjectiveKind},
        },
        mcp::server::LibraMcpServer,
        runtime::phase0::{
            ContextSnapshotItem, ContextSnapshotRequest, write_context_snapshot_if_needed,
            write_intent,
        },
    },
    utils::{storage::local::LocalStorage, storage_ext::StorageExt, test},
};
use serial_test::serial;
use tempfile::tempdir;

/// Integration test: Intent and Task objects share the single AI branch (refs/libra/intent).
///
/// Scenario: in a fresh temp-dir Libra repo, walk through the Intent flow end to end.
/// Asserts:
/// 1. `init` does NOT create the legacy on-disk `refs/libra/intent` file (it is in
///    the SQLite DB) and the legacy `refs/libra/history` file is also absent.
/// 2. Storing a root Intent and a child Intent (via `Intent::new_revision_from`)
///    succeeds and the child references the root via `parents()`.
/// 3. `IntentEvent::new` records lifecycle events (`Analyzed`) and round-trips
///    through `get_json`.
/// 4. A Task created via the same `HistoryManager` lands on the same branch — the
///    list filter sees 2 intents, 1 intent event, and 1 task.
/// 5. The AI branch HEAD resolves to a single hash after all writes.
///
/// `#[serial]` because `ChangeDirGuard` mutates process CWD.
#[tokio::test]
#[serial(cwd)]
async fn test_intent_flow() {
    // 1. Setup Storage and Repo Environment
    let dir = tempdir().unwrap();
    let _guard = test::ChangeDirGuard::new(dir.path());

    test::setup_with_new_libra_in(dir.path()).await;

    let libra_dir = dir.path().join(".libra");
    let objects_dir = libra_dir.join("objects");

    let storage = Arc::new(LocalStorage::new(objects_dir));

    // Single AI History Manager — all AI objects go here
    let db_path = libra_dir.join("libra.db");
    let db_conn = Arc::new(
        libra::internal::db::establish_connection(db_path.to_str().unwrap())
            .await
            .unwrap(),
    );
    let ai_history = HistoryManager::new(storage.clone(), libra_dir.clone(), db_conn);

    // 2. Verify init does NOT create the AI ref
    let ai_ref_path = libra_dir.join("refs/libra/intent");
    assert!(
        !ai_ref_path.exists(),
        "AI ref file should NOT be created during init at {:?}, it should be in DB",
        ai_ref_path
    );
    assert!(
        ai_history.resolve_history_head().await.unwrap().is_none(),
        "AI ref should be unborn (no commit) initially"
    );

    // The old history ref should NOT exist
    let old_history_ref = libra_dir.join("refs/libra/history");
    assert!(
        !old_history_ref.exists(),
        "Legacy refs/libra/history should not exist"
    );

    // 3. Create Root Intent
    let actor = ActorRef::human("jackie").unwrap();
    let root_intent = Intent::new(actor.clone(), "Initial high-level goal: Refactor system")
        .expect("root intent");

    let root_hash = storage
        .put_tracked(&root_intent, &ai_history)
        .await
        .unwrap();
    println!("Stored Root Intent: {}", root_hash);

    // 4. Create Child Intent (revision linked to root)
    let child_intent = Intent::new_revision_from(
        actor.clone(),
        "Sub-goal: Move Intent struct to libra",
        &root_intent,
    )
    .expect("child intent");

    let child_hash = storage
        .put_tracked(&child_intent, &ai_history)
        .await
        .unwrap();
    println!("Stored Child Intent: {}", child_hash);

    // 4.1 Record lifecycle via IntentEvent (0.7 model)
    let mut child_event = IntentEvent::new(
        actor.clone(),
        child_intent.header().object_id(),
        IntentEventKind::Analyzed,
    )
    .expect("intent event");
    child_event.set_reason(Some("analysis completed".to_string()));
    let child_event_hash = storage
        .put_tracked(&child_event, &ai_history)
        .await
        .unwrap();
    println!("Stored Child IntentEvent: {}", child_event_hash);

    // 5. Verify Retrieval
    let loaded_child: Intent = storage.get_json(&child_hash).await.unwrap();
    assert_eq!(
        loaded_child.header().object_id(),
        child_intent.header().object_id()
    );
    assert_eq!(loaded_child.parents(), &[root_intent.header().object_id()]);
    assert_eq!(loaded_child.prompt(), child_intent.prompt());
    assert_eq!(loaded_child.header().created_by().id(), "jackie");
    let loaded_event: IntentEvent = storage.get_json(&child_event_hash).await.unwrap();
    assert_eq!(loaded_event.intent_id(), child_intent.header().object_id());
    assert_eq!(loaded_event.kind(), &IntentEventKind::Analyzed);

    // 6. Create a Task object on the SAME branch
    let task = Task::new(ActorRef::human("me").unwrap(), "Main Task", None).unwrap();

    storage.put_tracked(&task, &ai_history).await.unwrap();

    // 7. Verify both Intent and Task coexist on the single AI branch
    let intents = ai_history.list_objects("intent").await.unwrap();
    assert_eq!(intents.len(), 2, "Should have 2 intents on AI branch");
    let intent_events = ai_history.list_objects("intent_event").await.unwrap();
    assert_eq!(
        intent_events.len(),
        1,
        "Should have 1 intent event on AI branch"
    );

    let tasks = ai_history.list_objects("task").await.unwrap();
    assert_eq!(tasks.len(), 1, "Should have 1 task on same AI branch");

    // 8. Verify HEAD is a single ref
    let head_hash = ai_history.resolve_history_head().await.unwrap().unwrap();
    println!("AI Branch HEAD: {}", head_hash);
}

/// Phase 0 formal helpers must preserve the intent-flow invariant that AI
/// snapshots live on the single history branch, while skipping ContextSnapshot
/// writes when there is no baseline content to freeze.
#[tokio::test]
#[serial(cwd)]
async fn phase0_runtime_helpers_persist_intent_and_context_snapshot_conditionally() {
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
    let spec = sample_phase0_spec();
    let actor = ActorRef::system("phase0-intent-flow").unwrap();

    let intent = write_intent(&spec, &mcp_server)
        .await
        .expect("Phase 0 Intent write should persist through MCP/history");
    assert_eq!(intent.source, spec);
    assert_eq!(ai_history.list_objects("intent").await.unwrap().len(), 1);

    let skipped = write_context_snapshot_if_needed(
        ContextSnapshotRequest {
            items: Vec::new(),
            selection_strategy: SelectionStrategy::Explicit,
            summary: None,
            actor: actor.clone(),
        },
        &mcp_server,
    )
    .await
    .expect("empty ContextSnapshot request should be a clean skip");
    assert!(skipped.is_none());
    assert_eq!(ai_history.list_objects("snapshot").await.unwrap().len(), 0);

    let snapshot = write_context_snapshot_if_needed(
        ContextSnapshotRequest {
            items: vec![ContextSnapshotItem {
                kind: Some("file".to_string()),
                path: "src/main.rs".to_string(),
                preview: Some("fn main() {}".to_string()),
                blob_hash: None,
            }],
            selection_strategy: SelectionStrategy::Explicit,
            summary: Some("Phase 0 changed worktree baseline".to_string()),
            actor,
        },
        &mcp_server,
    )
    .await
    .expect("dirty ContextSnapshot request should persist")
    .expect("non-empty ContextSnapshot request should return an id");

    assert_eq!(snapshot.item_count, 1);
    assert_eq!(
        snapshot.summary.as_deref(),
        Some("Phase 0 changed worktree baseline")
    );
    assert_eq!(ai_history.list_objects("snapshot").await.unwrap().len(), 1);
}

fn sample_phase0_spec() -> libra::internal::ai::intentspec::IntentSpec {
    resolve_intentspec(
        IntentDraft {
            intent: DraftIntent {
                summary: "Capture Phase 0 context".to_string(),
                problem_statement: "Need a frozen baseline for replay".to_string(),
                change_type: ChangeType::Feature,
                objectives: vec![Objective {
                    title: "Record changed-path context".to_string(),
                    kind: ObjectiveKind::Analysis,
                }],
                in_scope: vec!["src".to_string()],
                out_of_scope: vec![],
                touch_hints: None,
            },
            acceptance: DraftAcceptance {
                success_criteria: vec!["ContextSnapshot write is conditional".to_string()],
                fast_checks: vec![],
                integration_checks: vec![],
                security_checks: vec![],
                release_checks: vec![],
            },
            risk: DraftRisk {
                rationale: "test-only flow".to_string(),
                factors: vec![],
                level: Some(RiskLevel::Low),
            },
        },
        RiskLevel::Low,
        ResolveContext {
            working_dir: ".".to_string(),
            base_ref: "HEAD".to_string(),
            created_by_id: "intent-flow-test".to_string(),
        },
    )
}
