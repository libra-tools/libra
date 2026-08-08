//! Phase B projection resolver and scheduler repository contract tests.
//!
//! Pin the contracts that `ProjectionResolver`, `SchedulerStateRepository`, and
//! `ProjectionRebuilder` expose to the AI runtime:
//! - The scheduler repository round-trips a `SelectedPlanSet` and rejects stale
//!   compare-and-swap attempts via `SchedulerStateCasError::VersionConflict`.
//! - When a thread row exists but no scheduler row does, the resolver returns a
//!   `StaleReadOnly` bundle so callers know not to write to it.
//! - When neither row exists, the rebuilder can reconstruct a fresh bundle from the
//!   on-disk history (Intent + Task objects).
//!
//! **Layer:** L1 — uses an in-memory SQLite plus a temp-dir Libra repo. The history
//! rebuild test mutates CWD via `ChangeDirGuard` and is therefore `#[serial]`.

use std::sync::Arc;

use chrono::{DateTime, Utc};
use git_internal::internal::object::{intent::Intent, task::Task, types::ActorRef};
use libra::{
    internal::{
        ai::{
            history::HistoryManager,
            projection::{
                LiveContextFrameRef, LiveContextPinKind, LiveContextSourceKind, PlanHeadRef,
                ProjectionRebuilder, ProjectionResolver, ResumeAction, ResumeBundle, ResumeReason,
                SchedulerState, SchedulerStateCasError, SchedulerStateRepository, ThreadBundle,
                ThreadIntentLinkReason, ThreadIntentRef, ThreadParticipant, ThreadParticipantRole,
                ThreadProjection,
            },
            runtime::contracts::{ProjectionFreshness, WorkflowPhase},
        },
        db,
    },
    utils::{storage::local::LocalStorage, storage_ext::StorageExt, test},
};
use sea_orm::{ConnectionTrait, Database, DatabaseConnection, Statement};
use serde_json::json;
use serial_test::serial;
use tempfile::tempdir;
use uuid::Uuid;

const BOOTSTRAP_SQL: &str = include_str!("../sql/sqlite_20260309_init.sql");

/// Spin up an in-memory SQLite, run the canonical bootstrap SQL, and return the
/// connection. Used by tests that only need the schema (not a full Libra repo on
/// disk).
async fn setup_db() -> DatabaseConnection {
    let db = Database::connect("sqlite::memory:").await.unwrap();
    db.execute_raw(Statement::from_string(
        db.get_database_backend(),
        BOOTSTRAP_SQL,
    ))
    .await
    .unwrap();
    db
}

/// Provision a temp-dir Libra repository, switch CWD into it, and wire up the
/// `LocalStorage` + `HistoryManager` + DB connection that the rebuild test exercises.
///
/// Must be called from inside a `#[serial]` test because `ChangeDirGuard` mutates the
/// process-wide CWD. The returned `TempDir` must be held alive for the duration of
/// the test — dropping it removes the on-disk repo.
async fn setup_projection_history() -> (
    tempfile::TempDir,
    Arc<LocalStorage>,
    HistoryManager,
    Arc<DatabaseConnection>,
) {
    let dir = tempdir().unwrap();
    let _guard = test::ChangeDirGuard::new(dir.path());
    test::setup_with_new_libra_in(dir.path()).await;

    let libra_dir = dir.path().join(".libra");
    let storage = Arc::new(LocalStorage::new(libra_dir.join("objects")));
    let db_conn = Arc::new(
        db::establish_connection(libra_dir.join("libra.db").to_str().unwrap())
            .await
            .unwrap(),
    );
    let history = HistoryManager::new(storage.clone(), libra_dir, db_conn.clone());
    (dir, storage, history, db_conn)
}

/// Build a deterministic UTC timestamp from a Unix-seconds value for fixtures.
fn ts(seconds: i64) -> DateTime<Utc> {
    DateTime::<Utc>::from_timestamp(seconds, 0).unwrap()
}

/// Parse a hard-coded UUID literal used in fixtures. Panics on malformed input — the
/// literals are author-owned so this is a programming-error fast path.
fn id(value: &str) -> Uuid {
    Uuid::parse_str(value).unwrap()
}

/// Construct a minimal `ThreadProjection` fixture rooted at `thread_id` with one
/// intent and a single owner participant. Used as the precondition row when testing
/// the scheduler/resolver: the thread must exist before either component reads.
fn sample_thread(thread_id: Uuid) -> ThreadProjection {
    let owner = ActorRef::human("projection-test").unwrap();
    let intent_id = id("22222222-2222-4222-8222-222222222222");
    ThreadProjection {
        thread_id,
        title: Some("Projection resolver test".to_string()),
        owner: owner.clone(),
        participants: vec![ThreadParticipant {
            actor: owner,
            role: ThreadParticipantRole::Owner,
            joined_at: ts(1_700_000_000),
        }],
        current_intent_id: Some(intent_id),
        latest_intent_id: Some(intent_id),
        intents: vec![ThreadIntentRef {
            intent_id,
            ordinal: 0,
            is_head: true,
            linked_at: ts(1_700_000_001),
            link_reason: ThreadIntentLinkReason::Seed,
        }],
        metadata: Some(json!({ "source": "test" })),
        archived: false,
        created_at: ts(1_700_000_000),
        updated_at: ts(1_700_000_002),
        version: 1,
    }
}

/// Construct a minimal `SchedulerState` fixture: two selected plans (an execution
/// plan and a test plan) with the execution plan as the current head, plus one
/// pinned live-context frame. Mirrors the shape the runtime persists at v1.
fn sample_scheduler(thread_id: Uuid) -> SchedulerState {
    let execution_plan_id = id("33333333-3333-4333-8333-333333333333");
    let test_plan_id = id("44444444-4444-4444-8444-444444444444");
    SchedulerState {
        thread_id,
        selected_plan_id: Some(execution_plan_id),
        selected_plan_ids: vec![
            PlanHeadRef {
                plan_id: execution_plan_id,
                ordinal: 0,
            },
            PlanHeadRef {
                plan_id: test_plan_id,
                ordinal: 1,
            },
        ],
        current_plan_heads: vec![PlanHeadRef {
            plan_id: execution_plan_id,
            ordinal: 0,
        }],
        active_task_id: None,
        active_run_id: None,
        live_context_window: vec![LiveContextFrameRef {
            context_frame_id: id("55555555-5555-4555-8555-555555555555"),
            position: 0,
            source_kind: LiveContextSourceKind::Planning,
            pin_kind: Some(LiveContextPinKind::Seed),
            inserted_at: ts(1_700_000_003),
        }],
        metadata: Some(json!({ "ready_queue": [] })),
        updated_at: ts(1_700_000_004),
        version: 1,
    }
}

/// Scenario: insert a `SchedulerState` at version 1, load it back, then perform two
/// compare-and-swap attempts — one valid (1 → 2) and one stale (1 → next, after the
/// row is already at 2). Asserts the load preserves the selected-plan set ordering
/// and the live-context source kind, and that the stale CAS surfaces a
/// `VersionConflict { expected: 1, actual: Some(2) }` so callers can retry the read.
#[tokio::test]
async fn scheduler_repository_loads_selected_plan_set_and_enforces_cas() {
    let db = setup_db().await;
    let thread_id = id("11111111-1111-4111-8111-111111111111");
    sample_thread(thread_id).create(&db).await.unwrap();

    let repo = SchedulerStateRepository::new(db.clone());
    repo.insert_initial(&sample_scheduler(thread_id))
        .await
        .unwrap();

    let loaded = repo
        .load(thread_id)
        .await
        .unwrap()
        .expect("scheduler state");
    assert_eq!(loaded.version, 1);
    assert_eq!(loaded.selected_plan_ids.len(), 2);
    assert_eq!(
        loaded.live_context_window[0].source_kind,
        LiveContextSourceKind::Planning
    );

    let mut next = loaded.clone();
    next.version = 2;
    next.active_task_id = Some(id("66666666-6666-4666-8666-666666666666"));
    repo.compare_and_swap(1, &next).await.unwrap();

    let after = repo
        .load(thread_id)
        .await
        .unwrap()
        .expect("updated scheduler state");
    assert_eq!(after.version, 2);
    assert_eq!(after.active_task_id, next.active_task_id);

    let stale = repo
        .compare_and_swap(1, &next)
        .await
        .expect_err("stale CAS must fail");
    assert!(matches!(
        stale,
        SchedulerStateCasError::VersionConflict {
            expected: 1,
            actual: Some(2),
            ..
        }
    ));
}

/// Scenario: when both thread and scheduler projection rows exist,
/// `ProjectionResolver::load_thread_bundle` must return a Fresh bundle that
/// preserves the thread identity, selected plan ordering, and live context
/// window exactly as stored. This is the formal read contract consumed by
/// resume, diagnostics, and Code UI projection surfaces.
#[tokio::test]
async fn projection_resolver_loads_fresh_thread_scheduler_and_live_context_window() {
    let db = setup_db().await;
    let thread_id = id("12121212-1212-4121-8121-121212121212");
    let thread = sample_thread(thread_id);
    thread.create(&db).await.unwrap();
    let scheduler = sample_scheduler(thread_id);

    let repo = SchedulerStateRepository::new(db.clone());
    repo.insert_initial(&scheduler).await.unwrap();

    let resolver = ProjectionResolver::new(db);
    let bundle = resolver
        .load_thread_bundle(thread_id)
        .await
        .unwrap()
        .expect("thread bundle");

    assert_eq!(bundle.freshness, ProjectionFreshness::Fresh);
    assert_eq!(bundle.thread.thread_id, thread.thread_id);
    assert_eq!(bundle.thread.current_intent_id, thread.current_intent_id);
    assert_eq!(
        bundle.scheduler.selected_plan_ids,
        scheduler.selected_plan_ids
    );
    assert_eq!(
        bundle.scheduler.live_context_window,
        scheduler.live_context_window
    );
    assert_eq!(
        bundle.scheduler.live_context_window[0].source_kind,
        LiveContextSourceKind::Planning
    );
}

/// Scenario: when a thread projection row exists but its scheduler row is missing,
/// `ProjectionResolver::load_thread_bundle` must return a `StaleReadOnly` bundle with
/// an empty selected-plan set rather than an error. This is the contract the runtime
/// relies on to know when callers should not perform writes.
#[tokio::test]
async fn projection_resolver_returns_stale_read_only_when_scheduler_row_is_missing() {
    let db = setup_db().await;
    let thread_id = id("77777777-7777-4777-8777-777777777777");
    sample_thread(thread_id).create(&db).await.unwrap();

    let resolver = ProjectionResolver::new(db);
    let bundle = resolver
        .load_thread_bundle(thread_id)
        .await
        .unwrap()
        .expect("thread bundle");

    assert_eq!(bundle.thread.thread_id, thread_id);
    assert_eq!(bundle.freshness, ProjectionFreshness::StaleReadOnly);
    assert!(bundle.scheduler.selected_plan_ids.is_empty());
}

/// Scenario: the query-index read contract must degrade the same way as the
/// thread bundle. If the thread projection exists but the scheduler row is
/// missing, index reads are allowed for diagnostics/UI display but must be
/// marked `StaleReadOnly` so callers do not advance runtime state from them.
#[tokio::test]
async fn projection_resolver_returns_stale_read_only_query_indexes_when_scheduler_row_is_missing() {
    let db = setup_db().await;
    let thread_id = id("abababab-abab-4aba-8aba-abababababab");
    sample_thread(thread_id).create(&db).await.unwrap();

    let resolver = ProjectionResolver::new(db);
    let indexes = resolver
        .load_query_indexes(thread_id)
        .await
        .unwrap()
        .expect("query indexes");

    assert_eq!(indexes.thread_id, thread_id);
    assert_eq!(indexes.freshness, ProjectionFreshness::StaleReadOnly);
    assert!(indexes.intent_plan_index.is_empty());
    assert!(indexes.intent_task_index.is_empty());
}

/// Scenario: query-index reads must diagnose when the scheduler references
/// plans/tasks/runs that the denormalized indexes cannot resolve. Without this,
/// a resume or diagnostics surface can look "fresh" while silently losing the
/// links needed to rebuild the ready queue.
#[tokio::test]
async fn projection_resolver_query_indexes_diagnose_missing_scheduler_links() {
    let db = setup_db().await;
    let thread_id = id("acacacac-acac-4aca-8aca-acacacacacac");
    let active_task_id = id("adadadad-adad-4ada-8ada-adadadadadad");
    let active_run_id = id("aeaeaeae-aeae-4aea-8aea-aeaeaeaeaeae");
    sample_thread(thread_id).create(&db).await.unwrap();

    let repo = SchedulerStateRepository::new(db.clone());
    let mut scheduler = sample_scheduler(thread_id);
    scheduler.active_task_id = Some(active_task_id);
    scheduler.active_run_id = Some(active_run_id);
    repo.insert_initial(&scheduler).await.unwrap();

    let resolver = ProjectionResolver::new(db);
    let indexes = resolver
        .load_query_indexes(thread_id)
        .await
        .unwrap()
        .expect("query indexes");

    assert_eq!(indexes.freshness, ProjectionFreshness::Fresh);
    let codes = indexes
        .diagnostics
        .iter()
        .map(|diagnostic| diagnostic.code.as_str())
        .collect::<Vec<_>>();
    assert!(
        codes.contains(&"missing_intent_plan_index"),
        "expected missing plan index diagnostic, got {:#?}",
        indexes.diagnostics
    );
    assert!(
        codes.contains(&"missing_active_task_index"),
        "expected missing task index diagnostic, got {:#?}",
        indexes.diagnostics
    );
    assert!(
        codes.contains(&"missing_active_run_index"),
        "expected missing run index diagnostic, got {:#?}",
        indexes.diagnostics
    );
}

/// Scenario: live context frames carried by the scheduler must be reachable
/// through the intent-context-frame query index. Without this diagnostic, a
/// resume surface can show a fresh live context window while the read-side
/// indexes cannot rebuild or explain where that context came from.
#[tokio::test]
async fn projection_resolver_query_indexes_diagnose_missing_live_context_frame_links() {
    let db = setup_db().await;
    let thread_id = id("afafafaf-afaf-4afa-8afa-afafafafafaf");
    sample_thread(thread_id).create(&db).await.unwrap();

    let repo = SchedulerStateRepository::new(db.clone());
    let scheduler = sample_scheduler(thread_id);
    let frame_id = scheduler.live_context_window[0].context_frame_id;
    repo.insert_initial(&scheduler).await.unwrap();

    let resolver = ProjectionResolver::new(db);
    let indexes = resolver
        .load_query_indexes(thread_id)
        .await
        .unwrap()
        .expect("query indexes");

    assert_eq!(indexes.freshness, ProjectionFreshness::Fresh);
    assert!(
        indexes.diagnostics.iter().any(|diagnostic| {
            diagnostic.code == "missing_live_context_frame_index"
                && diagnostic.index_name == "ai_index_intent_context_frame"
                && diagnostic.subject_id == frame_id
        }),
        "expected missing live-context-frame diagnostic, got {:#?}",
        indexes.diagnostics
    );
}

/// Scenario: when neither projection nor scheduler rows exist but the underlying
/// history has Intent + Task objects, `load_or_rebuild_thread_bundle` reconstructs a
/// `Fresh` bundle from the AI history. Confirms the rebuild path is reachable end to
/// end (storage put_tracked → resolver → rebuilder), and that the rebuilt bundle
/// carries the right thread/intent identity.
///
/// `#[serial]` because `ChangeDirGuard` mutates process CWD.
#[tokio::test]
#[serial]
async fn projection_resolver_rebuilds_missing_thread_projection_from_history() {
    let (_dir, storage, history, db_conn) = setup_projection_history().await;
    let actor = ActorRef::human("projection-rebuild").unwrap();
    let intent = Intent::new(actor.clone(), "Recover missing projection").unwrap();
    storage.put_tracked(&intent, &history).await.unwrap();
    let mut task = Task::new(actor, "Recovered task", None).unwrap();
    task.set_intent(Some(intent.header().object_id()));
    storage.put_tracked(&task, &history).await.unwrap();

    let resolver = ProjectionResolver::new(db_conn.as_ref().clone());
    assert!(
        resolver
            .load_thread_bundle(intent.header().object_id())
            .await
            .unwrap()
            .is_none()
    );

    let rebuilder = ProjectionRebuilder::new(storage.as_ref(), &history);
    let bundle = resolver
        .load_or_rebuild_thread_bundle(intent.header().object_id(), &rebuilder)
        .await
        .unwrap()
        .expect("rebuilt thread bundle");

    assert_eq!(bundle.freshness, ProjectionFreshness::Fresh);
    assert_eq!(bundle.thread.thread_id, intent.header().object_id());
    assert_eq!(bundle.thread.intents.len(), 1);
    assert_eq!(bundle.scheduler.thread_id, intent.header().object_id());
}

/// Scenario: when query-index projections are missing but the immutable history
/// contains an Intent + Task pair, the resolver must run the same targeted rebuild
/// path before exposing the denormalized rows. This is the read-side contract for
/// consumers that need cheap `intent -> task` lookups without treating stale rows as
/// writable runtime state.
#[tokio::test]
#[serial]
async fn projection_resolver_rebuilds_and_loads_thread_query_indexes() {
    let (_dir, storage, history, db_conn) = setup_projection_history().await;
    let actor = ActorRef::human("projection-query-index").unwrap();
    let intent = Intent::new(actor.clone(), "Rebuild query indexes").unwrap();
    storage.put_tracked(&intent, &history).await.unwrap();
    let mut task = Task::new(actor, "Indexed task", None).unwrap();
    task.set_intent(Some(intent.header().object_id()));
    let task_id = task.header().object_id();
    storage.put_tracked(&task, &history).await.unwrap();

    let resolver = ProjectionResolver::new(db_conn.as_ref().clone());
    assert!(
        resolver
            .load_query_indexes(intent.header().object_id())
            .await
            .unwrap()
            .is_none()
    );

    let rebuilder = ProjectionRebuilder::new(storage.as_ref(), &history);
    let indexes = resolver
        .load_or_rebuild_query_indexes(intent.header().object_id(), &rebuilder)
        .await
        .unwrap()
        .expect("rebuilt query indexes");

    assert_eq!(indexes.thread_id, intent.header().object_id());
    assert_eq!(indexes.freshness, ProjectionFreshness::Fresh);
    assert!(indexes.intent_plan_index.is_empty());
    assert_eq!(indexes.intent_task_index.len(), 1);
    assert!(indexes.plan_step_task_index.is_empty());
    assert!(indexes.task_run_index.is_empty());
    assert!(indexes.run_event_index.is_empty());
    assert!(indexes.run_patchset_index.is_empty());
    assert!(indexes.intent_context_frame_index.is_empty());
    let intent_task = &indexes.intent_task_index[0];
    assert_eq!(intent_task.intent_id, intent.header().object_id());
    assert_eq!(intent_task.task_id, task_id);

    let direct = resolver
        .load_query_indexes(intent.header().object_id())
        .await
        .unwrap()
        .expect("materialized query indexes");
    assert_eq!(direct, indexes);
}

/// Scenario: the resume entrypoint must do the rebuild/freshness step before
/// returning phase-specific resume metadata. A thread rebuilt from an Intent + Task
/// history has no selected plan yet, so resume must reopen the planning review rather
/// than advancing the scheduler on an implicit stale bundle.
#[tokio::test]
#[serial]
async fn projection_resolver_load_for_resume_rebuilds_and_classifies_planning_review() {
    let (_dir, storage, history, db_conn) = setup_projection_history().await;
    let actor = ActorRef::human("projection-resume").unwrap();
    let intent = Intent::new(actor.clone(), "Resume missing projection").unwrap();
    storage.put_tracked(&intent, &history).await.unwrap();
    let mut task = Task::new(actor, "Recovered resume task", None).unwrap();
    task.set_intent(Some(intent.header().object_id()));
    storage.put_tracked(&task, &history).await.unwrap();

    let resolver = ProjectionResolver::new(db_conn.as_ref().clone());
    let rebuilder = ProjectionRebuilder::new(storage.as_ref(), &history);
    let resume = resolver
        .load_for_resume(intent.header().object_id(), &rebuilder)
        .await
        .unwrap()
        .expect("resume bundle");

    assert_eq!(resume.thread.thread_id, intent.header().object_id());
    assert_eq!(resume.scheduler.thread_id, intent.header().object_id());
    assert_eq!(resume.freshness, ProjectionFreshness::Fresh);
    assert_eq!(resume.phase_at_resume, WorkflowPhase::Planning);
    assert_eq!(resume.resume_reason, ResumeReason::FreshThread);
    assert_eq!(
        resume.resume_actions,
        vec![ResumeAction::ReopenPlanningReview]
    );
}

/// Scenario: if the scheduler already has an active task/run, resume must surface
/// the interrupted execution state explicitly. This guards the phase-aware contract
/// from regressing into a generic "fresh thread" resume action.
#[test]
fn resume_bundle_marks_active_scheduler_as_interrupted_execution() {
    let thread_id = id("88888888-8888-4888-8888-888888888888");
    let active_task_id = id("99999999-9999-4999-8999-999999999999");
    let active_run_id = id("aaaaaaaa-aaaa-4aaa-8aaa-aaaaaaaaaaaa");
    let mut scheduler = sample_scheduler(thread_id);
    scheduler.active_task_id = Some(active_task_id);
    scheduler.active_run_id = Some(active_run_id);

    let resume = ResumeBundle::from_thread_bundle(ThreadBundle {
        thread: sample_thread(thread_id),
        scheduler,
        freshness: ProjectionFreshness::Fresh,
    });

    assert_eq!(resume.phase_at_resume, WorkflowPhase::Execution);
    assert_eq!(resume.resume_reason, ResumeReason::InterruptedRun);
    assert_eq!(
        resume.resume_actions,
        vec![
            ResumeAction::ResumeScheduler,
            ResumeAction::RequeueInterruptedRun
        ]
    );
}

/// Scenario: stale projection rows must request a targeted rebuild and only expose
/// a read-only view. This keeps `--resume` from advancing the scheduler against a
/// degraded projection.
#[test]
fn resume_bundle_routes_stale_projection_to_rebuild_and_read_only() {
    let thread_id = id("bbbbbbbb-bbbb-4bbb-8bbb-bbbbbbbbbbbb");

    let resume = ResumeBundle::from_thread_bundle(ThreadBundle {
        thread: sample_thread(thread_id),
        scheduler: sample_scheduler(thread_id),
        freshness: ProjectionFreshness::StaleReadOnly,
    });

    assert_eq!(resume.resume_reason, ResumeReason::ProjectionStale);
    assert_eq!(
        resume.resume_actions,
        vec![
            ResumeAction::TriggerTargetedRebuild,
            ResumeAction::OpenReadOnly
        ]
    );
}

/// Scenario: unavailable projections must block automatic resume even when the
/// scheduler shape looks executable. A rebuild failure is a diagnostics problem,
/// not permission to continue with a stale active run.
#[test]
fn resume_bundle_blocks_unavailable_projection() {
    let thread_id = id("cccccccc-cccc-4ccc-8ccc-cccccccccccc");
    let mut scheduler = sample_scheduler(thread_id);
    scheduler.active_run_id = Some(id("dddddddd-dddd-4ddd-8ddd-dddddddddddd"));

    let resume = ResumeBundle::from_thread_bundle(ThreadBundle {
        thread: sample_thread(thread_id),
        scheduler,
        freshness: ProjectionFreshness::Unavailable,
    });

    assert_eq!(resume.resume_reason, ResumeReason::ProjectionUnavailable);
    assert_eq!(
        resume.resume_actions,
        vec![ResumeAction::BlockAutomaticResume]
    );
}

/// Scenario: `ResumeBundle` is a read-side contract that can be exposed to UI /
/// MCP / diagnostics. Pin the wire spelling for the phase, reason, and action list.
#[test]
fn resume_bundle_serializes_stable_contract_fields() {
    let thread_id = id("eeeeeeee-eeee-4eee-8eee-eeeeeeeeeeee");
    let mut scheduler = sample_scheduler(thread_id);
    scheduler.active_task_id = Some(id("ffffffff-ffff-4fff-8fff-ffffffffffff"));

    let resume = ResumeBundle::from_thread_bundle(ThreadBundle {
        thread: sample_thread(thread_id),
        scheduler,
        freshness: ProjectionFreshness::Fresh,
    });
    let value = serde_json::to_value(&resume).unwrap();

    assert_eq!(value["phase_at_resume"], json!("execution"));
    assert_eq!(value["resume_reason"], json!("interrupted_run"));
    assert_eq!(
        value["resume_actions"],
        json!(["resume_scheduler", "requeue_interrupted_run"])
    );
}

// ---------------------------------------------------------------------------
// advance_scheduler (v0.17.592): async wrapper around apply_scheduler_mutation
// ---------------------------------------------------------------------------

use libra::internal::ai::runtime::{
    contracts::{ProjectionVersions, SchedulerMutation},
    phase1::{AdvanceSchedulerError, advance_scheduler},
};

/// Scenario: an initial `SchedulerState` is inserted at version 1; an
/// `advance_scheduler` call with a `MarkTaskActive` mutation loads it,
/// applies the mutation, and CAS-saves the resulting state. Asserts
/// the persisted state has the new task / run ids and bumped version.
#[tokio::test]
async fn advance_scheduler_marks_task_active_end_to_end() {
    let db = setup_db().await;
    let thread_id = id("22222222-2222-4222-8222-222222222222");
    sample_thread(thread_id).create(&db).await.unwrap();

    let repo = SchedulerStateRepository::new(db.clone());
    repo.insert_initial(&sample_scheduler(thread_id))
        .await
        .unwrap();

    let task_id = Uuid::new_v4();
    let run_id = Uuid::new_v4();
    let next = advance_scheduler(
        &repo,
        thread_id,
        SchedulerMutation::MarkTaskActive {
            expected: ProjectionVersions {
                thread: 0,
                scheduler: 1,
                live_context_window: 0,
            },
            task_id,
            run_id: Some(run_id),
        },
    )
    .await
    .expect("advance_scheduler should apply and CAS-save");

    assert_eq!(next.active_task_id, Some(task_id));
    assert_eq!(next.active_run_id, Some(run_id));
    assert_eq!(next.version, 2);

    // Reload to confirm the CAS actually persisted.
    let reloaded = repo
        .load(thread_id)
        .await
        .unwrap()
        .expect("state should still exist after advance");
    assert_eq!(reloaded.active_task_id, Some(task_id));
    assert_eq!(reloaded.active_run_id, Some(run_id));
    assert_eq!(reloaded.version, 2);
}

/// Scenario: advance_scheduler against a thread with no scheduler state
/// row must surface `AdvanceSchedulerError::StateMissing { thread_id }`
/// so callers know to `SeedThread` first.
#[tokio::test]
async fn advance_scheduler_returns_state_missing_when_thread_has_no_row() {
    let db = setup_db().await;
    let thread_id = id("33333333-3333-4333-8333-333333333333");
    sample_thread(thread_id).create(&db).await.unwrap();
    // NOTE: no `repo.insert_initial(...)` — the scheduler row is
    // deliberately absent.

    let repo = SchedulerStateRepository::new(db.clone());
    let error = advance_scheduler(
        &repo,
        thread_id,
        SchedulerMutation::MarkTaskActive {
            expected: ProjectionVersions {
                thread: 0,
                scheduler: 0,
                live_context_window: 0,
            },
            task_id: Uuid::new_v4(),
            run_id: None,
        },
    )
    .await
    .expect_err("advance_scheduler must error when no scheduler row exists");

    match error {
        AdvanceSchedulerError::StateMissing { thread_id: tid } => {
            assert_eq!(tid, thread_id);
        }
        other => panic!("expected StateMissing, got {other:?}"),
    }
}

/// Scenario: when the mutation's `expected.scheduler` doesn't match the
/// loaded state's `version`, advance_scheduler must surface the
/// pure-function `ApplySchedulerMutationError::VersionMismatch` through
/// the `AdvanceSchedulerError::Apply` transparent wrapper — before
/// hitting the CAS path. This proves the pure-function precondition
/// check fires before the DB CAS would.
#[tokio::test]
async fn advance_scheduler_surfaces_version_mismatch_before_cas() {
    use libra::internal::ai::runtime::phase1::ApplySchedulerMutationError;

    let db = setup_db().await;
    let thread_id = id("44444444-4444-4444-8444-444444444444");
    sample_thread(thread_id).create(&db).await.unwrap();

    let repo = SchedulerStateRepository::new(db.clone());
    repo.insert_initial(&sample_scheduler(thread_id))
        .await
        .unwrap();
    // sample_scheduler installs version=1; we ask for expected=99.

    let error = advance_scheduler(
        &repo,
        thread_id,
        SchedulerMutation::MarkTaskActive {
            expected: ProjectionVersions {
                thread: 0,
                scheduler: 99,
                live_context_window: 0,
            },
            task_id: Uuid::new_v4(),
            run_id: None,
        },
    )
    .await
    .expect_err("version mismatch must fail-closed before CAS");

    match error {
        AdvanceSchedulerError::Apply(ApplySchedulerMutationError::VersionMismatch {
            expected,
            actual,
        }) => {
            assert_eq!(expected, 99);
            assert_eq!(actual, 1);
        }
        other => panic!("expected Apply(VersionMismatch), got {other:?}"),
    }

    // Reload — version must NOT have advanced (no CAS happened).
    let reloaded = repo.load(thread_id).await.unwrap().unwrap();
    assert_eq!(reloaded.version, 1, "no CAS should have run");
}
