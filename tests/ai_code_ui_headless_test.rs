//! Headless web-only runtime smoke tests.
//!
//! Exercises [`HeadlessCodeRuntime`] end-to-end against the deterministic
//! `test-provider` fixture: submitting a prompt should drive a tool-loop turn
//! whose final assistant text lands in the live `CodeUiSession`. Used as the
//! L1 verification anchor for Phase 3 of `docs/development/commands/_general.md` (the
//! `--web-only --provider <non-codex>` path that previously fell back to a
//! read-only placeholder).

#![cfg(feature = "test-provider")]

use std::{path::PathBuf, sync::Arc, time::Duration};

use libra::internal::ai::{
    agent::runtime::tool_loop::ToolLoopConfig,
    completion::Message,
    providers::fake,
    runtime::{InteractionState, ToolBoundaryRuntime, TracingAuditSink},
    sandbox::{ExecApprovalRequest, NetworkAccess},
    session::{
        SessionState, SessionStore,
        jsonl::{CodeWorkflowEventKind, SessionJsonlStore},
    },
    tools::{
        ToolHandler, ToolInvocation, ToolKind, ToolOutput, ToolRegistry, ToolRegistryBuilder,
        ToolResult, ToolSpec,
        context::{UserInputQuestion, UserInputRequest, UserInputResponse},
        handlers::{PlanHandler, ReadFileHandler, SubmitPlanDraftHandler},
    },
    web::{
        code_ui::{
            CodeUiApplyToFuture, CodeUiCommandAdapter, CodeUiInteractionResponse,
            CodeUiInteractionStatus, CodeUiProviderInfo, CodeUiReadModel, CodeUiSession,
            CodeUiSessionStatus, initial_snapshot,
        },
        headless::{HeadlessCodeRuntime, HeadlessSessionPersistence, headless_capabilities},
    },
};
use tokio::sync::{Notify, mpsc};
use uuid::Uuid;

fn fixture_path(name: &str) -> PathBuf {
    let mut path = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    path.push("tests/fixtures/code_ui");
    path.push(format!("{name}.json"));
    path
}

async fn build_runtime(
    fixture: &str,
    working_dir: PathBuf,
) -> (
    Arc<HeadlessCodeRuntime<fake::CompletionModel>>,
    mpsc::UnboundedSender<UserInputRequest>,
    mpsc::UnboundedSender<ExecApprovalRequest>,
) {
    build_runtime_with_persistence(fixture, working_dir, Vec::new(), None).await
}

async fn build_runtime_with_persistence(
    fixture: &str,
    working_dir: PathBuf,
    initial_history: Vec<Message>,
    persistence: Option<HeadlessSessionPersistence>,
) -> (
    Arc<HeadlessCodeRuntime<fake::CompletionModel>>,
    mpsc::UnboundedSender<UserInputRequest>,
    mpsc::UnboundedSender<ExecApprovalRequest>,
) {
    let registry = Arc::new(
        ToolRegistryBuilder::with_working_dir(working_dir.clone())
            .hardening(ToolBoundaryRuntime::system(
                Uuid::new_v4(),
                Arc::new(TracingAuditSink),
            ))
            .register("read_file", Arc::new(ReadFileHandler))
            .register("update_plan", Arc::new(PlanHandler))
            .register("submit_plan_draft", Arc::new(SubmitPlanDraftHandler))
            .build(),
    );
    build_runtime_with_registry(fixture, working_dir, initial_history, persistence, registry).await
}

async fn build_runtime_with_registry(
    fixture: &str,
    working_dir: PathBuf,
    initial_history: Vec<Message>,
    persistence: Option<HeadlessSessionPersistence>,
    registry: Arc<ToolRegistry>,
) -> (
    Arc<HeadlessCodeRuntime<fake::CompletionModel>>,
    mpsc::UnboundedSender<UserInputRequest>,
    mpsc::UnboundedSender<ExecApprovalRequest>,
) {
    build_runtime_with_registry_and_config(
        fixture,
        working_dir,
        initial_history,
        persistence,
        registry,
        Arc::new(ToolLoopConfig::default),
    )
    .await
}

async fn build_runtime_with_registry_and_config(
    fixture: &str,
    working_dir: PathBuf,
    initial_history: Vec<Message>,
    persistence: Option<HeadlessSessionPersistence>,
    registry: Arc<ToolRegistry>,
    config_factory: Arc<dyn Fn() -> ToolLoopConfig + Send + Sync>,
) -> (
    Arc<HeadlessCodeRuntime<fake::CompletionModel>>,
    mpsc::UnboundedSender<UserInputRequest>,
    mpsc::UnboundedSender<ExecApprovalRequest>,
) {
    build_runtime_with_registry_and_config_and_shutdown_timeout(
        fixture,
        working_dir,
        initial_history,
        persistence,
        registry,
        config_factory,
        None,
    )
    .await
}

async fn build_runtime_with_registry_and_config_and_shutdown_timeout(
    fixture: &str,
    working_dir: PathBuf,
    initial_history: Vec<Message>,
    persistence: Option<HeadlessSessionPersistence>,
    registry: Arc<ToolRegistry>,
    config_factory: Arc<dyn Fn() -> ToolLoopConfig + Send + Sync>,
    shutdown_timeout: Option<Duration>,
) -> (
    Arc<HeadlessCodeRuntime<fake::CompletionModel>>,
    mpsc::UnboundedSender<UserInputRequest>,
    mpsc::UnboundedSender<ExecApprovalRequest>,
) {
    let fake_client = fake::Client::from_fixture_path(&fixture_path(fixture))
        .expect("fake provider fixture must load");
    let model = fake_client.completion_model("fake");
    let capabilities = headless_capabilities();
    let provider = CodeUiProviderInfo {
        provider: "fake".to_string(),
        model: Some("fake".to_string()),
        mode: Some("web-headless".to_string()),
        managed: false,
    };
    let session = CodeUiSession::new(initial_snapshot(
        working_dir.to_string_lossy().to_string(),
        provider,
        capabilities.clone(),
    ));
    let (user_input_tx, user_input_rx) = mpsc::unbounded_channel::<UserInputRequest>();
    let (exec_approval_tx, exec_approval_rx) = mpsc::unbounded_channel::<ExecApprovalRequest>();

    let runtime = match shutdown_timeout {
        Some(shutdown_timeout) => HeadlessCodeRuntime::new_with_persistence_and_shutdown_timeout(
            session,
            capabilities,
            model,
            registry,
            user_input_rx,
            exec_approval_rx,
            config_factory,
            initial_history,
            persistence,
            shutdown_timeout,
        )
        .await
        .expect("test registry must retain the shared tool boundary"),
        None => HeadlessCodeRuntime::new_with_persistence(
            session,
            capabilities,
            model,
            registry,
            user_input_rx,
            exec_approval_rx,
            config_factory,
            initial_history,
            persistence,
        )
        .await
        .expect("test registry must retain the shared tool boundary"),
    };

    (runtime, user_input_tx, exec_approval_tx)
}

/// Deterministic mutating handler used to prove a cancellation request never
/// hard-aborts a tool after the side-effect boundary has been crossed.
struct BlockingMutationHandler {
    started: Arc<Notify>,
    release: Arc<Notify>,
}

#[async_trait::async_trait]
impl ToolHandler for BlockingMutationHandler {
    fn kind(&self) -> ToolKind {
        ToolKind::Function
    }

    async fn is_mutating(&self, _invocation: &ToolInvocation) -> bool {
        true
    }

    async fn handle(&self, _invocation: ToolInvocation) -> ToolResult<ToolOutput> {
        self.started.notify_one();
        self.release.notified().await;
        Ok(ToolOutput::success("blocking mutation completed"))
    }

    fn schema(&self) -> ToolSpec {
        ToolSpec::new(
            "blocking_mutation",
            "Test-only blocking mutating tool used to verify cancellation safety.",
        )
    }
}

/// The non-Codex headless runtime must expose a writable web-headless snapshot
/// immediately, not the legacy read-only `web-ui-placeholder` snapshot.
#[tokio::test(flavor = "multi_thread")]
async fn initial_snapshot_is_writable_non_placeholder_runtime() {
    let workdir = tempfile::tempdir().expect("tempdir for headless workdir");
    let (runtime, _, _) = build_runtime("basic_chat", workdir.path().to_path_buf()).await;

    let snapshot = runtime.snapshot().await;

    assert_eq!(snapshot.status, CodeUiSessionStatus::Idle);
    assert_eq!(snapshot.provider.provider, "fake");
    assert_eq!(snapshot.provider.mode.as_deref(), Some("web-headless"));
    assert!(snapshot.capabilities.message_input);
    assert!(snapshot.capabilities.streaming_text);
    assert!(snapshot.capabilities.tool_calls);
    assert!(
        snapshot
            .transcript
            .iter()
            .all(|entry| entry.id != "web-ui-placeholder"),
        "headless web-only must not expose the read-only placeholder transcript",
    );
}

/// Submitting a plain message must produce an assistant transcript entry that
/// matches the fake provider's deterministic response, with the snapshot
/// returning to `Idle` once the turn settles. This is the single anchor that
/// proves the headless runtime actually drives a model turn — every other
/// scenario (cancel, reject-on-empty, capability flags) builds on it.
#[tokio::test(flavor = "multi_thread")]
async fn submit_message_streams_assistant_reply_into_snapshot() {
    let workdir = tempfile::tempdir().expect("tempdir for headless workdir");
    let (runtime, _, _) = build_runtime("basic_chat", workdir.path().to_path_buf()).await;

    runtime
        .submit_message("hello headless".to_string())
        .await
        .expect("headless submit_message accepts non-empty text");

    // Wait for the spawned turn to finalize the assistant entry.
    let deadline = std::time::Instant::now() + Duration::from_secs(5);
    let mut final_snapshot = runtime.snapshot().await;
    while std::time::Instant::now() < deadline {
        if final_snapshot.status == CodeUiSessionStatus::Idle
            && final_snapshot.transcript.iter().any(|entry| {
                entry.kind
                    == libra::internal::ai::web::code_ui::CodeUiTranscriptEntryKind::AssistantMessage
                    && entry
                        .content
                        .as_deref()
                        .is_some_and(|c| c.contains("fake assistant"))
            })
        {
            break;
        }
        tokio::time::sleep(Duration::from_millis(40)).await;
        final_snapshot = runtime.snapshot().await;
    }

    assert_eq!(
        final_snapshot.status,
        CodeUiSessionStatus::Idle,
        "snapshot must return to idle once the turn finishes",
    );

    let assistant = final_snapshot
        .transcript
        .iter()
        .find(|entry| {
            entry.kind
                == libra::internal::ai::web::code_ui::CodeUiTranscriptEntryKind::AssistantMessage
        })
        .expect("an assistant entry must be appended");
    assert!(!assistant.streaming);
    assert_eq!(assistant.status.as_deref(), Some("completed"));
    assert!(
        assistant
            .content
            .as_deref()
            .is_some_and(|c| c.contains("fake assistant")),
        "assistant entry must carry the fake fixture text, got {:?}",
        assistant.content,
    );
}

/// Browser direct chat is admitted by the serialized AgentRuntime worker, not
/// by an adapter-local `tokio::spawn`. The delayed fixture gives us a stable
/// interval in which the worker must expose its active turn state.
#[tokio::test(flavor = "multi_thread")]
async fn submit_message_is_owned_by_agent_runtime_worker() {
    let workdir = tempfile::tempdir().expect("tempdir for headless workdir");
    let (runtime, _, _) = build_runtime("delayed_chat", workdir.path().to_path_buf()).await;

    runtime
        .submit_message("runtime-owned turn".to_string())
        .await
        .expect("headless submit should enter AgentRuntime");

    let deadline = std::time::Instant::now() + Duration::from_secs(3);
    let mut observed_running_turn = false;
    while std::time::Instant::now() < deadline {
        match runtime.runtime_snapshot().await {
            Ok(snapshot)
                if snapshot.active_turn_id.is_some()
                    && matches!(snapshot.interaction, InteractionState::Running) =>
            {
                observed_running_turn = true;
                break;
            }
            Ok(_) | Err(_) => tokio::time::sleep(Duration::from_millis(20)).await,
        }
    }
    assert!(
        observed_running_turn,
        "the delayed browser turn must be visible in AgentRuntime worker state"
    );

    runtime
        .cancel_turn()
        .await
        .expect("runtime-owned delayed turn must remain cancellable");
}

/// `submit_message("")` must fail loud rather than silently appending an
/// empty transcript entry — the browser will treat this as a UI bug rather
/// than a queued turn.
#[tokio::test(flavor = "multi_thread")]
async fn empty_message_is_rejected_before_any_transcript_mutation() {
    let workdir = tempfile::tempdir().expect("tempdir for headless workdir");
    let (runtime, _, _) = build_runtime("basic_chat", workdir.path().to_path_buf()).await;

    let result = runtime.submit_message("   ".to_string()).await;
    assert!(result.is_err(), "whitespace-only messages must be rejected");

    let snapshot = runtime.snapshot().await;
    assert!(
        snapshot.transcript.is_empty(),
        "rejected submits must not leave transcript residue",
    );
    assert_eq!(snapshot.status, CodeUiSessionStatus::Idle);
}

/// The first durable write is a precondition for a browser turn. If session
/// storage is unavailable, the request must fail without creating transcript
/// residue or starting the tool loop, so a client can repair storage and
/// retry without risking an untracked side effect.
#[tokio::test(flavor = "multi_thread")]
async fn submit_rejects_unpersistable_turn_before_live_session_mutation() {
    let workdir = tempfile::tempdir().expect("tempdir for headless workdir");
    let storage_parent = tempfile::tempdir().expect("tempdir for session storage parent");
    let storage_file = storage_parent.path().join("not-a-directory");
    std::fs::write(&storage_file, b"not a directory")
        .expect("create a file where session storage would require a directory");
    let store = Arc::new(SessionStore::from_storage_path(&storage_file));
    let state = SessionState::new(&workdir.path().to_string_lossy());
    let persistence = HeadlessSessionPersistence::new(store.clone(), state);
    let (runtime, _, _) = build_runtime_with_persistence(
        "basic_chat",
        workdir.path().to_path_buf(),
        Vec::new(),
        Some(persistence),
    )
    .await;

    let error = runtime
        .submit_message("must not start".to_string())
        .await
        .expect_err("an unpersistable browser turn must be rejected");
    assert!(
        error.to_string().contains("no turn was started"),
        "the error must make retry safety explicit: {error:#}",
    );

    let snapshot = runtime.snapshot().await;
    assert!(
        snapshot.transcript.is_empty(),
        "failed durable preflight must not expose partial user or assistant rows",
    );
    assert_eq!(
        snapshot.status,
        CodeUiSessionStatus::Idle,
        "failed durable preflight must not start a live turn",
    );
}

/// Headless web-only sessions must write enough state for `--resume` to
/// restore both model history and the browser transcript on the next process.
#[tokio::test(flavor = "multi_thread")]
async fn submit_message_persists_resumable_session_snapshot() {
    let workdir = tempfile::tempdir().expect("tempdir for headless workdir");
    let storage = tempfile::tempdir().expect("tempdir for session storage");
    let store = Arc::new(SessionStore::from_storage_path(storage.path()));
    let mut state = SessionState::new(&workdir.path().to_string_lossy());
    let thread_id = state.id.clone();
    state.metadata.insert(
        "thread_id".to_string(),
        serde_json::json!(thread_id.clone()),
    );
    let persistence = HeadlessSessionPersistence::new(store.clone(), state);
    let (runtime, _, _) = build_runtime_with_persistence(
        "basic_chat",
        workdir.path().to_path_buf(),
        Vec::new(),
        Some(persistence),
    )
    .await;

    runtime
        .submit_message("persist this turn".to_string())
        .await
        .expect("headless submit should accept non-empty text");

    let deadline = std::time::Instant::now() + Duration::from_secs(5);
    while std::time::Instant::now() < deadline {
        let saved = store.load(&thread_id).expect("session should load");
        if saved.messages.len() == 2 {
            let snapshot = saved
                .metadata
                .get("code_ui_snapshot")
                .expect("persisted session should include Code UI snapshot");
            assert_eq!(
                snapshot.get("threadId").and_then(|value| value.as_str()),
                Some(thread_id.as_str()),
                "persisted Code UI snapshot should carry the resumable thread id",
            );
            assert!(
                snapshot
                    .get("transcript")
                    .and_then(|value| value.as_array())
                    .is_some_and(|entries| entries.len() >= 2),
                "persisted Code UI snapshot should retain browser transcript entries",
            );
            assert_eq!(saved.to_history().len(), 2);

            let projection_store = SessionJsonlStore::new(store.session_root(&thread_id));
            let projection_replay = projection_store
                .load_code_workflow_replay()
                .expect("headless persistence should write a readable workflow projection");
            let durable_command = projection_replay
                .events
                .iter()
                .find_map(|event| match &event.event {
                    CodeWorkflowEventKind::CommandIntentPersisted { command }
                        if command.command_kind == "headless_direct_turn" =>
                    {
                        Some(command)
                    }
                    _ => None,
                })
                .expect("headless turn must durably record its command intent before dispatch");
            assert!(
                durable_command
                    .canonical_request_hash
                    .starts_with("sha256:"),
                "durable command intent must store a canonical request hash rather than raw browser text"
            );
            assert!(
                projection_replay.events.iter().any(|event| matches!(
                    &event.event,
                    CodeWorkflowEventKind::CommandTerminalSuccess { command, .. }
                        if command == &durable_command.identity
                )),
                "completed headless turn must durably record a terminal result"
            );
            let projection_deltas: Vec<_> = projection_replay
                .events
                .iter()
                .filter_map(|event| match &event.event {
                    CodeWorkflowEventKind::CodeUiProjectionDelta {
                        projection,
                        payload,
                        ..
                    } => Some((event.sequence, projection, payload)),
                    _ => None,
                })
                .collect();
            assert!(
                !projection_deltas.is_empty(),
                "headless persistence must write fine-grained Code UI projection events",
            );
            assert!(
                projection_deltas
                    .iter()
                    .all(|(_, _, payload)| !payload.is_null()),
                "every new projection event must carry a foldable payload",
            );
            assert!(
                projection_deltas
                    .iter()
                    .any(|(_, projection, _)| *projection == "transcript_upsert"),
                "the persisted turn must project its transcript entries",
            );
            assert_eq!(
                saved
                    .metadata
                    .get("code_ui_projection_cursor")
                    .and_then(serde_json::Value::as_u64),
                projection_deltas.last().map(|(sequence, _, _)| *sequence),
                "the compatibility snapshot must point at the last durable projection event",
            );
            return;
        }
        tokio::time::sleep(Duration::from_millis(40)).await;
    }

    panic!("session store did not receive the completed headless turn before deadline");
}

#[tokio::test(flavor = "multi_thread")]
async fn caller_supplied_command_id_is_durable_and_idempotent() {
    let workdir = tempfile::tempdir().expect("tempdir for headless workdir");
    let storage = tempfile::tempdir().expect("tempdir for session storage");
    let store = Arc::new(SessionStore::from_storage_path(storage.path()));
    let mut state = SessionState::new(&workdir.path().to_string_lossy());
    let thread_id = state.id.clone();
    state.metadata.insert(
        "thread_id".to_string(),
        serde_json::json!(thread_id.clone()),
    );
    let persistence = HeadlessSessionPersistence::new(store.clone(), state);
    let (runtime, _, _) = build_runtime_with_persistence(
        "basic_chat",
        workdir.path().to_path_buf(),
        Vec::new(),
        Some(persistence),
    )
    .await;
    let command_id = "browser-cmd-stable-1".to_string();

    runtime
        .submit_message_with_command_id(
            "persist with stable id".to_string(),
            Some(command_id.clone()),
        )
        .await
        .expect("first submit with commandId should admit");

    let deadline = std::time::Instant::now() + Duration::from_secs(5);
    let mut saw_terminal = false;
    while std::time::Instant::now() < deadline {
        let projection_store = SessionJsonlStore::new(store.session_root(&thread_id));
        if let Ok(replay) = projection_store.load_code_workflow_replay() {
            let durable_command = replay.events.iter().find_map(|event| match &event.event {
                CodeWorkflowEventKind::CommandIntentPersisted { command }
                    if command.command_kind == "headless_direct_turn"
                        && command.identity.command_id == command_id =>
                {
                    Some(command.clone())
                }
                _ => None,
            });
            if let Some(command) = durable_command {
                assert!(
                    command.canonical_request_hash.starts_with("sha256:"),
                    "caller-supplied commandId must still hash the request payload"
                );
                saw_terminal = replay.events.iter().any(|event| {
                    matches!(
                        &event.event,
                        CodeWorkflowEventKind::CommandTerminalSuccess {
                            command: identity,
                            ..
                        } if identity.command_id == command_id
                    )
                });
                if saw_terminal {
                    break;
                }
            }
        }
        tokio::time::sleep(Duration::from_millis(40)).await;
    }
    assert!(
        saw_terminal,
        "caller-supplied commandId must reach a durable terminal success"
    );

    // Same commandId + same payload is an idempotent retry (no second dispatch).
    runtime
        .submit_message_with_command_id(
            "persist with stable id".to_string(),
            Some(command_id.clone()),
        )
        .await
        .expect("matching retry must acknowledge without error");

    let conflict = runtime
        .submit_message_with_command_id(
            "different payload for same command id".to_string(),
            Some(command_id.clone()),
        )
        .await;
    let conflict_err = conflict.expect_err("same commandId with different text must fail closed");
    let conflict_message = conflict_err.to_string();
    assert!(
        conflict_message.contains("different canonical payload")
            || conflict_message.contains("COMMAND_PAYLOAD_CONFLICT")
            || conflict_err
                .downcast_ref::<libra::internal::ai::runtime::RuntimeWorkerError>()
                .is_some_and(|error| {
                    matches!(
                        error,
                        libra::internal::ai::runtime::RuntimeWorkerError::CommandPayloadConflict { .. }
                    )
                }),
        "payload conflict should surface clearly, got: {conflict_message}"
    );

    let projection_store = SessionJsonlStore::new(store.session_root(&thread_id));
    let replay = projection_store
        .load_code_workflow_replay()
        .expect("workflow projection should remain readable");
    let intent_count = replay
        .events
        .iter()
        .filter(|event| {
            matches!(
                &event.event,
                CodeWorkflowEventKind::CommandIntentPersisted { command }
                    if command.identity.command_id == command_id
            )
        })
        .count();
    assert_eq!(
        intent_count, 1,
        "idempotent retry and payload conflict must not append a second intent"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn in_flight_command_id_rejects_payload_mismatch() {
    let workdir = tempfile::tempdir().expect("tempdir for headless workdir");
    let storage = tempfile::tempdir().expect("tempdir for session storage");
    let store = Arc::new(SessionStore::from_storage_path(storage.path()));
    let mut state = SessionState::new(&workdir.path().to_string_lossy());
    let thread_id = state.id.clone();
    state.metadata.insert(
        "thread_id".to_string(),
        serde_json::json!(thread_id.clone()),
    );
    let persistence = HeadlessSessionPersistence::new(store.clone(), state);
    let (runtime, _, _) = build_runtime_with_persistence(
        "delayed_chat",
        workdir.path().to_path_buf(),
        Vec::new(),
        Some(persistence),
    )
    .await;
    let command_id = "browser-cmd-inflight-1".to_string();

    runtime
        .submit_message_with_command_id("slow".to_string(), Some(command_id.clone()))
        .await
        .expect("first submit must admit before the delayed reply finishes");

    let deadline = std::time::Instant::now() + Duration::from_secs(3);
    let mut saw_streaming = false;
    while std::time::Instant::now() < deadline {
        let snapshot = runtime.snapshot().await;
        if snapshot.transcript.iter().any(|entry| {
            entry.kind
                == libra::internal::ai::web::code_ui::CodeUiTranscriptEntryKind::AssistantMessage
                && entry.streaming
        }) {
            saw_streaming = true;
            break;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    assert!(
        saw_streaming,
        "assistant entry must be streaming before the conflicting retry"
    );

    let matching = runtime
        .submit_message_with_command_id("slow".to_string(), Some(command_id.clone()))
        .await;
    assert!(
        matching.is_ok(),
        "same commandId + same text while in flight must be idempotent"
    );

    let conflict = runtime
        .submit_message_with_command_id("different slow text".to_string(), Some(command_id.clone()))
        .await;
    let conflict_err = conflict.expect_err("same commandId with different text must fail closed");
    assert!(
        conflict_err
            .downcast_ref::<libra::internal::ai::runtime::RuntimeWorkerError>()
            .is_some_and(|error| {
                matches!(
                    error,
                    libra::internal::ai::runtime::RuntimeWorkerError::CommandPayloadConflict { .. }
                )
            }),
        "in-flight payload mismatch must surface CommandPayloadConflict, got: {conflict_err}"
    );

    runtime.cancel_turn().await.expect("cancel must succeed");
}

/// The headless runtime advertises the Phase 3 v1 browser surfaces it can
/// actually deliver. Locking these down catches accidental capability drift
/// between the Rust runtime and the Web UI feature gates.
#[test]
fn headless_capabilities_match_phase3_v1_contract() {
    let caps = headless_capabilities();
    assert!(caps.message_input);
    assert!(caps.streaming_text);
    assert!(caps.tool_calls);
    assert!(caps.plan_updates);
    assert!(caps.patchsets);
    assert!(caps.interactive_approvals);
    assert!(caps.structured_questions);
    assert!(caps.provider_session_resume);
    assert!(caps.command_idempotency);
}

#[tokio::test(flavor = "multi_thread")]
async fn update_plan_tool_call_projects_plan_into_snapshot() {
    let workdir = tempfile::tempdir().expect("tempdir for headless workdir");
    let (runtime, _, _) = build_runtime("plan_update", workdir.path().to_path_buf()).await;

    runtime
        .submit_message("please update the plan".to_string())
        .await
        .expect("headless submit should accept a prompt that triggers update_plan");

    let deadline = std::time::Instant::now() + Duration::from_secs(5);
    while std::time::Instant::now() < deadline {
        let snapshot = runtime.snapshot().await;
        if let Some(plan) = snapshot
            .plans
            .iter()
            .find(|plan| plan.id == "call_update_plan_1")
            && plan.status == "completed"
        {
            assert_eq!(plan.summary.as_deref(), Some("Project the live plan"));
            assert_eq!(plan.steps.len(), 2);
            assert_eq!(plan.steps[0].step, "Inspect Web UI contract");
            assert_eq!(plan.steps[0].status, "completed");
            assert_eq!(plan.steps[1].step, "Pin snapshot projection");
            assert_eq!(plan.steps[1].status, "in_progress");
            return;
        }
        tokio::time::sleep(Duration::from_millis(40)).await;
    }

    let snapshot = runtime.snapshot().await;
    panic!(
        "update_plan call did not project a completed plan into snapshot: {:?}",
        snapshot.plans
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn submit_plan_draft_tool_call_projects_draft_plan_into_snapshot() {
    let workdir = tempfile::tempdir().expect("tempdir for headless workdir");
    let (runtime, _, _) = build_runtime("plan_draft", workdir.path().to_path_buf()).await;

    runtime
        .submit_message("please draft an execution plan".to_string())
        .await
        .expect("headless submit should accept a prompt that triggers submit_plan_draft");

    let deadline = std::time::Instant::now() + Duration::from_secs(5);
    while std::time::Instant::now() < deadline {
        let snapshot = runtime.snapshot().await;
        if let Some(plan) = snapshot
            .plans
            .iter()
            .find(|plan| plan.id == "call_submit_plan_draft_1")
            && plan.status == "completed"
        {
            assert_eq!(
                plan.summary.as_deref(),
                Some("Draft from headless planning tool"),
            );
            assert_eq!(plan.title.as_deref(), Some("Draft execution plan"));
            assert_eq!(plan.steps.len(), 2);
            assert_eq!(
                plan.steps[0].step,
                "Inspect the current Code UI planning contract",
            );
            assert_eq!(plan.steps[0].status, "pending");
            assert_eq!(
                plan.steps[1].step,
                "Expose planning draft projection in the browser",
            );
            assert_eq!(plan.steps[1].status, "pending");
            // C11 regression: the same `on_tool_call_end` writes the tool_call
            // row and the tool-call transcript entry terminal BEFORE the plan,
            // so once the plan is "completed" they must be too. The ordering
            // barrier (`on_tool_call_end` awaits `on_tool_call_begin`) guarantees
            // a late "start" task cannot regress any of these id-keyed rows back
            // to "running" (previously ~40% flaky "plan stuck at running").
            // (Session status is a separate multi-writer race — see the C11
            // card — and is intentionally not asserted here.)
            let tool_call = snapshot
                .tool_calls
                .iter()
                .find(|call| call.id == "call_submit_plan_draft_1")
                .expect("submit_plan_draft tool call must be projected");
            assert_eq!(
                tool_call.status, "completed",
                "tool_call status must not regress to running"
            );
            let entry = snapshot
                .transcript
                .iter()
                .find(|entry| entry.id == "call_submit_plan_draft_1")
                .expect("submit_plan_draft transcript entry must be projected");
            assert_eq!(
                entry.status.as_deref(),
                Some("completed"),
                "tool-call transcript entry status must not regress to running"
            );
            return;
        }
        tokio::time::sleep(Duration::from_millis(40)).await;
    }

    let snapshot = runtime.snapshot().await;
    panic!(
        "submit_plan_draft call did not project a completed draft plan into snapshot: {:?}",
        snapshot.plans
    );
}

/// `cancel_turn` must finalize the streaming assistant entry — leaving it
/// flagged `streaming: true` would render as a perpetual typing indicator
/// in the browser. The fixture's delay() lets us cancel mid-flight with
/// a deterministic race window.
#[tokio::test(flavor = "multi_thread")]
async fn cancel_turn_finalizes_streaming_assistant_entry() {
    let workdir = tempfile::tempdir().expect("tempdir for headless workdir");
    let (runtime, _, _) = build_runtime("delayed_chat", workdir.path().to_path_buf()).await;

    runtime
        .submit_message("slow".to_string())
        .await
        .expect("submit must accept the prompt before delay fires");

    // Wait until the in-flight assistant entry shows up as streaming, then
    // cancel before the fake provider's delay completes.
    let deadline = std::time::Instant::now() + Duration::from_secs(3);
    let mut saw_streaming = false;
    while std::time::Instant::now() < deadline {
        let snapshot = runtime.snapshot().await;
        if snapshot.transcript.iter().any(|entry| {
            entry.kind
                == libra::internal::ai::web::code_ui::CodeUiTranscriptEntryKind::AssistantMessage
                && entry.streaming
        }) {
            saw_streaming = true;
            break;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    assert!(
        saw_streaming,
        "assistant entry must be visible as streaming before cancel fires",
    );

    runtime.cancel_turn().await.expect("cancel must succeed");

    // Cancellation is cooperative: the HTTP command acknowledges the request,
    // then the shared tool loop observes its token and finalizes the turn.
    let deadline = std::time::Instant::now() + Duration::from_secs(3);
    let mut snapshot = runtime.snapshot().await;
    while std::time::Instant::now() < deadline {
        snapshot = runtime.snapshot().await;
        let assistant_cancelled = snapshot.transcript.iter().any(|entry| {
            entry.kind
                == libra::internal::ai::web::code_ui::CodeUiTranscriptEntryKind::AssistantMessage
                && entry.status.as_deref() == Some("cancelled")
                && !entry.streaming
        });
        if snapshot.status == CodeUiSessionStatus::Idle && assistant_cancelled {
            break;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    assert_eq!(snapshot.status, CodeUiSessionStatus::Idle);
    let assistant = snapshot
        .transcript
        .iter()
        .find(|entry| {
            entry.kind
                == libra::internal::ai::web::code_ui::CodeUiTranscriptEntryKind::AssistantMessage
        })
        .expect("assistant entry must remain in the transcript after cancellation resolves");
    assert!(!assistant.streaming, "cancel must clear the streaming flag",);
    assert_eq!(assistant.status.as_deref(), Some("cancelled"));
}

/// Shutdown must not merely signal cancellation and return: callers are about
/// to tear down the Web server, so the in-flight turn must first finalize its
/// transcript and release its slot. New browser commands are rejected once
/// shutdown has begun.
#[tokio::test(flavor = "multi_thread")]
async fn shutdown_waits_for_cooperative_turn_finalization() {
    let workdir = tempfile::tempdir().expect("tempdir for headless workdir");
    let (runtime, _, _) = build_runtime("delayed_chat", workdir.path().to_path_buf()).await;

    runtime
        .submit_message("slow shutdown".to_string())
        .await
        .expect("submit must start a cancellable turn");

    runtime
        .shutdown()
        .await
        .expect("shutdown must wait for the cooperative turn to settle");

    let snapshot = runtime.snapshot().await;
    assert_eq!(
        snapshot.status,
        CodeUiSessionStatus::Idle,
        "shutdown must not return before the active turn reaches a terminal state",
    );
    assert!(snapshot.transcript.iter().any(|entry| {
        entry.kind == libra::internal::ai::web::code_ui::CodeUiTranscriptEntryKind::AssistantMessage
            && entry.status.as_deref() == Some("cancelled")
            && !entry.streaming
    }));
    let submit_error = runtime
        .submit_message("must not restart during shutdown".to_string())
        .await
        .expect_err("shutdown must close admission before waiting for the active turn");
    assert!(submit_error.to_string().contains("shutting down"));
}

/// Concurrent lifecycle owners (for example Ctrl-C plus a startup-failure
/// cleanup guard) must join one shutdown result instead of racing to detach
/// the turn. Both calls therefore see the same successful terminal outcome.
#[tokio::test(flavor = "multi_thread")]
async fn repeated_shutdown_joins_the_same_terminal_result() {
    let workdir = tempfile::tempdir().expect("tempdir for headless workdir");
    let (runtime, _, _) = build_runtime("delayed_chat", workdir.path().to_path_buf()).await;
    runtime
        .submit_message("slow repeated shutdown".to_string())
        .await
        .expect("submit must start a cancellable turn");

    let (first, second) = tokio::join!(runtime.shutdown(), runtime.shutdown());
    first.expect("the first shutdown caller should see clean completion");
    second.expect("the second shutdown caller must join the same clean completion");

    assert_eq!(runtime.snapshot().await.status, CodeUiSessionStatus::Idle);
}

/// A shutdown timeout during a started mutation is an indeterminate-side-effect
/// boundary. It must be persisted before the caller receives its failure, and
/// a late handler completion must not rewrite that state as a clean terminal
/// turn that could be retried automatically.
#[tokio::test(flavor = "multi_thread")]
async fn shutdown_timeout_persists_indeterminate_state() {
    let workdir = tempfile::tempdir().expect("tempdir for headless workdir");
    let storage = tempfile::tempdir().expect("tempdir for session storage");
    let store = Arc::new(SessionStore::from_storage_path(storage.path()));
    let state = SessionState::new(&workdir.path().to_string_lossy());
    let session_id = state.id.clone();
    let persistence = HeadlessSessionPersistence::new(store.clone(), state);
    let started = Arc::new(Notify::new());
    let release = Arc::new(Notify::new());
    let registry = Arc::new(
        ToolRegistryBuilder::with_working_dir(workdir.path().to_path_buf())
            .hardening(ToolBoundaryRuntime::system(
                Uuid::new_v4(),
                Arc::new(TracingAuditSink),
            ))
            .register("read_file", Arc::new(ReadFileHandler))
            .register("update_plan", Arc::new(PlanHandler))
            .register("submit_plan_draft", Arc::new(SubmitPlanDraftHandler))
            .register(
                "blocking_mutation",
                Arc::new(BlockingMutationHandler {
                    started: Arc::clone(&started),
                    release: Arc::clone(&release),
                }),
            )
            .build(),
    );
    let config_factory: Arc<dyn Fn() -> ToolLoopConfig + Send + Sync> =
        Arc::new(|| ToolLoopConfig {
            terminal_tools: Some(vec!["blocking_mutation".to_string()]),
            ..ToolLoopConfig::default()
        });
    let (runtime, _, _) = build_runtime_with_registry_and_config_and_shutdown_timeout(
        "blocking_mutation",
        workdir.path().to_path_buf(),
        Vec::new(),
        Some(persistence),
        registry,
        config_factory,
        Some(Duration::from_millis(40)),
    )
    .await;

    runtime
        .submit_message("start blocking mutation".to_string())
        .await
        .expect("the mutation fixture should start a headless turn");
    tokio::time::timeout(Duration::from_secs(3), started.notified())
        .await
        .expect("the blocking mutation handler should begin");

    let error = runtime
        .shutdown()
        .await
        .expect_err("a non-cooperative mutation must trip the shutdown deadline");
    assert!(error.to_string().contains("indeterminate"));
    assert_eq!(
        runtime.snapshot().await.status,
        CodeUiSessionStatus::IndeterminateSideEffect,
    );
    let saved = store
        .load(&session_id)
        .expect("shutdown timeout state must be persisted before returning");
    assert_eq!(
        saved
            .metadata
            .get("code_ui_snapshot")
            .and_then(|snapshot| snapshot.get("status"))
            .and_then(serde_json::Value::as_str),
        Some("indeterminate_side_effect"),
        "durable snapshot must force reconciliation after an uncertain shutdown",
    );
    let command_replay = SessionJsonlStore::new(store.session_root(&session_id))
        .load_code_workflow_replay()
        .expect("shutdown timeout must leave a readable durable command log");
    assert!(
        command_replay.events.iter().any(|event| matches!(
            &event.event,
            CodeWorkflowEventKind::CommandIndeterminateSideEffect { effect, .. }
                if effect == "runtime_shutdown_timeout"
        )),
        "shutdown timeout must durably mark the active browser command indeterminate",
    );
    let repeated_error = runtime
        .shutdown()
        .await
        .expect_err("repeated shutdown must return the original timeout result");
    assert_eq!(repeated_error.to_string(), error.to_string());

    release.notify_one();
    let deadline = std::time::Instant::now() + Duration::from_secs(3);
    while std::time::Instant::now() < deadline {
        if runtime
            .snapshot()
            .await
            .tool_calls
            .iter()
            .any(|call| call.id == "blocking-mutation-1" && call.status == "completed")
        {
            break;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    assert!(
        runtime
            .snapshot()
            .await
            .tool_calls
            .iter()
            .any(|call| call.id == "blocking-mutation-1" && call.status == "completed"),
        "the handler must really complete after the timeout before we assert state preservation",
    );
    assert_eq!(
        runtime.snapshot().await.status,
        CodeUiSessionStatus::IndeterminateSideEffect,
        "a late determinate completion must not erase the timeout reconciliation state",
    );
}

/// Once a handler that may mutate has begun, cancellation must refuse to
/// hard-abort its task. The caller receives a determinate error, the handler
/// stays alive until its own completion, and the turn can then settle normally.
#[tokio::test(flavor = "multi_thread")]
async fn cancel_does_not_abort_started_mutating_headless_tool() {
    let workdir = tempfile::tempdir().expect("tempdir for headless workdir");
    let started = Arc::new(Notify::new());
    let release = Arc::new(Notify::new());
    let registry = Arc::new(
        ToolRegistryBuilder::with_working_dir(workdir.path().to_path_buf())
            .hardening(ToolBoundaryRuntime::system(
                Uuid::new_v4(),
                Arc::new(TracingAuditSink),
            ))
            .register("read_file", Arc::new(ReadFileHandler))
            .register("update_plan", Arc::new(PlanHandler))
            .register("submit_plan_draft", Arc::new(SubmitPlanDraftHandler))
            .register(
                "blocking_mutation",
                Arc::new(BlockingMutationHandler {
                    started: Arc::clone(&started),
                    release: Arc::clone(&release),
                }),
            )
            .build(),
    );
    let config_factory: Arc<dyn Fn() -> ToolLoopConfig + Send + Sync> =
        Arc::new(|| ToolLoopConfig {
            terminal_tools: Some(vec!["blocking_mutation".to_string()]),
            ..ToolLoopConfig::default()
        });
    let (runtime, _, _) = build_runtime_with_registry_and_config(
        "blocking_mutation",
        workdir.path().to_path_buf(),
        Vec::new(),
        None,
        registry,
        config_factory,
    )
    .await;

    runtime
        .submit_message("start blocking mutation".to_string())
        .await
        .expect("the mutation fixture should start a headless turn");
    tokio::time::timeout(Duration::from_secs(3), started.notified())
        .await
        .expect("the blocking mutation handler should begin");

    let error = runtime
        .cancel_turn()
        .await
        .expect_err("cancellation must not abort an already-started mutation");
    assert!(
        error.to_string().contains("cannot safely abort"),
        "the error must make the indeterminate-side-effect boundary explicit: {error:#}",
    );
    let second_submit = runtime
        .submit_message("must wait for mutation".to_string())
        .await
        .expect_err("the still-running mutation must retain the turn slot");
    assert!(
        second_submit.to_string().contains("already running"),
        "a started mutation must retain the active-turn slot: {second_submit:#}",
    );

    // If `cancel_turn` had retained the old JoinHandle::abort() behavior, the
    // handler would never observe this release and the turn would not complete.
    release.notify_one();
    let deadline = std::time::Instant::now() + Duration::from_secs(3);
    let mut snapshot = runtime.snapshot().await;
    while std::time::Instant::now() < deadline {
        snapshot = runtime.snapshot().await;
        if snapshot.status == CodeUiSessionStatus::Idle
            && snapshot.transcript.iter().any(|entry| {
                entry.kind
                == libra::internal::ai::web::code_ui::CodeUiTranscriptEntryKind::AssistantMessage
                && entry.status.as_deref() == Some("completed")
                && entry
                    .content
                    .as_deref()
                    .is_some_and(|content| content.contains("mutation completed"))
            })
        {
            break;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    assert_eq!(
        snapshot.status,
        CodeUiSessionStatus::Idle,
        "the preserved mutating tool must let the turn reach a determinate result",
    );
}

/// Late-arriving stream deltas (e.g. from a still-pending tokio task spawned
/// by `HeadlessTurnObserver::on_model_stream_event`) must not resurrect the
/// `streaming: true` flag once the assistant entry has been finalized as
/// `cancelled`. Without this, the browser would briefly clear its typing
/// indicator and then see it return for any text delta that races past
/// `cancel_turn`.
#[tokio::test(flavor = "multi_thread")]
async fn late_stream_delta_does_not_resurrect_cancelled_entry() {
    use libra::internal::ai::web::code_ui::{
        CodeUiCapabilities, CodeUiProviderInfo, CodeUiSession, CodeUiTranscriptEntry,
        CodeUiTranscriptEntryKind, initial_snapshot,
    };

    let session = CodeUiSession::new(initial_snapshot(
        "/tmp/late-delta",
        CodeUiProviderInfo {
            provider: "fake".to_string(),
            model: None,
            mode: None,
            managed: false,
        },
        CodeUiCapabilities::default(),
    ));
    let now = chrono::Utc::now();
    let entry_id = "assistant-1".to_string();
    session
        .upsert_transcript_entry(CodeUiTranscriptEntry {
            id: entry_id.clone(),
            kind: CodeUiTranscriptEntryKind::AssistantMessage,
            title: None,
            content: Some(String::from("partial")),
            status: Some("cancelled".to_string()),
            streaming: false,
            metadata: serde_json::json!({}),
            created_at: now,
            updated_at: now,
        })
        .await;

    // Late delta from an already-finalized turn arrives — it must be ignored.
    session
        .append_assistant_delta(&entry_id, " more text")
        .await;

    let snapshot = session.snapshot().await;
    let entry = snapshot
        .transcript
        .iter()
        .find(|e| e.id == entry_id)
        .expect("entry must still exist");
    assert!(
        !entry.streaming,
        "late delta must not flip a finalized entry back to streaming",
    );
    assert_eq!(entry.status.as_deref(), Some("cancelled"));
    assert_eq!(
        entry.content.as_deref(),
        Some("partial"),
        "late delta must not append to finalized content",
    );
}

/// `append_assistant_delta` must keep accepting deltas while the entry is
/// in any non-terminal state (e.g. the TUI flow flags entries as
/// `thinking` rather than `streaming`). Only the terminal statuses
/// (`completed` / `error` / `cancelled`) short-circuit the append. This
/// regression test guards against tightening the guard back to a strict
/// `status == "streaming"` check that breaks the TUI's live streaming.
#[tokio::test(flavor = "multi_thread")]
async fn append_assistant_delta_still_accepts_thinking_status() {
    use libra::internal::ai::web::code_ui::{
        CodeUiCapabilities, CodeUiProviderInfo, CodeUiSession, CodeUiTranscriptEntry,
        CodeUiTranscriptEntryKind, initial_snapshot,
    };

    let session = CodeUiSession::new(initial_snapshot(
        "/tmp/thinking-delta",
        CodeUiProviderInfo {
            provider: "fake".to_string(),
            model: None,
            mode: None,
            managed: false,
        },
        CodeUiCapabilities::default(),
    ));
    let now = chrono::Utc::now();
    let entry_id = "assistant-tui".to_string();
    session
        .upsert_transcript_entry(CodeUiTranscriptEntry {
            id: entry_id.clone(),
            kind: CodeUiTranscriptEntryKind::AssistantMessage,
            title: None,
            content: Some(String::new()),
            // The TUI's live assistant row carries `status: "thinking"`
            // alongside `streaming: true` until the model finishes —
            // mirror that here.
            status: Some("thinking".to_string()),
            streaming: true,
            metadata: serde_json::json!({}),
            created_at: now,
            updated_at: now,
        })
        .await;

    session.append_assistant_delta(&entry_id, "hello ").await;
    session.append_assistant_delta(&entry_id, "world").await;

    let snapshot = session.snapshot().await;
    let entry = snapshot
        .transcript
        .iter()
        .find(|e| e.id == entry_id)
        .expect("entry must exist");
    assert!(entry.streaming);
    assert_eq!(entry.content.as_deref(), Some("hello world"));
}

/// `respond_interaction` should reject unknown interactions and only
/// accept requests that are currently pending.
#[tokio::test(flavor = "multi_thread")]
async fn respond_interaction_unknown_id() {
    let workdir = tempfile::tempdir().expect("tempdir for headless workdir");
    let (runtime, _, _) = build_runtime("basic_chat", workdir.path().to_path_buf()).await;

    let result = runtime
        .respond_interaction("ignored", CodeUiInteractionResponse::default())
        .await;
    let error = result.expect_err("interactions must surface a concrete error for unknown id");
    assert!(
        error.to_string().contains("Unknown pending interaction"),
        "error message must call out unknown interaction ids, got {error}",
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn request_user_input_request_is_reflected_in_snapshot_and_responded_to() {
    let workdir = tempfile::tempdir().expect("tempdir for headless workdir");
    let (runtime, user_input_tx, _) =
        build_runtime("basic_chat", workdir.path().to_path_buf()).await;

    let interaction_id = "request-user-input-1".to_string();
    let question_id = "q1".to_string();
    let (response_tx, response_rx) = tokio::sync::oneshot::channel::<UserInputResponse>();
    user_input_tx
        .send(UserInputRequest {
            call_id: interaction_id.clone(),
            questions: vec![UserInputQuestion {
                id: question_id.clone(),
                header: "Approve".to_string(),
                question: "Choose approach".to_string(),
                is_other: false,
                is_secret: false,
                options: None,
            }],
            response_tx,
        })
        .expect("request_user_input request should enqueue in runtime");

    let deadline = std::time::Instant::now() + Duration::from_secs(3);
    let mut saw_pending = false;
    while std::time::Instant::now() < deadline {
        let snapshot = runtime.snapshot().await;
        if snapshot.interactions.iter().any(|interaction| {
            interaction.id == interaction_id
                && interaction.status == CodeUiInteractionStatus::Pending
        }) {
            saw_pending = true;
            break;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    assert!(
        saw_pending,
        "request_user_input request should appear as pending interaction",
    );

    runtime
        .respond_interaction(
            &interaction_id,
            CodeUiInteractionResponse {
                selected_option: Some("selected option".to_string()),
                ..Default::default()
            },
        )
        .await
        .expect("respond_interaction should forward to pending request sender");

    let response = response_rx
        .await
        .expect("request_user_input request should deliver response");
    assert_eq!(
        response
            .answers
            .get(&question_id)
            .expect("response should include requested question")
            .answers,
        vec!["selected option".to_string()]
    );

    let final_snapshot = runtime.snapshot().await;
    assert_eq!(
        final_snapshot.status,
        CodeUiSessionStatus::ExecutingTool,
        "respond_interaction should set runtime status to executing tool",
    );
    assert!(
        final_snapshot
            .interactions
            .iter()
            .all(|interaction| interaction.status != CodeUiInteractionStatus::Pending),
        "all pending interactions should be resolved",
    );
}

/// A multi-question `request_user_input` response must be complete and keyed
/// only by the questions the tool requested.  In particular, do not silently
/// deliver the first answer and discard the rest: that would let the tool loop
/// proceed with an incomplete human decision.
#[tokio::test(flavor = "multi_thread")]
async fn request_user_input_validates_and_delivers_all_requested_answers() {
    let workdir = tempfile::tempdir().expect("tempdir for headless workdir");
    let (runtime, user_input_tx, _) =
        build_runtime("basic_chat", workdir.path().to_path_buf()).await;

    let interaction_id = "request-user-input-many".to_string();
    let (response_tx, response_rx) = tokio::sync::oneshot::channel::<UserInputResponse>();
    user_input_tx
        .send(UserInputRequest {
            call_id: interaction_id.clone(),
            questions: vec![
                UserInputQuestion {
                    id: "environment".to_string(),
                    header: "Environment".to_string(),
                    question: "Which environment should be used?".to_string(),
                    is_other: false,
                    is_secret: false,
                    options: None,
                },
                UserInputQuestion {
                    id: "risk".to_string(),
                    header: "Risk".to_string(),
                    question: "What risk should be accepted?".to_string(),
                    is_other: false,
                    is_secret: false,
                    options: None,
                },
            ],
            response_tx,
        })
        .expect("request_user_input request should enqueue in runtime");

    let deadline = std::time::Instant::now() + Duration::from_secs(3);
    while std::time::Instant::now() < deadline {
        if runtime
            .snapshot()
            .await
            .interactions
            .iter()
            .any(|interaction| {
                interaction.id == interaction_id
                    && interaction.status == CodeUiInteractionStatus::Pending
            })
        {
            break;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }

    let incomplete = runtime
        .respond_interaction(
            &interaction_id,
            CodeUiInteractionResponse {
                answers: [("environment".to_string(), vec!["staging".to_string()])]
                    .into_iter()
                    .collect(),
                ..Default::default()
            },
        )
        .await
        .expect_err("incomplete answers must remain recoverable rather than reaching the tool");
    assert!(
        incomplete
            .to_string()
            .contains("missing an answer for question 'risk'"),
        "error must identify the missing question, got {incomplete}"
    );
    assert!(
        runtime
            .snapshot()
            .await
            .interactions
            .iter()
            .any(|interaction| {
                interaction.id == interaction_id
                    && interaction.status == CodeUiInteractionStatus::Pending
            }),
        "invalid answers must keep the interaction pending for correction",
    );

    runtime
        .respond_interaction(
            &interaction_id,
            CodeUiInteractionResponse {
                answers: [
                    ("environment".to_string(), vec!["staging".to_string()]),
                    ("risk".to_string(), vec!["low".to_string()]),
                ]
                .into_iter()
                .collect(),
                ..Default::default()
            },
        )
        .await
        .expect("all requested answers should be delivered");

    let response = response_rx
        .await
        .expect("request_user_input request should receive the complete response");
    assert_eq!(
        response
            .answers
            .get("environment")
            .map(|answer| &answer.answers),
        Some(&vec!["staging".to_string()]),
    );
    assert_eq!(
        response.answers.get("risk").map(|answer| &answer.answers),
        Some(&vec!["low".to_string()]),
    );
}

/// A `request_user_input` emitted during a real browser turn must be visible
/// to the serialized runtime, not only in the Web session projection. The
/// turn remains active while the tool-loop continuation waits; a valid browser
/// answer must travel through the worker before the continuation is released.
#[tokio::test(flavor = "multi_thread")]
async fn live_user_input_interaction_is_registered_with_agent_runtime() {
    let workdir = tempfile::tempdir().expect("tempdir for headless workdir");
    let (runtime, user_input_tx, _) =
        build_runtime("delayed_chat", workdir.path().to_path_buf()).await;

    runtime
        .submit_message("slow turn with input".to_string())
        .await
        .expect("delayed turn starts");
    let deadline = std::time::Instant::now() + Duration::from_secs(3);
    while std::time::Instant::now() < deadline {
        if matches!(
            runtime.runtime_snapshot().await.expect("worker snapshot"),
            libra::internal::ai::runtime::AgentSnapshot {
                interaction: InteractionState::Running,
                ..
            }
        ) {
            break;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }

    let interaction_id = "live-request-user-input-1".to_string();
    let (response_tx, response_rx) = tokio::sync::oneshot::channel::<UserInputResponse>();
    user_input_tx
        .send(UserInputRequest {
            call_id: interaction_id.clone(),
            questions: vec![UserInputQuestion {
                id: "q1".to_string(),
                header: "Confirm".to_string(),
                question: "Continue?".to_string(),
                is_other: false,
                is_secret: false,
                options: None,
            }],
            response_tx,
        })
        .expect("input request reaches the headless listener");

    let deadline = std::time::Instant::now() + Duration::from_secs(3);
    while std::time::Instant::now() < deadline {
        let snapshot = runtime.runtime_snapshot().await.expect("worker snapshot");
        if snapshot.interaction
            == (InteractionState::AwaitingUserInput {
                interaction_id: interaction_id.clone(),
            })
        {
            break;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    assert_eq!(
        runtime
            .runtime_snapshot()
            .await
            .expect("worker snapshot")
            .interaction,
        InteractionState::AwaitingUserInput {
            interaction_id: interaction_id.clone(),
        },
        "the worker must own the active headless interaction state",
    );

    runtime
        .respond_interaction(
            &interaction_id,
            CodeUiInteractionResponse {
                selected_option: Some("continue".to_string()),
                ..Default::default()
            },
        )
        .await
        .expect("a valid browser response is delivered through AgentRuntime");
    let response = response_rx
        .await
        .expect("worker executor releases the original input continuation");
    assert_eq!(
        response.answers.get("q1").map(|answer| &answer.answers),
        Some(&vec!["continue".to_string()]),
    );
    assert_eq!(
        runtime
            .runtime_snapshot()
            .await
            .expect("worker snapshot after response")
            .interaction,
        InteractionState::Running,
        "successful delivery returns the live turn to worker-owned Running state",
    );

    runtime
        .cancel_turn()
        .await
        .expect("cancelling a live interaction is cooperative");
}

/// Cancelling an active headless interaction must drop the worker-owned sender
/// before the tool-loop future has finished unwinding. Otherwise a real
/// `request_user_input` handler can await that sender forever while the
/// runtime waits for the handler to exit.
#[tokio::test(flavor = "multi_thread")]
async fn cancelling_live_user_input_closes_worker_owned_continuation() {
    let workdir = tempfile::tempdir().expect("tempdir for headless workdir");
    let (runtime, user_input_tx, _) =
        build_runtime("delayed_chat", workdir.path().to_path_buf()).await;

    runtime
        .submit_message("slow turn with input".to_string())
        .await
        .expect("delayed turn starts");
    let deadline = std::time::Instant::now() + Duration::from_secs(3);
    while std::time::Instant::now() < deadline {
        if matches!(
            runtime.runtime_snapshot().await.expect("worker snapshot"),
            libra::internal::ai::runtime::AgentSnapshot {
                interaction: InteractionState::Running,
                ..
            }
        ) {
            break;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }

    let interaction_id = "cancel-live-request-user-input".to_string();
    let (response_tx, response_rx) = tokio::sync::oneshot::channel::<UserInputResponse>();
    user_input_tx
        .send(UserInputRequest {
            call_id: interaction_id.clone(),
            questions: vec![UserInputQuestion {
                id: "q1".to_string(),
                header: "Confirm".to_string(),
                question: "Continue?".to_string(),
                is_other: false,
                is_secret: false,
                options: None,
            }],
            response_tx,
        })
        .expect("input request reaches the headless listener");

    let awaiting = InteractionState::AwaitingUserInput {
        interaction_id: interaction_id.clone(),
    };
    let deadline = std::time::Instant::now() + Duration::from_secs(3);
    while std::time::Instant::now() < deadline {
        if runtime
            .runtime_snapshot()
            .await
            .expect("worker snapshot")
            .interaction
            == awaiting
        {
            break;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    assert_eq!(
        runtime
            .runtime_snapshot()
            .await
            .expect("worker snapshot")
            .interaction,
        awaiting,
    );

    runtime
        .cancel_turn()
        .await
        .expect("cancelling a live interaction is cooperative");
    assert!(
        tokio::time::timeout(Duration::from_secs(1), response_rx)
            .await
            .expect("cancellation must release the original tool continuation")
            .is_err(),
        "the worker-owned interaction sender must close on cancellation",
    );
    let deadline = std::time::Instant::now() + Duration::from_secs(3);
    while std::time::Instant::now() < deadline {
        if runtime
            .runtime_snapshot()
            .await
            .expect("worker snapshot")
            .active_turn_id
            .is_none()
        {
            break;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    assert!(
        runtime
            .snapshot()
            .await
            .interactions
            .iter()
            .all(|interaction| interaction.id != interaction_id),
        "cancelling a worker-owned interaction must clear the browser projection",
    );
}

/// An exec approval emitted during a real browser turn uses the same
/// worker-owned continuation path as structured input. Invalid approval data
/// must remain pending; only a validated, durably recorded decision may wake
/// the original sandbox/tool-loop receiver.
#[tokio::test(flavor = "multi_thread")]
async fn live_exec_approval_interaction_is_registered_with_agent_runtime() {
    let workdir = tempfile::tempdir().expect("tempdir for headless workdir");
    let (runtime, _, exec_approval_tx) =
        build_runtime("delayed_chat", workdir.path().to_path_buf()).await;

    runtime
        .submit_message("slow turn with approval".to_string())
        .await
        .expect("delayed turn starts");
    let deadline = std::time::Instant::now() + Duration::from_secs(3);
    while std::time::Instant::now() < deadline {
        if matches!(
            runtime.runtime_snapshot().await.expect("worker snapshot"),
            libra::internal::ai::runtime::AgentSnapshot {
                interaction: InteractionState::Running,
                ..
            }
        ) {
            break;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }

    let interaction_id = "live-exec-approval-1".to_string();
    let (response_tx, response_rx) = tokio::sync::oneshot::channel();
    exec_approval_tx
        .send(ExecApprovalRequest {
            call_id: interaction_id.clone(),
            command: "cargo check".to_string(),
            cwd: workdir.path().to_path_buf(),
            reason: Some("verify worker-owned approval continuation".to_string()),
            is_retry: false,
            sandbox_label: "workspace-write".to_string(),
            network_access: NetworkAccess::Denied,
            writable_roots: Vec::new(),
            cache_disabled_reason: None,
            response_tx,
        })
        .expect("approval request reaches the headless listener");

    let awaiting = InteractionState::AwaitingToolApproval {
        interaction_id: interaction_id.clone(),
        tool_name: "shell".to_string(),
    };
    let deadline = std::time::Instant::now() + Duration::from_secs(3);
    while std::time::Instant::now() < deadline {
        if runtime
            .runtime_snapshot()
            .await
            .expect("worker snapshot")
            .interaction
            == awaiting
        {
            break;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    assert_eq!(
        runtime
            .runtime_snapshot()
            .await
            .expect("worker snapshot")
            .interaction,
        awaiting,
        "the worker must own the active headless approval state",
    );

    let invalid = runtime
        .respond_interaction(&interaction_id, CodeUiInteractionResponse::default())
        .await
        .expect_err("approval without an explicit decision must remain pending");
    assert!(invalid.to_string().contains("explicit decision"));
    assert_eq!(
        runtime
            .runtime_snapshot()
            .await
            .expect("worker snapshot after invalid response")
            .interaction,
        awaiting,
        "worker validation must preserve the retryable approval state",
    );

    runtime
        .respond_interaction(
            &interaction_id,
            CodeUiInteractionResponse {
                selected_option: Some("approve".to_string()),
                ..Default::default()
            },
        )
        .await
        .expect("valid approval is delivered through AgentRuntime");
    assert_eq!(
        response_rx
            .await
            .expect("worker continuation releases the original approval receiver"),
        libra::internal::ai::sandbox::ReviewDecision::Approved,
    );
    assert_eq!(
        runtime
            .runtime_snapshot()
            .await
            .expect("worker snapshot after approval")
            .interaction,
        InteractionState::Running,
        "successful delivery returns the live turn to worker-owned Running state",
    );

    runtime
        .cancel_turn()
        .await
        .expect("cancelling a live interaction is cooperative");
}

#[tokio::test(flavor = "multi_thread")]
async fn exec_approval_request_is_reflected_in_snapshot_and_responded_to() {
    let workdir = tempfile::tempdir().expect("tempdir for headless workdir");
    let (runtime, _, exec_approval_tx) =
        build_runtime("basic_chat", workdir.path().to_path_buf()).await;

    let interaction_id = "exec-approval-1".to_string();
    let (response_tx, response_rx) = tokio::sync::oneshot::channel();
    let cwd = workdir.path().to_path_buf();

    exec_approval_tx
        .send(ExecApprovalRequest {
            call_id: interaction_id.clone(),
            command: "cargo check".to_string(),
            cwd,
            reason: Some("Run cargo check for repository validation".to_string()),
            is_retry: false,
            sandbox_label: "workspace-write".to_string(),
            network_access: NetworkAccess::Denied,
            writable_roots: Vec::new(),
            cache_disabled_reason: None,
            response_tx,
        })
        .expect("exec approval request should enqueue in runtime");

    let deadline = std::time::Instant::now() + Duration::from_secs(3);
    let mut saw_pending = false;
    while std::time::Instant::now() < deadline {
        let snapshot = runtime.snapshot().await;
        if snapshot.interactions.iter().any(|interaction| {
            interaction.id == interaction_id
                && interaction.status == CodeUiInteractionStatus::Pending
        }) {
            saw_pending = true;
            break;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    assert!(
        saw_pending,
        "exec approval request should appear as pending interaction",
    );

    let invalid_response = runtime
        .respond_interaction(&interaction_id, CodeUiInteractionResponse::default())
        .await
        .expect_err("an approval without an explicit decision must be rejected");
    assert!(invalid_response.to_string().contains("explicit decision"));
    assert!(
        runtime
            .snapshot()
            .await
            .interactions
            .iter()
            .any(|interaction| {
                interaction.id == interaction_id
                    && interaction.status == CodeUiInteractionStatus::Pending
            }),
        "invalid approval input must retain the pending interaction for correction",
    );

    runtime
        .respond_interaction(
            &interaction_id,
            CodeUiInteractionResponse {
                selected_option: Some("approve".to_string()),
                apply_to_future: Some(CodeUiApplyToFuture::AcceptAll),
                ..Default::default()
            },
        )
        .await
        .expect("respond_interaction should forward to pending execution approval sender");

    let decision = response_rx
        .await
        .expect("exec approval request should receive review decision");
    assert_eq!(
        decision,
        libra::internal::ai::sandbox::ReviewDecision::ApprovedForAllCommands,
        "accept_all should request persistent approval for future commands",
    );

    let final_snapshot = runtime.snapshot().await;
    assert_eq!(
        final_snapshot.status,
        CodeUiSessionStatus::ExecutingTool,
        "respond_interaction should set runtime status to executing tool",
    );
    assert!(
        final_snapshot
            .interactions
            .iter()
            .all(|interaction| interaction.status != CodeUiInteractionStatus::Pending),
        "all pending interactions should be resolved",
    );
}

/// Cancelling an idle browser runtime must close both kinds of pending
/// continuation. In particular, an injected exec approval has no active
/// worker turn to observe cancellation, so retaining its sender would leave
/// the sandbox/tool loop waiting indefinitely instead of failing closed.
#[tokio::test(flavor = "multi_thread")]
async fn cancelling_idle_runtime_closes_pending_exec_approval() {
    let workdir = tempfile::tempdir().expect("tempdir for headless workdir");
    let (runtime, _, exec_approval_tx) =
        build_runtime("basic_chat", workdir.path().to_path_buf()).await;

    let interaction_id = "cancel-idle-exec-approval".to_string();
    let (response_tx, response_rx) = tokio::sync::oneshot::channel();
    exec_approval_tx
        .send(ExecApprovalRequest {
            call_id: interaction_id.clone(),
            command: "cargo check".to_string(),
            cwd: workdir.path().to_path_buf(),
            reason: Some("verify cancellation closes the approval".to_string()),
            is_retry: false,
            sandbox_label: "workspace-write".to_string(),
            network_access: NetworkAccess::Denied,
            writable_roots: Vec::new(),
            cache_disabled_reason: None,
            response_tx,
        })
        .expect("exec approval request should enqueue in runtime");

    let deadline = std::time::Instant::now() + Duration::from_secs(3);
    while std::time::Instant::now() < deadline {
        if runtime
            .snapshot()
            .await
            .interactions
            .iter()
            .any(|interaction| {
                interaction.id == interaction_id
                    && interaction.status == CodeUiInteractionStatus::Pending
            })
        {
            break;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }

    runtime
        .cancel_turn()
        .await
        .expect("idle cancellation should close pending interactions");
    assert!(
        tokio::time::timeout(Duration::from_secs(1), response_rx)
            .await
            .expect("cancellation must close the approval continuation")
            .is_err(),
        "an idle cancellation must drop the approval sender rather than leave a tool waiting",
    );
    assert!(
        runtime
            .snapshot()
            .await
            .interactions
            .iter()
            .all(|interaction| interaction.id != interaction_id),
        "cancelled idle approval must be removed from the browser projection",
    );
}

/// A browser approval is a side-effect boundary. The response must reach
/// durable storage before it reaches the tool loop; otherwise a storage fault
/// could authorize an unresumable command after the browser was told to retry.
#[tokio::test(flavor = "multi_thread")]
async fn unpersistable_approval_response_does_not_release_the_tool_loop() {
    let workdir = tempfile::tempdir().expect("tempdir for headless workdir");
    let storage = tempfile::tempdir().expect("tempdir for session storage");
    let store = Arc::new(SessionStore::from_storage_path(storage.path()));
    let state = SessionState::new(&workdir.path().to_string_lossy());
    let session_id = state.id.clone();
    let persistence = HeadlessSessionPersistence::new(store.clone(), state);
    let (runtime, _, exec_approval_tx) = build_runtime_with_persistence(
        "basic_chat",
        workdir.path().to_path_buf(),
        Vec::new(),
        Some(persistence),
    )
    .await;

    let interaction_id = "persisted-exec-approval".to_string();
    let (response_tx, response_rx) = tokio::sync::oneshot::channel();
    exec_approval_tx
        .send(ExecApprovalRequest {
            call_id: interaction_id.clone(),
            command: "cargo check".to_string(),
            cwd: workdir.path().to_path_buf(),
            reason: Some("verify durable approval ordering".to_string()),
            is_retry: false,
            sandbox_label: "workspace-write".to_string(),
            network_access: NetworkAccess::Denied,
            writable_roots: Vec::new(),
            cache_disabled_reason: None,
            response_tx,
        })
        .expect("exec approval request should enqueue in runtime");

    let deadline = std::time::Instant::now() + Duration::from_secs(3);
    while std::time::Instant::now() < deadline {
        if runtime
            .snapshot()
            .await
            .interactions
            .iter()
            .any(|interaction| {
                interaction.id == interaction_id
                    && interaction.status == CodeUiInteractionStatus::Pending
            })
        {
            break;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    assert!(
        runtime
            .snapshot()
            .await
            .interactions
            .iter()
            .any(|interaction| {
                interaction.id == interaction_id
                    && interaction.status == CodeUiInteractionStatus::Pending
            }),
        "the approval must be visible before its persistence failure is induced",
    );

    let events_path = store.session_root(&session_id).join("events.jsonl");
    std::fs::remove_file(&events_path)
        .expect("remove the durable event file after request persistence");
    std::fs::create_dir(&events_path)
        .expect("replace the durable event file with a directory to force append failure");

    let error = runtime
        .respond_interaction(
            &interaction_id,
            CodeUiInteractionResponse {
                selected_option: Some("approve".to_string()),
                ..Default::default()
            },
        )
        .await
        .expect_err("an approval response with no durable record must be rejected");
    assert!(
        error.to_string().contains("no tool action was started"),
        "the error must make the no-dispatch guarantee explicit: {error:#}",
    );
    assert_eq!(
        runtime.snapshot().await.status,
        CodeUiSessionStatus::IndeterminateSideEffect,
        "the live session must require reconciliation after a failed approval write",
    );
    match tokio::time::timeout(Duration::from_millis(200), response_rx).await {
        Ok(Err(_)) => {}
        Ok(Ok(decision)) => {
            panic!("the tool loop received an approval despite failed persistence: {decision:?}")
        }
        Err(_) => panic!(
            "the response sender should close rather than leave the tool loop awaiting an unsafe approval"
        ),
    }
    let submit_error = runtime
        .submit_message("blocked after failed approval persistence".to_string())
        .await
        .expect_err("indeterminate sessions must reject follow-up turns");
    assert!(
        submit_error.to_string().contains("RECONCILIATION_REQUIRED"),
        "follow-up rejection must expose stable reconciliation code: {submit_error:#}",
    );
}

/// The interaction projection is not itself an audit event. A resolved
/// approval must add a durable, non-sensitive workflow record before the
/// original tool-loop continuation receives the decision.
#[tokio::test(flavor = "multi_thread")]
async fn persisted_approval_response_records_durable_interaction_audit_event() {
    let workdir = tempfile::tempdir().expect("tempdir for headless workdir");
    let storage = tempfile::tempdir().expect("tempdir for session storage");
    let store = Arc::new(SessionStore::from_storage_path(storage.path()));
    let state = SessionState::new(&workdir.path().to_string_lossy());
    let session_id = state.id.clone();
    let persistence = HeadlessSessionPersistence::new(store.clone(), state);
    let (runtime, _, exec_approval_tx) = build_runtime_with_persistence(
        "basic_chat",
        workdir.path().to_path_buf(),
        Vec::new(),
        Some(persistence),
    )
    .await;

    let interaction_id = "durable-exec-approval".to_string();
    let (response_tx, response_rx) = tokio::sync::oneshot::channel();
    exec_approval_tx
        .send(ExecApprovalRequest {
            call_id: interaction_id.clone(),
            command: "cargo check".to_string(),
            cwd: workdir.path().to_path_buf(),
            reason: Some("verify durable interaction audit".to_string()),
            is_retry: false,
            sandbox_label: "workspace-write".to_string(),
            network_access: NetworkAccess::Denied,
            writable_roots: Vec::new(),
            cache_disabled_reason: None,
            response_tx,
        })
        .expect("exec approval request should enqueue in runtime");

    let deadline = std::time::Instant::now() + Duration::from_secs(3);
    while std::time::Instant::now() < deadline {
        if runtime
            .snapshot()
            .await
            .interactions
            .iter()
            .any(|interaction| {
                interaction.id == interaction_id
                    && interaction.status == CodeUiInteractionStatus::Pending
            })
        {
            break;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }

    runtime
        .respond_interaction(
            &interaction_id,
            CodeUiInteractionResponse {
                selected_option: Some("approve".to_string()),
                ..Default::default()
            },
        )
        .await
        .expect("persisted approval response should be delivered");
    response_rx
        .await
        .expect("approval continuation receives the durable decision");

    let replay = SessionJsonlStore::new(store.session_root(&session_id))
        .load_code_workflow_replay()
        .expect("durable interaction audit event can be replayed");
    assert!(
        replay.events.iter().any(|event| matches!(
            &event.event,
            CodeWorkflowEventKind::InteractionResolved {
                interaction_id: event_interaction_id,
                resolution,
            } if event_interaction_id == &interaction_id && resolution == "approved"
        )),
        "approval delivery must have a durable interaction-resolution audit event",
    );
}

/// When an interaction belongs to a live worker turn, a failed durable
/// response must keep the reconciliation state through cancellation and the
/// original executor's eventual exit. Otherwise a late cancellation result
/// could make an unsafe session appear ready for a new browser command.
#[tokio::test(flavor = "multi_thread")]
async fn live_unpersistable_approval_preserves_indeterminate_state_after_executor_exit() {
    let workdir = tempfile::tempdir().expect("tempdir for headless workdir");
    let storage = tempfile::tempdir().expect("tempdir for session storage");
    let store = Arc::new(SessionStore::from_storage_path(storage.path()));
    let state = SessionState::new(&workdir.path().to_string_lossy());
    let session_id = state.id.clone();
    let persistence = HeadlessSessionPersistence::new(store.clone(), state);
    let (runtime, _, exec_approval_tx) = build_runtime_with_persistence(
        "brief_delayed_chat",
        workdir.path().to_path_buf(),
        Vec::new(),
        Some(persistence),
    )
    .await;

    runtime
        .submit_message("slow interaction persistence failure".to_string())
        .await
        .expect("delayed turn starts");
    let deadline = std::time::Instant::now() + Duration::from_secs(3);
    while std::time::Instant::now() < deadline {
        if matches!(
            runtime.runtime_snapshot().await.expect("worker snapshot"),
            libra::internal::ai::runtime::AgentSnapshot {
                interaction: InteractionState::Running,
                ..
            }
        ) {
            break;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }

    let interaction_id = "live-unpersistable-exec-approval".to_string();
    let (response_tx, response_rx) = tokio::sync::oneshot::channel();
    exec_approval_tx
        .send(ExecApprovalRequest {
            call_id: interaction_id.clone(),
            command: "cargo check".to_string(),
            cwd: workdir.path().to_path_buf(),
            reason: Some("verify active interaction persistence failure".to_string()),
            is_retry: false,
            sandbox_label: "workspace-write".to_string(),
            network_access: NetworkAccess::Denied,
            writable_roots: Vec::new(),
            cache_disabled_reason: None,
            response_tx,
        })
        .expect("exec approval request should enqueue in runtime");

    let deadline = std::time::Instant::now() + Duration::from_secs(3);
    while std::time::Instant::now() < deadline {
        if runtime
            .runtime_snapshot()
            .await
            .expect("worker snapshot")
            .interaction
            == (InteractionState::AwaitingToolApproval {
                interaction_id: interaction_id.clone(),
                tool_name: "shell".to_string(),
            })
        {
            break;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    assert_eq!(
        runtime
            .runtime_snapshot()
            .await
            .expect("worker snapshot")
            .interaction,
        InteractionState::AwaitingToolApproval {
            interaction_id: interaction_id.clone(),
            tool_name: "shell".to_string(),
        },
        "the live approval must be registered before its persistence failure is induced",
    );

    let events_path = store.session_root(&session_id).join("events.jsonl");
    std::fs::remove_file(&events_path)
        .expect("remove the durable event file after interaction persistence");
    std::fs::create_dir(&events_path)
        .expect("replace the durable event file with a directory to force append failure");

    let error = runtime
        .respond_interaction(
            &interaction_id,
            CodeUiInteractionResponse {
                selected_option: Some("approve".to_string()),
                ..Default::default()
            },
        )
        .await
        .expect_err("an unpersistable live approval response must be rejected");
    assert!(
        error.to_string().contains("no tool action was started"),
        "the browser must receive the no-dispatch guarantee: {error:#}",
    );
    assert!(
        matches!(
            tokio::time::timeout(Duration::from_millis(200), response_rx).await,
            Ok(Err(_))
        ),
        "the original tool loop must never receive an unpersisted approval",
    );

    let deadline = std::time::Instant::now() + Duration::from_secs(3);
    while std::time::Instant::now() < deadline {
        if runtime
            .runtime_snapshot()
            .await
            .expect("worker snapshot")
            .active_turn_id
            .is_none()
        {
            break;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    assert!(
        runtime
            .runtime_snapshot()
            .await
            .expect("worker snapshot")
            .active_turn_id
            .is_none(),
        "the worker must retain the failed response turn until the original executor exits",
    );
    // The brief fixture would complete normally after one second if the
    // worker had released the turn without cancelling its original executor.
    // Check after that deadline to prove the late terminal result cannot
    // overwrite the reconciliation state.
    tokio::time::sleep(Duration::from_millis(1_300)).await;
    assert_eq!(
        runtime.snapshot().await.status,
        CodeUiSessionStatus::IndeterminateSideEffect,
        "the late executor result must not overwrite the interaction persistence reconciliation state",
    );
}
