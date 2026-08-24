//! Headless web-only runtime smoke tests.
//!
//! Exercises [`HeadlessCodeRuntime`] lifecycle + the mounted
//! [`AgentRuntimeCodeUiAdapter`] write path against the deterministic
//! `test-provider` fixture. Explicit direct-chat turns use a leading `/`
//! (W3-03); plain messages default to Phase 0 plan routing.

#![cfg(feature = "test-provider")]

use std::{
    io::Write,
    path::{Path, PathBuf},
    sync::Arc,
    time::Duration,
};

use libra::{
    internal::{
        ai::{
            agent::runtime::{RuntimeUsageService, tool_loop::ToolLoopConfig},
            completion::{
                CompletionError, CompletionModel, CompletionRequest, CompletionResponse,
                CompletionStreamEvent, Message,
            },
            intentspec::{
                ResolveContext,
                draft::{DraftAcceptance, DraftIntent, DraftRisk, IntentDraft},
                resolve_intentspec,
                types::{ChangeType, Objective, ObjectiveKind, RiskLevel},
            },
            orchestrator::types::ExecutionPlanSpec,
            providers::fake,
            runtime::{
                InteractionState, RuntimeWorkerError, ToolBoundaryRuntime, TracingAuditSink,
                phase0::open_intent_review_from_workflow,
                phase1::{
                    Phase1CheckoutBinding, Phase1PersistedPlan, Phase1ReviewContext,
                    Phase1StartSeed, load_phase1_review_context, load_phase1_start_seed,
                    open_network_policy_from_workflow, open_plan_review_from_workflow,
                    pending_plan_revision_from_workflow, persist_phase1_review_context,
                    persist_phase1_start_seed, phase1_review_context_path,
                    phase1_turn_id_from_seed,
                },
            },
            sandbox::{ExecApprovalRequest, NetworkAccess},
            session::{
                INTENT_REVISION_CONSUMPTION_SCHEMA_VERSION, IntentRevisionConsumption,
                IntentRevisionConsumptionClaim, IntentRevisionRecovery,
                MAX_INTENT_REVISION_NOTE_BYTES, SessionState, SessionStore,
                jsonl::{
                    CodeCommandAdmission, CodeCommandIdentity, CodeCommandIntent,
                    CodeCommandStatus, CodeCommandStoreError, CodeWorkflowEvent,
                    CodeWorkflowEventKind, Phase1RetryIntentReview, SessionEvent,
                    SessionJsonlStore,
                },
            },
            tools::{
                ToolHandler, ToolInvocation, ToolKind, ToolOutput, ToolRegistry,
                ToolRegistryBuilder, ToolResult, ToolSpec,
                context::{
                    PlanDraftStep, SubmitPlanDraftArgs, UserInputQuestion, UserInputRequest,
                    UserInputResponse,
                },
                handlers::{
                    PlanHandler, ReadFileHandler, RequestUserInputHandler,
                    SubmitIntentDraftHandler, SubmitPlanDraftHandler,
                },
            },
            usage::{UsageContext, UsageQueryFilter, UsageRecorder},
            web::{
                code_ui::{
                    CodeUiApiError, CodeUiApplyToFuture, CodeUiCommandAdapter,
                    CodeUiInteractionKind, CodeUiInteractionResponse, CodeUiInteractionStatus,
                    CodeUiProviderInfo, CodeUiReadModel, CodeUiSession, CodeUiSessionSnapshot,
                    CodeUiSessionStatus, CodeUiToolCallSnapshot, CodeUiTranscriptEntry,
                    CodeUiTranscriptEntryKind, initial_snapshot,
                },
                headless::{
                    HeadlessCodeRuntime, HeadlessRecordUserMessageHook, HeadlessSessionPersistence,
                    headless_capabilities,
                },
                sse_wire::CodeUiWireV2Event,
                web_admission::CODE_UI_WEB_TURN_KIND,
            },
        },
        db::migration::run_builtin_migrations,
    },
    utils::test,
};
use sea_orm::{ConnectionTrait, Database, Statement};
use tokio::sync::{Notify, mpsc};
use uuid::Uuid;

fn fixture_path(name: &str) -> PathBuf {
    let mut path = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    path.push("tests/fixtures/code_ui");
    path.push(format!("{name}.json"));
    path
}

fn assert_code_ui_api_error<'a>(
    error: &'a anyhow::Error,
    expected_status: u16,
    expected_code: &str,
) -> &'a CodeUiApiError {
    let error = error.downcast_ref::<CodeUiApiError>().unwrap_or_else(|| {
        panic!("interaction failure must preserve the typed Code UI API contract: {error:#}")
    });
    assert_eq!(error.status, expected_status);
    assert_eq!(error.code, expected_code);
    error
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
    let (user_input_tx, user_input_rx) = mpsc::unbounded_channel::<UserInputRequest>();
    let (exec_approval_tx, exec_approval_rx) = mpsc::unbounded_channel::<ExecApprovalRequest>();
    let registry = Arc::new(
        ToolRegistryBuilder::with_working_dir(working_dir.clone())
            .hardening(ToolBoundaryRuntime::system(
                Uuid::new_v4(),
                Arc::new(TracingAuditSink),
            ))
            .register("read_file", Arc::new(ReadFileHandler))
            .register("update_plan", Arc::new(PlanHandler))
            .register("submit_plan_draft", Arc::new(SubmitPlanDraftHandler))
            .register("submit_intent_draft", Arc::new(SubmitIntentDraftHandler))
            .register(
                "request_user_input",
                Arc::new(RequestUserInputHandler::new(user_input_tx.clone())),
            )
            .build(),
    );
    build_runtime_with_registry_channels(
        fixture,
        working_dir,
        initial_history,
        persistence,
        registry,
        Arc::new(ToolLoopConfig::default),
        None,
        user_input_tx,
        user_input_rx,
        exec_approval_tx,
        exec_approval_rx,
    )
    .await
}

async fn try_build_runtime_with_persistence(
    fixture: &str,
    working_dir: PathBuf,
    persistence: HeadlessSessionPersistence,
) -> anyhow::Result<Arc<HeadlessCodeRuntime<fake::CompletionModel>>> {
    let (user_input_tx, user_input_rx) = mpsc::unbounded_channel::<UserInputRequest>();
    let (_exec_approval_tx, exec_approval_rx) = mpsc::unbounded_channel::<ExecApprovalRequest>();
    let registry = Arc::new(
        ToolRegistryBuilder::with_working_dir(working_dir.clone())
            .hardening(ToolBoundaryRuntime::system(
                Uuid::new_v4(),
                Arc::new(TracingAuditSink),
            ))
            .register("read_file", Arc::new(ReadFileHandler))
            .register("update_plan", Arc::new(PlanHandler))
            .register("submit_plan_draft", Arc::new(SubmitPlanDraftHandler))
            .register("submit_intent_draft", Arc::new(SubmitIntentDraftHandler))
            .register(
                "request_user_input",
                Arc::new(RequestUserInputHandler::new(user_input_tx)),
            )
            .build(),
    );
    let capabilities = headless_capabilities();
    let provider = CodeUiProviderInfo {
        provider: "fake".to_string(),
        model: Some("fake".to_string()),
        mode: Some("web-headless".to_string()),
        managed: false,
    };
    let session = CodeUiSession::new(initial_snapshot(
        working_dir.to_string_lossy().into_owned(),
        provider,
        capabilities.clone(),
    ));
    let fake_client = fake::Client::from_fixture_path(&fixture_path(fixture))?;
    let model = fake_client.completion_model("fake");
    HeadlessCodeRuntime::new_with_persistence(
        session,
        capabilities,
        model,
        registry,
        user_input_rx,
        exec_approval_rx,
        Arc::new(ToolLoopConfig::default),
        Vec::new(),
        Some(persistence),
        None,
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
    let (user_input_tx, user_input_rx) = mpsc::unbounded_channel::<UserInputRequest>();
    let (exec_approval_tx, exec_approval_rx) = mpsc::unbounded_channel::<ExecApprovalRequest>();
    build_runtime_with_registry_channels(
        fixture,
        working_dir,
        initial_history,
        persistence,
        registry,
        config_factory,
        shutdown_timeout,
        user_input_tx,
        user_input_rx,
        exec_approval_tx,
        exec_approval_rx,
    )
    .await
}

#[allow(clippy::too_many_arguments)]
async fn build_runtime_with_registry_channels(
    fixture: &str,
    working_dir: PathBuf,
    initial_history: Vec<Message>,
    persistence: Option<HeadlessSessionPersistence>,
    registry: Arc<ToolRegistry>,
    config_factory: Arc<dyn Fn() -> ToolLoopConfig + Send + Sync>,
    shutdown_timeout: Option<Duration>,
    user_input_tx: mpsc::UnboundedSender<UserInputRequest>,
    user_input_rx: mpsc::UnboundedReceiver<UserInputRequest>,
    exec_approval_tx: mpsc::UnboundedSender<ExecApprovalRequest>,
    exec_approval_rx: mpsc::UnboundedReceiver<ExecApprovalRequest>,
) -> (
    Arc<HeadlessCodeRuntime<fake::CompletionModel>>,
    mpsc::UnboundedSender<UserInputRequest>,
    mpsc::UnboundedSender<ExecApprovalRequest>,
) {
    let fixture_path = fixture_path(fixture);
    build_runtime_with_registry_channels_from_path(
        &fixture_path,
        working_dir,
        initial_history,
        persistence,
        registry,
        config_factory,
        shutdown_timeout,
        user_input_tx,
        user_input_rx,
        exec_approval_tx,
        exec_approval_rx,
    )
    .await
}

#[allow(clippy::too_many_arguments)]
async fn build_runtime_with_registry_channels_from_path(
    fixture_path: &Path,
    working_dir: PathBuf,
    initial_history: Vec<Message>,
    persistence: Option<HeadlessSessionPersistence>,
    registry: Arc<ToolRegistry>,
    config_factory: Arc<dyn Fn() -> ToolLoopConfig + Send + Sync>,
    shutdown_timeout: Option<Duration>,
    user_input_tx: mpsc::UnboundedSender<UserInputRequest>,
    user_input_rx: mpsc::UnboundedReceiver<UserInputRequest>,
    exec_approval_tx: mpsc::UnboundedSender<ExecApprovalRequest>,
    exec_approval_rx: mpsc::UnboundedReceiver<ExecApprovalRequest>,
) -> (
    Arc<HeadlessCodeRuntime<fake::CompletionModel>>,
    mpsc::UnboundedSender<UserInputRequest>,
    mpsc::UnboundedSender<ExecApprovalRequest>,
) {
    let fake_client =
        fake::Client::from_fixture_path(fixture_path).expect("fake provider fixture must load");
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
            None,
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
            None,
        )
        .await
        .expect("test registry must retain the shared tool boundary"),
    };

    (runtime, user_input_tx, exec_approval_tx)
}

/// Wait until the headless worker reports an active Running turn. Injection of
/// user-input / exec-approval without this fails closed after W2-05.
async fn await_worker_running(runtime: &HeadlessCodeRuntime<fake::CompletionModel>) {
    let deadline = std::time::Instant::now() + Duration::from_secs(3);
    while std::time::Instant::now() < deadline {
        if matches!(
            runtime.runtime_snapshot().await.expect("worker snapshot"),
            libra::internal::ai::runtime::AgentSnapshot {
                interaction: InteractionState::Running,
                ..
            }
        ) {
            return;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    panic!("worker did not reach InteractionState::Running within deadline");
}

async fn await_pending_interaction(
    runtime: &HeadlessCodeRuntime<fake::CompletionModel>,
    kind: CodeUiInteractionKind,
    message: &str,
) -> String {
    let deadline = std::time::Instant::now() + Duration::from_secs(5);
    while std::time::Instant::now() < deadline {
        let snapshot = runtime.snapshot().await;
        if let Some(interaction) = snapshot.interactions.iter().find(|interaction| {
            interaction.kind == kind && interaction.status == CodeUiInteractionStatus::Pending
        }) {
            return interaction.id.clone();
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    panic!("{message}");
}

struct InteractionCheckpointFixture {
    _workdir: tempfile::TempDir,
    _storage: tempfile::TempDir,
    runtime: Arc<HeadlessCodeRuntime<fake::CompletionModel>>,
    user_input_tx: mpsc::UnboundedSender<UserInputRequest>,
    exec_approval_tx: mpsc::UnboundedSender<ExecApprovalRequest>,
    goal_store: SessionJsonlStore,
    command_id: String,
}

async fn interaction_checkpoint_fixture(
    hook: HeadlessRecordUserMessageHook,
    command_suffix: &str,
) -> InteractionCheckpointFixture {
    let workdir = tempfile::tempdir().expect("tempdir for checkpoint-ordering workdir");
    let storage = tempfile::tempdir().expect("tempdir for checkpoint-ordering session");
    let store = Arc::new(SessionStore::from_storage_path(storage.path()));
    let mut state = SessionState::new(&workdir.path().to_string_lossy());
    let session_id = state.id.clone();
    state
        .metadata
        .insert("thread_id".to_string(), serde_json::json!(session_id));
    let persistence = HeadlessSessionPersistence::new(store.clone(), state)
        .expect("attach checkpoint-ordering persistence")
        .with_interaction_checkpoint_hook(hook);
    let goal_store = persistence.goal_event_store();
    let (runtime, user_input_tx, exec_approval_tx) = build_runtime_with_persistence(
        "delayed_chat",
        workdir.path().to_path_buf(),
        Vec::new(),
        Some(persistence),
    )
    .await;
    let command_id = format!("checkpoint-{command_suffix}-{}", Uuid::new_v4());
    runtime
        .submit_message_with_command_id(
            format!("/slow checkpoint ordering {command_suffix}"),
            Some(command_id.clone()),
        )
        .await
        .expect("checkpoint-ordering turn admits");
    await_worker_running(&runtime).await;

    InteractionCheckpointFixture {
        _workdir: workdir,
        _storage: storage,
        runtime,
        user_input_tx,
        exec_approval_tx,
        goal_store,
        command_id,
    }
}

struct Phase1StartupFixture {
    workdir: tempfile::TempDir,
    _storage: tempfile::TempDir,
    store: Arc<SessionStore>,
    session_id: String,
    persistence: HeadlessSessionPersistence,
    goal_store: SessionJsonlStore,
    seed: Phase1StartSeed,
    command: CodeCommandIntent,
}

async fn initialize_phase1_test_checkout(workdir: &Path) -> PathBuf {
    let canonical_workdir =
        std::fs::canonicalize(workdir).expect("canonicalize Phase 1 test checkout");
    test::setup_with_new_libra_in(&canonical_workdir).await;

    // Phase 1 checkout capture requires a born HEAD. The recovery tests do not
    // read the object, but they do exercise the real repo/HEAD identity checks,
    // so seed a syntactically valid object id rather than weakening production.
    let db_path = canonical_workdir.join(".libra/libra.db");
    let db = libra::internal::db::establish_connection(
        db_path.to_str().expect("UTF-8 Phase 1 test database path"),
    )
    .await
    .expect("open Phase 1 test repository database");
    db.execute_raw(Statement::from_string(
        db.get_database_backend(),
        format!(
            "INSERT INTO reference (name, kind, \"commit\", remote, worktree_id) \
             SELECT name, 'Branch', '{}', NULL, NULL \
             FROM reference \
             WHERE kind = 'Head' AND remote IS NULL AND worktree_id IS NULL \
             ON CONFLICT(name, kind) WHERE remote IS NULL \
             DO UPDATE SET \"commit\" = excluded.\"commit\"",
            "0".repeat(40)
        ),
    ))
    .await
    .expect("seed a born HEAD for Phase 1 checkout capture");
    drop(db);
    canonical_workdir
}

async fn phase1_startup_fixture(
    source_interaction_id: &str,
    command_id: &str,
    real_checkout: bool,
) -> Phase1StartupFixture {
    let workdir = tempfile::tempdir().expect("tempdir for Phase 1 checkout");
    let canonical_workdir = if real_checkout {
        initialize_phase1_test_checkout(workdir.path()).await
    } else {
        std::fs::canonicalize(workdir.path()).expect("canonicalize Phase 1 test checkout")
    };
    let intent_spec = resolve_intentspec(
        IntentDraft {
            intent: DraftIntent {
                summary: "Recover Phase 1 generation".to_string(),
                problem_statement: "A crash interrupted a durable Phase 1 start".to_string(),
                change_type: ChangeType::Test,
                objectives: vec![Objective {
                    title: "Restore the execution-plan review gate".to_string(),
                    kind: ObjectiveKind::Implementation,
                }],
                in_scope: vec!["README.md".to_string()],
                out_of_scope: vec![],
                touch_hints: None,
            },
            acceptance: DraftAcceptance {
                success_criteria: vec!["The plan review is restored exactly once".to_string()],
                fast_checks: vec![],
                integration_checks: vec![],
                security_checks: vec![],
                release_checks: vec![],
            },
            risk: DraftRisk {
                rationale: "durability regression fixture".to_string(),
                factors: vec![],
                level: Some(RiskLevel::Low),
            },
        },
        RiskLevel::Low,
        ResolveContext {
            working_dir: canonical_workdir.to_string_lossy().into_owned(),
            base_ref: "HEAD".to_string(),
            created_by_id: "ai-code-ui-headless-test".to_string(),
        },
    );
    let checkout = if real_checkout {
        Phase1CheckoutBinding::capture(&canonical_workdir, &intent_spec)
            .await
            .expect("capture the exact Phase 1 checkout binding")
    } else {
        Phase1CheckoutBinding {
            canonical_working_dir: canonical_workdir.to_string_lossy().into_owned(),
            repo_id: "phase1-startup-repo".to_string(),
            repo_locator: intent_spec.metadata.target.repo.locator.clone(),
            base_ref: intent_spec.metadata.target.base_ref.clone(),
            workspace_fingerprint: "0".repeat(64),
            workspace_change_token: String::new(),
            head_oid: Some("0".repeat(40)),
            branch_label: "main".to_string(),
            worktree_id: None,
        }
    };
    let seed = Phase1StartSeed {
        schema_version: Phase1StartSeed::SCHEMA_VERSION,
        source_interaction_id: source_interaction_id.to_string(),
        intent_id: "phase1-startup-intent".to_string(),
        intent_spec_id: intent_spec.metadata.id.clone(),
        intent_spec_json: serde_json::to_string(&intent_spec)
            .expect("serialize Phase 1 startup IntentSpec"),
        source_resolution: "confirm".to_string(),
        revision_note: None,
        checkout,
        prior_plan: None,
        prior_plan_id: None,
        prior_persisted_plan: Phase1PersistedPlan::Unavailable,
        browser_command_id: Some(command_id.to_string()),
        attempt_id: format!("attempt-{command_id}"),
    };

    let storage = tempfile::tempdir().expect("tempdir for Phase 1 session storage");
    let store = Arc::new(SessionStore::from_storage_path(storage.path()));
    let mut state = SessionState::new(&canonical_workdir.to_string_lossy());
    let session_id = state.id.clone();
    state.metadata.insert(
        "thread_id".to_string(),
        serde_json::json!(session_id.clone()),
    );
    store
        .save(&state)
        .expect("persist replayable Phase 1 startup session snapshot");
    let persistence = HeadlessSessionPersistence::new(store.clone(), state)
        .expect("attach Phase 1 startup workflow hub");
    let goal_store = persistence.goal_event_store();
    let intents_dir = goal_store.session_root().join("intents");
    std::fs::create_dir_all(&intents_dir)
        .expect("create durable IntentSpec directory for Phase 1 startup recovery");
    libra::utils::atomic_write::write_atomic(
        &intents_dir.join(format!("{}.json", seed.intent_id)),
        &serde_json::to_vec_pretty(&intent_spec)
            .expect("serialize durable Phase 1 startup IntentSpec"),
        true,
    )
    .expect("persist durable IntentSpec for Phase 1 startup recovery");
    goal_store
        .append_code_workflow_durable(CodeWorkflowEventKind::InteractionResolved {
            interaction_id: source_interaction_id.to_string(),
            resolution: seed.source_resolution.clone(),
            command: None,
            prior_interaction_resolutions: Vec::new(),
            intent_revision_consumption: None,
        })
        .expect("persist the Phase 1 source resolution");
    persist_phase1_start_seed(&goal_store, &seed).expect("persist the Phase 1 crash seed");

    let (_, repo_id, principal_id) = persistence.worker_durability_config();
    use sha2::Digest as _;
    let command = CodeCommandIntent::new(
        CodeCommandIdentity::new(repo_id, &session_id, principal_id, command_id),
        CODE_UI_WEB_TURN_KIND,
        format!(
            "sha256:{}",
            hex::encode(sha2::Sha256::digest(b"Phase 1 plan generation"))
        ),
        true,
    );
    assert!(matches!(
        goal_store
            .admit_code_command(command.clone())
            .expect("persist the interrupted Phase 1 command intent"),
        CodeCommandAdmission::Execute { .. }
    ));

    Phase1StartupFixture {
        workdir,
        _storage: storage,
        store,
        session_id,
        persistence,
        goal_store,
        seed,
        command,
    }
}

#[test]
fn headless_session_writer_lease_rejects_second_attach_and_reacquires_after_drop() {
    let storage = tempfile::tempdir().expect("tempdir for session storage");
    let first_store = Arc::new(SessionStore::from_storage_path(storage.path()));
    let second_store = Arc::new(SessionStore::from_storage_path(storage.path()));
    let state = SessionState::new("/repo/main");
    let lock_path = first_store
        .session_root(&state.id)
        .join("phase1-writer.lock");

    let first = HeadlessSessionPersistence::new(first_store, state.clone())
        .expect("first writable session attach acquires the lease");
    let opened_metadata = std::fs::metadata(&lock_path).expect("writer lease is a durable file");
    let error = match HeadlessSessionPersistence::new(second_store, state.clone()) {
        Ok(_) => panic!("a second writable attach to the same session must fail closed"),
        Err(error) => error,
    };
    assert_eq!(error.kind(), std::io::ErrorKind::WouldBlock);
    assert!(
        error.to_string().contains(&state.id),
        "lease conflict must identify the session so the operator can close the owner: {error}"
    );

    drop(first);
    let released_metadata = std::fs::metadata(&lock_path)
        .expect("releasing the lease must preserve its durable lock file");
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt as _;

        assert_eq!(
            (opened_metadata.dev(), opened_metadata.ino()),
            (released_metadata.dev(), released_metadata.ino()),
            "lease release must not unlink and recreate the lock inode"
        );
    }
    #[cfg(not(unix))]
    let _ = (opened_metadata, released_metadata);
    let reacquired_store = Arc::new(SessionStore::from_storage_path(storage.path()));
    HeadlessSessionPersistence::new(reacquired_store, state)
        .expect("dropping the first persistence guard must release the lease immediately");
}

#[test]
fn headless_session_writer_lease_cannot_be_rebound_to_another_session() {
    let storage = tempfile::tempdir().expect("tempdir for session lease binding");
    let store = Arc::new(SessionStore::from_storage_path(storage.path()));
    let leased_state = SessionState::new("/repo/leased");
    let other_state = SessionState::new("/repo/other");
    let lease = HeadlessSessionPersistence::acquire_session_lease(&store, &leased_state.id)
        .expect("acquire the first session's writer lease");

    let error = match HeadlessSessionPersistence::with_projection_checkpoint_and_lease(
        store,
        other_state.clone(),
        CodeUiSessionSnapshot::default(),
        0,
        lease,
    ) {
        Ok(_) => panic!("a writer lease must not authorize a different session"),
        Err(error) => error,
    };
    assert_eq!(error.kind(), std::io::ErrorKind::InvalidInput);
    assert!(
        error.to_string().contains(&leased_state.id) && error.to_string().contains(&other_state.id),
        "binding failure must identify both session authorities: {error}"
    );
}

#[test]
fn headless_session_writer_lease_clone_cannot_attach_a_second_persistence() {
    let storage = tempfile::tempdir().expect("tempdir for cloned session lease claim");
    let first_store = Arc::new(SessionStore::from_storage_path(storage.path()));
    let second_store = Arc::new(SessionStore::from_storage_path(storage.path()));
    let state = SessionState::new("/repo/cloned-lease");
    let lease = HeadlessSessionPersistence::acquire_session_lease(&first_store, &state.id)
        .expect("acquire the session writer lease");
    let duplicate = lease.clone();
    let first = HeadlessSessionPersistence::with_projection_checkpoint_and_lease(
        first_store,
        state.clone(),
        CodeUiSessionSnapshot::default(),
        0,
        lease,
    )
    .expect("the first lease clone claims the persistence authority");

    let error = match HeadlessSessionPersistence::with_projection_checkpoint_and_lease(
        second_store,
        state,
        CodeUiSessionSnapshot::default(),
        0,
        duplicate,
    ) {
        Ok(_) => panic!("a cloned acquisition token must not build a second persistence graph"),
        Err(error) => error,
    };
    assert_eq!(error.kind(), std::io::ErrorKind::AlreadyExists);
    assert!(
        error.to_string().contains("already authorized"),
        "duplicate claim failure must explain that the token was consumed: {error}"
    );
    drop(first);
}

#[cfg(unix)]
#[test]
fn headless_session_writer_lease_rejects_symlink_without_touching_target() {
    use std::os::unix::fs::symlink;

    let storage = tempfile::tempdir().expect("tempdir for symlinked session writer lease");
    let store = SessionStore::from_storage_path(storage.path());
    let session_id = "symlinked-session-writer";
    let session_root = store.session_root(session_id);
    std::fs::create_dir_all(&session_root).expect("create session root");
    let target = storage.path().join("unrelated-lock-target");
    std::fs::write(&target, b"must remain untouched").expect("seed unrelated target");
    symlink(&target, session_root.join("phase1-writer.lock"))
        .expect("plant symlinked writer lease");

    let error = match HeadlessSessionPersistence::acquire_session_lease(&store, session_id) {
        Ok(_) => panic!("a symlinked writer lease must fail closed"),
        Err(error) => error,
    };
    assert_eq!(error.kind(), std::io::ErrorKind::InvalidData);
    assert_eq!(
        std::fs::read(&target).expect("read unrelated target"),
        b"must remain untouched",
        "lease acquisition must neither lock through nor rewrite the symlink target"
    );
}

#[cfg(unix)]
#[test]
fn headless_session_writer_lease_rejects_replaced_lock_inode_before_attach() {
    let storage = tempfile::tempdir().expect("tempdir for replaced session writer lease");
    let store = Arc::new(SessionStore::from_storage_path(storage.path()));
    let state = SessionState::new("/repo/replaced-lock");
    let lease = HeadlessSessionPersistence::acquire_session_lease(&store, &state.id)
        .expect("open the original writer lease inode");
    let lock_path = store.session_root(&state.id).join("phase1-writer.lock");
    std::fs::remove_file(&lock_path).expect("unlink the original lock path fixture");
    std::fs::write(&lock_path, b"replacement").expect("install replacement lock inode fixture");

    let error = match HeadlessSessionPersistence::with_projection_checkpoint_and_lease(
        store,
        state,
        CodeUiSessionSnapshot::default(),
        0,
        lease,
    ) {
        Ok(_) => panic!("a lease whose path now names another inode must fail closed"),
        Err(error) => error,
    };
    assert_eq!(error.kind(), std::io::ErrorKind::InvalidData);
    assert!(
        error.to_string().contains("replaced after it was opened"),
        "identity failure must tell the operator to repair the lock path: {error}"
    );
}

#[cfg(unix)]
#[test]
fn headless_session_writer_lease_rejects_fifo_without_blocking() {
    use std::{ffi::CString, os::unix::ffi::OsStrExt, time::Instant};

    let storage = tempfile::tempdir().expect("tempdir for FIFO session writer lease");
    let store = SessionStore::from_storage_path(storage.path());
    let session_id = "fifo-session-writer";
    let session_root = store.session_root(session_id);
    std::fs::create_dir_all(&session_root).expect("create session root");
    let lock_path = session_root.join("phase1-writer.lock");
    let lock_path_c = CString::new(lock_path.as_os_str().as_bytes()).expect("FIFO path has no NUL");
    let created = unsafe { libc::mkfifo(lock_path_c.as_ptr(), 0o600) };
    assert_eq!(
        created,
        0,
        "create FIFO writer lease fixture: {}",
        std::io::Error::last_os_error()
    );

    let started = Instant::now();
    let error = match HeadlessSessionPersistence::acquire_session_lease(&store, session_id) {
        Ok(_) => panic!("a FIFO writer lease must fail closed"),
        Err(error) => error,
    };
    assert_eq!(error.kind(), std::io::ErrorKind::InvalidData);
    assert!(
        started.elapsed() < Duration::from_secs(1),
        "special-file rejection must not wait for a FIFO peer"
    );
}

#[cfg(unix)]
#[test]
fn headless_session_writer_lease_child_helper() {
    use std::io::Read;

    let Ok(storage_root) = std::env::var("LIBRA_TEST_PHASE1_LEASE_CHILD_ROOT") else {
        return;
    };
    let session_id =
        std::env::var("LIBRA_TEST_PHASE1_LEASE_CHILD_SESSION").expect("lease child session id");
    let ready_path = std::env::var_os("LIBRA_TEST_PHASE1_LEASE_CHILD_READY")
        .map(PathBuf::from)
        .expect("lease child ready path");
    let store = SessionStore::from_storage_path(std::path::Path::new(&storage_root));
    let _lease = HeadlessSessionPersistence::acquire_session_lease(&store, &session_id)
        .expect("child acquires the durable session writer lease");
    std::fs::write(&ready_path, b"ready")
        .expect("child publishes readiness only after acquiring the lease");

    // The parent keeps this pipe open and terminates us with SIGKILL. Blocking
    // on stdin avoids a timing sleep and models abrupt process loss precisely.
    let mut byte = [0_u8; 1];
    let _ = std::io::stdin().read_exact(&mut byte);
}

#[cfg(unix)]
#[test]
fn headless_session_writer_lease_is_released_immediately_after_sigkill() {
    use std::{
        os::unix::process::ExitStatusExt,
        process::{Child, Command, Stdio},
        time::Instant,
    };

    struct KillChildOnDrop(Option<Child>);

    impl Drop for KillChildOnDrop {
        fn drop(&mut self) {
            if let Some(child) = self.0.as_mut() {
                let _ = child.kill();
                let deadline = Instant::now() + Duration::from_secs(2);
                while Instant::now() < deadline {
                    match child.try_wait() {
                        Ok(Some(_)) | Err(_) => break,
                        Ok(None) => std::thread::sleep(Duration::from_millis(10)),
                    }
                }
            }
        }
    }

    let storage = tempfile::tempdir().expect("tempdir for child-held session lease");
    let ready_path = storage.path().join("lease-child-ready");
    let session_id = "sigkill-session-writer";
    let mut child = Command::new(std::env::current_exe().expect("current test executable"))
        .arg("--exact")
        .arg("headless_session_writer_lease_child_helper")
        .arg("--nocapture")
        .arg("--test-threads=1")
        .env("LIBRA_TEST_PHASE1_LEASE_CHILD_ROOT", storage.path())
        .env("LIBRA_TEST_PHASE1_LEASE_CHILD_SESSION", session_id)
        .env("LIBRA_TEST_PHASE1_LEASE_CHILD_READY", &ready_path)
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .expect("spawn exact lease-holder helper test");
    let child_stdin = child.stdin.take().expect("keep lease child stdin open");
    let mut child = KillChildOnDrop(Some(child));

    let ready_deadline = Instant::now() + Duration::from_secs(10);
    loop {
        if ready_path.exists() {
            break;
        }
        if let Some(status) = child
            .0
            .as_mut()
            .expect("lease child")
            .try_wait()
            .expect("poll lease child during startup")
        {
            panic!("lease child exited before publishing readiness: {status}");
        }
        assert!(
            Instant::now() < ready_deadline,
            "lease child did not acquire the writer lease within 10 seconds"
        );
        std::thread::sleep(Duration::from_millis(10));
    }

    let store = SessionStore::from_storage_path(storage.path());
    let error = match HeadlessSessionPersistence::acquire_session_lease(&store, session_id) {
        Ok(_) => panic!("parent must not acquire a writer lease held by the live child"),
        Err(error) => error,
    };
    assert_eq!(error.kind(), std::io::ErrorKind::WouldBlock);

    child
        .0
        .as_mut()
        .expect("lease child")
        .kill()
        .expect("SIGKILL the lease-holder child");
    let exit_deadline = Instant::now() + Duration::from_secs(5);
    let status = loop {
        if let Some(status) = child
            .0
            .as_mut()
            .expect("lease child")
            .try_wait()
            .expect("poll lease child after SIGKILL")
        {
            break status;
        }
        assert!(
            Instant::now() < exit_deadline,
            "lease-holder child did not exit within 5 seconds after SIGKILL"
        );
        std::thread::sleep(Duration::from_millis(10));
    };
    assert_eq!(status.signal(), Some(9), "child must die from SIGKILL");
    let _ = child.0.take();
    drop(child_stdin);

    HeadlessSessionPersistence::acquire_session_lease(&store, session_id)
        .expect("kernel-released writer lease must be immediately reacquirable after SIGKILL");
}

#[tokio::test(flavor = "multi_thread")]
async fn startup_reattaches_exact_seed_backed_phase1_pending_command_once() {
    let fixture =
        phase1_startup_fixture("source-confirm-reattach", "phase1-reattach-once", true).await;
    let runtime = build_runtime_with_persistence(
        "plan_review",
        fixture.workdir.path().to_path_buf(),
        Vec::new(),
        Some(fixture.persistence.clone()),
    )
    .await
    .0;

    let _plan_gate = await_pending_interaction(
        &runtime,
        CodeUiInteractionKind::PostPlanChoice,
        "the exact seed-backed Pending Phase 1 command did not reattach to one Plan gate",
    )
    .await;
    let replay = fixture
        .goal_store
        .load_code_workflow_replay()
        .expect("read the reattached Phase 1 workflow");
    assert_eq!(
        replay
            .events
            .iter()
            .filter(|event| matches!(
                &event.event,
                CodeWorkflowEventKind::CommandIntentPersisted { command }
                    if command.identity == fixture.command.identity
            ))
            .count(),
        1,
        "reattach must reuse the exact Pending command instead of admitting a second intent"
    );
    assert_eq!(
        replay
            .events
            .iter()
            .filter(|event| matches!(
                &event.event,
                CodeWorkflowEventKind::Phase1FormalWriteStarted { phase1_turn_id, .. }
                    if phase1_turn_id == &fixture.command.identity.command_id
            ))
            .count(),
        1,
        "one recovered start may cross the formal-write boundary only once"
    );
    assert_eq!(
        replay
            .events
            .iter()
            .filter(|event| matches!(
                &event.event,
                CodeWorkflowEventKind::PlanReviewRequested { phase1_turn_id, .. }
                    if phase1_turn_id == &fixture.command.identity.command_id
            ))
            .count(),
        1,
        "one recovered start must publish exactly one Plan review generation"
    );
    assert!(
        !replay.events.iter().any(|event| matches!(
            &event.event,
            CodeWorkflowEventKind::CommandIndeterminateSideEffect { command, .. }
                if command == &fixture.command.identity
        )),
        "the proven pre-write command must not be fenced during reattach"
    );
    assert!(
        load_phase1_start_seed(&fixture.goal_store)
            .expect("load the consumed Phase 1 seed")
            .is_none(),
        "successful reattach must clear its crash seed after the Plan gate is durable"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn startup_restores_embedded_failed_phase1_retry_gate_once_without_provider_rerun() {
    let fixture = phase1_startup_fixture(
        "source-confirm-embedded-retry",
        "phase1-failed-embedded-retry",
        true,
    )
    .await;
    let retry = Phase1RetryIntentReview {
        interaction_id: "intent-review-embedded-retry".to_string(),
        intent_id: fixture.seed.intent_id.clone(),
        intent_spec_id: fixture.seed.intent_spec_id.clone(),
        source_interaction_id: fixture.seed.source_interaction_id.clone(),
        source_resolution: fixture.seed.source_resolution.clone(),
        source_phase1_turn_id: fixture.command.identity.command_id.clone(),
        start_seed_digest: fixture.seed.durable_digest().expect("digest crash seed"),
    };
    fixture
        .goal_store
        .complete_code_command_failure_with_interaction_resolutions_and_retry_intent_review(
            &fixture.command.identity,
            "Phase 1 failed before its formal write",
            &[],
            Some(&retry),
        )
        .expect("persist failed command and replacement gate in one row");
    assert!(
        load_phase1_start_seed(&fixture.goal_store)
            .expect("load crash seed before restart")
            .is_some(),
        "the pre-restart fixture must preserve the seed that authenticates the embedded gate"
    );

    drop(fixture.persistence);
    let restored_state = fixture
        .store
        .load(&fixture.session_id)
        .expect("load session snapshot for embedded-gate restart");
    let persistence = HeadlessSessionPersistence::new(fixture.store.clone(), restored_state)
        .expect("reacquire session writer lease for embedded-gate restart");
    let restored = build_runtime_with_persistence(
        "basic_chat",
        fixture.workdir.path().to_path_buf(),
        Vec::new(),
        Some(persistence),
    )
    .await
    .0;

    let restored_interaction_id = await_pending_interaction(
        &restored,
        CodeUiInteractionKind::IntentReviewChoice,
        "restart did not restore the embedded retry Intent gate",
    )
    .await;
    assert_eq!(restored_interaction_id, retry.interaction_id);
    let snapshot = restored.snapshot().await;
    assert_eq!(
        snapshot
            .interactions
            .iter()
            .filter(|interaction| {
                interaction.kind == CodeUiInteractionKind::IntentReviewChoice
                    && interaction.status == CodeUiInteractionStatus::Pending
            })
            .count(),
        1,
        "restart must restore exactly one retry authority"
    );
    assert!(
        snapshot.tool_calls.is_empty(),
        "restoring a terminal command's retry gate must not rerun the provider"
    );
    let replay = fixture.goal_store.load_code_workflow_replay().unwrap();
    assert_eq!(
        replay
            .events
            .iter()
            .filter(|event| matches!(
                &event.event,
                CodeWorkflowEventKind::CommandIntentPersisted { command }
                    if command.identity == fixture.command.identity
            ))
            .count(),
        1,
        "startup must not re-admit the terminal Phase 1 command"
    );
    assert!(replay.events.iter().all(|event| !matches!(
        &event.event,
        CodeWorkflowEventKind::Phase1FormalWriteStarted { phase1_turn_id, .. }
            | CodeWorkflowEventKind::PlanReviewRequested { phase1_turn_id, .. }
            if phase1_turn_id == &fixture.command.identity.command_id
    )));
    assert!(
        load_phase1_start_seed(&fixture.goal_store)
            .expect("load seed after authenticated retry-gate restore")
            .is_none(),
        "startup must clear the seed only after authenticating the embedded terminal authority"
    );

    let first_restore_turn_id = restored
        .runtime_snapshot()
        .await
        .expect("read first restored retry-gate runtime")
        .active_turn_id
        .expect("first restore must bind the retry gate to one runtime command");
    let binding_turn_ids = |replay: &libra::internal::ai::session::CodeWorkflowReplay| {
        replay
            .events
            .iter()
            .filter_map(|event| match &event.event {
                CodeWorkflowEventKind::IntentReviewRequested {
                    interaction_id,
                    turn_id,
                    ..
                } if interaction_id == &retry.interaction_id && !turn_id.is_empty() => {
                    Some(turn_id.clone())
                }
                _ => None,
            })
            .collect::<Vec<_>>()
    };
    assert_eq!(
        binding_turn_ids(&replay),
        vec![first_restore_turn_id.clone()],
        "first restore must durably bind the embedded authority before tracking it"
    );

    restored
        .shutdown()
        .await
        .expect("first restored retry gate must survive graceful shutdown");
    drop(restored);
    tokio::time::sleep(Duration::from_millis(50)).await;
    let second_state = fixture
        .store
        .load(&fixture.session_id)
        .expect("load the first restored retry-gate snapshot");
    let persistence = HeadlessSessionPersistence::new(fixture.store.clone(), second_state)
        .expect("reacquire the retry-gate lease for a second restore");
    let second_restore = build_runtime_with_persistence(
        "basic_chat",
        fixture.workdir.path().to_path_buf(),
        Vec::new(),
        Some(persistence),
    )
    .await
    .0;
    assert_eq!(
        await_pending_interaction(
            &second_restore,
            CodeUiInteractionKind::IntentReviewChoice,
            "second restart did not retain the embedded retry Intent gate",
        )
        .await,
        retry.interaction_id
    );
    let second_snapshot = second_restore.snapshot().await;
    assert_eq!(
        second_snapshot
            .interactions
            .iter()
            .filter(|interaction| {
                interaction.kind == CodeUiInteractionKind::IntentReviewChoice
                    && interaction.status == CodeUiInteractionStatus::Pending
            })
            .count(),
        1,
        "second restore must keep one pending retry generation"
    );
    assert!(second_snapshot.tool_calls.is_empty());
    let second_restore_turn_id = second_restore
        .runtime_snapshot()
        .await
        .expect("read second restored retry-gate runtime")
        .active_turn_id
        .expect("second restore must reuse the durable retry-gate command");
    assert_eq!(second_restore_turn_id, first_restore_turn_id);
    let replay = fixture.goal_store.load_code_workflow_replay().unwrap();
    assert_eq!(
        binding_turn_ids(&replay),
        vec![first_restore_turn_id],
        "second restore must not append another binding or orphan a Pending command"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn startup_fences_phase1_pending_command_after_formal_write_without_plan_marker() {
    let fixture =
        phase1_startup_fixture("source-confirm-crossed", "phase1-crossed-formal", false).await;
    fixture
        .goal_store
        .append_code_workflow_durable(CodeWorkflowEventKind::Phase1FormalWriteStarted {
            phase1_turn_id: fixture.command.identity.command_id.clone(),
            source_interaction_id: fixture.seed.source_interaction_id.clone(),
            seed_digest: fixture
                .seed
                .durable_digest()
                .expect("digest the crash seed"),
        })
        .expect("persist the crossed formal-write boundary");

    let runtime = build_runtime_with_persistence(
        "basic_chat",
        fixture.workdir.path().to_path_buf(),
        Vec::new(),
        Some(fixture.persistence.clone()),
    )
    .await
    .0;

    assert_eq!(
        runtime.snapshot().await.status,
        CodeUiSessionStatus::IndeterminateSideEffect,
        "a Pending command past the formal-write boundary must require reconciliation"
    );
    let (_, status) = fixture
        .goal_store
        .code_command_intent_status(&fixture.command.identity)
        .expect("load the fenced Phase 1 command")
        .expect("the Phase 1 command intent remains durable");
    assert!(matches!(status, CodeCommandStatus::Indeterminate { .. }));
    let replay = fixture
        .goal_store
        .load_code_workflow_replay()
        .expect("read the fenced Phase 1 workflow");
    assert!(replay.events.iter().all(|event| !matches!(
        &event.event,
        CodeWorkflowEventKind::PlanReviewRequested { phase1_turn_id, .. }
            if phase1_turn_id == &fixture.command.identity.command_id
    )));
    assert!(
        load_phase1_start_seed(&fixture.goal_store)
            .expect("load the retained reconciliation seed")
            .is_some(),
        "the fence must retain seed evidence when no Plan marker proves completion"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn startup_clears_stale_phase1_seed_for_terminal_success_and_failure_without_thinking() {
    for terminal in ["succeeded", "failed"] {
        let fixture = phase1_startup_fixture(
            &format!("source-confirm-{terminal}"),
            &format!("phase1-terminal-{terminal}"),
            false,
        )
        .await;
        match terminal {
            "succeeded" => {
                fixture
                    .goal_store
                    .complete_code_command_success(
                        &fixture.command.identity,
                        "Phase 1 already completed",
                    )
                    .expect("persist prior Phase 1 success");
            }
            "failed" => {
                fixture
                    .goal_store
                    .complete_code_command_failure(
                        &fixture.command.identity,
                        "Phase 1 failed before restart",
                    )
                    .expect("persist prior Phase 1 failure");
            }
            _ => unreachable!(),
        }

        let runtime = build_runtime_with_persistence(
            "basic_chat",
            fixture.workdir.path().to_path_buf(),
            Vec::new(),
            Some(fixture.persistence.clone()),
        )
        .await
        .0;
        let snapshot = runtime.snapshot().await;
        assert_eq!(
            snapshot.status,
            CodeUiSessionStatus::Idle,
            "terminal {terminal} must not restore a stale Thinking state"
        );
        assert!(
            snapshot.transcript.iter().all(|entry| !entry.streaming),
            "terminal {terminal} must not leave a streaming/Thinking transcript row"
        );
        assert!(
            load_phase1_start_seed(&fixture.goal_store)
                .expect("load the terminal command's stale seed")
                .is_none(),
            "terminal {terminal} must clear its stale Phase 1 start seed"
        );
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn startup_terminal_success_clears_new_source_seed_before_old_digest_conflict() {
    let fixture =
        phase1_startup_fixture("old-source-confirm", "phase1-reused-command", false).await;
    fixture
        .goal_store
        .complete_code_command_success(&fixture.command.identity, "old Phase 1 completed")
        .expect("persist the old terminal success");
    let old_digest = fixture.seed.durable_digest().expect("digest the old seed");
    fixture
        .goal_store
        .append_code_workflow_durable(CodeWorkflowEventKind::Phase1FormalWriteStarted {
            phase1_turn_id: fixture.command.identity.command_id.clone(),
            source_interaction_id: fixture.seed.source_interaction_id.clone(),
            seed_digest: old_digest.clone(),
        })
        .expect("persist the old generation's formal-write marker");

    let mut new_seed = fixture.seed.clone();
    new_seed.source_interaction_id = "new-source-confirm".to_string();
    let new_digest = new_seed
        .durable_digest()
        .expect("digest the new source seed");
    assert_ne!(
        old_digest, new_digest,
        "the source generation changes the seed digest"
    );
    fixture
        .goal_store
        .append_code_workflow_durable(CodeWorkflowEventKind::InteractionResolved {
            interaction_id: new_seed.source_interaction_id.clone(),
            resolution: new_seed.source_resolution.clone(),
            command: None,
            prior_interaction_resolutions: Vec::new(),
            intent_revision_consumption: None,
        })
        .expect("persist the newer source resolution");
    persist_phase1_start_seed(&fixture.goal_store, &new_seed)
        .expect("replace the stale seed with the newer source generation");

    let runtime = build_runtime_with_persistence(
        "basic_chat",
        fixture.workdir.path().to_path_buf(),
        Vec::new(),
        Some(fixture.persistence.clone()),
    )
    .await
    .0;
    assert_eq!(runtime.snapshot().await.status, CodeUiSessionStatus::Idle);
    assert!(
        load_phase1_start_seed(&fixture.goal_store)
            .expect("load the newer source seed after terminal reconciliation")
            .is_none(),
        "same commandId/text with terminal success must clear the new source seed before comparing the old marker digest"
    );
    let (_, status) = fixture
        .goal_store
        .code_command_intent_status(&fixture.command.identity)
        .expect("load the reused terminal command")
        .expect("the reused command remains durable");
    assert!(matches!(status, CodeCommandStatus::Succeeded { .. }));
}

struct OversizedPhase1RetryFixture {
    workdir: tempfile::TempDir,
    _storage: tempfile::TempDir,
    store: Arc<SessionStore>,
    session_id: String,
    goal_store: SessionJsonlStore,
    runtime: Arc<HeadlessCodeRuntime<fake::CompletionModel>>,
    source_interaction_id: String,
    retry_interaction_id: String,
    failed_command: CodeCommandIdentity,
}

struct OversizedPhase1ConfirmFixture {
    workdir: tempfile::TempDir,
    _storage: tempfile::TempDir,
    store: Arc<SessionStore>,
    session_id: String,
    goal_store: SessionJsonlStore,
    persistence: Option<HeadlessSessionPersistence>,
    runtime: Arc<HeadlessCodeRuntime<fake::CompletionModel>>,
    source_interaction_id: String,
    risk_interaction_id: Option<String>,
    risk_pending_state: Option<SessionState>,
}

/// Supplies the Phase 0 risk answer without opening a browser interaction so
/// the registration hook can be reserved for the initial Intent review gate.
struct AutomaticLowRiskHandler;

#[async_trait::async_trait]
impl ToolHandler for AutomaticLowRiskHandler {
    fn kind(&self) -> ToolKind {
        ToolKind::Function
    }

    async fn is_mutating(&self, _invocation: &ToolInvocation) -> bool {
        false
    }

    async fn handle(&self, _invocation: ToolInvocation) -> ToolResult<ToolOutput> {
        Ok(ToolOutput::success(
            r#"{"answers":{"risk_profile":{"answers":["Low"]}}}"#,
        ))
    }

    fn schema(&self) -> ToolSpec {
        ToolSpec::new(
            "request_user_input",
            "Test-only automatic Low risk selection for registration-race coverage.",
        )
    }
}

async fn oversized_phase1_confirm_fixture(
    phase1_start_enqueued_hook: Option<HeadlessRecordUserMessageHook>,
) -> OversizedPhase1ConfirmFixture {
    oversized_phase1_confirm_fixture_with_hooks(phase1_start_enqueued_hook, None, None, false).await
}

async fn oversized_phase1_confirm_fixture_with_hooks(
    phase1_start_enqueued_hook: Option<HeadlessRecordUserMessageHook>,
    interaction_registration_hook: Option<HeadlessRecordUserMessageHook>,
    phase1_formal_write_hook: Option<HeadlessRecordUserMessageHook>,
    retain_persistence: bool,
) -> OversizedPhase1ConfirmFixture {
    let automatic_risk = interaction_registration_hook.is_some();
    let workdir = tempfile::tempdir().expect("tempdir for oversized Phase 1 checkout");
    let canonical_workdir = initialize_phase1_test_checkout(workdir.path()).await;
    let provider_dir = tempfile::tempdir().expect("tempdir for oversized provider fixture");
    let provider_path = provider_dir.path().join("oversized-plan-draft.json");
    let steps = (0..=libra::internal::ai::tools::context::MAX_SUBMIT_PLAN_DRAFT_STEPS)
        .map(|index| serde_json::json!({ "title": format!("oversized step {index}") }))
        .collect::<Vec<_>>();
    std::fs::write(
        &provider_path,
        serde_json::to_vec(&serde_json::json!({
            "responses": [
                {
                    "match": { "contains": "You are running /plan mode." },
                    "once": true,
                    "type": "tool_call",
                    "id": "oversized-risk-profile",
                    "name": "request_user_input",
                    "arguments": {
                        "questions": [{
                            "id": "risk_profile",
                            "header": "Risk",
                            "question": "What risk level should be used?",
                            "options": ["Low", "Medium", "High"]
                        }]
                    }
                },
                {
                    "match": { "equals": "" },
                    "once": true,
                    "type": "tool_call",
                    "id": "oversized-submit-intent",
                    "name": "submit_intent_draft",
                    "arguments": {
                        "draft": {
                            "intent": {
                                "summary": "Exercise oversized Phase 1 retry",
                                "problemStatement": "An invalid provider draft must not consume Confirm authority",
                                "changeType": "test",
                                "objectives": [{
                                    "title": "retry the bounded Phase 1 plan",
                                    "kind": "implementation"
                                }],
                                "inScope": ["README.md"],
                                "outOfScope": []
                            },
                            "acceptance": {
                                "successCriteria": ["one bounded Plan gate is produced"],
                                "fastChecks": [],
                                "integrationChecks": [],
                                "securityChecks": [],
                                "releaseChecks": []
                            },
                            "risk": {
                                "rationale": "durability regression fixture",
                                "level": "low"
                            }
                        }
                    }
                },
                {
                    "match": {
                        "contains": "You are generating an execution plan",
                        "afterToolResult": false
                    },
                    "once": true,
                    "type": "tool_call",
                    "id": "oversized-plan-draft",
                    "name": "submit_plan_draft",
                    "arguments": {
                        "explanation": "must fail before observer cloning",
                        "steps": steps
                    }
                },
                {
                    "match": { "afterToolResult": true },
                    "once": true,
                    "type": "text",
                    "text": "the oversized draft was rejected"
                },
                {
                    "match": {
                        "contains": "You are generating an execution plan",
                        "afterToolResult": false
                    },
                    "once": true,
                    "type": "tool_call",
                    "id": "bounded-plan-draft",
                    "name": "submit_plan_draft",
                    "arguments": {
                        "explanation": "bounded retry draft",
                        "steps": [
                            { "title": "Preserve the durable retry authority" },
                            { "title": "Publish exactly one Plan review generation" }
                        ]
                    }
                }
            ],
            "fallback": {
                "type": "text",
                "text": "fresh direct turn after explicit Cancel"
            }
        }))
        .expect("serialize oversized provider fixture"),
    )
    .expect("write oversized provider fixture");

    let storage = tempfile::tempdir().expect("tempdir for oversized Phase 1 session");
    let store = Arc::new(SessionStore::from_storage_path(storage.path()));
    let mut state = SessionState::new(&canonical_workdir.to_string_lossy());
    let session_id = state.id.clone();
    state.metadata.insert(
        "thread_id".to_string(),
        serde_json::json!(session_id.clone()),
    );
    let persistence = HeadlessSessionPersistence::new(store.clone(), state)
        .expect("attach oversized Phase 1 workflow hub");
    let persistence = match phase1_start_enqueued_hook {
        Some(hook) => persistence.with_phase1_start_enqueued_hook(hook),
        None => persistence,
    };
    let persistence = match interaction_registration_hook {
        Some(hook) => persistence.with_interaction_registration_hook(hook),
        None => persistence,
    };
    let persistence = match phase1_formal_write_hook {
        Some(hook) => persistence.with_phase1_formal_write_hook(hook),
        None => persistence,
    };
    let goal_store = persistence.goal_event_store();
    let fixture_persistence = retain_persistence.then(|| persistence.clone());

    let (user_input_tx, user_input_rx) = mpsc::unbounded_channel::<UserInputRequest>();
    let (exec_approval_tx, exec_approval_rx) = mpsc::unbounded_channel::<ExecApprovalRequest>();
    let mut registry_builder = ToolRegistryBuilder::with_working_dir(canonical_workdir.clone())
        .hardening(ToolBoundaryRuntime::system(
            Uuid::new_v4(),
            Arc::new(TracingAuditSink),
        ))
        .register("read_file", Arc::new(ReadFileHandler))
        .register("update_plan", Arc::new(PlanHandler))
        .register("submit_plan_draft", Arc::new(SubmitPlanDraftHandler))
        .register("submit_intent_draft", Arc::new(SubmitIntentDraftHandler));
    registry_builder = if automatic_risk {
        registry_builder.register("request_user_input", Arc::new(AutomaticLowRiskHandler))
    } else {
        registry_builder.register(
            "request_user_input",
            Arc::new(RequestUserInputHandler::new(user_input_tx.clone())),
        )
    };
    let registry = Arc::new(registry_builder.build());
    let (runtime, _, _) = build_runtime_with_registry_channels_from_path(
        &provider_path,
        canonical_workdir,
        Vec::new(),
        Some(persistence),
        registry,
        Arc::new(ToolLoopConfig::default),
        None,
        user_input_tx,
        user_input_rx,
        exec_approval_tx,
        exec_approval_rx,
    )
    .await;

    runtime
        .submit_message("draft a retry-safe Phase 1 plan".to_string())
        .await
        .expect("Phase 0 request must admit");
    let mut risk_interaction_id = None;
    let mut risk_pending_state = None;
    if !automatic_risk {
        let pending_risk_interaction_id = await_pending_interaction(
            &runtime,
            CodeUiInteractionKind::RequestUserInput,
            "Phase 0 did not request its risk profile",
        )
        .await;
        risk_pending_state = Some(
            store
                .load(&session_id)
                .expect("capture the durable risk-profile pending projection"),
        );
        runtime
            .respond_interaction(
                &pending_risk_interaction_id,
                CodeUiInteractionResponse {
                    selected_option: Some("Low".to_string()),
                    ..Default::default()
                },
            )
            .await
            .expect("risk profile response must be accepted");
        risk_interaction_id = Some(pending_risk_interaction_id);
    }
    let source_interaction_id = await_pending_interaction(
        &runtime,
        CodeUiInteractionKind::IntentReviewChoice,
        "Phase 0 did not publish the initial IntentSpec review",
    )
    .await;

    OversizedPhase1ConfirmFixture {
        workdir,
        _storage: storage,
        store,
        session_id,
        goal_store,
        persistence: fixture_persistence,
        runtime,
        source_interaction_id,
        risk_interaction_id,
        risk_pending_state,
    }
}

async fn oversized_phase1_retry_fixture() -> OversizedPhase1RetryFixture {
    let fixture = oversized_phase1_confirm_fixture(None).await;
    fixture
        .runtime
        .respond_interaction(
            &fixture.source_interaction_id,
            CodeUiInteractionResponse {
                selected_option: Some("confirm".to_string()),
                ..Default::default()
            },
        )
        .await
        .expect("initial Confirm must admit the first Phase 1 attempt");

    let deadline = std::time::Instant::now() + Duration::from_secs(8);
    let retry_interaction_id = loop {
        let snapshot = fixture.runtime.snapshot().await;
        if let Some(interaction) = snapshot.interactions.iter().find(|interaction| {
            interaction.kind == CodeUiInteractionKind::IntentReviewChoice
                && interaction.status == CodeUiInteractionStatus::Pending
                && interaction.id != fixture.source_interaction_id
        }) {
            break interaction.id.clone();
        }
        if std::time::Instant::now() >= deadline {
            let replay = fixture
                .goal_store
                .load_code_workflow_replay()
                .expect("read workflow while diagnosing the missing retry gate");
            panic!(
                "oversized Phase 1 failure did not restore a fresh durable Confirm gate; \
                 snapshot={snapshot:#?}; workflow={:#?}",
                replay.events
            );
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    };
    // The live interaction is projected before the async retry-restoration
    // task finishes persisting its browser snapshot. Do not let callers take
    // a workflow baseline until the embedded retry generation, its first
    // durable turn binding, and both live/disk projections agree.
    let durable_deadline = std::time::Instant::now() + Duration::from_secs(8);
    let failed_command = loop {
        let live = fixture.runtime.snapshot().await;
        let replay = fixture
            .goal_store
            .load_code_workflow_replay()
            .expect("read the oversized first-attempt workflow");
        let failed = replay
            .events
            .iter()
            .filter_map(|event| match &event.event {
                CodeWorkflowEventKind::CommandTerminalFailure {
                    command,
                    retry_intent_review: Some(retry),
                    ..
                } => Some((command.clone(), retry.clone())),
                _ => None,
            })
            .collect::<Vec<_>>();
        let binding_turns = replay
            .events
            .iter()
            .filter_map(|event| match &event.event {
                CodeWorkflowEventKind::IntentReviewRequested {
                    interaction_id,
                    turn_id,
                    ..
                } if interaction_id == &retry_interaction_id && !turn_id.is_empty() => {
                    Some(turn_id.clone())
                }
                _ => None,
            })
            .collect::<Vec<_>>();
        let owner_intents = binding_turns.first().map_or_else(Vec::new, |turn_id| {
            replay
                .events
                .iter()
                .filter_map(|event| match &event.event {
                    CodeWorkflowEventKind::CommandIntentPersisted { command }
                        if command.identity.command_id == *turn_id =>
                    {
                        Some(command.clone())
                    }
                    _ => None,
                })
                .collect::<Vec<_>>()
        });
        let persisted = fixture
            .store
            .load(&fixture.session_id)
            .ok()
            .and_then(|state| {
                state
                    .metadata
                    .get("code_ui_snapshot")
                    .cloned()
                    .and_then(|snapshot| serde_json::from_value(snapshot).ok())
            });
        let projection_is_stable =
            |snapshot: &libra::internal::ai::web::code_ui::CodeUiSessionSnapshot| {
                snapshot.status == CodeUiSessionStatus::AwaitingInteraction
                    && snapshot
                        .interactions
                        .iter()
                        .filter(|interaction| {
                            interaction.id == retry_interaction_id
                                && interaction.status == CodeUiInteractionStatus::Pending
                        })
                        .count()
                        == 1
                    && snapshot.interactions.iter().any(|interaction| {
                        interaction.id == fixture.source_interaction_id
                            && interaction.status != CodeUiInteractionStatus::Pending
                    })
            };
        let stable = failed.len() == 1
            && failed[0].1.interaction_id == retry_interaction_id
            && binding_turns.len() == 1
            && owner_intents.len() == 1
            && owner_intents[0].command_kind == CODE_UI_WEB_TURN_KIND
            && !owner_intents[0].mutating
            && projection_is_stable(&live)
            && persisted.as_ref().is_some_and(projection_is_stable);
        if stable {
            break failed[0].0.clone();
        }
        assert!(
            std::time::Instant::now() < durable_deadline,
            "fresh retry generation did not reach one fully durable owner before the fixture returned: retry={retry_interaction_id}, failed={}, bindings={binding_turns:?}, owners={owner_intents:#?}, live={live:#?}, persisted={persisted:#?}",
            failed.len(),
        );
        tokio::time::sleep(Duration::from_millis(20)).await;
    };

    OversizedPhase1RetryFixture {
        workdir: fixture.workdir,
        _storage: fixture._storage,
        store: fixture.store,
        session_id: fixture.session_id,
        goal_store: fixture.goal_store,
        runtime: fixture.runtime,
        source_interaction_id: fixture.source_interaction_id,
        retry_interaction_id,
        failed_command,
    }
}

fn phase1_context_file_count(store: &SessionStore, session_id: &str) -> usize {
    let phase1_root = store.session_root(session_id).join("phase1");
    std::fs::read_dir(phase1_root)
        .into_iter()
        .flatten()
        .filter_map(Result::ok)
        .filter(|entry| {
            let name = entry.file_name();
            let name = name.to_string_lossy();
            name.strip_suffix(".json").is_some_and(|digest| {
                digest.len() == 64 && digest.bytes().all(|byte| byte.is_ascii_hexdigit())
            })
        })
        .count()
}

async fn paused_initial_intent_registration_fixture()
-> (OversizedPhase1ConfirmFixture, HeadlessRecordUserMessageHook) {
    let registration = HeadlessRecordUserMessageHook::new();
    let fixture =
        oversized_phase1_confirm_fixture_with_hooks(None, Some(registration.clone()), None, false)
            .await;
    tokio::time::timeout(Duration::from_secs(5), registration.wait_until_entered())
        .await
        .expect("initial Intent review must reach the pre-registration snapshot window");
    assert!(
        fixture
            .runtime
            .snapshot()
            .await
            .interactions
            .iter()
            .any(
                |interaction| interaction.id == fixture.source_interaction_id
                    && interaction.status == CodeUiInteractionStatus::Pending
            )
    );
    (fixture, registration)
}

fn intent_review_response(decision: &str) -> CodeUiInteractionResponse {
    CodeUiInteractionResponse {
        selected_option: Some(decision.to_string()),
        ..Default::default()
    }
}

#[tokio::test(flavor = "current_thread")]
async fn initial_intent_early_confirm_beats_later_cancel_without_fencing() {
    let (fixture, registration) = paused_initial_intent_registration_fixture().await;
    let interaction_id = fixture.source_interaction_id.clone();

    let first_runtime = Arc::clone(&fixture.runtime);
    let first_interaction_id = interaction_id.clone();
    let mut first = tokio::spawn(async move {
        first_runtime
            .respond_interaction(&first_interaction_id, intent_review_response("confirm"))
            .await
    });
    assert!(
        tokio::time::timeout(Duration::from_millis(50), &mut first)
            .await
            .is_err(),
        "the first response must wait for runtime interaction registration"
    );

    let second_runtime = Arc::clone(&fixture.runtime);
    let second_interaction_id = interaction_id.clone();
    let second = tokio::spawn(async move {
        second_runtime
            .respond_interaction(&second_interaction_id, intent_review_response("cancel"))
            .await
    });
    tokio::task::yield_now().await;
    registration.release();

    tokio::time::timeout(Duration::from_secs(15), first)
        .await
        .expect("first-writer Confirm must complete within the bounded window")
        .expect("first-writer Confirm task must not panic")
        .expect("the early Confirm must be accepted");
    let conflict = tokio::time::timeout(Duration::from_secs(5), second)
        .await
        .expect("later Cancel must receive a bounded conflict")
        .expect("later Cancel task must not panic")
        .expect_err("a different later response must not overtake the early Confirm");
    let conflict = conflict
        .downcast_ref::<CodeUiApiError>()
        .expect("different early response must surface a typed Web conflict");
    assert_eq!(conflict.status, 409);
    assert_eq!(conflict.code, "INTERACTION_NOT_ACTIVE");
    assert_ne!(
        fixture.runtime.snapshot().await.status,
        CodeUiSessionStatus::IndeterminateSideEffect,
        "a deterministic first-writer conflict must not fence the session"
    );

    let replay = fixture.goal_store.load_code_workflow_replay().unwrap();
    assert_eq!(
        replay
            .events
            .iter()
            .filter(|event| matches!(
                &event.event,
                CodeWorkflowEventKind::CommandTerminalSuccessWithInteractionResolved {
                    interaction_id: resolved_id,
                    resolution,
                    ..
                } if resolved_id == &interaction_id && resolution == "confirm"
            ))
            .count(),
        1,
        "the winning Confirm must own the sole terminal resolution row"
    );
}

#[tokio::test(flavor = "current_thread")]
async fn duplicate_initial_intent_early_confirm_waits_for_and_shares_completion() {
    let (fixture, registration) = paused_initial_intent_registration_fixture().await;
    let interaction_id = fixture.source_interaction_id.clone();

    let first_runtime = Arc::clone(&fixture.runtime);
    let first_id = interaction_id.clone();
    let mut first = tokio::spawn(async move {
        first_runtime
            .respond_interaction(&first_id, intent_review_response("confirm"))
            .await
    });
    assert!(
        tokio::time::timeout(Duration::from_millis(50), &mut first)
            .await
            .is_err(),
        "the owner response must remain pending before registration"
    );
    let duplicate_runtime = Arc::clone(&fixture.runtime);
    let duplicate_id = interaction_id.clone();
    let duplicate = tokio::spawn(async move {
        duplicate_runtime
            .respond_interaction(&duplicate_id, intent_review_response("confirm"))
            .await
    });
    tokio::task::yield_now().await;
    registration.release();

    tokio::time::timeout(Duration::from_secs(15), first)
        .await
        .expect("owner response must complete in bounded time")
        .expect("owner response task must not panic")
        .expect("owner response must succeed");
    tokio::time::timeout(Duration::from_secs(5), duplicate)
        .await
        .expect("duplicate response must share bounded completion")
        .expect("duplicate response task must not panic")
        .expect("same-payload duplicate must acknowledge the owner's completion");

    let replay = fixture.goal_store.load_code_workflow_replay().unwrap();
    assert_eq!(
        replay
            .events
            .iter()
            .filter(|event| matches!(
                &event.event,
                CodeWorkflowEventKind::CommandTerminalSuccessWithInteractionResolved {
                    interaction_id: resolved_id,
                    resolution,
                    ..
                } if resolved_id == &interaction_id && resolution == "confirm"
            ))
            .count(),
        1,
        "same-response retries must share one terminal durable resolution"
    );
    assert_ne!(
        fixture.runtime.snapshot().await.status,
        CodeUiSessionStatus::IndeterminateSideEffect
    );
}

#[tokio::test(flavor = "current_thread")]
async fn initial_intent_pre_registration_control_cancel_clears_slot_before_fresh_submit() {
    let (fixture, registration) = paused_initial_intent_registration_fixture().await;
    let interaction_id = fixture.source_interaction_id.clone();

    let cancel_runtime = Arc::clone(&fixture.runtime);
    let mut cancel = tokio::spawn(async move { cancel_runtime.cancel_turn().await });
    assert!(
        tokio::time::timeout(Duration::from_millis(50), &mut cancel)
            .await
            .is_err(),
        "control Cancel must remain pending until registration leaves its durability hook"
    );
    registration.release();
    tokio::time::timeout(Duration::from_secs(5), cancel)
        .await
        .expect("pre-registration control Cancel must finish promptly after hook release")
        .expect("pre-registration control Cancel task must not panic")
        .expect("pre-registration control Cancel must durably acknowledge success");
    let replay = fixture
        .goal_store
        .load_code_workflow_replay_committed()
        .expect("read the fsynced pre-registration Cancel terminal");
    let combined = replay
        .events
        .iter()
        .filter_map(|event| match &event.event {
            CodeWorkflowEventKind::CommandTerminalSuccessWithInteractionResolved {
                command,
                interaction_id: resolved_id,
                resolution,
                ..
            } if resolved_id == &interaction_id && resolution == "cancel" => Some(command.clone()),
            _ => None,
        })
        .collect::<Vec<_>>();
    assert_eq!(
        combined.len(),
        1,
        "control Cancel 2xx must follow one durable combined terminal"
    );
    assert!(replay.events.iter().all(|event| !matches!(
        &event.event,
        CodeWorkflowEventKind::InteractionResolved {
            interaction_id: resolved_id,
            ..
        } if resolved_id == &interaction_id
    )));
    assert!(matches!(
        fixture
            .goal_store
            .recover_code_command(&combined[0])
            .unwrap(),
        libra::internal::ai::session::jsonl::CodeCommandRecovery::Existing {
            status: CodeCommandStatus::Succeeded { .. }
        }
    ));
    let after_cancel = fixture.runtime.snapshot().await;
    assert_eq!(after_cancel.status, CodeUiSessionStatus::Idle);
    assert!(after_cancel.interactions.iter().all(|interaction| {
        interaction.id != interaction_id || interaction.status != CodeUiInteractionStatus::Pending
    }));
    let tool_calls_after_cancel = serde_json::to_value(&after_cancel.tool_calls).unwrap();

    let cleanup_deadline = std::time::Instant::now() + Duration::from_secs(5);
    loop {
        let snapshot = fixture.runtime.snapshot().await;
        let runtime_snapshot = fixture
            .runtime
            .runtime_snapshot()
            .await
            .expect("read runtime after releasing cancelled registration");
        if snapshot.status == CodeUiSessionStatus::Idle
            && matches!(
                runtime_snapshot.interaction,
                InteractionState::Idle | InteractionState::Completed
            )
            && runtime_snapshot.active_turn_id.is_none()
        {
            assert_eq!(
                serde_json::to_value(&snapshot.tool_calls).unwrap(),
                tool_calls_after_cancel,
                "releasing a cancelled registration must not resume the old provider/tool loop"
            );
            break;
        }
        assert!(
            std::time::Instant::now() < cleanup_deadline,
            "cancelled pre-registration owner did not clear its worker slot/map"
        );
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    let replay = fixture.goal_store.load_code_workflow_replay().unwrap();
    assert_eq!(
        replay
            .events
            .iter()
            .filter(|event| matches!(
                &event.event,
                CodeWorkflowEventKind::CommandTerminalSuccessWithInteractionResolved {
                    interaction_id: resolved_id,
                    resolution,
                    ..
                } if resolved_id == &interaction_id && resolution == "cancel"
            ))
            .count(),
        1
    );
    assert!(replay.events.iter().all(|event| !matches!(
        &event.event,
        CodeWorkflowEventKind::CommandIndeterminateSideEffect { command, .. }
            if command == &combined[0]
    )));
    assert!(replay.events.iter().all(|event| !matches!(
        &event.event,
        CodeWorkflowEventKind::Phase1FormalWriteStarted { .. }
            | CodeWorkflowEventKind::PlanReviewRequested { .. }
    )));
    assert!(
        load_phase1_start_seed(&fixture.goal_store)
            .unwrap()
            .is_none(),
        "cancelled pre-registration Confirm path must not leave a Phase 1 seed"
    );

    let fresh_command_id = format!("fresh-after-registration-cancel-{}", Uuid::new_v4());
    fixture
        .runtime
        .submit_message_with_command_id(
            "/fresh turn after pre-registration control Cancel".to_string(),
            Some(fresh_command_id.clone()),
        )
        .await
        .expect("cleared slot/map must admit a fresh turn after Cancel 2xx");
    let replay = fixture
        .goal_store
        .load_code_workflow_replay_committed()
        .unwrap();
    assert_eq!(
        replay
            .events
            .iter()
            .filter(|event| matches!(
                &event.event,
                CodeWorkflowEventKind::CommandIntentPersisted { command }
                    if command.identity.command_id == fresh_command_id
            ))
            .count(),
        1,
        "fresh admission must persist one new command intent"
    );
    assert_ne!(
        fixture.runtime.snapshot().await.status,
        CodeUiSessionStatus::IndeterminateSideEffect
    );
}

struct SequentialUserInputFixture {
    _workdir: tempfile::TempDir,
    _provider: tempfile::TempDir,
    _storage: tempfile::TempDir,
    runtime: Arc<HeadlessCodeRuntime<fake::CompletionModel>>,
    goal_store: SessionJsonlStore,
    command_id: String,
    answered_interaction_ids: Vec<String>,
}

async fn sequential_user_input_fixture(answer_count: usize) -> SequentialUserInputFixture {
    assert!((1..=2).contains(&answer_count));
    let workdir = tempfile::tempdir().expect("tempdir for sequential user-input workdir");
    let canonical_workdir = std::fs::canonicalize(workdir.path()).unwrap();
    let provider = tempfile::tempdir().expect("tempdir for sequential user-input provider");
    let provider_path = provider.path().join("sequential-user-input.json");
    std::fs::write(
        &provider_path,
        serde_json::to_vec(&serde_json::json!({
            "responses": [
                {
                    "match": {
                        "contains": "sequential-interaction-regression",
                        "afterToolResult": false
                    },
                    "once": true,
                    "type": "tool_call",
                    "id": "sequential-user-input-1",
                    "name": "request_user_input",
                    "arguments": {
                        "questions": [{
                            "id": "answer-1",
                            "header": "First",
                            "question": "Supply the first answer"
                        }]
                    }
                },
                {
                    "match": { "afterToolResult": true },
                    "once": true,
                    "type": "tool_call",
                    "id": "sequential-user-input-2",
                    "name": "request_user_input",
                    "arguments": {
                        "questions": [{
                            "id": "answer-2",
                            "header": "Second",
                            "question": "Supply the second answer"
                        }]
                    }
                },
                {
                    "match": { "afterToolResult": true },
                    "once": true,
                    "type": "text",
                    "delayMs": 10000,
                    "text": "sequential user-input turn completed"
                }
            ],
            "fallback": {
                "type": "text",
                "delayMs": 10000,
                "text": "sequential user-input fallback"
            }
        }))
        .unwrap(),
    )
    .expect("write sequential user-input provider fixture");

    let storage = tempfile::tempdir().expect("tempdir for sequential user-input session");
    let store = Arc::new(SessionStore::from_storage_path(storage.path()));
    let mut state = SessionState::new(&canonical_workdir.to_string_lossy());
    let session_id = state.id.clone();
    state.metadata.insert(
        "thread_id".to_string(),
        serde_json::json!(session_id.clone()),
    );
    let persistence = HeadlessSessionPersistence::new(store, state)
        .expect("attach sequential user-input workflow hub");
    let goal_store = persistence.goal_event_store();
    let (user_input_tx, user_input_rx) = mpsc::unbounded_channel::<UserInputRequest>();
    let (exec_approval_tx, exec_approval_rx) = mpsc::unbounded_channel::<ExecApprovalRequest>();
    let registry = Arc::new(
        ToolRegistryBuilder::with_working_dir(canonical_workdir.clone())
            .hardening(ToolBoundaryRuntime::system(
                Uuid::new_v4(),
                Arc::new(TracingAuditSink),
            ))
            .register(
                "request_user_input",
                Arc::new(RequestUserInputHandler::new(user_input_tx.clone())),
            )
            .build(),
    );
    let (runtime, _, _) = build_runtime_with_registry_channels_from_path(
        &provider_path,
        canonical_workdir,
        Vec::new(),
        Some(persistence),
        registry,
        Arc::new(ToolLoopConfig::default),
        None,
        user_input_tx,
        user_input_rx,
        exec_approval_tx,
        exec_approval_rx,
    )
    .await;
    let command_id = format!("sequential-interactions-{answer_count}-{}", Uuid::new_v4());
    runtime
        .submit_message_with_command_id(
            "/sequential-interaction-regression".to_string(),
            Some(command_id.clone()),
        )
        .await
        .expect("sequential user-input turn must admit");

    let mut answered_interaction_ids = Vec::new();
    for index in 0..2 {
        let interaction_id = await_pending_interaction(
            &runtime,
            CodeUiInteractionKind::RequestUserInput,
            "sequential user-input tool did not publish its next gate",
        )
        .await;
        if index == answer_count {
            break;
        }
        runtime
            .respond_interaction(
                &interaction_id,
                CodeUiInteractionResponse {
                    selected_option: Some(format!("answer {}", index + 1)),
                    ..Default::default()
                },
            )
            .await
            .expect("sequential user-input response must be delivered");
        answered_interaction_ids.push(interaction_id);
    }
    if answer_count == 2 {
        await_worker_running(&runtime).await;
    }

    SequentialUserInputFixture {
        _workdir: workdir,
        _provider: provider,
        _storage: storage,
        runtime,
        goal_store,
        command_id,
        answered_interaction_ids,
    }
}

async fn assert_sequential_user_input_terminal_preserves_resolutions(
    answer_count: usize,
    shutdown: bool,
) {
    let fixture = sequential_user_input_fixture(answer_count).await;
    if shutdown {
        fixture
            .runtime
            .shutdown()
            .await
            .expect("graceful shutdown must settle the sequential interaction turn");
    } else {
        fixture
            .runtime
            .cancel_turn()
            .await
            .expect("control cancel must settle the sequential interaction turn");
    }

    let deadline = std::time::Instant::now() + Duration::from_secs(5);
    let (identity, resolutions) = loop {
        let replay = fixture
            .goal_store
            .load_code_workflow_replay()
            .expect("read sequential interaction terminal workflow");
        if let Some(terminal) = replay.events.iter().find_map(|event| match &event.event {
            CodeWorkflowEventKind::CommandTerminalFailure {
                command,
                interaction_resolutions,
                ..
            } if command.command_id == fixture.command_id => {
                Some((command.clone(), interaction_resolutions.clone()))
            }
            _ => None,
        }) {
            assert!(replay.events.iter().all(|event| !matches!(
                &event.event,
                CodeWorkflowEventKind::CommandIndeterminateSideEffect { command, .. }
                    if command.command_id == fixture.command_id
            )));
            break terminal;
        }
        assert!(
            std::time::Instant::now() < deadline,
            "sequential interaction turn did not reach a durable failure/cancel terminal"
        );
        tokio::time::sleep(Duration::from_millis(20)).await;
    };
    for answered in &fixture.answered_interaction_ids {
        assert!(
            resolutions
                .iter()
                .any(|(interaction_id, resolution)| interaction_id == answered
                    && resolution == "answered"),
            "terminal row must retain replayable resolution for '{answered}': {resolutions:?}"
        );
    }
    assert_eq!(
        resolutions
            .iter()
            .filter(|(interaction_id, _)| fixture.answered_interaction_ids.contains(interaction_id))
            .count(),
        answer_count,
        "each delivered user-input response must appear exactly once"
    );
    let (_, status) = fixture
        .goal_store
        .code_command_intent_status(&identity)
        .unwrap()
        .expect("sequential interaction command must remain indexed");
    assert!(matches!(status, CodeCommandStatus::Failed { .. }));
    assert_ne!(
        fixture.runtime.snapshot().await.status,
        CodeUiSessionStatus::IndeterminateSideEffect,
        "ordinary cancel/shutdown after answered interactions must not fence"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn cancel_after_one_sequential_user_input_preserves_resolution_without_fence() {
    assert_sequential_user_input_terminal_preserves_resolutions(1, false).await;
}

#[tokio::test(flavor = "multi_thread")]
async fn cancel_after_two_sequential_user_inputs_preserves_resolutions_without_fence() {
    assert_sequential_user_input_terminal_preserves_resolutions(2, false).await;
}

#[tokio::test(flavor = "multi_thread")]
async fn shutdown_after_one_sequential_user_input_preserves_resolution_without_fence() {
    assert_sequential_user_input_terminal_preserves_resolutions(1, true).await;
}

#[tokio::test(flavor = "multi_thread")]
async fn shutdown_after_two_sequential_user_inputs_preserves_resolutions_without_fence() {
    assert_sequential_user_input_terminal_preserves_resolutions(2, true).await;
}

#[tokio::test(flavor = "multi_thread")]
async fn shutdown_deadline_and_simulated_drop_preserve_checkpointed_history() {
    use async_trait::async_trait;
    use libra::internal::ai::runtime::{
        AgentRuntimeWorker, AgentRuntimeWorkerConfig, InteractionResponse,
        RuntimeCommandDurability, RuntimeExecutionContext, RuntimeInteractionDelivery,
        RuntimeShutdownError, RuntimeTurnExecution, RuntimeTurnExecutor, RuntimeWorkerError,
        TurnRequest,
    };
    struct HungExecutor {
        mutating: bool,
        started: Arc<Notify>,
    }

    #[async_trait]
    impl RuntimeTurnExecutor for HungExecutor {
        async fn execute(
            &self,
            _request: TurnRequest,
            context: RuntimeExecutionContext,
        ) -> Result<RuntimeTurnExecution, RuntimeWorkerError> {
            if self.mutating {
                context.mark_mutation_started();
            }
            self.started.notify_one();
            std::future::pending().await
        }
    }

    struct CheckpointedInputDelivery;

    #[async_trait]
    impl RuntimeInteractionDelivery for CheckpointedInputDelivery {
        fn validate(&self, _interaction: &InteractionResponse) -> Result<(), RuntimeWorkerError> {
            Ok(())
        }

        fn checkpoint_interaction_resolved_before_delivery(&self) -> bool {
            true
        }

        fn interaction_resolution(&self, _interaction: &InteractionResponse) -> String {
            "answered".to_string()
        }

        async fn deliver(
            self: Box<Self>,
            _request: TurnRequest,
            _interaction: InteractionResponse,
            _context: RuntimeExecutionContext,
        ) -> Result<RuntimeTurnExecution, RuntimeWorkerError> {
            Ok(RuntimeTurnExecution::InteractionResponseDelivered)
        }
    }

    for mutating in [false, true] {
        let temp = tempfile::tempdir().expect("temporary hung-executor workflow root");
        let store = SessionJsonlStore::new(temp.path().join("session"));
        let durability = RuntimeCommandDurability::new(store.clone());
        let started = Arc::new(Notify::new());
        let session_id = if mutating {
            "hung-mutating"
        } else {
            "hung-read-only"
        };
        let turn_id = if mutating {
            "hung-mutating-turn"
        } else {
            "hung-read-only-turn"
        };
        let mut config = AgentRuntimeWorkerConfig::new(
            Arc::new(HungExecutor {
                mutating,
                started: Arc::clone(&started),
            }),
            ToolBoundaryRuntime::system(Uuid::new_v4(), Arc::new(TracingAuditSink)),
        )
        .with_durability(durability, "repo", "principal")
        .with_durability_command_kind(CODE_UI_WEB_TURN_KIND);
        config.shutdown_timeout = Duration::from_millis(40);
        let (handle, worker) = AgentRuntimeWorker::spawn(config);
        handle
            .submit(TurnRequest::new(
                session_id,
                turn_id,
                "checkpoint then hang",
                mutating,
            ))
            .await
            .expect("hung executor turn admits");
        tokio::time::timeout(Duration::from_secs(1), started.notified())
            .await
            .expect("hung executor crosses its dispatch boundary");
        handle
            .register_interaction_with_delivery(
                session_id,
                turn_id,
                InteractionState::AwaitingUserInput {
                    interaction_id: "hung-input".to_string(),
                },
                Box::new(CheckpointedInputDelivery),
            )
            .await
            .expect("worker owns the hung executor input continuation");
        handle
            .respond(
                session_id,
                turn_id,
                InteractionResponse::new("hung-input", "opaque answer"),
            )
            .await
            .expect("the response checkpoints before entering the hung continuation");

        let identity = CodeCommandIdentity::new("repo", session_id, "principal", turn_id);
        let checkpoint_replay = store.load_code_workflow_replay().unwrap();
        assert!(checkpoint_replay.events.iter().any(|event| matches!(
            &event.event,
            CodeWorkflowEventKind::InteractionResolved {
                interaction_id,
                resolution,
                command: Some(command),
                ..
            } if interaction_id == "hung-input"
                && resolution == "answered"
                && command == &identity
        )));
        assert_eq!(
            handle.snapshot(session_id).await.unwrap().active_turn_id,
            Some(turn_id.to_string()),
            "a delivered response must not discard its still-running executor"
        );

        let shutdown = handle
            .shutdown()
            .await
            .expect_err("the deliberately hung executor must reach the shutdown deadline");
        let expected_resource = if mutating {
            "mutating_runtime_turn_reconciliation"
        } else {
            "runtime_turn"
        };
        assert!(matches!(
            shutdown,
            RuntimeShutdownError::TimedOut { unreleased_resources }
                if unreleased_resources == vec![expected_resource.to_string()]
        ));
        tokio::time::timeout(Duration::from_secs(1), worker)
            .await
            .expect("worker exits after classifying the hung turn")
            .expect("worker task does not panic");

        let replay = store.load_code_workflow_replay().unwrap();
        if mutating {
            assert!(replay.events.iter().any(|event| matches!(
                &event.event,
                CodeWorkflowEventKind::CommandIndeterminateSideEffect { command, .. }
                    if command == &identity
            )));
            assert!(matches!(
                store.recover_code_command(&identity).unwrap(),
                libra::internal::ai::session::jsonl::CodeCommandRecovery::Existing {
                    status: CodeCommandStatus::Indeterminate { .. }
                }
            ));
        } else {
            assert!(replay.events.iter().any(|event| matches!(
                &event.event,
                CodeWorkflowEventKind::CommandTerminalFailure {
                    command,
                    interaction_resolutions,
                    ..
                } if command == &identity
                    && interaction_resolutions
                        == &vec![("hung-input".to_string(), "answered".to_string())]
            )));
            assert!(matches!(
                store.recover_code_command(&identity).unwrap(),
                libra::internal::ai::session::jsonl::CodeCommandRecovery::Existing {
                    status: CodeCommandStatus::Failed { .. }
                }
            ));
        }
    }

    let temp = tempfile::tempdir().expect("temporary simulated-drop workflow root");
    let session_root = temp.path().join("session");
    let store = SessionJsonlStore::new(session_root.clone());
    let started = Arc::new(Notify::new());
    let (handle, worker) = AgentRuntimeWorker::spawn(
        AgentRuntimeWorkerConfig::new(
            Arc::new(HungExecutor {
                mutating: true,
                started: Arc::clone(&started),
            }),
            ToolBoundaryRuntime::system(Uuid::new_v4(), Arc::new(TracingAuditSink)),
        )
        .with_durability(
            RuntimeCommandDurability::new(store.clone()),
            "repo",
            "principal",
        )
        .with_durability_command_kind(CODE_UI_WEB_TURN_KIND),
    );
    handle
        .submit(TurnRequest::new(
            "drop-session",
            "drop-mutating-turn",
            "checkpoint before simulated process loss",
            true,
        ))
        .await
        .expect("simulated-drop mutation admits");
    tokio::time::timeout(Duration::from_secs(1), started.notified())
        .await
        .expect("simulated-drop mutation crosses its boundary");
    handle
        .register_interaction_with_delivery(
            "drop-session",
            "drop-mutating-turn",
            InteractionState::AwaitingUserInput {
                interaction_id: "drop-input".to_string(),
            },
            Box::new(CheckpointedInputDelivery),
        )
        .await
        .expect("simulated-drop continuation registers");
    handle
        .respond(
            "drop-session",
            "drop-mutating-turn",
            InteractionResponse::new("drop-input", "opaque answer"),
        )
        .await
        .expect("response is durably checkpointed before simulated process loss");
    let identity =
        CodeCommandIdentity::new("repo", "drop-session", "principal", "drop-mutating-turn");
    assert!(
        store
            .load_code_workflow_replay()
            .unwrap()
            .events
            .iter()
            .any(|event| matches!(
                &event.event,
                CodeWorkflowEventKind::InteractionResolved {
                    interaction_id,
                    resolution,
                    command: Some(command),
                    ..
                } if interaction_id == "drop-input"
                    && resolution == "answered"
                    && command == &identity
            ))
    );

    worker.abort();
    let _ = worker.await;
    drop(handle);
    let restarted_store = SessionJsonlStore::new(session_root);
    assert!(matches!(
        restarted_store.recover_code_command(&identity).unwrap(),
        libra::internal::ai::session::jsonl::CodeCommandRecovery::Existing {
            status: CodeCommandStatus::Indeterminate { .. }
        }
    ));
    let replay = restarted_store.load_code_workflow_replay().unwrap();
    assert!(replay.events.iter().any(|event| matches!(
        &event.event,
        CodeWorkflowEventKind::InteractionResolved {
            interaction_id,
            resolution,
            command: Some(command),
            ..
        } if interaction_id == "drop-input"
            && resolution == "answered"
            && command == &identity
    )));
    assert!(replay.events.iter().any(|event| matches!(
        &event.event,
        CodeWorkflowEventKind::CommandIndeterminateSideEffect { command, .. }
            if command == &identity
    )));
}

#[tokio::test(flavor = "current_thread")]
async fn aborted_confirm_after_start_enqueue_and_same_source_retry_produce_one_attempt_and_gate() {
    let start_enqueued = HeadlessRecordUserMessageHook::new();
    let fixture = oversized_phase1_confirm_fixture(Some(start_enqueued.clone())).await;
    let source_interaction_id = fixture.source_interaction_id.clone();
    let first_runtime = Arc::clone(&fixture.runtime);
    let first_source = source_interaction_id.clone();
    let first = tokio::spawn(async move {
        first_runtime
            .respond_interaction(
                &first_source,
                CodeUiInteractionResponse {
                    selected_option: Some("confirm".to_string()),
                    ..Default::default()
                },
            )
            .await
    });
    tokio::time::timeout(Duration::from_secs(5), start_enqueued.wait_until_entered())
        .await
        .expect("first Confirm must pause after its Start command is enqueued");
    assert!(
        fixture
            .runtime
            .snapshot()
            .await
            .interactions
            .iter()
            .any(|interaction| {
                interaction.id == source_interaction_id
                    && interaction.status == CodeUiInteractionStatus::Pending
            }),
        "the injected abort window must precede Web closeout of the source Confirm"
    );
    first.abort();
    assert!(
        first
            .await
            .expect_err("the first browser response future must be aborted at the hook")
            .is_cancelled()
    );

    let retry_runtime = Arc::clone(&fixture.runtime);
    let retry_source = source_interaction_id.clone();
    let retry = tokio::spawn(async move {
        retry_runtime
            .respond_interaction(
                &retry_source,
                CodeUiInteractionResponse {
                    selected_option: Some("confirm".to_string()),
                    ..Default::default()
                },
            )
            .await
    });
    start_enqueued.release();
    tokio::time::timeout(Duration::from_secs(10), retry)
        .await
        .expect("same-source Confirm retry must receive a bounded acknowledgement")
        .expect("same-source Confirm retry task must not panic")
        .expect("same-source Confirm retry must ACK the already-enqueued Start");

    let retry_interaction_id = {
        let deadline = std::time::Instant::now() + Duration::from_secs(8);
        loop {
            let snapshot = fixture.runtime.snapshot().await;
            assert_ne!(
                snapshot.status,
                CodeUiSessionStatus::IndeterminateSideEffect,
                "the duplicate old Start must not fence the fresh retry gate"
            );
            if let Some(interaction) = snapshot.interactions.iter().find(|interaction| {
                interaction.kind == CodeUiInteractionKind::IntentReviewChoice
                    && interaction.status == CodeUiInteractionStatus::Pending
                    && interaction.id != source_interaction_id
            }) {
                break interaction.id.clone();
            }
            assert!(
                std::time::Instant::now() < deadline,
                "the one failed pre-write attempt did not rearm one fresh Confirm gate"
            );
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    };
    let replay = fixture
        .goal_store
        .load_code_workflow_replay()
        .expect("read duplicate-Start workflow");
    let failed_attempts = replay
        .events
        .iter()
        .filter_map(|event| match &event.event {
            CodeWorkflowEventKind::CommandTerminalFailure { command, .. } => Some(command.clone()),
            _ => None,
        })
        .collect::<Vec<_>>();
    assert_eq!(
        failed_attempts.len(),
        1,
        "A abort plus B retry must execute exactly one Phase 1 attempt"
    );
    assert_eq!(
        replay
            .events
            .iter()
            .filter(|event| matches!(
                &event.event,
                CodeWorkflowEventKind::CommandIntentPersisted { command }
                    if command.identity == failed_attempts[0]
            ))
            .count(),
        1,
        "the duplicate Start must reuse the first durable command intent"
    );
    assert!(replay.events.iter().all(|event| !matches!(
        &event.event,
        CodeWorkflowEventKind::Phase1FormalWriteStarted { .. }
            | CodeWorkflowEventKind::PlanReviewRequested { .. }
    )));
    assert_eq!(
        replay
            .events
            .iter()
            .filter(|event| matches!(
                &event.event,
                CodeWorkflowEventKind::CommandTerminalFailure {
                    command,
                    retry_intent_review: Some(retry),
                    ..
                } if retry.interaction_id == retry_interaction_id
                    && retry.source_phase1_turn_id == command.command_id
            ))
            .count(),
        1,
        "the failed attempt must atomically embed exactly one fresh retry authority"
    );
    assert_eq!(
        replay
            .events
            .iter()
            .filter(|event| matches!(
                &event.event,
                CodeWorkflowEventKind::IntentReviewRequested {
                    interaction_id,
                    turn_id,
                    ..
                } if interaction_id == &retry_interaction_id && !turn_id.is_empty()
            ))
            .count(),
        1,
        "the embedded retry authority must gain exactly one durable runtime-turn binding"
    );
    libra::internal::ai::runtime::phase1::validate_single_open_gate_authority(&fixture.goal_store)
        .expect("duplicate old Start must leave exactly one healthy retry authority");
}

async fn abort_confirm_at_start_enqueue() -> (
    OversizedPhase1ConfirmFixture,
    HeadlessRecordUserMessageHook,
    String,
) {
    abort_confirm_at_start_enqueue_with_persistence(false).await
}

async fn abort_confirm_at_start_enqueue_with_persistence(
    retain_persistence: bool,
) -> (
    OversizedPhase1ConfirmFixture,
    HeadlessRecordUserMessageHook,
    String,
) {
    let start_enqueued = HeadlessRecordUserMessageHook::new();
    let fixture = oversized_phase1_confirm_fixture_with_hooks(
        Some(start_enqueued.clone()),
        None,
        None,
        retain_persistence,
    )
    .await;
    let source_interaction_id = fixture.source_interaction_id.clone();
    let runtime = Arc::clone(&fixture.runtime);
    let first = tokio::spawn(async move {
        runtime
            .respond_interaction(
                &source_interaction_id,
                CodeUiInteractionResponse {
                    selected_option: Some("confirm".to_string()),
                    ..Default::default()
                },
            )
            .await
    });
    tokio::time::timeout(Duration::from_secs(5), start_enqueued.wait_until_entered())
        .await
        .expect("Confirm must pause after its Start command is enqueued");
    let phase1_turn_id = phase1_turn_id_from_seed(
        &load_phase1_start_seed(&fixture.goal_store)
            .expect("load enqueued Confirm seed")
            .expect("enqueued Confirm must retain its start seed"),
    )
    .expect("derive enqueued Confirm turn id");
    first.abort();
    assert!(
        first
            .await
            .expect_err("the browser response future must abort at the Start hook")
            .is_cancelled()
    );
    (fixture, start_enqueued, phase1_turn_id)
}

#[tokio::test(flavor = "current_thread")]
async fn aborted_confirm_after_start_enqueue_then_control_cancel_never_admits_phase1() {
    let (fixture, start_enqueued, phase1_turn_id) = abort_confirm_at_start_enqueue().await;
    fixture
        .runtime
        .cancel_turn()
        .await
        .expect("Cancel must consume the pre-admission Confirm owner");
    start_enqueued.release();
    tokio::time::sleep(Duration::from_millis(100)).await;

    let replay = fixture
        .goal_store
        .load_code_workflow_replay()
        .expect("read cancelled pre-admission Confirm workflow");
    assert!(replay.events.iter().all(|event| !matches!(
        &event.event,
        CodeWorkflowEventKind::CommandIntentPersisted { command }
            if command.identity.command_id == phase1_turn_id
    )));
    assert!(
        load_phase1_start_seed(&fixture.goal_store)
            .expect("load cancelled Confirm seed")
            .is_none(),
        "control Cancel must consume the pre-admission start seed"
    );
    assert_eq!(
        fixture.runtime.snapshot().await.status,
        CodeUiSessionStatus::Idle
    );
}

#[tokio::test(flavor = "current_thread")]
async fn aborted_duplicate_starts_after_control_cancel_skip_redundant_snapshot_persist() {
    let (mut fixture, start_enqueued, phase1_turn_id) =
        abort_confirm_at_start_enqueue_with_persistence(true).await;
    let source_interaction_id = fixture.source_interaction_id.clone();
    let runtime = Arc::clone(&fixture.runtime);
    let mut second = tokio::spawn(async move {
        runtime
            .respond_interaction(
                &source_interaction_id,
                CodeUiInteractionResponse {
                    selected_option: Some("confirm".to_string()),
                    ..Default::default()
                },
            )
            .await
    });
    assert!(
        tokio::time::timeout(Duration::from_millis(100), &mut second)
            .await
            .is_err(),
        "the same-source retry must remain queued behind the paused first Start"
    );
    second.abort();
    assert!(
        second
            .await
            .expect_err("the second browser retry must abort while queued")
            .is_cancelled()
    );

    let persistence = fixture
        .persistence
        .as_ref()
        .expect("fault regression retains the persistence handle")
        .clone();
    persistence.fail_snapshot_persist_after_successes_for_test(1);
    assert_eq!(persistence.snapshot_persist_failure_countdown_for_test(), 2);
    fixture
        .runtime
        .cancel_turn()
        .await
        .expect("control Cancel must durably settle both queued Start owners");
    assert_eq!(
        persistence.snapshot_persist_failure_countdown_for_test(),
        1,
        "the acknowledged primary Cancel closeout must consume one successful persist"
    );

    start_enqueued.release();
    tokio::time::sleep(Duration::from_millis(200)).await;
    assert_eq!(
        persistence.snapshot_persist_failure_countdown_for_test(),
        1,
        "late settlement for two aborted Starts must not attempt another snapshot persist"
    );
    let snapshot = fixture.runtime.snapshot().await;
    assert_eq!(snapshot.status, CodeUiSessionStatus::Idle);
    assert!(snapshot.tool_calls.iter().all(|tool_call| {
        tool_call.id != "oversized-plan-draft" && tool_call.id != "bounded-plan-draft"
    }));
    assert!(
        load_phase1_start_seed(&fixture.goal_store)
            .unwrap()
            .is_none()
    );
    let replay = fixture.goal_store.load_code_workflow_replay().unwrap();
    assert!(replay.events.iter().all(|event| !matches!(
        &event.event,
        CodeWorkflowEventKind::CommandIntentPersisted { command }
            if command.identity.command_id == phase1_turn_id
    )));
    assert!(replay.events.iter().all(|event| !matches!(
        &event.event,
        CodeWorkflowEventKind::CommandIndeterminateSideEffect { .. }
            | CodeWorkflowEventKind::Phase1FormalWriteStarted { .. }
    )));

    persistence.clear_snapshot_persist_failure_for_test();
    fixture
        .runtime
        .shutdown()
        .await
        .expect("settled duplicate Starts must allow clean shutdown");
    drop(fixture.runtime);
    drop(fixture.persistence.take());
    drop(persistence);
    tokio::time::sleep(Duration::from_millis(50)).await;
    let state = fixture.store.load(&fixture.session_id).unwrap();
    let restored_persistence = HeadlessSessionPersistence::new(fixture.store.clone(), state)
        .expect("reacquire duplicate-Start session after Cancel");
    let restored = build_runtime_with_persistence(
        "basic_chat",
        fixture.workdir.path().to_path_buf(),
        Vec::new(),
        Some(restored_persistence),
    )
    .await
    .0;
    assert_eq!(restored.snapshot().await.status, CodeUiSessionStatus::Idle);
    restored
        .submit_message("/new turn after two cancelled Starts".to_string())
        .await
        .expect("restart must admit new work after the settled Cancel");
}

#[tokio::test(flavor = "current_thread")]
async fn aborted_confirm_after_start_enqueue_then_shutdown_preserves_recoverable_seed() {
    let (fixture, start_enqueued, phase1_turn_id) = abort_confirm_at_start_enqueue().await;
    tokio::time::timeout(Duration::from_secs(5), fixture.runtime.shutdown())
        .await
        .expect("shutdown must not wait for the paused Phase 1 coordinator")
        .expect("shutdown must preserve the pre-admission Confirm authority");
    start_enqueued.release();
    tokio::time::sleep(Duration::from_millis(100)).await;

    let replay = fixture
        .goal_store
        .load_code_workflow_replay()
        .expect("read shutdown pre-admission Confirm workflow");
    assert!(replay.events.iter().all(|event| !matches!(
        &event.event,
        CodeWorkflowEventKind::CommandIntentPersisted { command }
            if command.identity.command_id == phase1_turn_id
    )));
    assert!(
        load_phase1_start_seed(&fixture.goal_store)
            .expect("load shutdown-preserved Confirm seed")
            .is_some(),
        "shutdown must leave the durable seed for startup recovery"
    );
    assert_ne!(
        fixture.runtime.snapshot().await.status,
        CodeUiSessionStatus::IndeterminateSideEffect
    );
}

#[tokio::test(flavor = "current_thread")]
async fn aborted_confirm_after_start_enqueue_then_different_choice_is_conflict_not_fence() {
    let (fixture, start_enqueued, _phase1_turn_id) = abort_confirm_at_start_enqueue().await;
    let conflict = fixture
        .runtime
        .respond_interaction(
            &fixture.source_interaction_id,
            CodeUiInteractionResponse {
                selected_option: Some("cancel".to_string()),
                ..Default::default()
            },
        )
        .await
        .expect_err("a different retry choice must not replace durable Confirm");
    let conflict = conflict
        .downcast_ref::<CodeUiApiError>()
        .expect("different durable retry choice must be a typed Web conflict");
    assert_eq!(conflict.status, 409);
    assert_eq!(conflict.code, "INTERACTION_NOT_ACTIVE");
    assert_ne!(
        fixture.runtime.snapshot().await.status,
        CodeUiSessionStatus::IndeterminateSideEffect
    );
    start_enqueued.release();
}

fn is_canonical_intent_revision_digest(value: &str) -> bool {
    value.len() == "hmac-sha256:".len() + 64
        && value.starts_with("hmac-sha256:")
        && value["hmac-sha256:".len()..]
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
}

fn assert_raw_note_absent_from_workflow_and_sse(
    goal_store: &SessionJsonlStore,
    raw_note_sentinel: &str,
) {
    let mut workflow_events = 0usize;
    for event in goal_store
        .load_events()
        .expect("load session events for raw-note boundary audit")
    {
        let SessionEvent::CodeWorkflow(workflow) = event else {
            continue;
        };
        workflow_events += 1;
        let durable_wire = serde_json::to_string(&SessionEvent::CodeWorkflow(workflow.clone()))
            .expect("serialize durable CodeWorkflow event");
        assert!(
            !durable_wire.contains(raw_note_sentinel),
            "raw Modify note leaked into durable CodeWorkflow event: {durable_wire}"
        );
        let sse = CodeUiWireV2Event::from_workflow_event(&workflow);
        let sse_wire = serde_json::to_string(&sse).expect("serialize workflow-v2 SSE event");
        assert!(
            !sse_wire.contains(raw_note_sentinel),
            "raw Modify note leaked into workflow-v2 SSE event: {sse_wire}"
        );
    }
    assert!(
        workflow_events > 0,
        "raw-note audit requires workflow events"
    );
}

async fn assert_initial_phase0_terminal_resolution_is_atomic(decision: &str) {
    let fixture = oversized_phase1_confirm_fixture(None).await;
    let OversizedPhase1ConfirmFixture {
        workdir,
        _storage,
        store,
        session_id,
        goal_store,
        persistence: _,
        runtime,
        source_interaction_id,
        risk_interaction_id: _,
        risk_pending_state: _,
    } = fixture;
    runtime
        .respond_interaction(
            &source_interaction_id,
            CodeUiInteractionResponse {
                selected_option: Some(decision.to_string()),
                note: (decision == "modify").then(|| "tighten the retry contract".to_string()),
                ..Default::default()
            },
        )
        .await
        .unwrap_or_else(|error| panic!("initial Phase 0 {decision} must be accepted: {error:#}"));

    let deadline = std::time::Instant::now() + Duration::from_secs(8);
    loop {
        let replay = goal_store
            .load_code_workflow_replay()
            .expect("read Phase 0 terminal workflow");
        let old_source_open =
            open_intent_review_from_workflow(replay.events.iter().map(|event| &event.event))
                .is_some_and(|(interaction_id, ..)| interaction_id == source_interaction_id);
        let settled = match decision {
            "confirm" => runtime
                .snapshot()
                .await
                .interactions
                .iter()
                .any(|interaction| {
                    interaction.kind == CodeUiInteractionKind::IntentReviewChoice
                        && interaction.status == CodeUiInteractionStatus::Pending
                        && interaction.id != source_interaction_id
                }),
            "modify" | "cancel" => runtime.snapshot().await.status == CodeUiSessionStatus::Idle,
            _ => unreachable!("unsupported Phase 0 decision"),
        };
        if !old_source_open && settled {
            break;
        }
        assert!(
            std::time::Instant::now() < deadline,
            "initial Phase 0 {decision} did not durably settle its source generation"
        );
        tokio::time::sleep(Duration::from_millis(20)).await;
    }

    let replay = goal_store
        .load_code_workflow_replay()
        .expect("read the settled Phase 0 workflow");
    let atomic = replay
        .events
        .iter()
        .filter_map(|event| match &event.event {
            CodeWorkflowEventKind::CommandTerminalSuccessWithInteractionResolved {
                command,
                interaction_id,
                resolution,
                prior_interaction_resolutions,
                intent_revision,
                ..
            } if interaction_id == &source_interaction_id => Some((
                command.clone(),
                resolution.clone(),
                prior_interaction_resolutions.clone(),
                intent_revision.clone(),
            )),
            _ => None,
        })
        .collect::<Vec<_>>();
    assert_eq!(
        atomic.len(),
        1,
        "initial Phase 0 {decision} must terminalize its command and source resolution in one durable row"
    );
    assert_eq!(atomic[0].1, decision);
    match (decision, atomic[0].3.as_ref()) {
        ("modify", Some(recovery)) => {
            assert_eq!(recovery.interaction_id, source_interaction_id);
            assert!(is_canonical_intent_revision_digest(
                &recovery.sidecar_digest
            ));
        }
        ("modify", None) => panic!("Modify must commit its digest-only sidecar binding"),
        (_, None) => {}
        (_, Some(_)) => panic!("only Modify may commit a revision sidecar binding"),
    }
    if decision == "modify" {
        assert_raw_note_absent_from_workflow_and_sse(&goal_store, "tighten the retry contract");
    }
    assert_eq!(
        atomic[0]
            .2
            .iter()
            .filter(|(_, resolution)| resolution == "answered")
            .count(),
        1,
        "initial Phase 0 {decision} must retain the real risk-profile response in the same terminal row"
    );
    let intent_marker = replay
        .events
        .iter()
        .find_map(|event| match &event.event {
            marker @ CodeWorkflowEventKind::IntentReviewRequested { interaction_id, .. }
                if interaction_id == &source_interaction_id =>
            {
                Some(marker.clone())
            }
            _ => None,
        })
        .expect("initial Intent marker must precede its terminal resolution");
    let combined = replay
        .events
        .iter()
        .find_map(|event| match &event.event {
            terminal @ CodeWorkflowEventKind::CommandTerminalSuccessWithInteractionResolved {
                interaction_id,
                ..
            } if interaction_id == &source_interaction_id => Some(terminal.clone()),
            _ => None,
        })
        .expect("combined terminal row must remain replayable");
    let mut legacy_value = serde_json::to_value(combined).unwrap();
    legacy_value
        .as_object_mut()
        .expect("workflow event kind is a tagged object")
        .remove("prior_interaction_resolutions");
    legacy_value
        .as_object_mut()
        .expect("workflow event kind is a tagged object")
        .remove("intent_revision");
    let legacy_terminal: CodeWorkflowEventKind = serde_json::from_value(legacy_value)
        .expect("an older reader may ignore the additive prior resolutions");
    assert!(
        open_intent_review_from_workflow([&intent_marker, &legacy_terminal].into_iter()).is_none(),
        "the legacy primary resolution fields alone must close the Intent authority"
    );
    assert!(
        replay.events.iter().all(|event| !matches!(
            &event.event,
            CodeWorkflowEventKind::InteractionResolved { interaction_id, .. }
                if interaction_id == &source_interaction_id
        )),
        "Web closeout must not append a second standalone source resolution for {decision}"
    );
    assert_eq!(
        replay
            .events
            .iter()
            .filter(|event| matches!(
                &event.event,
                CodeWorkflowEventKind::CommandTerminalSuccess { command, .. }
                    | CodeWorkflowEventKind::CommandTerminalSuccessWithInteractionResolved {
                        command,
                        ..
                    } if command == &atomic[0].0
            ))
            .count(),
        1,
        "the Phase 0 command must have exactly one terminal-success row"
    );

    drop(runtime);
    tokio::time::sleep(Duration::from_millis(50)).await;
    let restored_state = store
        .load(&session_id)
        .expect("load Phase 0 terminal session for startup replay");
    let persistence = HeadlessSessionPersistence::new(store.clone(), restored_state)
        .expect("reacquire the dropped Phase 0 session writer lease");
    let restored = build_runtime_with_persistence(
        "basic_chat",
        workdir.path().to_path_buf(),
        Vec::new(),
        Some(persistence),
    )
    .await
    .0;
    tokio::time::sleep(Duration::from_millis(50)).await;
    let snapshot = restored.snapshot().await;
    assert!(
        snapshot.interactions.iter().all(|interaction| {
            interaction.id != source_interaction_id
                || interaction.status != CodeUiInteractionStatus::Pending
        }),
        "startup must not revive the terminal Phase 0 {decision} source Intent gate"
    );
    let replay = goal_store
        .load_code_workflow_replay()
        .expect("read workflow after Phase 0 startup replay");
    assert!(
        open_intent_review_from_workflow(replay.events.iter().map(|event| &event.event))
            .is_none_or(|(interaction_id, ..)| interaction_id != source_interaction_id),
        "startup workflow scan must keep the terminal {decision} source generation closed"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn initial_phase0_confirm_terminal_resolution_is_atomic_and_not_revived() {
    assert_initial_phase0_terminal_resolution_is_atomic("confirm").await;
}

#[tokio::test(flavor = "multi_thread")]
async fn initial_phase0_modify_terminal_resolution_is_atomic_and_not_revived() {
    assert_initial_phase0_terminal_resolution_is_atomic("modify").await;
}

#[tokio::test(flavor = "multi_thread")]
async fn initial_phase0_cancel_terminal_resolution_is_atomic_and_not_revived() {
    assert_initial_phase0_terminal_resolution_is_atomic("cancel").await;
}

fn pending_intent_revision_path(store: &SessionStore, session_id: &str) -> PathBuf {
    store
        .session_root(session_id)
        .join("intents/pending_revision.json")
}

fn intent_revision_hmac_key_path(store: &SessionStore, session_id: &str) -> PathBuf {
    store
        .session_root(session_id)
        .join("intents/revision_hmac.key")
}

async fn intent_revision_restart_error(
    store: &Arc<SessionStore>,
    session_id: &str,
    working_dir: &Path,
) -> String {
    let state = store
        .load(session_id)
        .expect("load malformed revision restart fixture");
    let persistence = HeadlessSessionPersistence::new(store.clone(), state)
        .expect("reacquire malformed revision restart fixture lease");
    match try_build_runtime_with_persistence("basic_chat", working_dir.to_path_buf(), persistence)
        .await
    {
        Ok(runtime) => {
            drop(runtime);
            panic!("malformed revision durability fixture must fail closed")
        }
        Err(error) => format!("{error:#}"),
    }
}

struct RealBoundIntentRevisionFixture {
    fixture: OversizedPhase1ConfirmFixture,
    active_sidecar: serde_json::Value,
    terminal: CodeWorkflowEvent,
    source_command: CodeCommandIdentity,
    summary: String,
    recovery: IntentRevisionRecovery,
    intent_id: String,
    hmac_key: Vec<u8>,
}

async fn real_bound_intent_revision_fixture(note: &str) -> RealBoundIntentRevisionFixture {
    let fixture = oversized_phase1_confirm_fixture(None).await;
    fixture
        .runtime
        .respond_interaction(
            &fixture.source_interaction_id,
            CodeUiInteractionResponse {
                selected_option: Some("modify".to_string()),
                note: Some(note.to_string()),
                ..Default::default()
            },
        )
        .await
        .expect("create a real digest-bound Modify terminal and Active sidecar");
    let active_sidecar: serde_json::Value = serde_json::from_slice(
        &std::fs::read(pending_intent_revision_path(
            &fixture.store,
            &fixture.session_id,
        ))
        .expect("read real bound Active sidecar"),
    )
    .expect("real bound Active sidecar must be valid JSON");
    let replay = fixture.goal_store.load_code_workflow_replay().unwrap();
    let (terminal, source_command, summary, recovery) = replay
        .events
        .iter()
        .find_map(|event| match &event.event {
            CodeWorkflowEventKind::CommandTerminalSuccessWithInteractionResolved {
                command,
                summary,
                interaction_id,
                resolution,
                intent_revision: Some(recovery),
                ..
            } if interaction_id == &fixture.source_interaction_id && resolution == "modify" => {
                Some((
                    event.clone(),
                    command.clone(),
                    summary.clone(),
                    recovery.clone(),
                ))
            }
            _ => None,
        })
        .expect("real bound Modify terminal must remain replayable");
    let intent_id = active_sidecar["authority"]["intentId"]
        .as_str()
        .expect("Active authority must carry intentId")
        .to_string();
    let hmac_key = std::fs::read(intent_revision_hmac_key_path(
        &fixture.store,
        &fixture.session_id,
    ))
    .expect("read real revision HMAC key");
    RealBoundIntentRevisionFixture {
        fixture,
        active_sidecar,
        terminal,
        source_command,
        summary,
        recovery,
        intent_id,
        hmac_key,
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn active_revision_blocks_new_direct_but_preserves_exact_terminal_retry() {
    use sha2::Digest as _;

    let bound = real_bound_intent_revision_fixture(
        "retain the private revision while direct retries are checked",
    )
    .await;
    let command_id = "completed-direct-before-retry";
    let input = "/already completed direct command";
    let direct = CodeCommandIntent::new(
        CodeCommandIdentity::new(
            bound.source_command.repo_id.clone(),
            bound.source_command.session_id.clone(),
            bound.source_command.principal_id.clone(),
            command_id,
        ),
        CODE_UI_WEB_TURN_KIND,
        format!(
            "sha256:{}",
            hex::encode(sha2::Sha256::digest(input.as_bytes()))
        ),
        true,
    );
    bound
        .fixture
        .goal_store
        .admit_code_command(direct.clone())
        .expect("seed the exact historical direct command");
    bound
        .fixture
        .goal_store
        .complete_code_command_success(&direct.identity, "historical direct command complete")
        .expect("terminalize the historical direct command");
    let sidecar_path =
        pending_intent_revision_path(&bound.fixture.store, &bound.fixture.session_id);
    let sidecar_before = std::fs::read(&sidecar_path).expect("read Active revision sidecar");
    let replay_before = bound
        .fixture
        .goal_store
        .load_code_workflow_replay()
        .expect("read workflow before direct checks");

    let blocked = bound
        .fixture
        .runtime
        .submit_message("/new direct command".to_string())
        .await
        .expect_err("an Active revision must block a new explicit direct command");
    assert_code_ui_api_error(&blocked, 409, "SESSION_BUSY");
    bound
        .fixture
        .runtime
        .submit_message_with_command_id(input.to_string(), Some(command_id.to_string()))
        .await
        .expect("an exact terminal retry remains an idempotent acknowledgement");

    assert_eq!(
        std::fs::read(&sidecar_path).expect("revision sidecar remains readable"),
        sidecar_before,
        "neither a blocked direct command nor an exact retry may alter revision authority"
    );
    let replay_after = bound
        .fixture
        .goal_store
        .load_code_workflow_replay()
        .expect("read workflow after direct checks");
    assert_eq!(
        replay_after.events.len(),
        replay_before.events.len(),
        "direct rejection and terminal retry must append no workflow rows"
    );
    assert_eq!(
        replay_after
            .events
            .iter()
            .filter(|event| matches!(
                &event.event,
                CodeWorkflowEventKind::CommandIntentPersisted { command }
                    if command.identity.command_id == command_id
            ))
            .count(),
        1
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn oversized_plain_intent_revision_note_is_typed_400_without_claiming() {
    let bound = real_bound_intent_revision_fixture(
        "retain the private revision when an oversized plain follow-up is rejected",
    )
    .await;
    let sidecar_path =
        pending_intent_revision_path(&bound.fixture.store, &bound.fixture.session_id);
    let sidecar_before =
        std::fs::read(&sidecar_path).expect("read Active revision before oversize");
    let replay_before = bound
        .fixture
        .goal_store
        .load_code_workflow_replay()
        .unwrap();
    let error = bound
        .fixture
        .runtime
        .submit_message_with_command_id(
            "x".repeat(MAX_INTENT_REVISION_NOTE_BYTES + 1),
            Some("oversized-plain-intent-modify".to_string()),
        )
        .await
        .expect_err(
            "oversized plain IntentSpec revision note must fail before Claiming or Runtime admission",
        );
    assert_code_ui_api_error(&error, 400, "INVALID_QUERY_PARAM");
    assert_eq!(std::fs::read(&sidecar_path).unwrap(), sidecar_before);
    assert_eq!(
        bound
            .fixture
            .goal_store
            .load_code_workflow_replay()
            .unwrap()
            .events
            .len(),
        replay_before.events.len(),
        "oversized plain IntentSpec revision note must append no workflow rows"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn slash_intent_modify_consumes_the_active_revision() {
    let bound = real_bound_intent_revision_fixture(
        "retain the private revision until the canonical slash command consumes it",
    )
    .await;
    let sidecar_path =
        pending_intent_revision_path(&bound.fixture.store, &bound.fixture.session_id);
    let sidecar_before = std::fs::read(&sidecar_path).expect("read Active revision before guards");
    let replay_before = bound
        .fixture
        .goal_store
        .load_code_workflow_replay()
        .unwrap();
    let oversized = format!(
        "/intent modify {}",
        "x".repeat(MAX_INTENT_REVISION_NOTE_BYTES + 1)
    );
    let error = bound
        .fixture
        .runtime
        .submit_message_with_command_id(
            oversized,
            Some("oversized-slash-intent-modify".to_string()),
        )
        .await
        .expect_err("oversized slash Modify must fail before Claiming or Runtime admission");
    assert_code_ui_api_error(&error, 400, "INVALID_QUERY_PARAM");
    assert_eq!(std::fs::read(&sidecar_path).unwrap(), sidecar_before);
    assert_eq!(
        bound
            .fixture
            .goal_store
            .load_code_workflow_replay()
            .unwrap()
            .events
            .len(),
        replay_before.events.len(),
        "oversized slash Modify must append no workflow rows"
    );
    let command_id = "slash-intent-modify-consumer";
    let input = "/intent modify narrow the scope to README examples";

    bound
        .fixture
        .runtime
        .submit_message_with_command_id(input.to_string(), Some(command_id.to_string()))
        .await
        .expect("canonical /intent modify must enter the active revision consumer");

    let first_identity = CodeCommandIdentity::new(
        bound.source_command.repo_id.clone(),
        bound.source_command.session_id.clone(),
        bound.source_command.principal_id.clone(),
        command_id,
    );
    let deadline = std::time::Instant::now() + Duration::from_secs(5);
    loop {
        let status = bound
            .fixture
            .goal_store
            .code_command_intent_status(&first_identity)
            .expect("read slash Modify command status")
            .map(|(_, status)| status);
        if matches!(status, Some(CodeCommandStatus::Indeterminate { .. }))
            && bound.fixture.runtime.snapshot().await.status
                == CodeUiSessionStatus::IndeterminateSideEffect
        {
            break;
        }
        assert!(
            std::time::Instant::now() < deadline,
            "prose-only slash Modify must fail closed before consuming the Active revision"
        );
        tokio::time::sleep(Duration::from_millis(20)).await;
    }

    let replay_after_prose = bound
        .fixture
        .goal_store
        .load_code_workflow_replay()
        .expect("read prose-only slash Modify workflow");
    assert!(sidecar_path.exists());
    assert!(replay_after_prose.events.iter().all(|event| !matches!(
        &event.event,
        CodeWorkflowEventKind::InteractionResolved {
            intent_revision_consumption: Some(consumption),
            ..
        } if consumption.claim.consumer_intent.identity == first_identity
    )));

    let OversizedPhase1ConfirmFixture {
        workdir,
        _storage,
        store,
        session_id,
        goal_store,
        persistence,
        runtime,
        ..
    } = bound.fixture;
    drop(runtime);
    drop(persistence);
    tokio::time::sleep(Duration::from_millis(50)).await;
    let state = store
        .load(&session_id)
        .expect("load prose-only slash Modify session for recovery");
    let persistence = HeadlessSessionPersistence::new(store.clone(), state)
        .expect("reacquire prose-only slash Modify session lease");
    let revised = build_runtime_with_persistence(
        "phase0_intent_review",
        workdir.path().to_path_buf(),
        Vec::new(),
        Some(persistence),
    )
    .await
    .0;
    assert_eq!(revised.snapshot().await.status, CodeUiSessionStatus::Idle);

    let retry_command_id = "slash-intent-modify-retry";
    revised
        .submit_message_with_command_id(input.to_string(), Some(retry_command_id.to_string()))
        .await
        .expect("recovered slash Modify must admit a new provider attempt");
    let risk_id = await_pending_interaction(
        &revised,
        CodeUiInteractionKind::RequestUserInput,
        "slash Modify retry must ask for risk before submitting the replacement draft",
    )
    .await;
    revised
        .respond_interaction(
            &risk_id,
            CodeUiInteractionResponse {
                selected_option: Some("Low".to_string()),
                ..Default::default()
            },
        )
        .await
        .expect("slash Modify retry risk answer must be accepted");
    let revised_review = await_pending_interaction(
        &revised,
        CodeUiInteractionKind::IntentReviewChoice,
        "slash Modify retry must park a replacement IntentReviewChoice",
    )
    .await;
    assert!(!sidecar_path.exists());
    let replay = goal_store.load_code_workflow_replay().unwrap();
    assert_eq!(
        replay
            .events
            .iter()
            .filter(|event| matches!(
                &event.event,
                CodeWorkflowEventKind::InteractionResolved {
                    intent_revision_consumption: Some(consumption),
                    ..
                } if consumption.claim.consumer_intent.identity.command_id == retry_command_id
            ))
            .count(),
        1,
        "successful retry must commit one revision receipt"
    );
    revised
        .respond_interaction(
            &revised_review,
            CodeUiInteractionResponse {
                selected_option: Some("cancel".to_string()),
                ..Default::default()
            },
        )
        .await
        .expect("replacement review Cancel must settle the slash Modify command");
    let deadline = std::time::Instant::now() + Duration::from_secs(5);
    while revised.snapshot().await.status != CodeUiSessionStatus::Idle {
        assert!(
            std::time::Instant::now() < deadline,
            "replacement review Cancel must settle before restart"
        );
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    drop(revised);
    tokio::time::sleep(Duration::from_millis(50)).await;
    let state = store
        .load(&session_id)
        .expect("load the closed slash Modify replacement session");
    let persistence = HeadlessSessionPersistence::new(store, state)
        .expect("reacquire the closed slash Modify replacement session");
    let restarted =
        try_build_runtime_with_persistence("basic_chat", workdir.path().to_path_buf(), persistence)
            .await
            .expect("closed replacement review must keep prior retry attempts exempt on restart");
    assert_eq!(restarted.snapshot().await.status, CodeUiSessionStatus::Idle);
    restarted
        .submit_message("/fresh direct turn after replacement review".to_string())
        .await
        .expect("a fresh direct turn must be admitted after the replacement gate closes");
}

#[tokio::test(flavor = "multi_thread")]
async fn slash_intent_modify_provider_prompt_uses_only_the_change_suffix() {
    let bound = real_bound_intent_revision_fixture(
        "retain the private source revision while inspecting the slash Modify prompt",
    )
    .await;
    let OversizedPhase1ConfirmFixture {
        workdir,
        _storage,
        store,
        session_id,
        persistence,
        runtime,
        ..
    } = bound.fixture;
    drop(runtime);
    drop(persistence);
    tokio::time::sleep(Duration::from_millis(50)).await;
    let state = store
        .load(&session_id)
        .expect("load slash Modify prompt-capture session");
    let persistence = HeadlessSessionPersistence::new(store, state)
        .expect("reacquire slash Modify prompt-capture session");
    let provider_entered = Arc::new(Notify::new());
    let provider_entered_wait = provider_entered.notified();
    tokio::pin!(provider_entered_wait);
    provider_entered_wait.as_mut().enable();
    let captured_history = Arc::new(std::sync::Mutex::new(None));
    let runtime = build_pending_completion_runtime_with_capture(
        workdir.path().to_path_buf(),
        persistence,
        Arc::clone(&provider_entered),
        Some(Arc::clone(&captured_history)),
    )
    .await;
    let change_suffix = "CAPTURE_SUFFIX narrow scope to README examples only";
    runtime
        .submit_message_with_command_id(
            format!("/intent modify {change_suffix}"),
            Some("slash-intent-modify-prompt-capture".to_string()),
        )
        .await
        .expect("slash Modify prompt-capture command admitted");
    tokio::time::timeout(Duration::from_secs(5), &mut provider_entered_wait)
        .await
        .expect("slash Modify prompt-capture command reaches the provider");
    let provider_history = captured_history
        .lock()
        .expect("read slash Modify provider capture")
        .clone()
        .expect("slash Modify provider history was captured");
    assert!(provider_history.contains(change_suffix));
    assert!(
        !provider_history.contains("/intent modify"),
        "the control prefix must not be copied into the provider revision request"
    );
    runtime
        .cancel_turn()
        .await
        .expect("cancel the deliberately pending slash Modify provider");
}

#[tokio::test(flavor = "multi_thread")]
async fn slash_intent_modify_multiple_successful_drafts_preserve_the_revision() {
    let bound = real_bound_intent_revision_fixture(
        "retain the source revision when a provider submits multiple replacement drafts",
    )
    .await;
    let source_command = bound.source_command.clone();
    let sidecar_path =
        pending_intent_revision_path(&bound.fixture.store, &bound.fixture.session_id);
    let OversizedPhase1ConfirmFixture {
        workdir,
        _storage,
        store,
        session_id,
        goal_store,
        persistence,
        runtime,
        ..
    } = bound.fixture;
    drop(runtime);
    drop(persistence);
    tokio::time::sleep(Duration::from_millis(50)).await;

    let state = store
        .load(&session_id)
        .expect("load multiple-draft slash Modify session");
    let persistence = HeadlessSessionPersistence::new(store.clone(), state)
        .expect("reacquire multiple-draft slash Modify session");
    let runtime = build_runtime_with_persistence(
        "phase0_intent_review_multiple_drafts",
        workdir.path().to_path_buf(),
        Vec::new(),
        Some(persistence),
    )
    .await
    .0;
    let command_id = "slash-intent-modify-multiple-drafts";
    runtime
        .submit_message_with_command_id(
            "/intent modify keep exactly one replacement".to_string(),
            Some(command_id.to_string()),
        )
        .await
        .expect("multiple-draft slash Modify command admitted");
    let risk_id = await_pending_interaction(
        &runtime,
        CodeUiInteractionKind::RequestUserInput,
        "multiple-draft provider must request a risk selection",
    )
    .await;
    runtime
        .respond_interaction(
            &risk_id,
            CodeUiInteractionResponse {
                selected_option: Some("Low".to_string()),
                ..Default::default()
            },
        )
        .await
        .expect("multiple-draft risk selection accepted");

    let identity = CodeCommandIdentity::new(
        source_command.repo_id,
        source_command.session_id,
        source_command.principal_id,
        command_id,
    );
    let deadline = std::time::Instant::now() + Duration::from_secs(20);
    loop {
        let status = goal_store
            .code_command_intent_status(&identity)
            .expect("read multiple-draft consumer status")
            .map(|(_, status)| status);
        if matches!(status, Some(CodeCommandStatus::Indeterminate { .. }))
            && runtime.snapshot().await.status == CodeUiSessionStatus::IndeterminateSideEffect
        {
            break;
        }
        assert!(
            std::time::Instant::now() < deadline,
            "multiple successful IntentDrafts must fail closed; command={status:?} session={:?}",
            runtime.snapshot().await.status
        );
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    assert!(sidecar_path.exists());
    let replay = goal_store
        .load_code_workflow_replay()
        .expect("load multiple-draft workflow");
    assert!(replay.events.iter().all(|event| !matches!(
        &event.event,
        CodeWorkflowEventKind::InteractionResolved {
            intent_revision_consumption: Some(consumption),
            ..
        } if consumption.claim.consumer_intent.identity == identity
    )));
    assert!(replay.events.iter().all(|event| !matches!(
        &event.event,
        CodeWorkflowEventKind::IntentReviewRequested { phase0_turn_id, .. }
            if phase0_turn_id == command_id
    )));

    drop(runtime);
    tokio::time::sleep(Duration::from_millis(50)).await;
    let state = store
        .load(&session_id)
        .expect("load multiple-draft session for recovery");
    let persistence = HeadlessSessionPersistence::new(store, state)
        .expect("reacquire multiple-draft session for recovery");
    let restarted =
        try_build_runtime_with_persistence("basic_chat", workdir.path().to_path_buf(), persistence)
            .await
            .expect("multiple-draft failure must restore the source revision without fencing");
    assert_eq!(restarted.snapshot().await.status, CodeUiSessionStatus::Idle);
    assert!(sidecar_path.exists());
}

#[tokio::test(flavor = "multi_thread")]
async fn plain_intent_modify_multiple_successful_drafts_preserve_the_revision() {
    let bound = real_bound_intent_revision_fixture(
        "retain the source revision when a plain follow-up submits multiple replacement drafts",
    )
    .await;
    let source_command = bound.source_command.clone();
    let sidecar_path =
        pending_intent_revision_path(&bound.fixture.store, &bound.fixture.session_id);
    let OversizedPhase1ConfirmFixture {
        workdir,
        _storage,
        store,
        session_id,
        goal_store,
        persistence,
        runtime,
        ..
    } = bound.fixture;
    drop(runtime);
    drop(persistence);
    tokio::time::sleep(Duration::from_millis(50)).await;

    let state = store
        .load(&session_id)
        .expect("load multiple-draft plain Modify session");
    let persistence = HeadlessSessionPersistence::new(store.clone(), state)
        .expect("reacquire multiple-draft plain Modify session");
    let runtime = build_runtime_with_persistence(
        "phase0_intent_review_multiple_drafts",
        workdir.path().to_path_buf(),
        Vec::new(),
        Some(persistence),
    )
    .await
    .0;
    let command_id = "plain-intent-modify-multiple-drafts";
    runtime
        .submit_message_with_command_id(
            "keep exactly one replacement".to_string(),
            Some(command_id.to_string()),
        )
        .await
        .expect("multiple-draft plain Modify command admitted");
    let risk_id = await_pending_interaction(
        &runtime,
        CodeUiInteractionKind::RequestUserInput,
        "plain multiple-draft provider must request a risk selection",
    )
    .await;
    runtime
        .respond_interaction(
            &risk_id,
            CodeUiInteractionResponse {
                selected_option: Some("Low".to_string()),
                ..Default::default()
            },
        )
        .await
        .expect("plain multiple-draft risk selection accepted");

    let identity = CodeCommandIdentity::new(
        source_command.repo_id,
        source_command.session_id,
        source_command.principal_id,
        command_id,
    );
    let deadline = std::time::Instant::now() + Duration::from_secs(20);
    loop {
        let status = goal_store
            .code_command_intent_status(&identity)
            .expect("read plain multiple-draft consumer status")
            .map(|(_, status)| status);
        if matches!(status, Some(CodeCommandStatus::Indeterminate { .. }))
            && runtime.snapshot().await.status == CodeUiSessionStatus::IndeterminateSideEffect
        {
            break;
        }
        assert!(
            std::time::Instant::now() < deadline,
            "plain multiple successful IntentDrafts must fail closed; command={status:?} session={:?}",
            runtime.snapshot().await.status
        );
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    assert!(sidecar_path.exists());
    let replay = goal_store
        .load_code_workflow_replay()
        .expect("load plain multiple-draft workflow");
    assert!(replay.events.iter().all(|event| !matches!(
        &event.event,
        CodeWorkflowEventKind::InteractionResolved {
            intent_revision_consumption: Some(consumption),
            ..
        } if consumption.claim.consumer_intent.identity == identity
    )));
    assert!(replay.events.iter().all(|event| !matches!(
        &event.event,
        CodeWorkflowEventKind::IntentReviewRequested { phase0_turn_id, .. }
            if phase0_turn_id == command_id
    )));

    drop(runtime);
    tokio::time::sleep(Duration::from_millis(50)).await;
    let state = store
        .load(&session_id)
        .expect("load plain multiple-draft session for recovery");
    let persistence = HeadlessSessionPersistence::new(store, state)
        .expect("reacquire plain multiple-draft session for recovery");
    let restarted =
        try_build_runtime_with_persistence("basic_chat", workdir.path().to_path_buf(), persistence)
            .await
            .expect(
                "plain multiple-draft failure must restore the source revision without fencing",
            );
    assert_eq!(restarted.snapshot().await.status, CodeUiSessionStatus::Idle);
    assert!(sidecar_path.exists());
}

#[tokio::test(flavor = "multi_thread")]
async fn slash_intent_cancel_durably_exits_revision_and_unblocks_direct_turns() {
    let bound = real_bound_intent_revision_fixture(
        "retain the private revision until the canonical cancel command",
    )
    .await;
    let command_id = "slash-intent-cancel-control";
    let input = "/intent cancel";
    let identity = CodeCommandIdentity::new(
        bound.source_command.repo_id.clone(),
        bound.source_command.session_id.clone(),
        bound.source_command.principal_id.clone(),
        command_id,
    );
    let sidecar_path =
        pending_intent_revision_path(&bound.fixture.store, &bound.fixture.session_id);
    let sidecar_before = std::fs::read(&sidecar_path).expect("read Active revision before guard");
    let replay_before = bound
        .fixture
        .goal_store
        .load_code_workflow_replay()
        .unwrap();
    let error = bound
        .fixture
        .runtime
        .submit_message_with_command_id(
            " /intent cancel ".to_string(),
            Some("noncanonical-padded-intent-cancel".to_string()),
        )
        .await
        .expect_err("padded slash Cancel must not acquire canonical control authority");
    assert_code_ui_api_error(&error, 409, "SESSION_BUSY");
    assert_eq!(std::fs::read(&sidecar_path).unwrap(), sidecar_before);
    assert_eq!(
        bound
            .fixture
            .goal_store
            .load_code_workflow_replay()
            .unwrap()
            .events
            .len(),
        replay_before.events.len(),
        "noncanonical padded Cancel must append no workflow rows"
    );

    bound
        .fixture
        .runtime
        .submit_message_with_command_id(input.to_string(), Some(command_id.to_string()))
        .await
        .expect("canonical /intent cancel must be admitted for an Active revision");

    let deadline = std::time::Instant::now() + Duration::from_secs(5);
    loop {
        let status = bound
            .fixture
            .goal_store
            .code_command_intent_status(&identity)
            .expect("read slash Cancel command status")
            .map(|(_, status)| status);
        if !sidecar_path.exists()
            && matches!(status, Some(CodeCommandStatus::Succeeded { .. }))
            && bound.fixture.runtime.snapshot().await.status == CodeUiSessionStatus::Idle
        {
            break;
        }
        assert!(
            std::time::Instant::now() < deadline,
            "slash Cancel must remove the exact Active sidecar and complete"
        );
        tokio::time::sleep(Duration::from_millis(20)).await;
    }

    let replay_before_retry = bound
        .fixture
        .goal_store
        .load_code_workflow_replay()
        .expect("read workflow before exact slash Cancel retry");
    assert_eq!(
        replay_before_retry
            .events
            .iter()
            .filter(|event| matches!(
                &event.event,
                CodeWorkflowEventKind::InteractionResolved {
                    intent_revision_consumption: Some(consumption),
                    ..
                } if consumption.claim.consumer_intent.identity == identity
            ))
            .count(),
        1,
        "slash Cancel must commit exactly one command-bound revision consumption receipt before unlinking the sidecar"
    );
    bound
        .fixture
        .runtime
        .submit_message_with_command_id(input.to_string(), Some(command_id.to_string()))
        .await
        .expect("exact terminal slash Cancel retry must remain an idempotent acknowledgement");
    assert_eq!(
        bound
            .fixture
            .goal_store
            .load_code_workflow_replay()
            .expect("read workflow after exact slash Cancel retry")
            .events
            .len(),
        replay_before_retry.events.len(),
        "exact slash Cancel retry must append no workflow rows"
    );
    assert!(
        bound
            .fixture
            .runtime
            .snapshot()
            .await
            .transcript
            .iter()
            .any(|entry| {
                entry
                    .content
                    .as_deref()
                    .is_some_and(|content| content.contains("IntentSpec revision mode cancelled"))
            })
    );

    let OversizedPhase1ConfirmFixture {
        workdir,
        _storage,
        store,
        session_id,
        persistence,
        runtime,
        ..
    } = bound.fixture;
    drop(runtime);
    drop(persistence);
    tokio::time::sleep(Duration::from_millis(50)).await;
    let state = store
        .load(&session_id)
        .expect("load slash Cancel session for restart validation");
    let persistence = HeadlessSessionPersistence::new(store, state)
        .expect("reacquire slash Cancel session writer lease");
    let restarted =
        try_build_runtime_with_persistence("basic_chat", workdir.path().to_path_buf(), persistence)
            .await
            .expect("receipt-backed slash Cancel must restart without a missing-sidecar fence");
    assert_eq!(restarted.snapshot().await.status, CodeUiSessionStatus::Idle);
    assert!(restarted.snapshot().await.transcript.iter().all(|entry| {
        entry
            .metadata
            .get("intentRevisionMode")
            .and_then(serde_json::Value::as_bool)
            != Some(true)
            || entry
                .metadata
                .get("restored")
                .and_then(serde_json::Value::as_bool)
                != Some(true)
    }));

    restarted
        .submit_message("/fresh direct turn after revision cancel".to_string())
        .await
        .expect("explicit direct turns must remain available after slash Cancel and restart");
}

#[derive(Clone, Copy)]
enum IntentCancelCrashSeam {
    ConsumingSidecar,
    MissingSidecar,
    TerminalBeforeProjection,
    AcknowledgementPersistedIndeterminate,
    SucceededWithStaleNonStreamingAcknowledgement,
    PendingReceiptFollowedByLaterWeb,
}

async fn assert_intent_cancel_receipt_crash_recovers(seam: IntentCancelCrashSeam) {
    use sha2::Digest as _;

    let bound = real_bound_intent_revision_fixture("cancel receipt crash recovery source").await;
    let command_id = match seam {
        IntentCancelCrashSeam::ConsumingSidecar => "cancel-crash-consuming",
        IntentCancelCrashSeam::MissingSidecar => "cancel-crash-missing-sidecar",
        IntentCancelCrashSeam::TerminalBeforeProjection => "cancel-crash-after-terminal",
        IntentCancelCrashSeam::AcknowledgementPersistedIndeterminate => {
            "cancel-crash-ack-indeterminate"
        }
        IntentCancelCrashSeam::SucceededWithStaleNonStreamingAcknowledgement => {
            "cancel-crash-success-stale-ack"
        }
        IntentCancelCrashSeam::PendingReceiptFollowedByLaterWeb => {
            "cancel-crash-pending-before-later-web"
        }
    };
    let consumer_intent = CodeCommandIntent::new(
        CodeCommandIdentity::new(
            bound.source_command.repo_id.clone(),
            bound.source_command.session_id.clone(),
            bound.source_command.principal_id.clone(),
            command_id,
        ),
        CODE_UI_WEB_TURN_KIND,
        format!(
            "sha256:{}",
            hex::encode(sha2::Sha256::digest(b"/intent cancel"))
        ),
        true,
    );
    assert!(matches!(
        bound
            .fixture
            .goal_store
            .admit_code_command(consumer_intent.clone())
            .unwrap(),
        CodeCommandAdmission::Execute { .. }
    ));
    let claim = IntentRevisionConsumptionClaim {
        schema_version: INTENT_REVISION_CONSUMPTION_SCHEMA_VERSION,
        interaction_id: bound.recovery.interaction_id.clone(),
        source_command: bound.source_command.clone(),
        consumer_intent: consumer_intent.clone(),
        terminal_event_id: bound.terminal.event_id,
        terminal_sequence: bound.terminal.sequence,
        intent_id: bound.intent_id.clone(),
        sidecar_digest: Some(bound.recovery.sidecar_digest.clone()),
    };
    let consumption: IntentRevisionConsumption = bound
        .fixture
        .goal_store
        .prepare_intent_revision_consumption(&consumer_intent, &claim)
        .expect("prepare canonical slash Cancel consumption");
    let sidecar_path =
        pending_intent_revision_path(&bound.fixture.store, &bound.fixture.session_id);
    let consuming_envelope = serde_json::json!({
        "intentSpec": "",
        "consuming": {
            "schemaVersion": INTENT_REVISION_CONSUMPTION_SCHEMA_VERSION,
            "active": bound.active_sidecar,
            "consumption": consumption.clone(),
        }
    });
    libra::utils::atomic_write::write_atomic(
        &sidecar_path,
        &serde_json::to_vec_pretty(&consuming_envelope).unwrap(),
        true,
    )
    .expect("persist slash Cancel Consuming crash seam");
    bound
        .fixture
        .goal_store
        .record_intent_revision_consumption(&consumption)
        .expect("commit slash Cancel receipt before simulated crash");
    if matches!(
        seam,
        IntentCancelCrashSeam::TerminalBeforeProjection
            | IntentCancelCrashSeam::SucceededWithStaleNonStreamingAcknowledgement
    ) {
        bound
            .fixture
            .goal_store
            .complete_code_command_success(
                &consumer_intent.identity,
                "IntentSpec revision mode cancelled",
            )
            .expect("simulate recovery terminal fsync before projection repair");
    }
    if matches!(
        seam,
        IntentCancelCrashSeam::AcknowledgementPersistedIndeterminate
    ) {
        std::fs::remove_file(&sidecar_path)
            .expect("simulate sidecar unlink before acknowledgement persistence ACK loss");
        bound
            .fixture
            .goal_store
            .mark_code_command_indeterminate(
                &consumer_intent.identity,
                "mutating_runtime_turn",
                "IntentSpec revision was cancelled, but its acknowledgement could not be persisted; session requires reconciliation",
            )
            .expect("simulate acknowledgement save ACK-loss terminal");
    } else if matches!(
        seam,
        IntentCancelCrashSeam::MissingSidecar
            | IntentCancelCrashSeam::SucceededWithStaleNonStreamingAcknowledgement
            | IntentCancelCrashSeam::PendingReceiptFollowedByLaterWeb
    ) {
        std::fs::remove_file(&sidecar_path)
            .expect("simulate crash after slash Cancel sidecar unlink");
    }

    let later_web = matches!(
        seam,
        IntentCancelCrashSeam::PendingReceiptFollowedByLaterWeb
    )
    .then(|| {
        let intent = CodeCommandIntent::new(
            CodeCommandIdentity::new(
                bound.source_command.repo_id.clone(),
                bound.source_command.session_id.clone(),
                bound.source_command.principal_id.clone(),
                "later-web-after-pending-cancel-receipt",
            ),
            CODE_UI_WEB_TURN_KIND,
            format!(
                "sha256:{}",
                hex::encode(sha2::Sha256::digest(b"later durable Web turn"))
            ),
            true,
        );
        bound
            .fixture
            .goal_store
            .append_code_workflow_durable(CodeWorkflowEventKind::CommandIntentPersisted {
                command: intent.clone(),
            })
            .expect("persist later Web intent after the cancel receipt");
        bound
            .fixture
            .goal_store
            .append_code_workflow_durable(CodeWorkflowEventKind::CommandTerminalSuccess {
                command: intent.identity.clone(),
                summary: "later Web turn completed".to_string(),
            })
            .expect("persist later Web terminal after the cancel receipt");
        intent
    });

    let mut stale_snapshot = bound.fixture.runtime.snapshot().await;
    let acknowledgement_persisted_indeterminate = matches!(
        seam,
        IntentCancelCrashSeam::AcknowledgementPersistedIndeterminate
    );
    let stale_non_streaming_ack = acknowledgement_persisted_indeterminate
        || matches!(
            seam,
            IntentCancelCrashSeam::SucceededWithStaleNonStreamingAcknowledgement
                | IntentCancelCrashSeam::PendingReceiptFollowedByLaterWeb
        );
    stale_snapshot.status = if stale_non_streaming_ack {
        CodeUiSessionStatus::Idle
    } else {
        CodeUiSessionStatus::Thinking
    };
    let now = chrono::Utc::now();
    stale_snapshot.transcript.push(CodeUiTranscriptEntry {
        id: format!("{command_id}-user"),
        kind: CodeUiTranscriptEntryKind::UserMessage,
        title: None,
        content: Some("/intent cancel".to_string()),
        status: Some("submitted".to_string()),
        streaming: false,
        metadata: serde_json::json!({ "webTurnMode": "IntentRevisionCancel" }),
        created_at: now,
        updated_at: now,
    });
    stale_snapshot.transcript.push(CodeUiTranscriptEntry {
        id: format!("{command_id}-assistant"),
        kind: CodeUiTranscriptEntryKind::AssistantMessage,
        title: None,
        content: Some(if stale_non_streaming_ack {
            "IntentSpec revision cancellation acknowledgement persistence failed".to_string()
        } else {
            String::new()
        }),
        status: Some(if stale_non_streaming_ack {
            "error".to_string()
        } else {
            "streaming".to_string()
        }),
        streaming: !stale_non_streaming_ack,
        metadata: serde_json::json!({}),
        created_at: now,
        updated_at: now,
    });
    if let Some(later_intent) = later_web.as_ref() {
        stale_snapshot.transcript.push(CodeUiTranscriptEntry {
            id: format!("{}-user", later_intent.identity.command_id),
            kind: CodeUiTranscriptEntryKind::UserMessage,
            title: None,
            content: Some("later durable Web turn".to_string()),
            status: Some("submitted".to_string()),
            streaming: false,
            metadata: serde_json::json!({ "webTurnMode": "ExplicitDirect" }),
            created_at: now,
            updated_at: now,
        });
        stale_snapshot.transcript.push(CodeUiTranscriptEntry {
            id: format!("{}-assistant", later_intent.identity.command_id),
            kind: CodeUiTranscriptEntryKind::AssistantMessage,
            title: None,
            content: Some("later assistant projection must survive".to_string()),
            status: Some("completed".to_string()),
            streaming: false,
            metadata: serde_json::json!({}),
            created_at: now,
            updated_at: now,
        });
    }

    let OversizedPhase1ConfirmFixture {
        workdir,
        _storage,
        store,
        session_id,
        goal_store,
        persistence,
        runtime,
        ..
    } = bound.fixture;
    drop(runtime);
    drop(persistence);
    tokio::time::sleep(Duration::from_millis(50)).await;
    let mut stale_state = store
        .load(&session_id)
        .expect("load slash Cancel crash session state");
    stale_state.metadata.insert(
        "code_ui_snapshot".to_string(),
        serde_json::to_value(&stale_snapshot).unwrap(),
    );
    stale_state.metadata.insert(
        "code_ui_projection_cursor".to_string(),
        serde_json::json!(
            goal_store
                .load_code_workflow_replay()
                .unwrap()
                .events
                .last()
                .map(|event| event.sequence)
                .unwrap_or(0)
        ),
    );
    store
        .save(&stale_state)
        .expect("persist stale slash Cancel browser projection");

    let replay_before_restart = goal_store
        .load_code_workflow_replay()
        .expect("load slash Cancel replay before restart");
    let state = store.load(&session_id).unwrap();
    let projection_snapshot: CodeUiSessionSnapshot = serde_json::from_value(
        state
            .metadata
            .get("code_ui_snapshot")
            .cloned()
            .expect("stale slash Cancel snapshot must be durable"),
    )
    .expect("decode stale slash Cancel snapshot");
    let projection_sequence = state
        .metadata
        .get("code_ui_projection_cursor")
        .and_then(serde_json::Value::as_u64)
        .expect("stale slash Cancel projection cursor must be durable");
    let persistence = HeadlessSessionPersistence::with_projection_checkpoint(
        store.clone(),
        state,
        projection_snapshot,
        projection_sequence,
    )
    .expect("reacquire slash Cancel crash session lease");
    if let Some(later_intent) = later_web.as_ref() {
        let error = match try_build_runtime_with_persistence(
            "basic_chat",
            workdir.path().to_path_buf(),
            persistence,
        )
        .await
        {
            Ok(_) => panic!(
                "a pending slash Cancel receipt followed by a later Web command must fail closed"
            ),
            Err(error) => error,
        };
        let message = format!("{error:#}");
        assert!(
            message.contains("pending consumer for IntentSpec revision receipt")
                && message.contains("followed by a later Web command"),
            "startup must explain the inconsistent durable ordering: {message}"
        );
        assert!(matches!(
            error.downcast_ref::<CodeCommandStoreError>(),
            Some(CodeCommandStoreError::PendingRevisionReceiptFollowedByWebIntent {
                command_id,
            }) if command_id == &consumer_intent.identity.command_id
        ));
        assert!(!sidecar_path.exists());

        let persisted_state = store
            .load(&session_id)
            .expect("reload failed-closed slash Cancel session state");
        let persisted_snapshot: CodeUiSessionSnapshot = serde_json::from_value(
            persisted_state
                .metadata
                .get("code_ui_snapshot")
                .cloned()
                .expect("failed startup must preserve the durable browser projection"),
        )
        .expect("decode projection preserved across failed startup");
        assert_eq!(
            serde_json::to_value(&persisted_snapshot)
                .expect("encode projection preserved across failed startup"),
            serde_json::to_value(&stale_snapshot)
                .expect("encode expected stale browser projection"),
            "failed startup must preserve the entire browser projection value-for-value"
        );
        assert!(persisted_snapshot.transcript.iter().any(|entry| {
            entry.id == format!("{}-assistant", later_intent.identity.command_id)
                && entry.content.as_deref() == Some("later assistant projection must survive")
        }));
        assert_eq!(
            goal_store
                .load_code_workflow_replay()
                .expect("reload failed-closed slash Cancel replay"),
            replay_before_restart,
            "startup validation must reject the impossible ordering before any recovery append"
        );
        return;
    }

    let restarted =
        try_build_runtime_with_persistence("basic_chat", workdir.path().to_path_buf(), persistence)
            .await
            .expect("canonical slash Cancel receipt must recover without a fence");
    let snapshot = restarted.snapshot().await;
    assert_eq!(snapshot.status, CodeUiSessionStatus::Idle);
    assert!(!sidecar_path.exists());
    assert!(snapshot.transcript.iter().any(|entry| {
        entry.id == format!("{command_id}-assistant")
            && !entry.streaming
            && entry.status.as_deref() == Some("completed")
            && entry
                .content
                .as_deref()
                .is_some_and(|content| content.contains("IntentSpec revision mode cancelled"))
    }));
    let recovered_status = goal_store
        .code_command_intent_status(&consumer_intent.identity)
        .unwrap()
        .map(|(_, status)| status);
    if acknowledgement_persisted_indeterminate {
        assert!(matches!(
            recovered_status,
            Some(CodeCommandStatus::Indeterminate { effect, reason })
                if effect == "mutating_runtime_turn"
                    && reason.contains("acknowledgement could not be persisted")
        ));
    } else {
        assert!(matches!(
            recovered_status,
            Some(CodeCommandStatus::Succeeded { .. })
        ));
    }
    let replay_before_retry = goal_store.load_code_workflow_replay().unwrap();
    if acknowledgement_persisted_indeterminate {
        restarted
            .submit_message("/fresh direct turn after recovered cancellation".to_string())
            .await
            .expect("a proven cancellation Indeterminate must not fence a fresh direct turn");
        let deadline = std::time::Instant::now() + Duration::from_secs(5);
        while restarted.snapshot().await.status != CodeUiSessionStatus::Idle {
            assert!(
                std::time::Instant::now() < deadline,
                "fresh direct turn after recovered cancellation must finish"
            );
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    } else {
        restarted
            .submit_message_with_command_id(
                "/intent cancel".to_string(),
                Some(command_id.to_string()),
            )
            .await
            .expect("recovered slash Cancel exact retry must be an acknowledgement");
        assert_eq!(
            goal_store.load_code_workflow_replay().unwrap().events.len(),
            replay_before_retry.events.len()
        );
    }
    let replay_before_second_restart = goal_store.load_code_workflow_replay().unwrap();

    drop(restarted);
    tokio::time::sleep(Duration::from_millis(50)).await;
    let state = store.load(&session_id).unwrap();
    let persistence = HeadlessSessionPersistence::new(store, state)
        .expect("reacquire twice-restarted slash Cancel session");
    let restarted_again =
        try_build_runtime_with_persistence("basic_chat", workdir.path().to_path_buf(), persistence)
            .await
            .expect("slash Cancel projection repair must remain idempotent");
    assert_eq!(
        restarted_again.snapshot().await.status,
        CodeUiSessionStatus::Idle
    );
    assert_eq!(
        goal_store.load_code_workflow_replay().unwrap().events.len(),
        replay_before_second_restart.events.len()
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn slash_intent_cancel_receipt_with_consuming_sidecar_recovers() {
    assert_intent_cancel_receipt_crash_recovers(IntentCancelCrashSeam::ConsumingSidecar).await;
}

#[tokio::test(flavor = "multi_thread")]
async fn slash_intent_cancel_receipt_without_sidecar_recovers() {
    assert_intent_cancel_receipt_crash_recovers(IntentCancelCrashSeam::MissingSidecar).await;
}

#[tokio::test(flavor = "multi_thread")]
async fn slash_intent_cancel_terminal_before_projection_recovers_twice() {
    assert_intent_cancel_receipt_crash_recovers(IntentCancelCrashSeam::TerminalBeforeProjection)
        .await;
}

#[tokio::test(flavor = "multi_thread")]
async fn slash_intent_cancel_indeterminate_after_ack_snapshot_does_not_fence() {
    assert_intent_cancel_receipt_crash_recovers(
        IntentCancelCrashSeam::AcknowledgementPersistedIndeterminate,
    )
    .await;
}

#[tokio::test(flavor = "multi_thread")]
async fn slash_intent_cancel_succeeded_with_stale_non_streaming_ack_recovers() {
    assert_intent_cancel_receipt_crash_recovers(
        IntentCancelCrashSeam::SucceededWithStaleNonStreamingAcknowledgement,
    )
    .await;
}

#[tokio::test(flavor = "multi_thread")]
async fn pending_cancel_receipt_before_later_web_never_rewrites_later_projection() {
    assert_intent_cancel_receipt_crash_recovers(
        IntentCancelCrashSeam::PendingReceiptFollowedByLaterWeb,
    )
    .await;
}

const REPLACEMENT_REVIEW_INDETERMINATE_EFFECT: &str = "mutating_runtime_turn";
const INTENT_REVISION_REPLACEMENT_REVIEW_INDETERMINATE_REASON: &str = "IntentSpec revision produced a durable replacement review; startup must restore that review before accepting another turn";

#[derive(Clone, Copy)]
enum IntentReplacementReviewCrashSeam {
    MarkerBeforeReceiptPending,
    ReceiptBeforeTerminalPending,
    MarkerBeforeReceiptIndeterminate,
}

async fn assert_intent_replacement_review_crash_recovers(seam: IntentReplacementReviewCrashSeam) {
    use sha2::Digest as _;

    let seam_name = match seam {
        IntentReplacementReviewCrashSeam::MarkerBeforeReceiptPending => {
            "marker-before-receipt-pending"
        }
        IntentReplacementReviewCrashSeam::ReceiptBeforeTerminalPending => {
            "receipt-before-terminal-pending"
        }
        IntentReplacementReviewCrashSeam::MarkerBeforeReceiptIndeterminate => {
            "marker-before-receipt-indeterminate"
        }
    };
    let bound = real_bound_intent_revision_fixture(&format!(
        "private replacement-review crash note for {seam_name}"
    ))
    .await;
    let consumer_input = format!("revise the IntentSpec across {seam_name}");
    let consumer_intent = CodeCommandIntent::new(
        CodeCommandIdentity::new(
            bound.source_command.repo_id.clone(),
            bound.source_command.session_id.clone(),
            bound.source_command.principal_id.clone(),
            format!("replacement-review-consumer-{seam_name}"),
        ),
        CODE_UI_WEB_TURN_KIND,
        format!(
            "sha256:{}",
            hex::encode(sha2::Sha256::digest(consumer_input.as_bytes()))
        ),
        true,
    );
    assert!(matches!(
        bound
            .fixture
            .goal_store
            .admit_code_command(consumer_intent.clone())
            .expect("admit replacement-review revision consumer"),
        CodeCommandAdmission::Execute { .. }
    ));
    let claim = IntentRevisionConsumptionClaim {
        schema_version: INTENT_REVISION_CONSUMPTION_SCHEMA_VERSION,
        interaction_id: bound.recovery.interaction_id.clone(),
        source_command: bound.source_command.clone(),
        consumer_intent: consumer_intent.clone(),
        terminal_event_id: bound.terminal.event_id,
        terminal_sequence: bound.terminal.sequence,
        intent_id: bound.intent_id.clone(),
        sidecar_digest: Some(bound.recovery.sidecar_digest.clone()),
    };
    let consumption = bound
        .fixture
        .goal_store
        .prepare_intent_revision_consumption(&consumer_intent, &claim)
        .expect("prepare exact replacement-review revision consumption");
    let sidecar_path =
        pending_intent_revision_path(&bound.fixture.store, &bound.fixture.session_id);
    let consuming_envelope = serde_json::json!({
        "intentSpec": "",
        "consuming": {
            "schemaVersion": INTENT_REVISION_CONSUMPTION_SCHEMA_VERSION,
            "active": bound.active_sidecar.clone(),
            "consumption": consumption.clone(),
        }
    });
    libra::utils::atomic_write::write_atomic(
        &sidecar_path,
        &serde_json::to_vec_pretty(&consuming_envelope)
            .expect("serialize replacement-review Consuming sidecar"),
        true,
    )
    .expect("persist replacement-review Consuming sidecar");

    let mut replacement_spec: libra::internal::ai::intentspec::IntentSpec = serde_json::from_str(
        bound.active_sidecar["intentSpec"]
            .as_str()
            .expect("real bound sidecar carries a valid IntentSpec"),
    )
    .expect("decode the source IntentSpec for a replacement generation");
    let replacement_intent_id = format!("replacement-intent-{}", Uuid::new_v4());
    replacement_spec.metadata.id = replacement_intent_id.clone();
    replacement_spec.intent.summary = format!("Recover {seam_name} replacement review");
    let replacement_intent_path = bound
        .fixture
        .goal_store
        .session_root()
        .join("intents")
        .join(format!("{replacement_intent_id}.json"));
    libra::utils::atomic_write::write_atomic(
        &replacement_intent_path,
        &serde_json::to_vec_pretty(&replacement_spec)
            .expect("serialize the valid replacement IntentSpec"),
        true,
    )
    .expect("persist the replacement IntentSpec before its review marker");

    let replacement_interaction_id = format!("replacement-review-{}", Uuid::new_v4());
    let replacement_turn_id = format!("replacement-review-gate-{}", Uuid::new_v4());
    bound
        .fixture
        .goal_store
        .append_code_workflow_durable(CodeWorkflowEventKind::IntentReviewRequested {
            interaction_id: replacement_interaction_id.clone(),
            intent_id: replacement_intent_id.clone(),
            turn_id: replacement_turn_id,
            phase0_turn_id: consumer_intent.identity.command_id.clone(),
        })
        .expect("persist the replacement IntentReviewRequested crash marker");

    match seam {
        IntentReplacementReviewCrashSeam::MarkerBeforeReceiptPending => {}
        IntentReplacementReviewCrashSeam::ReceiptBeforeTerminalPending => {
            bound
                .fixture
                .goal_store
                .record_intent_revision_consumption(&consumption)
                .expect("persist the replacement-review receipt before its terminal");
        }
        IntentReplacementReviewCrashSeam::MarkerBeforeReceiptIndeterminate => {
            bound
                .fixture
                .goal_store
                .mark_code_command_indeterminate(
                    &consumer_intent.identity,
                    REPLACEMENT_REVIEW_INDETERMINATE_EFFECT,
                    INTENT_REVISION_REPLACEMENT_REVIEW_INDETERMINATE_REASON,
                )
                .expect("persist the canonical replacement-review Indeterminate terminal");
        }
    }

    let before_restart = bound
        .fixture
        .goal_store
        .load_code_workflow_replay()
        .expect("load the replacement-review crash boundary");
    let receipts_before_restart = before_restart
        .events
        .iter()
        .filter(|event| {
            matches!(
                &event.event,
                CodeWorkflowEventKind::InteractionResolved {
                    intent_revision_consumption: Some(candidate),
                    ..
                } if candidate == &consumption
            )
        })
        .count();
    assert_eq!(
        receipts_before_restart,
        usize::from(matches!(
            seam,
            IntentReplacementReviewCrashSeam::ReceiptBeforeTerminalPending
        )),
        "the fixture must stop at the requested replacement-review receipt boundary"
    );
    let status_before_restart = bound
        .fixture
        .goal_store
        .code_command_intent_status(&consumer_intent.identity)
        .expect("read replacement-review consumer status before restart")
        .map(|(_, status)| status)
        .expect("replacement-review consumer intent remains durable");
    match seam {
        IntentReplacementReviewCrashSeam::MarkerBeforeReceiptPending
        | IntentReplacementReviewCrashSeam::ReceiptBeforeTerminalPending => {
            assert_eq!(status_before_restart, CodeCommandStatus::Pending);
        }
        IntentReplacementReviewCrashSeam::MarkerBeforeReceiptIndeterminate => {
            assert!(matches!(
                status_before_restart,
                CodeCommandStatus::Indeterminate { effect, reason }
                    if effect == REPLACEMENT_REVIEW_INDETERMINATE_EFFECT
                        && reason == INTENT_REVISION_REPLACEMENT_REVIEW_INDETERMINATE_REASON
            ));
        }
    }

    let consumer_user_entry_id = format!("replacement-review-user-{seam_name}");
    let consumer_assistant_entry_id = format!("replacement-review-assistant-{seam_name}");
    let submit_intent_tool_id = format!("replacement-review-submit-intent-{seam_name}");
    let now = chrono::Utc::now();
    let mut stale_snapshot = bound.fixture.runtime.snapshot().await;
    stale_snapshot.status = CodeUiSessionStatus::Thinking;
    stale_snapshot.transcript.push(CodeUiTranscriptEntry {
        id: consumer_user_entry_id,
        kind: CodeUiTranscriptEntryKind::UserMessage,
        title: None,
        content: Some(consumer_input),
        status: Some("submitted".to_string()),
        streaming: false,
        metadata: serde_json::json!({ "webTurnMode": "PlanPhase0" }),
        created_at: now,
        updated_at: now,
    });
    stale_snapshot.transcript.push(CodeUiTranscriptEntry {
        id: consumer_assistant_entry_id.clone(),
        kind: CodeUiTranscriptEntryKind::AssistantMessage,
        title: Some("Revising IntentSpec".to_string()),
        content: Some("partial replacement IntentSpec provider output".to_string()),
        status: Some("streaming".to_string()),
        streaming: true,
        metadata: serde_json::json!({ "phase": "intent-revision-consumer" }),
        created_at: now,
        updated_at: now,
    });
    stale_snapshot.tool_calls.push(CodeUiToolCallSnapshot {
        id: submit_intent_tool_id.clone(),
        tool_name: "submit_intent_draft".to_string(),
        status: "running".to_string(),
        summary: Some("Persisting replacement IntentSpec".to_string()),
        details: None,
        updated_at: now,
    });
    stale_snapshot.transcript.push(CodeUiTranscriptEntry {
        id: submit_intent_tool_id.clone(),
        kind: CodeUiTranscriptEntryKind::ToolCall,
        title: Some("submit_intent_draft".to_string()),
        content: None,
        status: Some("running".to_string()),
        streaming: true,
        metadata: serde_json::json!({ "toolName": "submit_intent_draft" }),
        created_at: now,
        updated_at: now,
    });
    if matches!(
        seam,
        IntentReplacementReviewCrashSeam::MarkerBeforeReceiptIndeterminate
    ) {
        stale_snapshot.status = CodeUiSessionStatus::Idle;
        let assistant = stale_snapshot
            .transcript
            .iter_mut()
            .find(|entry| entry.id == consumer_assistant_entry_id)
            .expect("ACK-loss fixture retains its consumer assistant row");
        assistant.content = Some(
            "provider returned a replacement draft but its acknowledgement was lost".to_string(),
        );
        assistant.status = Some("error".to_string());
        assistant.streaming = false;
        let tool_call = stale_snapshot
            .tool_calls
            .iter_mut()
            .find(|tool_call| tool_call.id == submit_intent_tool_id)
            .expect("ACK-loss fixture retains its submit_intent_draft row");
        tool_call.status = "completed".to_string();
        tool_call.details = Some("replacement IntentSpec is durable".to_string());
        let tool_entry = stale_snapshot
            .transcript
            .iter_mut()
            .find(|entry| {
                entry.id == submit_intent_tool_id
                    && entry.kind == CodeUiTranscriptEntryKind::ToolCall
            })
            .expect("ACK-loss fixture retains its submit_intent_draft transcript row");
        tool_entry.content = Some("replacement IntentSpec is durable".to_string());
        tool_entry.status = Some("completed".to_string());
        tool_entry.streaming = false;
    }
    let stale_projection_cursor = before_restart
        .events
        .last()
        .map(|event| event.sequence)
        .unwrap_or(0);

    let RealBoundIntentRevisionFixture {
        fixture,
        active_sidecar: _,
        terminal: _,
        source_command: _,
        summary: _,
        recovery: _,
        intent_id: _,
        hmac_key: _,
    } = bound;
    let OversizedPhase1ConfirmFixture {
        workdir,
        _storage,
        store,
        session_id,
        goal_store,
        persistence,
        runtime,
        source_interaction_id: _,
        risk_interaction_id: _,
        risk_pending_state: _,
    } = fixture;
    drop(runtime);
    drop(persistence);
    tokio::time::sleep(Duration::from_millis(50)).await;

    let mut stale_state = store
        .load(&session_id)
        .expect("load replacement-review session before injecting stale projection");
    stale_state.metadata.insert(
        "code_ui_snapshot".to_string(),
        serde_json::to_value(stale_snapshot)
            .expect("serialize stale replacement-review browser projection"),
    );
    stale_state.metadata.insert(
        "code_ui_projection_cursor".to_string(),
        serde_json::json!(stale_projection_cursor),
    );
    store
        .save(&stale_state)
        .expect("persist stale replacement-review browser projection");

    let state = store
        .load(&session_id)
        .expect("load replacement-review crash fixture for startup 1");
    let projection_snapshot: CodeUiSessionSnapshot = serde_json::from_value(
        state
            .metadata
            .get("code_ui_snapshot")
            .cloned()
            .expect("startup 1 state carries the stale browser projection"),
    )
    .expect("decode startup 1 browser projection checkpoint");
    let projection_sequence = state
        .metadata
        .get("code_ui_projection_cursor")
        .and_then(serde_json::Value::as_u64)
        .expect("startup 1 state carries its projection cursor");
    let persistence = HeadlessSessionPersistence::with_projection_checkpoint(
        store.clone(),
        state,
        projection_snapshot.clone(),
        projection_sequence,
    )
    .expect("reacquire replacement-review lease for startup 1");
    let provider_entered = Arc::new(Notify::new());
    let provider_entered_wait = provider_entered.notified();
    tokio::pin!(provider_entered_wait);
    provider_entered_wait.as_mut().enable();
    let restored = build_pending_completion_runtime_with_snapshot(
        workdir.path().to_path_buf(),
        persistence,
        Arc::clone(&provider_entered),
        projection_snapshot,
    )
    .await;
    let snapshot = restored.snapshot().await;
    assert_eq!(
        snapshot.status,
        CodeUiSessionStatus::AwaitingInteraction,
        "startup 1 must restore the replacement review without fencing"
    );
    let restored_review = snapshot
        .interactions
        .iter()
        .find(|interaction| {
            interaction.id == replacement_interaction_id
                && interaction.kind == CodeUiInteractionKind::IntentReviewChoice
                && interaction.status == CodeUiInteractionStatus::Pending
        })
        .unwrap_or_else(|| {
            panic!("startup 1 must restore replacement review {replacement_interaction_id}")
        });
    assert_eq!(
        restored_review
            .metadata
            .get("intentId")
            .and_then(serde_json::Value::as_str),
        Some(replacement_intent_id.as_str())
    );
    let restored_spec_text = restored_review
        .metadata
        .get("intentSpec")
        .and_then(serde_json::Value::as_str)
        .expect("restored replacement review carries its durable IntentSpec JSON");
    let restored_spec: libra::internal::ai::intentspec::IntentSpec =
        serde_json::from_str(restored_spec_text)
            .expect("restored replacement IntentSpec remains schema-valid");
    assert_eq!(restored_spec.metadata.id, replacement_intent_id);
    assert!(replacement_intent_path.is_file());
    assert!(
        !sidecar_path.exists(),
        "startup 1 must clear the consumed revision sidecar"
    );
    assert!(
        tokio::time::timeout(Duration::from_millis(100), provider_entered_wait.as_mut())
            .await
            .is_err(),
        "startup 1 must restore the replacement review without rerunning its provider"
    );
    let recovered_assistant = snapshot
        .transcript
        .iter()
        .find(|entry| entry.id == consumer_assistant_entry_id)
        .expect("startup 1 must retain the interrupted replacement assistant row");
    assert!(!recovered_assistant.streaming);
    assert_eq!(recovered_assistant.status.as_deref(), Some("completed"));
    assert!(
        recovered_assistant
            .content
            .as_deref()
            .is_some_and(|content| {
                content.contains("Revised IntentSpec recovered and ready for review")
            })
    );
    let recovered_tool = snapshot
        .tool_calls
        .iter()
        .find(|tool_call| tool_call.id == submit_intent_tool_id)
        .expect("startup 1 must retain the interrupted submit_intent_draft row");
    assert_eq!(recovered_tool.status, "completed");
    let recovered_tool_transcript = snapshot
        .transcript
        .iter()
        .find(|entry| {
            entry.id == submit_intent_tool_id && entry.kind == CodeUiTranscriptEntryKind::ToolCall
        })
        .expect("startup 1 must close the streaming submit_intent_draft transcript row");
    assert!(!recovered_tool_transcript.streaming);
    assert_eq!(
        recovered_tool_transcript.status.as_deref(),
        Some("completed")
    );

    let replay = goal_store
        .load_code_workflow_replay()
        .expect("load the recovered replacement-review workflow");
    let receipts = replay
        .events
        .iter()
        .filter_map(|event| match &event.event {
            CodeWorkflowEventKind::InteractionResolved {
                intent_revision_consumption: Some(candidate),
                ..
            } => Some(candidate),
            _ => None,
        })
        .collect::<Vec<_>>();
    assert_eq!(
        receipts,
        vec![&consumption],
        "startup 1 must leave exactly one exact replacement-review receipt"
    );
    let open_review =
        open_intent_review_from_workflow(replay.events.iter().map(|event| &event.event))
            .expect("the replacement Intent review remains durably open");
    assert_eq!(open_review.0, replacement_interaction_id);
    assert_eq!(open_review.1, replacement_intent_id);
    assert_eq!(
        open_review.3, consumer_intent.identity.command_id,
        "replacement markers must retain their exact revision-consumer owner"
    );
    assert!(replay.events.iter().all(|event| !matches!(
        &event.event,
        CodeWorkflowEventKind::CommandIndeterminateSideEffect {
            command,
            effect,
            reason,
        } if command == &consumer_intent.identity
            && (effect != REPLACEMENT_REVIEW_INDETERMINATE_EFFECT
                || reason != INTENT_REVISION_REPLACEMENT_REVIEW_INDETERMINATE_REASON)
    )));
    match seam {
        IntentReplacementReviewCrashSeam::MarkerBeforeReceiptPending
        | IntentReplacementReviewCrashSeam::ReceiptBeforeTerminalPending => {
            assert!(matches!(
                goal_store
                    .code_command_intent_status(&consumer_intent.identity)
                    .expect("read recovered Pending replacement-review consumer")
                    .map(|(_, status)| status),
                Some(CodeCommandStatus::Succeeded { .. })
            ));
            drop(restored);
        }
        IntentReplacementReviewCrashSeam::MarkerBeforeReceiptIndeterminate => {
            assert!(matches!(
                goal_store
                    .code_command_intent_status(&consumer_intent.identity)
                    .expect("read recovered Indeterminate replacement-review consumer")
                    .map(|(_, status)| status),
                Some(CodeCommandStatus::Indeterminate { effect, reason })
                    if effect == REPLACEMENT_REVIEW_INDETERMINATE_EFFECT
                        && reason == INTENT_REVISION_REPLACEMENT_REVIEW_INDETERMINATE_REASON
            ));

            restored
                .respond_interaction(
                    &replacement_interaction_id,
                    CodeUiInteractionResponse {
                        selected_option: Some("cancel".to_string()),
                        ..Default::default()
                    },
                )
                .await
                .expect("the restored replacement review must accept a real Cancel response");
            let mut terminal_projection = restored.snapshot().await;
            assert_eq!(terminal_projection.status, CodeUiSessionStatus::Idle);
            assert!(terminal_projection.interactions.iter().all(|interaction| {
                interaction.id != replacement_interaction_id
                    || interaction.status != CodeUiInteractionStatus::Pending
            }));
            let historical_assistant = terminal_projection
                .transcript
                .iter_mut()
                .find(|entry| entry.id == consumer_assistant_entry_id)
                .expect("Cancel must retain the recovered historical assistant row");
            historical_assistant.streaming = false;
            historical_assistant.status = Some("error".to_string());
            historical_assistant.content =
                Some("provider handoff ended after its durable replacement marker".to_string());

            drop(restored);
            tokio::time::sleep(Duration::from_millis(50)).await;
            let mut terminal_state = store
                .load(&session_id)
                .expect("load the Cancelled replacement-review session");
            terminal_state.metadata.insert(
                "code_ui_snapshot".to_string(),
                serde_json::to_value(terminal_projection)
                    .expect("serialize nonstreaming error/Idle historical projection"),
            );
            terminal_state.metadata.insert(
                "code_ui_projection_cursor".to_string(),
                serde_json::json!(
                    goal_store
                        .load_code_workflow_replay()
                        .expect("load workflow after replacement review Cancel")
                        .events
                        .last()
                        .map(|event| event.sequence)
                        .unwrap_or(0)
                ),
            );
            store
                .save(&terminal_state)
                .expect("persist nonstreaming error/Idle historical projection");

            let state = store
                .load(&session_id)
                .expect("load replacement-review session for startup 2");
            let projection_snapshot: CodeUiSessionSnapshot = serde_json::from_value(
                state
                    .metadata
                    .get("code_ui_snapshot")
                    .cloned()
                    .expect("startup 2 state carries its terminal browser projection"),
            )
            .expect("decode startup 2 browser projection checkpoint");
            let projection_sequence = state
                .metadata
                .get("code_ui_projection_cursor")
                .and_then(serde_json::Value::as_u64)
                .expect("startup 2 state carries its projection cursor");
            let persistence = HeadlessSessionPersistence::with_projection_checkpoint(
                store.clone(),
                state,
                projection_snapshot.clone(),
                projection_sequence,
            )
            .expect("reacquire replacement-review lease for startup 2");
            let provider_entered = Arc::new(Notify::new());
            let provider_entered_wait = provider_entered.notified();
            tokio::pin!(provider_entered_wait);
            provider_entered_wait.as_mut().enable();
            let restarted = build_pending_completion_runtime_with_snapshot(
                workdir.path().to_path_buf(),
                persistence,
                Arc::clone(&provider_entered),
                projection_snapshot,
            )
            .await;
            let snapshot = restarted.snapshot().await;
            assert_eq!(
                snapshot.status,
                CodeUiSessionStatus::Idle,
                "startup 2 must permanently exempt the historical replacement consumer"
            );
            assert!(snapshot.interactions.iter().all(|interaction| {
                interaction.id != replacement_interaction_id
                    || interaction.status != CodeUiInteractionStatus::Pending
            }));
            let historical_assistant = snapshot
                .transcript
                .iter()
                .find(|entry| entry.id == consumer_assistant_entry_id)
                .expect("startup 2 must preserve the terminal historical assistant row");
            assert!(!historical_assistant.streaming);
            assert_eq!(historical_assistant.status.as_deref(), Some("error"));
            assert!(
                tokio::time::timeout(Duration::from_millis(100), provider_entered_wait.as_mut())
                    .await
                    .is_err(),
                "startup 2 must not rerun the historical replacement provider"
            );

            let replay = goal_store
                .load_code_workflow_replay()
                .expect("load the twice-started replacement-review workflow");
            let receipts = replay
                .events
                .iter()
                .filter_map(|event| match &event.event {
                    CodeWorkflowEventKind::InteractionResolved {
                        intent_revision_consumption: Some(candidate),
                        ..
                    } => Some(candidate),
                    _ => None,
                })
                .collect::<Vec<_>>();
            assert_eq!(
                receipts,
                vec![&consumption],
                "startup 2 must retain exactly one replacement-review receipt"
            );
            assert!(
                open_intent_review_from_workflow(replay.events.iter().map(|event| &event.event))
                    .is_none()
            );
            assert!(matches!(
                goal_store
                    .code_command_intent_status(&consumer_intent.identity)
                    .expect("read permanently exempt historical replacement consumer")
                    .map(|(_, status)| status),
                Some(CodeCommandStatus::Indeterminate { effect, reason })
                    if effect == REPLACEMENT_REVIEW_INDETERMINATE_EFFECT
                        && reason == INTENT_REVISION_REPLACEMENT_REVIEW_INDETERMINATE_REASON
            ));
            assert!(replay.events.iter().all(|event| !matches!(
                &event.event,
                CodeWorkflowEventKind::CommandIndeterminateSideEffect {
                    command,
                    effect,
                    reason,
                } if command == &consumer_intent.identity
                    && (effect != REPLACEMENT_REVIEW_INDETERMINATE_EFFECT
                        || reason != INTENT_REVISION_REPLACEMENT_REVIEW_INDETERMINATE_REASON)
            )));
            assert!(!sidecar_path.exists());
            drop(restarted);
        }
    }
    tokio::time::sleep(Duration::from_millis(50)).await;
}

#[tokio::test(flavor = "multi_thread")]
async fn replacement_review_marker_without_receipt_pending_consumer_recovers() {
    assert_intent_replacement_review_crash_recovers(
        IntentReplacementReviewCrashSeam::MarkerBeforeReceiptPending,
    )
    .await;
}

#[tokio::test(flavor = "multi_thread")]
async fn replacement_review_marker_and_receipt_without_terminal_recovers() {
    assert_intent_replacement_review_crash_recovers(
        IntentReplacementReviewCrashSeam::ReceiptBeforeTerminalPending,
    )
    .await;
}

#[tokio::test(flavor = "multi_thread")]
async fn replacement_review_indeterminate_handoff_recovers_same_gate_twice() {
    assert_intent_replacement_review_crash_recovers(
        IntentReplacementReviewCrashSeam::MarkerBeforeReceiptIndeterminate,
    )
    .await;
}

fn record_bound_revision_receipt(
    bound: &RealBoundIntentRevisionFixture,
    consumer_command_id: &str,
) {
    let consumer_intent = CodeCommandIntent::new(
        CodeCommandIdentity::new(
            bound.source_command.repo_id.clone(),
            bound.source_command.session_id.clone(),
            bound.source_command.principal_id.clone(),
            consumer_command_id,
        ),
        CODE_UI_WEB_TURN_KIND,
        format!("sha256:{}", "e".repeat(64)),
        true,
    );
    bound
        .fixture
        .goal_store
        .admit_code_command(consumer_intent.clone())
        .expect("admit exact revision consumer");
    let claim = IntentRevisionConsumptionClaim {
        schema_version: INTENT_REVISION_CONSUMPTION_SCHEMA_VERSION,
        interaction_id: bound.recovery.interaction_id.clone(),
        source_command: bound.source_command.clone(),
        consumer_intent: consumer_intent.clone(),
        terminal_event_id: bound.terminal.event_id,
        terminal_sequence: bound.terminal.sequence,
        intent_id: bound.intent_id.clone(),
        sidecar_digest: Some(bound.recovery.sidecar_digest.clone()),
    };
    let consumption = bound
        .fixture
        .goal_store
        .prepare_intent_revision_consumption(&consumer_intent, &claim)
        .expect("prepare exact revision consumer receipt");
    bound
        .fixture
        .goal_store
        .record_intent_revision_consumption(&consumption)
        .expect("commit exact revision consumer receipt");
    bound
        .fixture
        .goal_store
        .complete_code_command_success(&consumer_intent.identity, "revision consumer complete")
        .expect("terminalize revision consumer after its receipt");
}

fn revision_sidecar_digest_for_test(
    schema_version: u32,
    interaction_id: &str,
    command: &CodeCommandIdentity,
    intent_id: &str,
    intent_spec: &str,
    note: Option<&str>,
    hmac_key: &[u8],
) -> String {
    fn update(context: &mut ring::hmac::Context, label: &[u8], value: &[u8]) {
        context.update(&(label.len() as u64).to_be_bytes());
        context.update(label);
        context.update(&(value.len() as u64).to_be_bytes());
        context.update(value);
    }

    let key = ring::hmac::Key::new(ring::hmac::HMAC_SHA256, hmac_key);
    let mut digest = ring::hmac::Context::with_key(&key);
    digest.update(b"libra.intent-revision-sidecar.v1");
    update(
        &mut digest,
        b"schema_version",
        &schema_version.to_be_bytes(),
    );
    update(&mut digest, b"interaction_id", interaction_id.as_bytes());
    update(&mut digest, b"repo_id", command.repo_id.as_bytes());
    update(&mut digest, b"session_id", command.session_id.as_bytes());
    update(
        &mut digest,
        b"principal_id",
        command.principal_id.as_bytes(),
    );
    update(&mut digest, b"command_id", command.command_id.as_bytes());
    update(&mut digest, b"intent_id", intent_id.as_bytes());
    update(&mut digest, b"intent_spec", intent_spec.as_bytes());
    match note {
        Some(note) => {
            update(&mut digest, b"note_present", b"1");
            update(&mut digest, b"note", note.as_bytes());
        }
        None => update(&mut digest, b"note_present", b"0"),
    }
    format!("hmac-sha256:{}", hex::encode(digest.sign().as_ref()))
}

async fn assert_malformed_bound_revision_fails_closed(
    bound: RealBoundIntentRevisionFixture,
    shape: &str,
) {
    let OversizedPhase1ConfirmFixture {
        workdir,
        _storage,
        store,
        session_id,
        goal_store: _,
        persistence: _,
        runtime,
        source_interaction_id: _,
        risk_interaction_id: _,
        risk_pending_state: _,
    } = bound.fixture;
    drop(runtime);
    tokio::time::sleep(Duration::from_millis(50)).await;
    let state = store
        .load(&session_id)
        .expect("load duplicate source-terminal fixture");
    let persistence = HeadlessSessionPersistence::new(store, state)
        .expect("reacquire duplicate source-terminal fixture lease");
    let provider_entered = Arc::new(Notify::new());
    let provider_entered_wait = provider_entered.notified();
    tokio::pin!(provider_entered_wait);
    provider_entered_wait.as_mut().enable();
    match try_build_pending_completion_runtime_with_capture(
        workdir.path().to_path_buf(),
        persistence,
        Arc::clone(&provider_entered),
        None,
    )
    .await
    {
        Err(error) => {
            let error = format!("{error:#}");
            assert!(
                error.contains("conflict")
                    || error.contains("multiple")
                    || error.contains("ambiguous")
                    || error.contains("authority replay is invalid"),
                "{shape} must fail with an authority/terminal conflict: {error}"
            );
        }
        Ok(runtime) => {
            let snapshot = runtime.snapshot().await;
            panic!(
                "{shape} must fail in startup preflight before exposing a runtime; got status {:?}",
                snapshot.status
            );
        }
    }
    assert!(
        tokio::time::timeout(Duration::from_millis(100), provider_entered_wait.as_mut())
            .await
            .is_err(),
        "{shape} must fail before invoking any provider"
    );
}

async fn assert_ordinary_intent_response_terminal_failure_fences_and_survives_restart(
    decision: &str,
) {
    let fixture = oversized_phase1_confirm_fixture(None).await;
    let source_interaction_id = fixture.source_interaction_id.clone();
    let runtime_turn_id = fixture
        .runtime
        .runtime_snapshot()
        .await
        .expect("read Intent owner before injected terminal failure")
        .active_turn_id
        .expect("pending Intent review must have a runtime owner");
    fixture
        .goal_store
        .fail_next_combined_terminal_append_for_test();
    let error = fixture
        .runtime
        .respond_interaction(
            &source_interaction_id,
            CodeUiInteractionResponse {
                selected_option: Some(decision.to_string()),
                note: (decision == "modify")
                    .then(|| "this note must not escape an uncommitted Modify".to_string()),
                ..Default::default()
            },
        )
        .await
        .expect_err("an unpersistable ordinary Intent response must not acknowledge success");
    assert_eq!(
        error.to_string(),
        "Phase 1 Web close-out is indeterminate; restart and reconcile the durable session before retrying"
    );

    let snapshot = fixture.runtime.snapshot().await;
    assert_eq!(
        snapshot.status,
        CodeUiSessionStatus::IndeterminateSideEffect
    );
    assert!(snapshot.interactions.iter().any(|interaction| {
        interaction.id == source_interaction_id
            && interaction.status == CodeUiInteractionStatus::Pending
    }));
    assert!(snapshot.transcript.iter().all(|entry| {
        entry.content.as_deref().is_none_or(|content| {
            !content.contains("IntentSpec revise mode is active")
                && !content.contains("this note must not escape")
        })
    }));
    assert!(matches!(
        fixture
            .runtime
            .runtime_snapshot()
            .await
            .expect("worker snapshot after failed Intent terminal")
            .interaction,
        InteractionState::IndeterminateSideEffect { .. }
    ));
    let replay = fixture.goal_store.load_code_workflow_replay().unwrap();
    let sidecar_path = pending_intent_revision_path(&fixture.store, &fixture.session_id);
    if decision == "modify" {
        let source_command = replay
            .events
            .iter()
            .find_map(|event| match &event.event {
                CodeWorkflowEventKind::CommandIntentPersisted { command }
                    if command.identity.command_id == runtime_turn_id =>
                {
                    Some(command.identity.clone())
                }
                _ => None,
            })
            .expect("the failed Modify owner must retain its durable command intent");
        let (open_interaction_id, intent_id, ..) =
            open_intent_review_from_workflow(replay.events.iter().map(|event| &event.event))
                .expect("the failed Modify must retain its open Intent lineage");
        assert_eq!(open_interaction_id, source_interaction_id);

        let sidecar: serde_json::Value = serde_json::from_slice(
            &std::fs::read(&sidecar_path)
                .expect("an ambiguous Modify must retain its pre-terminal Prepared sidecar"),
        )
        .expect("the retained Prepared sidecar must remain valid JSON");
        let envelope = sidecar
            .as_object()
            .expect("the retained Prepared sidecar must be an object");
        assert_eq!(
            envelope.len(),
            3,
            "the ambiguous Modify may retain only the empty baseline plus dormant Prepared envelope"
        );
        assert_eq!(sidecar["intentSpec"].as_str(), Some(""));
        assert!(sidecar["note"].is_null());
        assert!(!envelope.contains_key("authority"));
        assert!(!envelope.contains_key("consuming"));
        let prepared = sidecar["prepared"]
            .as_object()
            .expect("the ambiguous Modify sidecar must remain dormant Prepared state");
        assert_eq!(
            prepared["interactionId"].as_str(),
            Some(source_interaction_id.as_str())
        );
        assert_eq!(
            prepared["command"],
            serde_json::to_value(&source_command).unwrap()
        );
        assert_eq!(prepared["intentId"].as_str(), Some(intent_id.as_str()));
        assert_eq!(
            prepared["note"].as_str(),
            Some("this note must not escape an uncommitted Modify")
        );
        let schema_version = u32::try_from(
            prepared["schemaVersion"]
                .as_u64()
                .expect("Prepared sidecar must carry a schema version"),
        )
        .expect("Prepared sidecar schema version must fit u32");
        let intent_spec = std::fs::read_to_string(
            fixture
                .goal_store
                .session_root()
                .join("intents")
                .join(format!("{intent_id}.json")),
        )
        .expect("read the exact durable IntentSpec bound by Prepared");
        let hmac_key = std::fs::read(intent_revision_hmac_key_path(
            &fixture.store,
            &fixture.session_id,
        ))
        .expect("read the session HMAC key bound by Prepared");
        let expected_digest = revision_sidecar_digest_for_test(
            schema_version,
            &source_interaction_id,
            &source_command,
            &intent_id,
            &intent_spec,
            Some("this note must not escape an uncommitted Modify"),
            &hmac_key,
        );
        assert_eq!(
            prepared["sidecarDigest"].as_str(),
            Some(expected_digest.as_str()),
            "the dormant Prepared envelope must retain the exact authenticated body"
        );
        assert!(is_canonical_intent_revision_digest(&expected_digest));
    } else {
        assert!(
            !sidecar_path.exists(),
            "Cancel must not create an IntentSpec revision sidecar"
        );
    }

    assert!(
        open_intent_review_from_workflow(replay.events.iter().map(|event| &event.event))
            .is_some_and(|(interaction_id, ..)| interaction_id == source_interaction_id),
        "the failed combined append must leave the original durable Intent authority open"
    );
    assert!(replay.events.iter().all(|event| !matches!(
        &event.event,
        CodeWorkflowEventKind::CommandTerminalSuccessWithInteractionResolved {
            interaction_id,
            ..
        } if interaction_id == &source_interaction_id
    )));
    assert!(replay.events.iter().all(|event| !matches!(
        &event.event,
        CodeWorkflowEventKind::InteractionResolved { interaction_id, .. }
            if interaction_id == &source_interaction_id
    )));
    assert_eq!(
        replay
            .events
            .iter()
            .filter(|event| matches!(
                &event.event,
                CodeWorkflowEventKind::CommandIndeterminateSideEffect { command, .. }
                    if command.command_id == runtime_turn_id
            ))
            .count(),
        1,
        "the ambiguous {decision} dispatch must append one durable Indeterminate terminal"
    );
    if decision == "modify" {
        assert_raw_note_absent_from_workflow_and_sse(
            &fixture.goal_store,
            "this note must not escape an uncommitted Modify",
        );
    }
    let submit = fixture
        .runtime
        .submit_message("/must remain blocked after ambiguous Intent response".to_string())
        .await
        .expect_err("the live fenced session must reject new admission");
    assert_code_ui_api_error(&submit, 409, "RECONCILIATION_REQUIRED");

    let OversizedPhase1ConfirmFixture {
        workdir,
        _storage,
        store,
        session_id,
        goal_store: _,
        persistence: _,
        runtime,
        source_interaction_id: _,
        risk_interaction_id: _,
        risk_pending_state: _,
    } = fixture;
    drop(runtime);
    tokio::time::sleep(Duration::from_millis(50)).await;
    let restored_state = store
        .load(&session_id)
        .expect("load failed-response session for original-gate recovery");
    let persistence = HeadlessSessionPersistence::new(store.clone(), restored_state)
        .expect("reacquire failed-response session lease");
    let restored = build_runtime_with_persistence(
        "basic_chat",
        workdir.path().to_path_buf(),
        Vec::new(),
        Some(persistence),
    )
    .await
    .0;
    let restored_snapshot = restored.snapshot().await;
    assert_eq!(
        restored_snapshot.status,
        CodeUiSessionStatus::IndeterminateSideEffect,
        "restart must remain fenced across the ambiguous {decision} terminal boundary"
    );
    assert!(matches!(
        restored
            .runtime_snapshot()
            .await
            .expect("worker snapshot after ambiguous Intent restart")
            .interaction,
        InteractionState::IndeterminateSideEffect { .. }
    ));
    let replay = SessionJsonlStore::new(store.session_root(&session_id))
        .load_code_workflow_replay()
        .unwrap();
    assert!(
        open_intent_review_from_workflow(replay.events.iter().map(|event| &event.event))
            .is_some_and(|(interaction_id, ..)| interaction_id == source_interaction_id),
        "the open marker remains forensic evidence but must not bypass its terminal ambiguity"
    );
    assert_eq!(
        replay
            .events
            .iter()
            .filter(|event| matches!(
                &event.event,
                CodeWorkflowEventKind::CommandIndeterminateSideEffect { command, .. }
                    if command.command_id == runtime_turn_id
            ))
            .count(),
        1,
        "restart must not append a duplicate Indeterminate terminal"
    );
    let submit = restored
        .submit_message("/restart must remain fenced".to_string())
        .await
        .expect_err("restart must reject new admission after ambiguous Intent response");
    assert_code_ui_api_error(&submit, 409, "RECONCILIATION_REQUIRED");
    assert!(!pending_intent_revision_path(&store, &session_id).exists());
}

#[tokio::test(flavor = "multi_thread")]
async fn ordinary_intent_modify_terminal_failure_fences_and_survives_restart() {
    assert_ordinary_intent_response_terminal_failure_fences_and_survives_restart("modify").await;
}

#[tokio::test(flavor = "multi_thread")]
async fn ordinary_intent_cancel_terminal_failure_fences_and_survives_restart() {
    assert_ordinary_intent_response_terminal_failure_fences_and_survives_restart("cancel").await;
}

async fn assert_restored_nonmutating_intent_gate_terminal_owner(decision: &str) {
    let fixture = oversized_phase1_confirm_fixture(None).await;
    let OversizedPhase1ConfirmFixture {
        workdir,
        _storage,
        store,
        session_id,
        goal_store,
        persistence: _,
        runtime,
        source_interaction_id,
        risk_interaction_id: _,
        risk_pending_state: _,
    } = fixture;
    runtime
        .shutdown()
        .await
        .expect("graceful shutdown must preserve the initial Intent review authority");
    drop(runtime);
    tokio::time::sleep(Duration::from_millis(50)).await;

    let restored_state = store
        .load(&session_id)
        .expect("load parked Intent gate for replacement-owner terminal test");
    let persistence = HeadlessSessionPersistence::new(store, restored_state)
        .expect("reacquire parked Intent gate lease");
    let restored = build_runtime_with_persistence(
        "basic_chat",
        workdir.path().to_path_buf(),
        Vec::new(),
        Some(persistence),
    )
    .await
    .0;
    assert_eq!(
        await_pending_interaction(
            &restored,
            CodeUiInteractionKind::IntentReviewChoice,
            "restart must restore the pending Intent review",
        )
        .await,
        source_interaction_id
    );
    let restored_turn_id = restored
        .runtime_snapshot()
        .await
        .expect("read restored Intent gate owner")
        .active_turn_id
        .expect("restored Intent gate must have a replacement turn owner");
    let before = goal_store.load_code_workflow_replay().unwrap();
    let markers = before
        .events
        .iter()
        .filter_map(|event| match &event.event {
            CodeWorkflowEventKind::IntentReviewRequested {
                interaction_id,
                intent_id,
                turn_id,
                phase0_turn_id,
            } if interaction_id == &source_interaction_id => {
                Some((intent_id.clone(), turn_id.clone(), phase0_turn_id.clone()))
            }
            _ => None,
        })
        .collect::<Vec<_>>();
    assert!(
        !markers.is_empty(),
        "restart must retain the durable Intent review binding"
    );
    let latest = markers.last().unwrap();
    assert_eq!(latest.1, restored_turn_id);
    assert_ne!(
        latest.1, latest.2,
        "Phase 0 owner may not settle the restored review"
    );
    assert!(markers.iter().all(|marker| {
        marker.0 == latest.0 && marker.2 == latest.2 && !marker.2.trim().is_empty()
    }));
    let prior_turn_ids = markers
        .iter()
        .filter_map(|marker| {
            (!marker.1.is_empty() && marker.1 != restored_turn_id).then_some(marker.1.clone())
        })
        .collect::<Vec<_>>();
    let restored_owner = before
        .events
        .iter()
        .filter_map(|event| match &event.event {
            CodeWorkflowEventKind::CommandIntentPersisted { command }
                if command.identity.command_id == restored_turn_id =>
            {
                Some(command.clone())
            }
            _ => None,
        })
        .collect::<Vec<_>>();
    assert_eq!(restored_owner.len(), 1);
    assert_eq!(restored_owner[0].command_kind, CODE_UI_WEB_TURN_KIND);
    assert!(!restored_owner[0].mutating);

    restored
        .respond_interaction(
            &source_interaction_id,
            CodeUiInteractionResponse {
                selected_option: Some(decision.to_string()),
                ..Default::default()
            },
        )
        .await
        .unwrap_or_else(|error| {
            panic!("restored Intent {decision} must commit under its latest owner: {error:#}")
        });
    let after = goal_store
        .load_code_workflow_replay_committed()
        .expect("re-sync the acknowledged restored-gate terminal");
    let combined = after
        .events
        .iter()
        .filter_map(|event| match &event.event {
            CodeWorkflowEventKind::CommandTerminalSuccessWithInteractionResolved {
                command,
                interaction_id,
                resolution,
                intent_revision,
                ..
            } if interaction_id == &source_interaction_id && resolution == decision => {
                Some((command, intent_revision))
            }
            _ => None,
        })
        .collect::<Vec<_>>();
    assert_eq!(
        combined.len(),
        1,
        "restored Intent {decision} must have one durable combined terminal"
    );
    assert_eq!(combined[0].0, &restored_owner[0].identity);
    assert!(combined[0].1.is_none());
    assert!(prior_turn_ids.iter().all(|prior| {
        after.events.iter().all(|event| {
            !matches!(
                &event.event,
                CodeWorkflowEventKind::CommandTerminalSuccessWithInteractionResolved {
                    command,
                    interaction_id,
                    ..
                } if interaction_id == &source_interaction_id && &command.command_id == prior
            )
        })
    }));
    assert!(
        open_intent_review_from_workflow(after.events.iter().map(|event| &event.event))
            .is_none_or(|(interaction_id, ..)| interaction_id != source_interaction_id),
        "the acknowledged restored Intent {decision} must close the original generation"
    );
    assert_ne!(
        restored.snapshot().await.status,
        CodeUiSessionStatus::IndeterminateSideEffect
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn restored_nonmutating_intent_gate_confirm_and_cancel_use_latest_owner() {
    for decision in ["confirm", "cancel"] {
        assert_restored_nonmutating_intent_gate_terminal_owner(decision).await;
    }
}

async fn assert_multimarker_intent_gate_uses_latest_owner(decision: &str) {
    let fixture = oversized_phase1_confirm_fixture(None).await;
    let initial_replay = fixture.goal_store.load_code_workflow_replay().unwrap();
    let (intent_id, stale_turn_id, phase0_turn_id) = initial_replay
        .events
        .iter()
        .find_map(|event| match &event.event {
            CodeWorkflowEventKind::IntentReviewRequested {
                interaction_id,
                intent_id,
                turn_id,
                phase0_turn_id,
            } if interaction_id == &fixture.source_interaction_id => {
                Some((intent_id.clone(), turn_id.clone(), phase0_turn_id.clone()))
            }
            _ => None,
        })
        .expect("initial Intent review must have a durable marker");
    assert!(!stale_turn_id.is_empty());
    let phase0_source = initial_replay
        .events
        .iter()
        .find_map(|event| match &event.event {
            CodeWorkflowEventKind::CommandIntentPersisted { command }
                if command.identity.command_id == phase0_turn_id =>
            {
                Some(command.clone())
            }
            _ => None,
        })
        .expect("initial Intent marker must retain its mutating Phase 0 command lineage");
    assert!(phase0_source.mutating);

    let OversizedPhase1ConfirmFixture {
        workdir,
        _storage,
        store,
        session_id,
        goal_store,
        persistence: _,
        runtime,
        source_interaction_id,
        risk_interaction_id: _,
        risk_pending_state: _,
    } = fixture;
    runtime
        .shutdown()
        .await
        .expect("graceful shutdown must park the stale synthetic Intent owner");
    drop(runtime);
    tokio::time::sleep(Duration::from_millis(50)).await;

    let stale_owner = CodeCommandIntent::new(
        CodeCommandIdentity::new(
            phase0_source.identity.repo_id.clone(),
            phase0_source.identity.session_id.clone(),
            phase0_source.identity.principal_id.clone(),
            stale_turn_id.clone(),
        ),
        CODE_UI_WEB_TURN_KIND,
        format!("sha256:{}", "4".repeat(64)),
        false,
    );
    assert!(matches!(
        goal_store
            .admit_code_command(stale_owner.clone())
            .expect("persist the stale review owner fixture"),
        CodeCommandAdmission::Execute { .. }
    ));
    goal_store
        .complete_code_command_failure(
            &stale_owner.identity,
            "synthetic prior Intent review owner stopped before response",
        )
        .expect("terminalize only the stale owner, leaving the review authority open");
    let latest_turn_id = format!("replacement-intent-owner-{decision}-{}", Uuid::new_v4());
    goal_store
        .append_code_workflow_durable(CodeWorkflowEventKind::IntentReviewRequested {
            interaction_id: source_interaction_id.clone(),
            intent_id,
            turn_id: latest_turn_id.clone(),
            phase0_turn_id,
        })
        .expect("append a same-generation replacement binding after the stale owner");

    let state = store
        .load(&session_id)
        .expect("load multi-marker Intent gate fixture");
    let persistence = HeadlessSessionPersistence::new(store, state)
        .expect("reacquire multi-marker Intent gate lease");
    let restored = build_runtime_with_persistence(
        "basic_chat",
        workdir.path().to_path_buf(),
        Vec::new(),
        Some(persistence),
    )
    .await
    .0;
    assert_eq!(
        await_pending_interaction(
            &restored,
            CodeUiInteractionKind::IntentReviewChoice,
            "multi-marker restart must restore the latest Intent owner",
        )
        .await,
        source_interaction_id
    );
    assert_eq!(
        restored
            .runtime_snapshot()
            .await
            .expect("read multi-marker runtime owner")
            .active_turn_id
            .as_deref(),
        Some(latest_turn_id.as_str())
    );
    let before = goal_store.load_code_workflow_replay().unwrap();
    let latest_owner = before
        .events
        .iter()
        .filter_map(|event| match &event.event {
            CodeWorkflowEventKind::CommandIntentPersisted { command }
                if command.identity.command_id == latest_turn_id =>
            {
                Some(command.clone())
            }
            _ => None,
        })
        .collect::<Vec<_>>();
    assert_eq!(latest_owner.len(), 1);
    assert_eq!(latest_owner[0].command_kind, CODE_UI_WEB_TURN_KIND);
    assert!(!latest_owner[0].mutating);

    restored
        .respond_interaction(
            &source_interaction_id,
            CodeUiInteractionResponse {
                selected_option: Some(decision.to_string()),
                ..Default::default()
            },
        )
        .await
        .unwrap_or_else(|error| {
            panic!("latest multi-marker Intent owner must commit {decision}: {error:#}")
        });
    let after = goal_store.load_code_workflow_replay_committed().unwrap();
    assert_eq!(
        after
            .events
            .iter()
            .filter(|event| matches!(
                &event.event,
                CodeWorkflowEventKind::CommandTerminalSuccessWithInteractionResolved {
                    command,
                    interaction_id,
                    resolution,
                    ..
                } if command == &latest_owner[0].identity
                    && interaction_id == &source_interaction_id
                    && resolution == decision
            ))
            .count(),
        1
    );
    assert!(after.events.iter().all(|event| !matches!(
        &event.event,
        CodeWorkflowEventKind::CommandTerminalSuccessWithInteractionResolved {
            command,
            interaction_id,
            ..
        } if command == &stale_owner.identity && interaction_id == &source_interaction_id
    )));
    assert!(matches!(
        goal_store
            .recover_code_command(&stale_owner.identity)
            .unwrap(),
        libra::internal::ai::session::jsonl::CodeCommandRecovery::Existing {
            status: CodeCommandStatus::Failed { .. }
        }
    ));
    assert_ne!(
        restored.snapshot().await.status,
        CodeUiSessionStatus::IndeterminateSideEffect
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn restored_multimarker_intent_gate_confirm_and_cancel_use_latest_owner() {
    for decision in ["confirm", "cancel"] {
        assert_multimarker_intent_gate_uses_latest_owner(decision).await;
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn restored_nonmutating_intent_gate_modify_binds_revision_and_rearms_after_restart() {
    let fixture = oversized_phase1_confirm_fixture(None).await;
    let OversizedPhase1ConfirmFixture {
        workdir,
        _storage,
        store,
        session_id,
        goal_store,
        persistence: _,
        runtime,
        source_interaction_id,
        risk_interaction_id: _,
        risk_pending_state: _,
    } = fixture;
    runtime
        .shutdown()
        .await
        .expect("graceful shutdown must preserve the initial Intent review authority");
    drop(runtime);
    tokio::time::sleep(Duration::from_millis(50)).await;

    let restored_state = store
        .load(&session_id)
        .expect("load parked Intent gate for its replacement owner");
    let persistence = HeadlessSessionPersistence::new(store.clone(), restored_state)
        .expect("reacquire parked Intent gate lease");
    let restored = build_runtime_with_persistence(
        "basic_chat",
        workdir.path().to_path_buf(),
        Vec::new(),
        Some(persistence),
    )
    .await
    .0;
    assert_eq!(
        await_pending_interaction(
            &restored,
            CodeUiInteractionKind::IntentReviewChoice,
            "restart must restore the pending Intent review",
        )
        .await,
        source_interaction_id
    );
    let restored_turn_id = restored
        .runtime_snapshot()
        .await
        .expect("read restored Intent gate owner")
        .active_turn_id
        .expect("restored Intent gate must have a replacement turn owner");
    let before_modify = goal_store.load_code_workflow_replay().unwrap();
    let (_, _, durable_turn_id, _) =
        open_intent_review_from_workflow(before_modify.events.iter().map(|event| &event.event))
            .expect("restored Intent authority must remain open before Modify");
    assert_eq!(durable_turn_id, restored_turn_id);
    let restored_owner = before_modify
        .events
        .iter()
        .filter_map(|event| match &event.event {
            CodeWorkflowEventKind::CommandIntentPersisted { command }
                if command.identity.command_id == restored_turn_id =>
            {
                Some(command.clone())
            }
            _ => None,
        })
        .collect::<Vec<_>>();
    assert_eq!(
        restored_owner.len(),
        1,
        "the restored gate must have one durable replacement command"
    );
    assert_eq!(restored_owner[0].command_kind, CODE_UI_WEB_TURN_KIND);
    assert!(
        !restored_owner[0].mutating,
        "a parked IntentReview replacement is a nonmutating command owner"
    );

    let note = "preserve this restored-gate revision request privately".to_string();
    restored
        .respond_interaction(
            &source_interaction_id,
            CodeUiInteractionResponse {
                selected_option: Some("modify".to_string()),
                note: Some(note.clone()),
                ..Default::default()
            },
        )
        .await
        .expect("Modify on the restored nonmutating gate owner must durably commit");
    let sidecar_path = pending_intent_revision_path(&store, &session_id);
    let deadline = std::time::Instant::now() + Duration::from_secs(5);
    while std::time::Instant::now() < deadline {
        if restored.snapshot().await.status == CodeUiSessionStatus::Idle && sidecar_path.is_file() {
            break;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    assert_eq!(restored.snapshot().await.status, CodeUiSessionStatus::Idle);
    let replay = goal_store.load_code_workflow_replay().unwrap();
    let terminals = replay
        .events
        .iter()
        .filter_map(|event| match &event.event {
            CodeWorkflowEventKind::CommandTerminalSuccessWithInteractionResolved {
                command,
                interaction_id,
                resolution,
                intent_revision: Some(recovery),
                ..
            } if interaction_id == &source_interaction_id && resolution == "modify" => {
                Some((command, recovery))
            }
            _ => None,
        })
        .collect::<Vec<_>>();
    assert_eq!(terminals.len(), 1);
    assert_eq!(terminals[0].0, &restored_owner[0].identity);
    assert!(is_canonical_intent_revision_digest(
        &terminals[0].1.sidecar_digest
    ));
    let active_before_restart: serde_json::Value = serde_json::from_slice(
        &std::fs::read(&sidecar_path).expect("restored Modify must persist Active sidecar"),
    )
    .expect("restored Modify Active sidecar JSON");
    assert_eq!(active_before_restart["note"].as_str(), Some(note.as_str()));
    assert_eq!(
        active_before_restart["authority"]["command"],
        serde_json::to_value(&restored_owner[0].identity).unwrap()
    );
    assert_raw_note_absent_from_workflow_and_sse(&goal_store, &note);

    drop(restored);
    tokio::time::sleep(Duration::from_millis(50)).await;
    let state = store
        .load(&session_id)
        .expect("load restored-gate Modify session for revision rearm");
    let persistence = HeadlessSessionPersistence::new(store.clone(), state)
        .expect("reacquire restored-gate Modify session lease");
    let provider_entered = Arc::new(Notify::new());
    let provider_entered_wait = provider_entered.notified();
    tokio::pin!(provider_entered_wait);
    provider_entered_wait.as_mut().enable();
    let captured_history = Arc::new(std::sync::Mutex::new(None));
    let rearmed = build_pending_completion_runtime_with_capture(
        workdir.path().to_path_buf(),
        persistence,
        Arc::clone(&provider_entered),
        Some(Arc::clone(&captured_history)),
    )
    .await;
    let snapshot = rearmed.snapshot().await;
    assert_eq!(snapshot.status, CodeUiSessionStatus::Idle);
    assert!(snapshot.transcript.iter().all(|entry| {
        entry
            .content
            .as_deref()
            .is_none_or(|content| !content.contains(&note))
    }));
    assert!(snapshot.transcript.iter().any(|entry| {
        entry.content.as_deref().is_some_and(|content| {
            content.contains("retained privately for the next Phase 0 revision prompt")
        })
    }));
    let active_after_restart: serde_json::Value = serde_json::from_slice(
        &std::fs::read(&sidecar_path).expect("revision rearm must retain Active sidecar"),
    )
    .expect("rearmed Active sidecar JSON");
    assert_eq!(active_after_restart, active_before_restart);

    rearmed
        .submit_message("consume the restored private revision".to_string())
        .await
        .expect("rearmed revision mode must accept its next command");
    tokio::time::timeout(Duration::from_secs(5), &mut provider_entered_wait)
        .await
        .expect("rearmed revision consumer must reach the provider");
    let provider_history = captured_history
        .lock()
        .expect("read restored revision provider history")
        .clone()
        .expect("revision consumer must capture its provider prompt");
    assert!(provider_history.contains(&note));
    assert_raw_note_absent_from_workflow_and_sse(&goal_store, &note);
    rearmed
        .cancel_turn()
        .await
        .expect("cancel deliberately pending restored revision consumer");
}

#[tokio::test(flavor = "multi_thread")]
async fn intent_modify_prepared_terminal_crash_restores_exact_note_across_double_restart() {
    let fixture = oversized_phase1_confirm_fixture(None).await;
    let source_interaction_id = fixture.source_interaction_id.clone();
    let note = "retain this exact crash-safe revision request".to_string();
    fixture
        .runtime
        .respond_interaction(
            &source_interaction_id,
            CodeUiInteractionResponse {
                selected_option: Some("modify".to_string()),
                note: Some(note.clone()),
                ..Default::default()
            },
        )
        .await
        .expect("the real Modify path must prepare, terminalize, and promote its sidecar");

    let sidecar_path = pending_intent_revision_path(&fixture.store, &fixture.session_id);
    let deadline = std::time::Instant::now() + Duration::from_secs(5);
    while std::time::Instant::now() < deadline {
        if fixture.runtime.snapshot().await.status == CodeUiSessionStatus::Idle
            && sidecar_path.is_file()
        {
            break;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    assert_eq!(
        fixture.runtime.snapshot().await.status,
        CodeUiSessionStatus::Idle
    );

    let replay = fixture.goal_store.load_code_workflow_replay().unwrap();
    let (source_command, recovery) = replay
        .events
        .iter()
        .find_map(|event| match &event.event {
            CodeWorkflowEventKind::CommandTerminalSuccessWithInteractionResolved {
                command,
                interaction_id,
                resolution,
                intent_revision: Some(recovery),
                ..
            } if interaction_id == &source_interaction_id && resolution == "modify" => {
                Some((command.clone(), recovery.clone()))
            }
            _ => None,
        })
        .expect("the real Modify path must commit one digest-bound terminal");
    assert_eq!(recovery.interaction_id, source_interaction_id);
    assert!(is_canonical_intent_revision_digest(
        &recovery.sidecar_digest
    ));
    assert_raw_note_absent_from_workflow_and_sse(&fixture.goal_store, &note);

    let active: serde_json::Value = serde_json::from_slice(
        &std::fs::read(&sidecar_path).expect("read the real promoted revision sidecar"),
    )
    .expect("the promoted revision sidecar must remain valid JSON");
    assert_eq!(active["note"].as_str(), Some(note.as_str()));
    let authority = active["authority"]
        .as_object()
        .expect("the promoted sidecar must carry exact terminal authority");
    assert_eq!(
        authority["sidecarDigest"].as_str(),
        Some(recovery.sidecar_digest.as_str())
    );
    assert_eq!(
        authority["command"],
        serde_json::to_value(&source_command).unwrap()
    );

    // Model the exact crash window after the digest-only terminal fsync and
    // projection replay have resolved the source gate, but before Prepared is
    // promoted to Active. The body is derived from a real HMAC-authenticated
    // Active sidecar so startup exercises production verification rather than
    // a test-computed digest. In particular, startup must not require the
    // already-folded source interaction to remain Pending.
    let prepared_envelope = serde_json::json!({
        "intentSpec": "",
        "note": null,
        "prepared": {
            "schemaVersion": authority["schemaVersion"].clone(),
            "interactionId": authority["interactionId"].clone(),
            "command": authority["command"].clone(),
            "intentId": authority["intentId"].clone(),
            "note": note.clone(),
            "sidecarDigest": authority["sidecarDigest"].clone(),
        }
    });
    libra::utils::atomic_write::write_atomic(
        &sidecar_path,
        &serde_json::to_vec_pretty(&prepared_envelope).unwrap(),
        true,
    )
    .expect("persist the valid Prepared crash fixture durably");

    let OversizedPhase1ConfirmFixture {
        workdir,
        _storage,
        store,
        session_id,
        goal_store: _,
        persistence: _,
        runtime,
        source_interaction_id: _,
        risk_interaction_id: _,
        risk_pending_state: _,
    } = fixture;
    drop(runtime);
    tokio::time::sleep(Duration::from_millis(50)).await;
    let restored_state = store
        .load(&session_id)
        .expect("load terminal-only Modify crash fixture");
    let persistence = HeadlessSessionPersistence::new(store.clone(), restored_state)
        .expect("reacquire terminal-only Modify session lease");
    let restored = build_runtime_with_persistence(
        "basic_chat",
        workdir.path().to_path_buf(),
        Vec::new(),
        Some(persistence),
    )
    .await
    .0;
    let snapshot = restored.snapshot().await;
    assert_eq!(snapshot.status, CodeUiSessionStatus::Idle);
    assert!(snapshot.interactions.iter().all(|interaction| {
        interaction.id != source_interaction_id
            || interaction.status != CodeUiInteractionStatus::Pending
    }));
    assert!(snapshot.transcript.iter().all(|entry| {
        entry
            .content
            .as_deref()
            .is_none_or(|content| !content.contains(&note))
    }));
    assert!(snapshot.transcript.iter().any(|entry| {
        entry.content.as_deref().is_some_and(|content| {
            content.contains("retained privately for the next Phase 0 revision prompt")
        })
    }));
    assert!(snapshot.transcript.iter().all(|entry| {
        entry.id != "stale-current-worker-assistant"
            && !entry.streaming
            && entry.status.as_deref() != Some("running")
    }));
    let sidecar_after_first_restore: serde_json::Value = serde_json::from_slice(
        &std::fs::read(&sidecar_path).expect("startup must promote the durable revision sidecar"),
    )
    .expect("recovered revision sidecar must remain valid JSON");
    assert_eq!(
        sidecar_after_first_restore
            .get("note")
            .and_then(serde_json::Value::as_str),
        Some(note.as_str())
    );
    assert!(sidecar_after_first_restore.get("prepared").is_none());
    assert_eq!(
        sidecar_after_first_restore["authority"]["sidecarDigest"].as_str(),
        Some(recovery.sidecar_digest.as_str())
    );

    drop(restored);
    tokio::time::sleep(Duration::from_millis(50)).await;
    let restored_state = store
        .load(&session_id)
        .expect("load the once-promoted revision session a second time");
    let persistence = HeadlessSessionPersistence::new(store.clone(), restored_state)
        .expect("reacquire the promoted revision session lease");
    let provider_entered = Arc::new(Notify::new());
    let provider_entered_wait = provider_entered.notified();
    tokio::pin!(provider_entered_wait);
    provider_entered_wait.as_mut().enable();
    let captured_history = Arc::new(std::sync::Mutex::new(None));
    let restored_again = build_pending_completion_runtime_with_capture(
        workdir.path().to_path_buf(),
        persistence,
        Arc::clone(&provider_entered),
        Some(Arc::clone(&captured_history)),
    )
    .await;
    assert_eq!(
        restored_again.snapshot().await.status,
        CodeUiSessionStatus::Idle
    );
    let sidecar_after_second_restore: serde_json::Value = serde_json::from_slice(
        &std::fs::read(&sidecar_path).expect("the second restore must retain Active sidecar state"),
    )
    .expect("the twice-restored sidecar must remain valid JSON");
    assert_eq!(sidecar_after_second_restore, sidecar_after_first_restore);
    let replay = SessionJsonlStore::new(store.session_root(&session_id))
        .load_code_workflow_replay()
        .expect("read the twice-restored revision workflow");
    assert_eq!(
        replay
            .events
            .iter()
            .filter(|event| matches!(
                &event.event,
                CodeWorkflowEventKind::CommandTerminalSuccessWithInteractionResolved {
                    interaction_id,
                    intent_revision: Some(actual),
                    ..
                } if interaction_id == &source_interaction_id && actual == &recovery
            ))
            .count(),
        1,
        "double startup must reuse the single digest-bound Modify terminal"
    );
    assert!(replay.events.iter().all(|event| !matches!(
        &event.event,
        CodeWorkflowEventKind::InteractionResolved {
            intent_revision_consumption: Some(_),
            ..
        }
    )));

    restored_again
        .submit_message("apply the privately retained revision".to_string())
        .await
        .expect("the twice-restored revision must admit its exact consumer");
    tokio::time::timeout(Duration::from_secs(5), &mut provider_entered_wait)
        .await
        .expect("the restored revision consumer must reach the provider");
    let provider_history = captured_history
        .lock()
        .expect("read captured revision provider history")
        .clone()
        .expect("the pending provider must capture its request history");
    assert!(
        provider_history.contains(&note),
        "the private sidecar note must still reach the next Phase 0 provider prompt"
    );
    assert!(provider_history.contains("apply the privately retained revision"));
    let consuming_after_provider: serde_json::Value =
        serde_json::from_slice(&std::fs::read(&sidecar_path).expect(
            "Consuming sidecar must remain until the replacement review and receipt are durable",
        ))
        .expect("the in-flight consumer sidecar must remain valid JSON");
    assert!(
        consuming_after_provider.get("consuming").is_some(),
        "the pending provider must keep the Consuming envelope until submit_intent_draft succeeds"
    );
    assert_raw_note_absent_from_workflow_and_sse(
        &SessionJsonlStore::new(store.session_root(&session_id)),
        &note,
    );
    restored_again
        .cancel_turn()
        .await
        .expect("cancel the deliberately pending revision provider");
    drop(restored_again);
    tokio::time::sleep(Duration::from_millis(50)).await;
    let state = store
        .load(&session_id)
        .expect("load the pre-receipt Consuming revision session");
    let persistence = HeadlessSessionPersistence::new(store.clone(), state)
        .expect("reacquire pre-receipt Consuming revision lease");
    let after_pre_receipt_cancel =
        try_build_runtime_with_persistence("basic_chat", workdir.path().to_path_buf(), persistence)
            .await
            .expect("a pre-receipt Consuming envelope must restart without fencing");
    assert_eq!(
        after_pre_receipt_cancel.snapshot().await.status,
        CodeUiSessionStatus::Idle
    );
    assert!(
        sidecar_path.exists(),
        "canceling a pending replacement provider must keep the unconsumed revision sidecar"
    );
    let replay = SessionJsonlStore::new(store.session_root(&session_id))
        .load_code_workflow_replay()
        .unwrap();
    assert!(
        replay.events.iter().all(|event| !matches!(
            &event.event,
            CodeWorkflowEventKind::InteractionResolved {
                intent_revision_consumption: Some(consumption),
                ..
            } if consumption.claim.interaction_id == source_interaction_id
        )),
        "a cancelled pre-receipt consumer must not commit a revision receipt"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn prepared_intent_revision_without_terminal_is_gc_before_gate_restore() {
    let fixture = oversized_phase1_confirm_fixture(None).await;
    let source_interaction_id = fixture.source_interaction_id.clone();
    let durable_open_prefix = fixture
        .goal_store
        .load_events()
        .expect("capture the durable open-gate prefix before Prepare");
    let note = "discard this dormant Prepared note".to_string();
    fixture
        .runtime
        .respond_interaction(
            &source_interaction_id,
            CodeUiInteractionResponse {
                selected_option: Some("modify".to_string()),
                note: Some(note.clone()),
                ..Default::default()
            },
        )
        .await
        .expect("derive a real HMAC-authenticated sidecar body");
    let sidecar_path = pending_intent_revision_path(&fixture.store, &fixture.session_id);
    let active: serde_json::Value = serde_json::from_slice(&std::fs::read(&sidecar_path).unwrap())
        .expect("real Active sidecar JSON");
    let authority = active["authority"]
        .as_object()
        .expect("real Active authority");
    let prepared = serde_json::json!({
        "intentSpec": "",
        "prepared": {
            "schemaVersion": authority["schemaVersion"].clone(),
            "interactionId": authority["interactionId"].clone(),
            "command": authority["command"].clone(),
            "intentId": authority["intentId"].clone(),
            "note": note.clone(),
            "sidecarDigest": authority["sidecarDigest"].clone(),
        }
    });
    libra::utils::atomic_write::write_atomic(
        &sidecar_path,
        &serde_json::to_vec_pretty(&prepared).unwrap(),
        true,
    )
    .expect("persist the real Prepared body");

    let mut prefix = durable_open_prefix
        .iter()
        .map(|event| serde_json::to_string(event).unwrap())
        .collect::<Vec<_>>()
        .join("\n");
    prefix.push('\n');
    libra::utils::atomic_write::write_atomic(
        &fixture.goal_store.events_path(),
        prefix.as_bytes(),
        true,
    )
    .expect("restore the exact preterminal workflow prefix");

    let OversizedPhase1ConfirmFixture {
        workdir,
        _storage,
        store,
        session_id,
        goal_store,
        persistence: _,
        runtime,
        source_interaction_id: _,
        risk_interaction_id: _,
        risk_pending_state: _,
    } = fixture;
    drop(runtime);
    tokio::time::sleep(Duration::from_millis(50)).await;
    let state = store
        .load(&session_id)
        .expect("load the restored open-gate prefix");
    let persistence = HeadlessSessionPersistence::new(store.clone(), state)
        .expect("reacquire the dormant Prepared fixture lease");
    let restored =
        try_build_runtime_with_persistence("basic_chat", workdir.path().to_path_buf(), persistence)
            .await
            .expect("dormant Prepared residue must be GCed before restoring its source gate");
    let snapshot = restored.snapshot().await;
    assert_eq!(snapshot.status, CodeUiSessionStatus::AwaitingInteraction);
    assert!(snapshot.interactions.iter().any(|interaction| {
        interaction.id == source_interaction_id
            && interaction.status == CodeUiInteractionStatus::Pending
    }));
    assert!(
        !sidecar_path.exists(),
        "preterminal Prepared residue must be durably removed"
    );
    assert!(intent_revision_hmac_key_path(&store, &session_id).is_file());
    let replay = goal_store.load_code_workflow_replay().unwrap();
    assert!(replay.events.iter().all(|event| !matches!(
        &event.event,
        CodeWorkflowEventKind::CommandTerminalSuccessWithInteractionResolved {
            interaction_id,
            ..
        } if interaction_id == &source_interaction_id
    )));
    assert_raw_note_absent_from_workflow_and_sse(&goal_store, &note);
}

#[tokio::test(flavor = "multi_thread")]
async fn consuming_intent_revision_without_receipt_survives_two_real_startups() {
    const RECOVERED_CONSUMER_FAILURE: &str = "IntentSpec revision consumer stopped before its durable consumption receipt; the revision remains available for retry";

    let fixture = oversized_phase1_confirm_fixture(None).await;
    let note = "keep this Consuming handoff private and stable".to_string();
    fixture
        .runtime
        .respond_interaction(
            &fixture.source_interaction_id,
            CodeUiInteractionResponse {
                selected_option: Some("modify".to_string()),
                note: Some(note.clone()),
                ..Default::default()
            },
        )
        .await
        .expect("the real Modify path must create an authenticated Active sidecar");

    let sidecar_path = pending_intent_revision_path(&fixture.store, &fixture.session_id);
    let active: serde_json::Value = serde_json::from_slice(
        &std::fs::read(&sidecar_path).expect("read the real Active revision sidecar"),
    )
    .expect("the real Active sidecar must be valid JSON");
    assert_eq!(active["note"].as_str(), Some(note.as_str()));
    let authority = active["authority"]
        .as_object()
        .expect("Active must retain its bound terminal authority");
    let replay = fixture.goal_store.load_code_workflow_replay().unwrap();
    let (source_command, terminal_event_id, terminal_sequence, sidecar_digest) = replay
        .events
        .iter()
        .find_map(|event| match &event.event {
            CodeWorkflowEventKind::CommandTerminalSuccessWithInteractionResolved {
                command,
                interaction_id,
                resolution,
                intent_revision: Some(recovery),
                ..
            } if interaction_id == &fixture.source_interaction_id && resolution == "modify" => {
                Some((
                    command.clone(),
                    event.event_id,
                    event.sequence,
                    recovery.sidecar_digest.clone(),
                ))
            }
            _ => None,
        })
        .expect("Active must have one exact digest-bound terminal");
    assert_eq!(
        authority["sidecarDigest"].as_str(),
        Some(sidecar_digest.as_str())
    );
    let consumer_intent = CodeCommandIntent::new(
        CodeCommandIdentity::new(
            source_command.repo_id.clone(),
            source_command.session_id.clone(),
            source_command.principal_id.clone(),
            "consuming-crash-command",
        ),
        CODE_UI_WEB_TURN_KIND,
        format!("sha256:{}", "d".repeat(64)),
        true,
    );
    assert!(matches!(
        fixture
            .goal_store
            .admit_code_command(consumer_intent.clone())
            .unwrap(),
        CodeCommandAdmission::Execute { .. }
    ));
    let claim = IntentRevisionConsumptionClaim {
        schema_version: INTENT_REVISION_CONSUMPTION_SCHEMA_VERSION,
        interaction_id: fixture.source_interaction_id.clone(),
        source_command,
        consumer_intent: consumer_intent.clone(),
        terminal_event_id,
        terminal_sequence,
        intent_id: authority["intentId"]
            .as_str()
            .expect("Active authority carries intentId")
            .to_string(),
        sidecar_digest: Some(sidecar_digest),
    };
    let consumption = fixture
        .goal_store
        .prepare_intent_revision_consumption(&consumer_intent, &claim)
        .expect("resolve the exact durable consumer without appending its receipt");
    let consuming_envelope = serde_json::json!({
        "intentSpec": "",
        "consuming": {
            "schemaVersion": INTENT_REVISION_CONSUMPTION_SCHEMA_VERSION,
            "active": active,
            "consumption": consumption,
        }
    });
    let consuming_bytes = serde_json::to_vec_pretty(&consuming_envelope).unwrap();
    libra::utils::atomic_write::write_atomic(&sidecar_path, &consuming_bytes, true)
        .expect("persist the Consuming-before-receipt crash boundary");
    assert!(
        fixture
            .goal_store
            .load_code_workflow_replay()
            .unwrap()
            .events
            .iter()
            .all(|event| !matches!(
                &event.event,
                CodeWorkflowEventKind::InteractionResolved {
                    intent_revision_consumption: Some(_),
                    ..
                }
            ))
    );

    let OversizedPhase1ConfirmFixture {
        workdir,
        _storage,
        store,
        session_id,
        goal_store,
        persistence: _,
        runtime,
        source_interaction_id: _,
        risk_interaction_id: _,
        risk_pending_state: _,
    } = fixture;
    drop(runtime);
    tokio::time::sleep(Duration::from_millis(50)).await;

    for startup in 1..=2 {
        let restored_state = store.load(&session_id).unwrap_or_else(|error| {
            panic!("load Consuming fixture before startup {startup}: {error}")
        });
        let persistence = HeadlessSessionPersistence::new(store.clone(), restored_state)
            .unwrap_or_else(|error| {
                panic!("reacquire Consuming lease for startup {startup}: {error}")
            });
        let provider_entered = Arc::new(Notify::new());
        let provider_entered_wait = provider_entered.notified();
        tokio::pin!(provider_entered_wait);
        provider_entered_wait.as_mut().enable();
        let restored = build_pending_completion_runtime(
            workdir.path().to_path_buf(),
            persistence,
            Arc::clone(&provider_entered),
        )
        .await;
        let snapshot = restored.snapshot().await;
        assert_eq!(
            snapshot.status,
            CodeUiSessionStatus::Idle,
            "startup {startup} must recover unconsumed revision mode without fencing"
        );
        assert!(
            snapshot.tool_calls.is_empty(),
            "startup must not rerun a provider"
        );
        assert!(
            tokio::time::timeout(Duration::from_millis(100), provider_entered_wait.as_mut())
                .await
                .is_err(),
            "startup {startup} must not invoke the provider for the failed Consuming owner"
        );
        assert!(snapshot.transcript.iter().all(|entry| {
            entry
                .content
                .as_deref()
                .is_none_or(|content| !content.contains(&note))
        }));
        assert!(snapshot.transcript.iter().any(|entry| {
            entry.content.as_deref().is_some_and(|content| {
                content.contains("retained privately for the next Phase 0 revision prompt")
            })
        }));
        let disk = std::fs::read(&sidecar_path)
            .unwrap_or_else(|error| panic!("startup {startup} retains Consuming: {error}"));
        assert_eq!(disk, consuming_bytes);
        let disk: serde_json::Value = serde_json::from_slice(&disk).unwrap();
        assert_eq!(disk, consuming_envelope);
        let replay = goal_store.load_code_workflow_replay().unwrap();
        assert_eq!(
            replay
                .events
                .iter()
                .filter(|event| matches!(
                    &event.event,
                    CodeWorkflowEventKind::CommandIntentPersisted { command }
                        if command == &consumer_intent
                ))
                .count(),
            1
        );
        assert_eq!(
            replay
                .events
                .iter()
                .filter(|event| matches!(
                    &event.event,
                    CodeWorkflowEventKind::CommandTerminalFailure {
                        command,
                        reason,
                        interaction_resolutions,
                        retry_intent_review,
                    } if command == &consumer_intent.identity
                        && reason == RECOVERED_CONSUMER_FAILURE
                        && interaction_resolutions.is_empty()
                        && retry_intent_review.is_none()
                ))
                .count(),
            1,
            "startup {startup} must retain exactly one determinate failed consumer tombstone"
        );
        assert!(matches!(
            goal_store
                .recover_code_command(&consumer_intent.identity)
                .unwrap(),
            libra::internal::ai::session::jsonl::CodeCommandRecovery::Existing {
                status: CodeCommandStatus::Failed { reason }
            } if reason == RECOVERED_CONSUMER_FAILURE
        ));
        assert!(replay.events.iter().all(|event| !matches!(
            &event.event,
            CodeWorkflowEventKind::CommandIndeterminateSideEffect { command, .. }
                if command == &consumer_intent.identity
        )));
        assert!(replay.events.iter().all(|event| !matches!(
            &event.event,
            CodeWorkflowEventKind::InteractionResolved {
                intent_revision_consumption: Some(_),
                ..
            }
        )));
        assert_raw_note_absent_from_workflow_and_sse(&goal_store, &note);
        drop(restored);
        tokio::time::sleep(Duration::from_millis(50)).await;
    }

    let state = store
        .load(&session_id)
        .expect("load twice-restored Consuming fixture for a new consumer");
    let persistence = HeadlessSessionPersistence::new(store.clone(), state)
        .expect("reacquire twice-restored Consuming fixture lease");
    let replacement = build_runtime_with_persistence(
        "phase0_intent_review",
        workdir.path().to_path_buf(),
        Vec::new(),
        Some(persistence),
    )
    .await
    .0;
    replacement
        .submit_message("continue after the aborted revision consumer".to_string())
        .await
        .expect("a later legal consumer must not be blocked by the failed Consuming owner");
    let risk_id = await_pending_interaction(
        &replacement,
        CodeUiInteractionKind::RequestUserInput,
        "the replacement consumer must ask for risk before submitting the replacement draft",
    )
    .await;
    replacement
        .respond_interaction(
            &risk_id,
            CodeUiInteractionResponse {
                selected_option: Some("Low".to_string()),
                ..Default::default()
            },
        )
        .await
        .expect("replacement consumer risk answer must be accepted");
    let _revised_review = await_pending_interaction(
        &replacement,
        CodeUiInteractionKind::IntentReviewChoice,
        "the replacement consumer must park a replacement IntentReviewChoice",
    )
    .await;
    assert!(
        !sidecar_path.exists(),
        "the replacement consumer must unlink Consuming after its durable receipt"
    );
    let receipts = goal_store
        .load_code_workflow_replay()
        .unwrap()
        .events
        .iter()
        .filter_map(|event| match &event.event {
            CodeWorkflowEventKind::InteractionResolved {
                intent_revision_consumption: Some(consumption),
                ..
            } => Some(consumption.clone()),
            _ => None,
        })
        .collect::<Vec<_>>();
    assert_eq!(
        receipts.len(),
        1,
        "the replacement consumer must commit its unique receipt"
    );
    assert_ne!(
        receipts[0].claim.consumer_intent.identity,
        consumer_intent.identity
    );
    let replay = goal_store.load_code_workflow_replay().unwrap();
    assert_eq!(
        replay
            .events
            .iter()
            .filter(|event| matches!(
                &event.event,
                CodeWorkflowEventKind::CommandTerminalFailure {
                    command,
                    reason,
                    interaction_resolutions,
                    retry_intent_review,
                } if command == &consumer_intent.identity
                    && reason == RECOVERED_CONSUMER_FAILURE
                    && interaction_resolutions.is_empty()
                    && retry_intent_review.is_none()
            ))
            .count(),
        1
    );
    assert!(replay.events.iter().all(|event| !matches!(
        &event.event,
        CodeWorkflowEventKind::CommandIndeterminateSideEffect { command, .. }
            if command == &consumer_intent.identity
    )));
    assert_raw_note_absent_from_workflow_and_sse(&goal_store, &note);
}

async fn assert_current_worker_pre_receipt_indeterminate_restarts_unfenced() {
    const EFFECT: &str = "mutating_runtime_turn";
    const REASON: &str = "IntentSpec revision consumption stopped before its durable receipt; the revision remains available for retry";

    let RealBoundIntentRevisionFixture {
        fixture,
        active_sidecar,
        terminal,
        source_command,
        summary: _,
        recovery,
        intent_id,
        hmac_key: _,
    } = real_bound_intent_revision_fixture(
        "private current-worker note survives a pre-receipt persistence failure",
    )
    .await;
    let consumer_intent = CodeCommandIntent::new(
        CodeCommandIdentity::new(
            source_command.repo_id.clone(),
            source_command.session_id.clone(),
            source_command.principal_id.clone(),
            "current-worker-pre-receipt-consumer",
        ),
        CODE_UI_WEB_TURN_KIND,
        format!("sha256:{}", "7".repeat(64)),
        true,
    );
    assert!(matches!(
        fixture
            .goal_store
            .admit_code_command(consumer_intent.clone())
            .unwrap(),
        CodeCommandAdmission::Execute { .. }
    ));
    let claim = IntentRevisionConsumptionClaim {
        schema_version: INTENT_REVISION_CONSUMPTION_SCHEMA_VERSION,
        interaction_id: recovery.interaction_id.clone(),
        source_command,
        consumer_intent: consumer_intent.clone(),
        terminal_event_id: terminal.event_id,
        terminal_sequence: terminal.sequence,
        intent_id,
        sidecar_digest: Some(recovery.sidecar_digest),
    };
    let consumption = fixture
        .goal_store
        .prepare_intent_revision_consumption(&consumer_intent, &claim)
        .expect("resolve the exact current-worker revision consumer");
    let sidecar_path = pending_intent_revision_path(&fixture.store, &fixture.session_id);
    let consuming_envelope = serde_json::json!({
        "intentSpec": "",
        "consuming": {
            "schemaVersion": active_sidecar["authority"]["schemaVersion"].clone(),
            "active": active_sidecar,
            "consumption": consumption,
        }
    });
    let consuming_bytes = serde_json::to_vec_pretty(&consuming_envelope).unwrap();
    libra::utils::atomic_write::write_atomic(&sidecar_path, &consuming_bytes, true)
        .expect("persist the reachable Consuming-before-receipt state");
    fixture
        .goal_store
        .mark_code_command_indeterminate(&consumer_intent.identity, EFFECT, REASON)
        .expect("persist the canonical current-worker pre-receipt Indeterminate terminal");

    let mut stale_snapshot = fixture.runtime.snapshot().await;
    stale_snapshot.status = CodeUiSessionStatus::IndeterminateSideEffect;
    let now = chrono::Utc::now();
    stale_snapshot.transcript.push(CodeUiTranscriptEntry {
        id: "stale-current-worker-assistant".to_string(),
        kind: CodeUiTranscriptEntryKind::AssistantMessage,
        title: Some("Interrupted revision consumer".to_string()),
        content: Some("stale partial output".to_string()),
        status: Some("streaming".to_string()),
        streaming: true,
        metadata: serde_json::json!({ "phase": "intent-revision-consumer" }),
        created_at: now,
        updated_at: now,
    });
    stale_snapshot.tool_calls.push(CodeUiToolCallSnapshot {
        id: "stale-current-worker-tool".to_string(),
        tool_name: "revision_consumer".to_string(),
        status: "running".to_string(),
        summary: Some("Interrupted before durable receipt".to_string()),
        details: None,
        updated_at: now,
    });

    let OversizedPhase1ConfirmFixture {
        workdir,
        _storage,
        store,
        session_id,
        goal_store,
        persistence: _,
        runtime,
        source_interaction_id: _,
        risk_interaction_id: _,
        risk_pending_state: _,
    } = fixture;
    drop(runtime);
    tokio::time::sleep(Duration::from_millis(50)).await;

    let mut stale_state = store
        .load(&session_id)
        .expect("load current-worker session before injecting stale Web projection");
    stale_state.metadata.insert(
        "code_ui_snapshot".to_string(),
        serde_json::to_value(stale_snapshot).expect("serialize stale Indeterminate projection"),
    );
    stale_state.metadata.insert(
        "code_ui_projection_cursor".to_string(),
        serde_json::json!(
            goal_store
                .load_code_workflow_replay()
                .unwrap()
                .events
                .last()
                .map(|event| event.sequence)
                .unwrap_or(0)
        ),
    );
    store
        .save(&stale_state)
        .expect("persist the stale current-worker Indeterminate projection");

    let state = store
        .load(&session_id)
        .expect("load current-worker pre-receipt crash fixture");
    let persistence = HeadlessSessionPersistence::new(store, state)
        .expect("reacquire current-worker pre-receipt crash fixture");
    let provider_entered = Arc::new(Notify::new());
    let provider_entered_wait = provider_entered.notified();
    tokio::pin!(provider_entered_wait);
    provider_entered_wait.as_mut().enable();
    let restored = build_pending_completion_runtime(
        workdir.path().to_path_buf(),
        persistence,
        Arc::clone(&provider_entered),
    )
    .await;
    let snapshot = restored.snapshot().await;
    assert_eq!(snapshot.status, CodeUiSessionStatus::Idle);
    assert_ne!(
        snapshot.status,
        CodeUiSessionStatus::IndeterminateSideEffect
    );
    assert!(snapshot.tool_calls.is_empty());
    assert!(snapshot.transcript.iter().all(|entry| {
        entry.content.as_deref().is_none_or(|content| {
            !content
                .contains("private current-worker note survives a pre-receipt persistence failure")
        })
    }));
    assert!(snapshot.transcript.iter().any(|entry| {
        entry.content.as_deref().is_some_and(|content| {
            content.contains("retained privately for the next Phase 0 revision prompt")
        })
    }));
    assert!(
        tokio::time::timeout(Duration::from_millis(100), provider_entered_wait.as_mut())
            .await
            .is_err(),
        "startup must not rerun the current-worker consumer"
    );
    assert_eq!(std::fs::read(&sidecar_path).unwrap(), consuming_bytes);
    let replay = goal_store.load_code_workflow_replay().unwrap();
    assert!(replay.events.iter().all(|event| !matches!(
        &event.event,
        CodeWorkflowEventKind::InteractionResolved {
            intent_revision_consumption: Some(_),
            ..
        }
    )));
    assert_eq!(
        replay
            .events
            .iter()
            .filter(|event| matches!(
                &event.event,
                CodeWorkflowEventKind::CommandIndeterminateSideEffect {
                    command,
                    effect,
                    reason,
                } if command == &consumer_intent.identity
                    && effect == EFFECT
                    && reason == REASON
            ))
            .count(),
        1
    );
    assert!(replay.events.iter().all(|event| !matches!(
        &event.event,
        CodeWorkflowEventKind::CommandTerminalFailure { command, .. }
            if command == &consumer_intent.identity
    )));
    assert!(matches!(
        goal_store
            .recover_code_command(&consumer_intent.identity)
            .unwrap(),
        libra::internal::ai::session::jsonl::CodeCommandRecovery::Existing {
            status: CodeCommandStatus::Indeterminate { effect, reason }
        } if effect == EFFECT && reason == REASON
    ));
    assert_raw_note_absent_from_workflow_and_sse(
        &goal_store,
        "private current-worker note survives a pre-receipt persistence failure",
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn current_worker_pre_receipt_indeterminate_repairs_stale_projection_unfenced() {
    assert_current_worker_pre_receipt_indeterminate_restarts_unfenced().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn intent_revision_sidecar_and_hmac_files_are_bounded_and_fail_closed() {
    let fixture = oversized_phase1_confirm_fixture(None).await;
    let note = "authenticate this local revision sidecar".to_string();
    fixture
        .runtime
        .respond_interaction(
            &fixture.source_interaction_id,
            CodeUiInteractionResponse {
                selected_option: Some("modify".to_string()),
                note: Some(note.clone()),
                ..Default::default()
            },
        )
        .await
        .expect("create a real digest-bound Active sidecar");
    let sidecar_path = pending_intent_revision_path(&fixture.store, &fixture.session_id);
    let hmac_key_path = intent_revision_hmac_key_path(&fixture.store, &fixture.session_id);
    let intents_dir = fixture
        .store
        .session_root(&fixture.session_id)
        .join("intents");
    assert_eq!(sidecar_path.parent(), Some(intents_dir.as_path()));
    assert_eq!(hmac_key_path.parent(), Some(intents_dir.as_path()));
    let sidecar_bytes = std::fs::read(&sidecar_path).expect("read real Active sidecar bytes");
    let hmac_key_bytes = std::fs::read(&hmac_key_path).expect("read session-local HMAC key");
    assert_eq!(hmac_key_bytes.len(), 32);
    assert!(sidecar_bytes.len() < 24 * 1024 * 1024);
    assert!(
        std::fs::symlink_metadata(&sidecar_path)
            .unwrap()
            .file_type()
            .is_file()
    );
    assert!(
        std::fs::symlink_metadata(&hmac_key_path)
            .unwrap()
            .file_type()
            .is_file()
    );
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;

        assert_eq!(
            std::fs::metadata(&hmac_key_path)
                .unwrap()
                .permissions()
                .mode()
                & 0o077,
            0,
            "the private HMAC key must not be accessible to group/other users"
        );
    }

    let OversizedPhase1ConfirmFixture {
        workdir,
        _storage,
        store,
        session_id,
        goal_store,
        persistence: _,
        runtime,
        source_interaction_id: _,
        risk_interaction_id: _,
        risk_pending_state: _,
    } = fixture;
    drop(runtime);
    tokio::time::sleep(Duration::from_millis(50)).await;

    let key_backup = intents_dir.join("revision_hmac.key.backup");
    std::fs::rename(&hmac_key_path, &key_backup).expect("hide committed HMAC key");
    let error = intent_revision_restart_error(&store, &session_id, workdir.path()).await;
    assert!(
        error.contains("HMAC key") && error.contains("missing"),
        "missing committed key must fail closed: {error}"
    );
    assert!(
        !hmac_key_path.exists(),
        "startup must not rotate a missing committed HMAC key"
    );
    assert_eq!(std::fs::read(&key_backup).unwrap(), hmac_key_bytes);
    std::fs::rename(&key_backup, &hmac_key_path).expect("restore committed HMAC key");

    #[cfg(unix)]
    {
        use std::os::unix::fs::symlink;

        let key_target = intents_dir.join("revision_hmac.key.real");
        std::fs::rename(&hmac_key_path, &key_target).expect("move key behind a symlink");
        symlink(&key_target, &hmac_key_path).expect("create matching-shape key symlink");
        let error = intent_revision_restart_error(&store, &session_id, workdir.path()).await;
        assert!(
            error.contains("symbolic link") || error.contains("regular file"),
            "HMAC key symlinks must fail closed: {error}"
        );
        std::fs::remove_file(&hmac_key_path).expect("remove temporary key symlink");
        std::fs::rename(&key_target, &hmac_key_path).expect("restore regular HMAC key");

        let sidecar_target = intents_dir.join("pending_revision.json.real");
        std::fs::rename(&sidecar_path, &sidecar_target).expect("move sidecar behind a symlink");
        symlink(&sidecar_target, &sidecar_path).expect("create matching-shape sidecar symlink");
        let error = intent_revision_restart_error(&store, &session_id, workdir.path()).await;
        assert!(
            error.contains("symbolic link") || error.contains("regular file"),
            "revision sidecar symlinks must fail closed: {error}"
        );
        std::fs::remove_file(&sidecar_path).expect("remove temporary sidecar symlink");
        std::fs::rename(&sidecar_target, &sidecar_path).expect("restore regular sidecar");
    }

    let sidecar_backup = intents_dir.join("pending_revision.json.backup");
    std::fs::rename(&sidecar_path, &sidecar_backup).expect("hide bound Active sidecar");
    let error = intent_revision_restart_error(&store, &session_id, workdir.path()).await;
    assert!(
        error.contains("missing both its sidecar") && error.contains("receipt"),
        "a bound unconsumed terminal may not infer consumption from absence: {error}"
    );
    assert!(!sidecar_path.exists());
    std::fs::rename(&sidecar_backup, &sidecar_path).expect("restore Active sidecar");

    std::fs::rename(&sidecar_path, &sidecar_backup).expect("replace sidecar with a directory");
    std::fs::create_dir(&sidecar_path).expect("create non-file sidecar fixture");
    let error = intent_revision_restart_error(&store, &session_id, workdir.path()).await;
    assert!(
        error.contains("not a regular file"),
        "a directory at the sidecar path must fail closed: {error}"
    );
    std::fs::remove_dir(&sidecar_path).expect("remove non-file sidecar fixture");
    std::fs::rename(&sidecar_backup, &sidecar_path).expect("restore Active sidecar");

    std::fs::rename(&sidecar_path, &sidecar_backup).expect("replace sidecar with oversized file");
    let oversized = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&sidecar_path)
        .expect("create oversized sidecar fixture");
    oversized
        .set_len(24 * 1024 * 1024 + 1)
        .expect("extend oversized sidecar fixture");
    drop(oversized);
    let error = intent_revision_restart_error(&store, &session_id, workdir.path()).await;
    assert!(
        error.contains("exceeds the 25165824-byte limit"),
        "oversized sidecar must fail before parsing: {error}"
    );
    std::fs::remove_file(&sidecar_path).expect("remove oversized sidecar fixture");
    std::fs::rename(&sidecar_backup, &sidecar_path).expect("restore Active sidecar");

    let mut tampered: serde_json::Value = serde_json::from_slice(&sidecar_bytes).unwrap();
    tampered["note"] = serde_json::Value::String("tampered revision note".to_string());
    libra::utils::atomic_write::write_atomic(
        &sidecar_path,
        &serde_json::to_vec_pretty(&tampered).unwrap(),
        true,
    )
    .expect("persist digest-conflicting sidecar fixture");
    let error = intent_revision_restart_error(&store, &session_id, workdir.path()).await;
    assert!(
        error.contains("digest does not match"),
        "a digest/body conflict must fence startup: {error}"
    );

    let mut wrong_authority: serde_json::Value = serde_json::from_slice(&sidecar_bytes).unwrap();
    let mut wrong_command: CodeCommandIdentity =
        serde_json::from_value(wrong_authority["authority"]["command"].clone()).unwrap();
    wrong_command.repo_id = "wrong-repo".to_string();
    let wrong_digest = revision_sidecar_digest_for_test(
        u32::try_from(
            wrong_authority["authority"]["schemaVersion"]
                .as_u64()
                .unwrap(),
        )
        .unwrap(),
        wrong_authority["authority"]["interactionId"]
            .as_str()
            .unwrap(),
        &wrong_command,
        wrong_authority["authority"]["intentId"].as_str().unwrap(),
        wrong_authority["intentSpec"].as_str().unwrap(),
        wrong_authority["note"].as_str(),
        &hmac_key_bytes,
    );
    wrong_authority["authority"]["command"] = serde_json::to_value(wrong_command).unwrap();
    wrong_authority["authority"]["sidecarDigest"] = serde_json::Value::String(wrong_digest);
    libra::utils::atomic_write::write_atomic(
        &sidecar_path,
        &serde_json::to_vec_pretty(&wrong_authority).unwrap(),
        true,
    )
    .expect("persist wrong-authority sidecar fixture");
    let error = intent_revision_restart_error(&store, &session_id, workdir.path()).await;
    assert!(
        error.contains("belongs to another durable session"),
        "a sidecar authority copied from another repo/session must fence: {error}"
    );

    libra::utils::atomic_write::write_atomic(&sidecar_path, &sidecar_bytes, true)
        .expect("restore the valid Active sidecar");
    let state = store.load(&session_id).unwrap();
    let persistence = HeadlessSessionPersistence::new(store.clone(), state).unwrap();
    let restored =
        try_build_runtime_with_persistence("basic_chat", workdir.path().to_path_buf(), persistence)
            .await
            .expect("the repaired regular sidecar and original key must restart cleanly");
    assert_eq!(restored.snapshot().await.status, CodeUiSessionStatus::Idle);
    assert_raw_note_absent_from_workflow_and_sse(&goal_store, &note);
}

#[tokio::test(flavor = "multi_thread")]
async fn duplicate_source_terminal_shapes_fail_closed_before_revision_restore() {
    let plain_and_bound =
        real_bound_intent_revision_fixture("plain plus bound source conflict").await;
    record_bound_revision_receipt(&plain_and_bound, "plain-bound-consumer");
    std::fs::remove_file(pending_intent_revision_path(
        &plain_and_bound.fixture.store,
        &plain_and_bound.fixture.session_id,
    ))
    .expect("model normal sidecar removal after the exact bound receipt");
    plain_and_bound
        .fixture
        .goal_store
        .append_code_workflow_durable(CodeWorkflowEventKind::CommandTerminalSuccess {
            command: plain_and_bound.source_command.clone(),
            summary: plain_and_bound.summary.clone(),
        })
        .expect("append conflicting plain terminal with the same source identity");
    assert_malformed_bound_revision_fails_closed(plain_and_bound, "plain plus bound terminal")
        .await;

    let failure_and_bound =
        real_bound_intent_revision_fixture("failure plus bound source conflict").await;
    failure_and_bound
        .fixture
        .goal_store
        .append_code_workflow_durable(CodeWorkflowEventKind::CommandTerminalFailure {
            command: failure_and_bound.source_command.clone(),
            reason: "conflicting source failure".to_string(),
            interaction_resolutions: Vec::new(),
            retry_intent_review: None,
        })
        .expect("append conflicting Failure terminal with the same source identity");
    assert_malformed_bound_revision_fails_closed(failure_and_bound, "failure plus bound terminal")
        .await;

    let indeterminate_and_bound =
        real_bound_intent_revision_fixture("indeterminate plus bound source conflict").await;
    indeterminate_and_bound
        .fixture
        .goal_store
        .append_code_workflow_durable(CodeWorkflowEventKind::CommandIndeterminateSideEffect {
            command: indeterminate_and_bound.source_command.clone(),
            effect: "conflicting_source_terminal".to_string(),
            reason: "conflicting source terminal must fail closed".to_string(),
        })
        .expect("append conflicting Indeterminate terminal with the same source identity");
    assert_malformed_bound_revision_fails_closed(
        indeterminate_and_bound,
        "indeterminate plus bound terminal",
    )
    .await;

    let legacy_and_bound =
        real_bound_intent_revision_fixture("legacy plus bound source conflict").await;
    record_bound_revision_receipt(&legacy_and_bound, "legacy-bound-consumer");
    std::fs::remove_file(pending_intent_revision_path(
        &legacy_and_bound.fixture.store,
        &legacy_and_bound.fixture.session_id,
    ))
    .expect("model normal sidecar removal after the exact legacy/bound receipt");
    legacy_and_bound
        .fixture
        .goal_store
        .append_code_workflow_durable(CodeWorkflowEventKind::IntentReviewRequested {
            interaction_id: "legacy-source-review".to_string(),
            intent_id: legacy_and_bound.intent_id.clone(),
            turn_id: "legacy-source-gate".to_string(),
            phase0_turn_id: legacy_and_bound.source_command.command_id.clone(),
        })
        .unwrap();
    legacy_and_bound
        .fixture
        .goal_store
        .append_code_workflow_durable(
            CodeWorkflowEventKind::CommandTerminalSuccessWithInteractionResolved {
                command: legacy_and_bound.source_command.clone(),
                summary: legacy_and_bound.summary.clone(),
                interaction_id: "legacy-source-review".to_string(),
                resolution: "modify".to_string(),
                prior_interaction_resolutions: Vec::new(),
                intent_revision: None,
            },
        )
        .expect("append conflicting legacy Modify terminal with the same source identity");
    assert_malformed_bound_revision_fails_closed(legacy_and_bound, "legacy plus bound terminal")
        .await;

    let bound_twice = real_bound_intent_revision_fixture("first bound source revision").await;
    record_bound_revision_receipt(&bound_twice, "bound-twice-consumer");
    let interaction_b = "second-bound-source-review";
    let note_b = "second private note under the duplicate source command";
    bound_twice
        .fixture
        .goal_store
        .append_code_workflow_durable(CodeWorkflowEventKind::IntentReviewRequested {
            interaction_id: interaction_b.to_string(),
            intent_id: bound_twice.intent_id.clone(),
            turn_id: "second-bound-source-gate".to_string(),
            phase0_turn_id: bound_twice.source_command.command_id.clone(),
        })
        .unwrap();
    let intent_spec = bound_twice.active_sidecar["intentSpec"]
        .as_str()
        .expect("real Active sidecar carries its IntentSpec");
    let digest_b = revision_sidecar_digest_for_test(
        u32::try_from(
            bound_twice.active_sidecar["authority"]["schemaVersion"]
                .as_u64()
                .unwrap(),
        )
        .unwrap(),
        interaction_b,
        &bound_twice.source_command,
        &bound_twice.intent_id,
        intent_spec,
        Some(note_b),
        &bound_twice.hmac_key,
    );
    let recovery_b = IntentRevisionRecovery {
        interaction_id: interaction_b.to_string(),
        sidecar_digest: digest_b.clone(),
    };
    let terminal_b = bound_twice
        .fixture
        .goal_store
        .append_code_workflow_durable(
            CodeWorkflowEventKind::CommandTerminalSuccessWithInteractionResolved {
                command: bound_twice.source_command.clone(),
                summary: bound_twice.summary.clone(),
                interaction_id: interaction_b.to_string(),
                resolution: "modify".to_string(),
                prior_interaction_resolutions: Vec::new(),
                intent_revision: Some(recovery_b),
            },
        )
        .expect("append second valid bound terminal under the same source command");
    let active_b = serde_json::json!({
        "intentSpec": intent_spec,
        "note": note_b,
        "authority": {
            "schemaVersion": INTENT_REVISION_CONSUMPTION_SCHEMA_VERSION,
            "legacyTerminal": false,
            "interactionId": interaction_b,
            "command": bound_twice.source_command.clone(),
            "terminalEventId": terminal_b.event_id,
            "terminalSequence": terminal_b.sequence,
            "intentId": bound_twice.intent_id.clone(),
            "sidecarDigest": digest_b,
        }
    });
    libra::utils::atomic_write::write_atomic(
        &pending_intent_revision_path(&bound_twice.fixture.store, &bound_twice.fixture.session_id),
        &serde_json::to_vec_pretty(&active_b).unwrap(),
        true,
    )
    .expect("persist the valid second Active sidecar");
    assert_malformed_bound_revision_fails_closed(
        bound_twice,
        "two bound interactions under one source command",
    )
    .await;
}

fn append_raw_headless_workflow_event(store: &SessionJsonlStore, event: CodeWorkflowEvent) {
    let mut file = std::fs::OpenOptions::new()
        .append(true)
        .open(store.events_path())
        .expect("open headless workflow log for strict-replay corruption fixture");
    serde_json::to_writer(&mut file, &SessionEvent::code_workflow(event))
        .expect("serialize strict-replay corruption fixture");
    writeln!(file).expect("terminate strict-replay corruption fixture row");
    file.sync_all()
        .expect("durably persist strict-replay corruption fixture");
}

#[tokio::test(flavor = "multi_thread")]
async fn strict_revision_replay_corruption_fails_before_any_startup_mutation() {
    for corruption in ["sequence-gap", "conflicting-event-id"] {
        let bound = real_bound_intent_revision_fixture(&format!(
            "private sidecar survives {corruption} startup rejection"
        ))
        .await;
        let intent_spec_text = bound.active_sidecar["intentSpec"]
            .as_str()
            .expect("real Active sidecar carries IntentSpec JSON");
        let intent_spec: libra::internal::ai::intentspec::IntentSpec =
            serde_json::from_str(intent_spec_text).expect("decode real sidecar IntentSpec");
        let checkout = Phase1CheckoutBinding::capture(bound.fixture.workdir.path(), &intent_spec)
            .await
            .expect("capture valid strict-replay Phase 1 checkout binding");
        let seed = Phase1StartSeed {
            schema_version: Phase1StartSeed::SCHEMA_VERSION,
            source_interaction_id: format!("strict-replay-source-{corruption}"),
            intent_id: bound.intent_id.clone(),
            intent_spec_id: intent_spec.metadata.id.clone(),
            intent_spec_json: intent_spec_text.to_string(),
            source_resolution: "confirm".to_string(),
            revision_note: None,
            checkout: checkout.clone(),
            prior_plan: None,
            prior_plan_id: None,
            prior_persisted_plan: Phase1PersistedPlan::Unavailable,
            browser_command_id: Some(format!("strict-replay-command-{corruption}")),
            attempt_id: format!("strict-replay-attempt-{corruption}"),
        };
        persist_phase1_start_seed(&bound.fixture.goal_store, &seed)
            .expect("persist a valid Phase 1 seed that startup must not mutate");
        let context_interaction_id = format!("strict-replay-context-{corruption}");
        persist_phase1_review_context(
            &bound.fixture.goal_store,
            &Phase1ReviewContext {
                schema_version: Phase1ReviewContext::SCHEMA_VERSION,
                interaction_id: context_interaction_id.clone(),
                intent_id: bound.intent_id.clone(),
                intent_spec_id: intent_spec.metadata.id.clone(),
                persisted_plan: Phase1PersistedPlan::Persisted {
                    execution_plan_id: format!("strict-replay-plan-{corruption}"),
                    test_plan_id: format!("strict-replay-tests-{corruption}"),
                },
                intent_spec: intent_spec.clone(),
                plan_draft: SubmitPlanDraftArgs {
                    explanation: Some("startup must not GC this context".to_string()),
                    steps: vec![PlanDraftStep {
                        title: "Preserve artifacts before strict replay rejection".to_string(),
                    }],
                },
                execution_plan: ExecutionPlanSpec {
                    intent_spec_id: intent_spec.metadata.id.clone(),
                    revision: 1,
                    parent_revision: None,
                    replan_reason: None,
                    tasks: Vec::new(),
                    max_parallel: 1,
                    checkpoints: Vec::new(),
                },
                default_allow_network: false,
                checkout,
            },
        )
        .expect("persist a valid Phase 1 context that startup must not GC");

        let seed_path = bound
            .fixture
            .goal_store
            .session_root()
            .join("phase1/pending-start.json");
        let context_path =
            phase1_review_context_path(&bound.fixture.goal_store, &context_interaction_id);
        let sidecar_path =
            pending_intent_revision_path(&bound.fixture.store, &bound.fixture.session_id);
        let events_path = bound.fixture.goal_store.events_path();

        let RealBoundIntentRevisionFixture {
            fixture,
            active_sidecar: _,
            terminal: _,
            source_command: _,
            summary: _,
            recovery: _,
            intent_id: _,
            hmac_key: _,
        } = bound;
        let OversizedPhase1ConfirmFixture {
            workdir,
            _storage,
            store,
            session_id,
            goal_store,
            persistence: _,
            runtime,
            source_interaction_id: _,
            risk_interaction_id: _,
            risk_pending_state: _,
        } = fixture;
        drop(runtime);
        tokio::time::sleep(Duration::from_millis(50)).await;
        let replay = goal_store.load_code_workflow_replay().unwrap();
        let tip = replay
            .events
            .last()
            .expect("strict-replay fixture has workflow");
        let malformed = if corruption == "sequence-gap" {
            CodeWorkflowEvent::new(
                tip.sequence + 2,
                CodeWorkflowEventKind::CodeUiProjectionDelta {
                    projection: "strict_replay_gap".to_string(),
                    summary: "constructor must reject this workflow gap".to_string(),
                    payload: serde_json::Value::Null,
                },
            )
        } else {
            let mut conflicting = (*tip).clone();
            conflicting.sequence += 1;
            conflicting.event = CodeWorkflowEventKind::CodeUiProjectionDelta {
                projection: "strict_replay_conflicting_uuid".to_string(),
                summary: "same UUID with another durable payload".to_string(),
                payload: serde_json::Value::Null,
            };
            conflicting
        };
        append_raw_headless_workflow_event(&goal_store, malformed);
        let seed_bytes = std::fs::read(&seed_path).expect("capture seed bytes before startup");
        let context_bytes =
            std::fs::read(&context_path).expect("capture context bytes before startup");
        let sidecar_bytes =
            std::fs::read(&sidecar_path).expect("capture sidecar bytes before startup");
        let events_bytes =
            std::fs::read(&events_path).expect("capture corrupt event bytes before startup");
        let state = store
            .load(&session_id)
            .expect("generic session replay must retain its last valid snapshot");
        let persistence = HeadlessSessionPersistence::new(store, state)
            .expect("reacquire strict-replay fixture lease");
        let provider_entered = Arc::new(Notify::new());
        let provider_entered_wait = provider_entered.notified();
        tokio::pin!(provider_entered_wait);
        provider_entered_wait.as_mut().enable();
        let error = match try_build_pending_completion_runtime_with_capture(
            workdir.path().to_path_buf(),
            persistence,
            Arc::clone(&provider_entered),
            None,
        )
        .await
        {
            Ok(runtime) => {
                drop(runtime);
                panic!("{corruption} must fail constructor preflight")
            }
            Err(error) => format!("{error:#}"),
        };
        if corruption == "sequence-gap" {
            assert!(
                error.contains("complete Code workflow replay"),
                "sequence gap must fail the strict replay preflight: {error}"
            );
        } else {
            assert!(
                error.contains("conflicting durable payloads"),
                "conflicting UUID must fail the strict replay preflight: {error}"
            );
        }
        assert!(
            tokio::time::timeout(Duration::from_millis(100), provider_entered_wait.as_mut())
                .await
                .is_err(),
            "{corruption} must fail before starting any provider"
        );
        for (path, before, label) in [
            (&seed_path, &seed_bytes, "Phase 1 seed"),
            (&context_path, &context_bytes, "Phase 1 context"),
            (&sidecar_path, &sidecar_bytes, "revision sidecar"),
            (&events_path, &events_bytes, "events/projection log"),
        ] {
            assert_eq!(
                std::fs::read(path).unwrap().as_slice(),
                before.as_slice(),
                "{corruption} must fail before mutating {label}"
            );
        }
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn stale_modify_retry_does_not_fence_or_replace_new_active_revision() {
    let workdir = tempfile::tempdir().expect("tempdir for stale Modify retry checkout");
    let canonical_workdir = initialize_phase1_test_checkout(workdir.path()).await;
    let storage = tempfile::tempdir().expect("tempdir for stale Modify retry storage");
    let store = Arc::new(SessionStore::from_storage_path(storage.path()));
    let mut state = SessionState::new(&canonical_workdir.to_string_lossy());
    let session_id = state.id.clone();
    state.metadata.insert(
        "thread_id".to_string(),
        serde_json::json!(session_id.clone()),
    );
    let persistence = HeadlessSessionPersistence::new(store.clone(), state).unwrap();
    let goal_store = persistence.goal_event_store();
    let runtime = build_runtime_with_persistence(
        "phase0_intent_review",
        canonical_workdir,
        Vec::new(),
        Some(persistence),
    )
    .await
    .0;

    runtime
        .submit_message("draft an IntentSpec for a stale retry test".to_string())
        .await
        .unwrap();
    let risk_a = await_pending_interaction(
        &runtime,
        CodeUiInteractionKind::RequestUserInput,
        "initial revision source must ask for risk",
    )
    .await;
    runtime
        .respond_interaction(
            &risk_a,
            CodeUiInteractionResponse {
                selected_option: Some("Low".to_string()),
                ..Default::default()
            },
        )
        .await
        .unwrap();
    let source_a = await_pending_interaction(
        &runtime,
        CodeUiInteractionKind::IntentReviewChoice,
        "initial revision source must publish Intent review A",
    )
    .await;
    let note_a = "private revision note A";
    runtime
        .respond_interaction(
            &source_a,
            CodeUiInteractionResponse {
                selected_option: Some("modify".to_string()),
                note: Some(note_a.to_string()),
                ..Default::default()
            },
        )
        .await
        .unwrap();

    runtime
        .submit_message("produce the replacement IntentSpec generation".to_string())
        .await
        .expect("consume revision A under one durable ordinary command");
    let risk_b = await_pending_interaction(
        &runtime,
        CodeUiInteractionKind::RequestUserInput,
        "revision consumer must ask for risk before Intent review B",
    )
    .await;
    runtime
        .respond_interaction(
            &risk_b,
            CodeUiInteractionResponse {
                selected_option: Some("Low".to_string()),
                ..Default::default()
            },
        )
        .await
        .unwrap();
    let source_b = await_pending_interaction(
        &runtime,
        CodeUiInteractionKind::IntentReviewChoice,
        "revision consumer must publish Intent review B",
    )
    .await;
    assert_ne!(source_b, source_a);
    let note_b = "private revision note B remains Active";
    runtime
        .respond_interaction(
            &source_b,
            CodeUiInteractionResponse {
                selected_option: Some("modify".to_string()),
                note: Some(note_b.to_string()),
                ..Default::default()
            },
        )
        .await
        .unwrap();
    let sidecar_path = pending_intent_revision_path(&store, &session_id);
    let active_b = std::fs::read(&sidecar_path).expect("revision B must be durably Active");

    runtime
        .respond_interaction(
            &source_a,
            CodeUiInteractionResponse {
                selected_option: Some("modify".to_string()),
                note: Some(note_a.to_string()),
                ..Default::default()
            },
        )
        .await
        .expect("lost ACK retry A with the same private note must be a pure ACK");
    assert_eq!(std::fs::read(&sidecar_path).unwrap(), active_b);
    assert_eq!(runtime.snapshot().await.status, CodeUiSessionStatus::Idle);

    let conflict = runtime
        .respond_interaction(
            &source_a,
            CodeUiInteractionResponse {
                selected_option: Some("modify".to_string()),
                note: Some("different stale note".to_string()),
                ..Default::default()
            },
        )
        .await
        .expect_err("lost ACK retry A with a different note must conflict");
    assert_code_ui_api_error(&conflict, 409, "INTERACTION_NOT_ACTIVE");
    assert_eq!(std::fs::read(&sidecar_path).unwrap(), active_b);
    assert_ne!(
        runtime.snapshot().await.status,
        CodeUiSessionStatus::IndeterminateSideEffect
    );

    let oversized = runtime
        .respond_interaction(
            &source_a,
            CodeUiInteractionResponse {
                selected_option: Some("modify".to_string()),
                note: Some("x".repeat(MAX_INTENT_REVISION_NOTE_BYTES + 1)),
                ..Default::default()
            },
        )
        .await
        .expect_err("oversized stale Modify retry must be rejected before digest comparison");
    let oversized = assert_code_ui_api_error(&oversized, 400, "INVALID_QUERY_PARAM");
    assert_eq!(
        oversized.message,
        format!(
            "IntentSpec Modify note exceeds the {MAX_INTENT_REVISION_NOTE_BYTES}-byte UTF-8 limit"
        ),
        "resolved lost-ACK retries must expose the same bounded public error as active gates"
    );
    assert_eq!(std::fs::read(&sidecar_path).unwrap(), active_b);
    assert_raw_note_absent_from_workflow_and_sse(&goal_store, note_a);
    assert_raw_note_absent_from_workflow_and_sse(&goal_store, note_b);
    let replay = goal_store.load_code_workflow_replay().unwrap();
    assert_eq!(
        replay
            .events
            .iter()
            .filter(|event| matches!(
                &event.event,
                CodeWorkflowEventKind::InteractionResolved {
                    intent_revision_consumption: Some(consumption),
                    ..
                } if consumption.claim.interaction_id == source_a
            ))
            .count(),
        1,
        "stale retries must not duplicate A's receipt"
    );
    assert!(replay.events.iter().all(|event| !matches!(
        &event.event,
        CodeWorkflowEventKind::InteractionResolved {
            intent_revision_consumption: Some(consumption),
            ..
        } if consumption.claim.interaction_id == source_b
    )));
    assert!(replay.events.iter().all(|event| !matches!(
        &event.event,
        CodeWorkflowEventKind::CommandIndeterminateSideEffect { .. }
    )));
}

#[tokio::test(flavor = "multi_thread")]
async fn oversized_intent_modify_note_is_typed_400_without_consuming_gate() {
    let fixture = oversized_phase1_confirm_fixture(None).await;
    let source_interaction_id = fixture.source_interaction_id.clone();
    let before = fixture.goal_store.load_code_workflow_replay().unwrap();
    let error = fixture
        .runtime
        .respond_interaction(
            &source_interaction_id,
            CodeUiInteractionResponse {
                selected_option: Some("modify".to_string()),
                note: Some("x".repeat(MAX_INTENT_REVISION_NOTE_BYTES + 1)),
                ..Default::default()
            },
        )
        .await
        .expect_err("an oversized Modify note must be rejected before delivery");
    let error = assert_code_ui_api_error(&error, 400, "INVALID_QUERY_PARAM");
    assert_eq!(
        error.message,
        format!(
            "IntentSpec Modify note exceeds the {MAX_INTENT_REVISION_NOTE_BYTES}-byte UTF-8 limit"
        )
    );
    let snapshot = fixture.runtime.snapshot().await;
    assert_eq!(snapshot.status, CodeUiSessionStatus::AwaitingInteraction);
    assert!(snapshot.interactions.iter().any(|interaction| {
        interaction.id == source_interaction_id
            && interaction.status == CodeUiInteractionStatus::Pending
    }));
    assert!(!pending_intent_revision_path(&fixture.store, &fixture.session_id).exists());

    let after = fixture.goal_store.load_code_workflow_replay().unwrap();
    assert_eq!(after.events.len(), before.events.len());
    assert!(
        open_intent_review_from_workflow(after.events.iter().map(|event| &event.event))
            .is_some_and(|(interaction_id, ..)| interaction_id == source_interaction_id)
    );
    assert!(after.events.iter().all(|event| !matches!(
        &event.event,
        CodeWorkflowEventKind::CommandTerminalSuccessWithInteractionResolved {
            interaction_id,
            ..
        } if interaction_id == &source_interaction_id
    )));
}

enum StaleIntentCloseout {
    Respond(&'static str),
    ControlCancel,
}

async fn assert_restart_reconciles_stale_intent_projection(action: StaleIntentCloseout) {
    let fixture = oversized_phase1_confirm_fixture(None).await;
    let stale_pending_state = fixture
        .store
        .load(&fixture.session_id)
        .expect("capture the durable snapshot while the Intent gate is pending");
    let expected_resolution = match action {
        StaleIntentCloseout::Respond(decision) => {
            fixture
                .runtime
                .respond_interaction(
                    &fixture.source_interaction_id,
                    CodeUiInteractionResponse {
                        selected_option: Some(decision.to_string()),
                        note: (decision == "modify")
                            .then(|| "retain only the durable revision mode".to_string()),
                        ..Default::default()
                    },
                )
                .await
                .unwrap_or_else(|error| {
                    panic!(
                        "Intent {decision} must commit before stale projection replay: {error:#}"
                    )
                });
            decision
        }
        StaleIntentCloseout::ControlCancel => {
            fixture
                .runtime
                .cancel_turn()
                .await
                .expect("control Cancel must commit before stale projection replay");
            "cancel"
        }
    };
    let replay = fixture.goal_store.load_code_workflow_replay().unwrap();
    assert_eq!(
        replay
            .events
            .iter()
            .filter(|event| matches!(
                &event.event,
                CodeWorkflowEventKind::CommandTerminalSuccessWithInteractionResolved {
                    interaction_id,
                    resolution,
                    ..
                } if interaction_id == &fixture.source_interaction_id
                    && resolution == expected_resolution
            ))
            .count(),
        1,
        "the workflow must be terminal before the stale Web snapshot is injected"
    );

    // Model a process dying after the combined workflow fsync but before the
    // Web projection closeout reaches its own durable snapshot: retain the
    // workflow log and deliberately restore the last pending projection.
    fixture
        .store
        .save(&stale_pending_state)
        .expect("restore the stale pending Web projection fixture");
    drop(fixture.runtime);
    tokio::time::sleep(Duration::from_millis(50)).await;

    let restored_state = fixture.store.load(&fixture.session_id).unwrap();
    let persistence = HeadlessSessionPersistence::new(fixture.store.clone(), restored_state)
        .expect("reacquire stale-projection session writer lease");
    let restored = build_runtime_with_persistence(
        "basic_chat",
        fixture.workdir.path().to_path_buf(),
        Vec::new(),
        Some(persistence),
    )
    .await
    .0;
    let snapshot = restored.snapshot().await;
    assert!(snapshot.interactions.iter().all(|interaction| {
        interaction.id != fixture.source_interaction_id
            || interaction.status != CodeUiInteractionStatus::Pending
    }));
    assert_ne!(
        snapshot.status,
        CodeUiSessionStatus::IndeterminateSideEffect
    );
    if expected_resolution == "modify" {
        assert!(
            fixture
                .store
                .session_root(&fixture.session_id)
                .join("intents/pending_revision.json")
                .is_file(),
            "Modify restart must retain only its durable revision mode"
        );
        assert!(snapshot.transcript.iter().any(|entry| {
            entry.content.as_deref().is_some_and(|content| {
                content.contains("retained privately for the next Phase 0 revision prompt")
            })
        }));
        assert!(snapshot.transcript.iter().all(|entry| {
            entry
                .content
                .as_deref()
                .is_none_or(|content| !content.contains("retain only the durable revision mode"))
        }));
        assert_raw_note_absent_from_workflow_and_sse(
            &fixture.goal_store,
            "retain only the durable revision mode",
        );
    } else {
        assert_eq!(snapshot.status, CodeUiSessionStatus::Idle);
        restored
            .submit_message("/fresh turn after stale Intent closeout".to_string())
            .await
            .expect("Cancel restart must release the stale Intent slot for new work");
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn restart_reconciles_stale_intent_projection_after_atomic_modify() {
    assert_restart_reconciles_stale_intent_projection(StaleIntentCloseout::Respond("modify")).await;
}

#[tokio::test(flavor = "multi_thread")]
async fn restart_reconciles_stale_intent_projection_after_atomic_cancel_response() {
    assert_restart_reconciles_stale_intent_projection(StaleIntentCloseout::Respond("cancel")).await;
}

#[tokio::test(flavor = "multi_thread")]
async fn restart_reconciles_stale_intent_projection_after_atomic_control_cancel() {
    assert_restart_reconciles_stale_intent_projection(StaleIntentCloseout::ControlCancel).await;
}

async fn assert_projection_fold_expands_combined_primary_and_prior(control_cancel: bool) {
    let fixture = oversized_phase1_confirm_fixture(None).await;
    let risk_interaction_id = fixture
        .risk_interaction_id
        .clone()
        .expect("the normal Phase 0 fixture must use a real risk interaction");
    let stale_risk_pending_state = fixture
        .risk_pending_state
        .clone()
        .expect("capture the projection before the risk response");
    let expected_replacement = if control_cancel {
        fixture
            .runtime
            .cancel_turn()
            .await
            .expect("control Cancel must atomically settle the Intent gate");
        None
    } else {
        fixture
            .runtime
            .respond_interaction(
                &fixture.source_interaction_id,
                CodeUiInteractionResponse {
                    selected_option: Some("confirm".to_string()),
                    ..Default::default()
                },
            )
            .await
            .expect("Intent Confirm must atomically settle the source gate");
        let deadline = std::time::Instant::now() + Duration::from_secs(8);
        Some(loop {
            if let Some(interaction) =
                fixture
                    .runtime
                    .snapshot()
                    .await
                    .interactions
                    .iter()
                    .find(|interaction| {
                        interaction.kind == CodeUiInteractionKind::IntentReviewChoice
                            && interaction.status == CodeUiInteractionStatus::Pending
                            && interaction.id != fixture.source_interaction_id
                    })
            {
                break interaction.id.clone();
            }
            assert!(
                std::time::Instant::now() < deadline,
                "the oversized Confirm did not publish its fresh retry authority"
            );
            tokio::time::sleep(Duration::from_millis(20)).await;
        })
    };
    let replay = fixture.goal_store.load_code_workflow_replay().unwrap();
    let combined = replay
        .events
        .iter()
        .find_map(|event| match &event.event {
            CodeWorkflowEventKind::CommandTerminalSuccessWithInteractionResolved {
                interaction_id,
                resolution,
                prior_interaction_resolutions,
                ..
            } if interaction_id == &fixture.source_interaction_id => {
                Some((resolution.clone(), prior_interaction_resolutions.clone()))
            }
            _ => None,
        })
        .expect("Intent terminal must use one combined workflow row");
    assert_eq!(
        combined.0,
        if control_cancel { "cancel" } else { "confirm" }
    );
    assert_eq!(
        combined.1,
        vec![(risk_interaction_id.clone(), "answered".to_string())]
    );

    fixture
        .store
        .save(&stale_risk_pending_state)
        .expect("append the stale pre-risk projection after the combined terminal");
    drop(fixture.runtime);
    tokio::time::sleep(Duration::from_millis(50)).await;
    let restored_state = fixture.store.load(&fixture.session_id).unwrap();
    let persistence = HeadlessSessionPersistence::new(fixture.store.clone(), restored_state)
        .expect("reacquire combined-projection test session lease");
    let restored = build_runtime_with_persistence(
        "basic_chat",
        fixture.workdir.path().to_path_buf(),
        Vec::new(),
        Some(persistence),
    )
    .await
    .0;
    let snapshot = restored.snapshot().await;
    assert!(snapshot.interactions.iter().all(|interaction| {
        (interaction.id != risk_interaction_id && interaction.id != fixture.source_interaction_id)
            || interaction.status != CodeUiInteractionStatus::Pending
    }));
    match expected_replacement {
        Some(expected) => {
            let restored_id = await_pending_interaction(
                &restored,
                CodeUiInteractionKind::IntentReviewChoice,
                "projection fold lost the fresh retry authority",
            )
            .await;
            assert_eq!(restored_id, expected);
        }
        None => assert_eq!(snapshot.status, CodeUiSessionStatus::Idle),
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn projection_fold_expands_risk_prior_and_confirm_primary() {
    assert_projection_fold_expands_combined_primary_and_prior(false).await;
}

#[tokio::test(flavor = "multi_thread")]
async fn projection_fold_expands_risk_prior_and_control_cancel_primary() {
    assert_projection_fold_expands_combined_primary_and_prior(true).await;
}

async fn oversized_phase1_plan_gate_fixture() -> (OversizedPhase1RetryFixture, String) {
    let fixture = oversized_phase1_retry_fixture().await;
    fixture
        .runtime
        .respond_interaction(
            &fixture.retry_interaction_id,
            CodeUiInteractionResponse {
                selected_option: Some("confirm".to_string()),
                ..Default::default()
            },
        )
        .await
        .expect("bounded retry Confirm must start Phase 1");
    let plan_interaction_id = await_pending_interaction(
        &fixture.runtime,
        CodeUiInteractionKind::PostPlanChoice,
        "bounded retry did not park its Plan gate",
    )
    .await;
    (fixture, plan_interaction_id)
}

struct LegacyPhase1GateFixture {
    workdir: tempfile::TempDir,
    _storage: tempfile::TempDir,
    store: Arc<SessionStore>,
    session_id: String,
    goal_store: SessionJsonlStore,
    persistence: HeadlessSessionPersistence,
    interaction_id: String,
}

async fn legacy_empty_turn_phase1_gate_fixture(network: bool) -> LegacyPhase1GateFixture {
    let workdir = tempfile::tempdir().expect("tempdir for legacy Phase 1 gate checkout");
    let canonical_workdir = initialize_phase1_test_checkout(workdir.path()).await;
    let intent_spec = resolve_intentspec(
        IntentDraft {
            intent: DraftIntent {
                summary: "Restore a legacy Phase 1 gate".to_string(),
                problem_statement: "Legacy gate markers did not persist runtime turn ids"
                    .to_string(),
                change_type: ChangeType::Test,
                objectives: vec![Objective {
                    title: "Bind one stable runtime command on restore".to_string(),
                    kind: ObjectiveKind::Implementation,
                }],
                in_scope: vec!["README.md".to_string()],
                out_of_scope: vec![],
                touch_hints: None,
            },
            acceptance: DraftAcceptance {
                success_criteria: vec!["Two restarts retain one gate".to_string()],
                fast_checks: vec![],
                integration_checks: vec![],
                security_checks: vec![],
                release_checks: vec![],
            },
            risk: DraftRisk {
                rationale: "legacy recovery regression".to_string(),
                factors: vec![],
                level: Some(RiskLevel::Low),
            },
        },
        RiskLevel::Low,
        ResolveContext {
            working_dir: canonical_workdir.to_string_lossy().into_owned(),
            base_ref: "HEAD".to_string(),
            created_by_id: "ai-code-ui-headless-test".to_string(),
        },
    );
    let checkout = Phase1CheckoutBinding::capture(&canonical_workdir, &intent_spec)
        .await
        .expect("capture legacy-gate checkout identity");
    let plan_interaction_id = "legacy-empty-turn-plan-review".to_string();
    let network_interaction_id = "legacy-empty-turn-network-review".to_string();
    let plan_id = "legacy-empty-turn-execution-plan".to_string();
    let intent_spec_id = intent_spec.metadata.id.clone();

    let storage = tempfile::tempdir().expect("tempdir for legacy Phase 1 gate session");
    let store = Arc::new(SessionStore::from_storage_path(storage.path()));
    let mut state = SessionState::new(&canonical_workdir.to_string_lossy());
    let session_id = state.id.clone();
    state.metadata.insert(
        "thread_id".to_string(),
        serde_json::json!(session_id.clone()),
    );
    store
        .save(&state)
        .expect("persist replayable legacy-gate session snapshot");
    let goal_store = SessionJsonlStore::new(store.session_root(&session_id));
    persist_phase1_review_context(
        &goal_store,
        &Phase1ReviewContext {
            schema_version: Phase1ReviewContext::SCHEMA_VERSION,
            interaction_id: plan_interaction_id.clone(),
            intent_id: "legacy-empty-turn-intent".to_string(),
            intent_spec_id: intent_spec_id.clone(),
            persisted_plan: Phase1PersistedPlan::Persisted {
                execution_plan_id: plan_id.clone(),
                test_plan_id: "legacy-empty-turn-test-plan".to_string(),
            },
            intent_spec,
            plan_draft: SubmitPlanDraftArgs {
                explanation: Some("Legacy recovery plan".to_string()),
                steps: vec![PlanDraftStep {
                    title: "Restore the parked review gate".to_string(),
                }],
            },
            execution_plan: ExecutionPlanSpec {
                intent_spec_id,
                revision: 1,
                parent_revision: None,
                replan_reason: None,
                tasks: vec![],
                max_parallel: 1,
                checkpoints: vec![],
            },
            default_allow_network: false,
            checkout,
        },
    )
    .expect("persist legacy-gate Phase 1 review context");
    goal_store
        .append_code_workflow_durable(CodeWorkflowEventKind::PlanReviewRequested {
            interaction_id: plan_interaction_id.clone(),
            plan_id: plan_id.clone(),
            turn_id: if network {
                "legacy-resolved-plan-turn".to_string()
            } else {
                String::new()
            },
            phase1_turn_id: "legacy-phase1-generation".to_string(),
            context_id: plan_interaction_id.clone(),
            revision_of: None,
            prepared_from_network: None,
        })
        .expect("persist legacy Plan review marker");
    let interaction_id = if network {
        goal_store
            .append_code_workflow_durable(CodeWorkflowEventKind::NetworkPolicyRequested {
                interaction_id: network_interaction_id.clone(),
                plan_id,
                turn_id: String::new(),
                default_allow: false,
            })
            .expect("persist legacy Network marker without a runtime turn");
        goal_store
            .append_code_workflow_durable(CodeWorkflowEventKind::InteractionResolved {
                interaction_id: plan_interaction_id,
                resolution: "execute".to_string(),
                command: None,
                prior_interaction_resolutions: Vec::new(),
                intent_revision_consumption: None,
            })
            .expect("promote the legacy Network marker");
        network_interaction_id
    } else {
        plan_interaction_id
    };
    let persistence = HeadlessSessionPersistence::new(store.clone(), state)
        .expect("attach legacy Phase 1 gate session");

    LegacyPhase1GateFixture {
        workdir,
        _storage: storage,
        store,
        session_id,
        goal_store,
        persistence,
        interaction_id,
    }
}

async fn assert_legacy_empty_turn_gate_reuses_binding_after_two_restarts(network: bool) {
    let fixture = legacy_empty_turn_phase1_gate_fixture(network).await;
    let kind = CodeUiInteractionKind::PostPlanChoice;
    let phase = network.then_some("networkPolicy");
    let restored = build_runtime_with_persistence(
        "basic_chat",
        fixture.workdir.path().to_path_buf(),
        Vec::new(),
        Some(fixture.persistence),
    )
    .await
    .0;
    assert_eq!(
        await_pending_interaction(
            &restored,
            kind.clone(),
            "legacy empty-turn gate was not restored",
        )
        .await,
        fixture.interaction_id
    );
    if let Some(phase) = phase {
        assert_eq!(
            restored
                .snapshot()
                .await
                .interactions
                .iter()
                .find(|interaction| interaction.id == fixture.interaction_id)
                .and_then(|interaction| interaction.metadata.get("phase"))
                .and_then(serde_json::Value::as_str),
            Some(phase)
        );
    }
    let first_turn_id = restored
        .runtime_snapshot()
        .await
        .expect("read first legacy-gate runtime")
        .active_turn_id
        .expect("first restore must create one bound gate command");
    let bound_turn_ids = |replay: &libra::internal::ai::session::CodeWorkflowReplay| {
        replay
            .events
            .iter()
            .filter_map(|event| match (&event.event, network) {
                (
                    CodeWorkflowEventKind::NetworkPolicyRequested {
                        interaction_id,
                        turn_id,
                        ..
                    },
                    true,
                ) if interaction_id == &fixture.interaction_id && !turn_id.is_empty() => {
                    Some(turn_id.clone())
                }
                (
                    CodeWorkflowEventKind::PlanReviewRequested {
                        interaction_id,
                        turn_id,
                        ..
                    },
                    false,
                ) if interaction_id == &fixture.interaction_id && !turn_id.is_empty() => {
                    Some(turn_id.clone())
                }
                _ => None,
            })
            .collect::<Vec<_>>()
    };
    let replay = fixture.goal_store.load_code_workflow_replay().unwrap();
    assert_eq!(bound_turn_ids(&replay), vec![first_turn_id.clone()]);

    restored
        .shutdown()
        .await
        .expect("legacy restored gate must survive graceful shutdown");
    drop(restored);
    tokio::time::sleep(Duration::from_millis(50)).await;
    let state = fixture.store.load(&fixture.session_id).unwrap();
    let persistence = HeadlessSessionPersistence::new(fixture.store.clone(), state)
        .expect("reacquire legacy-gate lease for second restart");
    let second = build_runtime_with_persistence(
        "basic_chat",
        fixture.workdir.path().to_path_buf(),
        Vec::new(),
        Some(persistence),
    )
    .await
    .0;
    assert_eq!(
        await_pending_interaction(&second, kind, "second legacy-gate restore did not reattach")
            .await,
        fixture.interaction_id
    );
    assert_eq!(
        second
            .snapshot()
            .await
            .interactions
            .iter()
            .filter(|interaction| {
                interaction.id == fixture.interaction_id
                    && interaction.status == CodeUiInteractionStatus::Pending
            })
            .count(),
        1
    );
    assert_eq!(
        second
            .runtime_snapshot()
            .await
            .expect("read second legacy-gate runtime")
            .active_turn_id
            .as_deref(),
        Some(first_turn_id.as_str())
    );
    let replay = fixture.goal_store.load_code_workflow_replay().unwrap();
    assert_eq!(
        bound_turn_ids(&replay),
        vec![first_turn_id],
        "second restart must reuse the one durable legacy-gate binding"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn legacy_empty_turn_plan_gate_double_restart_reuses_one_binding_command() {
    assert_legacy_empty_turn_gate_reuses_binding_after_two_restarts(false).await;
}

#[tokio::test(flavor = "multi_thread")]
async fn legacy_empty_turn_network_gate_double_restart_reuses_one_binding_command() {
    assert_legacy_empty_turn_gate_reuses_binding_after_two_restarts(true).await;
}

#[derive(Clone)]
struct PendingCompletionModel {
    entered: Arc<Notify>,
    captured_history: Option<Arc<std::sync::Mutex<Option<String>>>>,
}

impl CompletionModel for PendingCompletionModel {
    type Response = ();

    async fn completion(
        &self,
        request: CompletionRequest,
    ) -> Result<CompletionResponse<Self::Response>, CompletionError> {
        if let Some(captured_history) = self.captured_history.as_ref() {
            *captured_history
                .lock()
                .expect("capture pending completion history") = Some(
                serde_json::to_string(&request.chat_history)
                    .expect("serialize pending completion history"),
            );
        }
        self.entered.notify_waiters();
        std::future::pending().await
    }
}

async fn build_pending_completion_runtime(
    working_dir: PathBuf,
    persistence: HeadlessSessionPersistence,
    entered: Arc<Notify>,
) -> Arc<HeadlessCodeRuntime<PendingCompletionModel>> {
    build_pending_completion_runtime_with_capture(working_dir, persistence, entered, None).await
}

async fn build_pending_completion_runtime_with_capture(
    working_dir: PathBuf,
    persistence: HeadlessSessionPersistence,
    entered: Arc<Notify>,
    captured_history: Option<Arc<std::sync::Mutex<Option<String>>>>,
) -> Arc<HeadlessCodeRuntime<PendingCompletionModel>> {
    try_build_pending_completion_runtime_with_capture(
        working_dir,
        persistence,
        entered,
        captured_history,
    )
    .await
    .expect("build pending-completion revision runtime")
}

async fn try_build_pending_completion_runtime_with_capture(
    working_dir: PathBuf,
    persistence: HeadlessSessionPersistence,
    entered: Arc<Notify>,
    captured_history: Option<Arc<std::sync::Mutex<Option<String>>>>,
) -> anyhow::Result<Arc<HeadlessCodeRuntime<PendingCompletionModel>>> {
    try_build_pending_completion_runtime_with_capture_and_snapshot(
        working_dir,
        persistence,
        entered,
        captured_history,
        None,
    )
    .await
}

async fn build_pending_completion_runtime_with_snapshot(
    working_dir: PathBuf,
    persistence: HeadlessSessionPersistence,
    entered: Arc<Notify>,
    snapshot: CodeUiSessionSnapshot,
) -> Arc<HeadlessCodeRuntime<PendingCompletionModel>> {
    try_build_pending_completion_runtime_with_capture_and_snapshot(
        working_dir,
        persistence,
        entered,
        None,
        Some(snapshot),
    )
    .await
    .expect("build pending-completion runtime from its persisted projection checkpoint")
}

async fn try_build_pending_completion_runtime_with_capture_and_snapshot(
    working_dir: PathBuf,
    persistence: HeadlessSessionPersistence,
    entered: Arc<Notify>,
    captured_history: Option<Arc<std::sync::Mutex<Option<String>>>>,
    restored_snapshot: Option<CodeUiSessionSnapshot>,
) -> anyhow::Result<Arc<HeadlessCodeRuntime<PendingCompletionModel>>> {
    let (user_input_tx, user_input_rx) = mpsc::unbounded_channel::<UserInputRequest>();
    let (_exec_approval_tx, exec_approval_rx) = mpsc::unbounded_channel::<ExecApprovalRequest>();
    let registry = Arc::new(
        ToolRegistryBuilder::with_working_dir(working_dir.clone())
            .hardening(ToolBoundaryRuntime::system(
                Uuid::new_v4(),
                Arc::new(TracingAuditSink),
            ))
            .register("read_file", Arc::new(ReadFileHandler))
            .register("update_plan", Arc::new(PlanHandler))
            .register("submit_plan_draft", Arc::new(SubmitPlanDraftHandler))
            .register("submit_intent_draft", Arc::new(SubmitIntentDraftHandler))
            .register(
                "request_user_input",
                Arc::new(RequestUserInputHandler::new(user_input_tx)),
            )
            .build(),
    );
    let capabilities = headless_capabilities();
    let session = CodeUiSession::new(restored_snapshot.unwrap_or_else(|| {
        initial_snapshot(
            working_dir.to_string_lossy().into_owned(),
            CodeUiProviderInfo {
                provider: "pending-completion-test".to_string(),
                model: Some("pending".to_string()),
                mode: Some("web-headless".to_string()),
                managed: false,
            },
            capabilities.clone(),
        )
    }));
    HeadlessCodeRuntime::new_with_persistence(
        session,
        capabilities,
        PendingCompletionModel {
            entered,
            captured_history,
        },
        registry,
        user_input_rx,
        exec_approval_rx,
        Arc::new(ToolLoopConfig::default),
        Vec::new(),
        Some(persistence),
        None,
    )
    .await
}

fn phase1_revision_seed(
    fixture: &LegacyPhase1GateFixture,
    command_id: &str,
    revision_note: &str,
) -> Phase1StartSeed {
    let context = load_phase1_review_context(&fixture.goal_store, &fixture.interaction_id)
        .expect("load source Plan context for revision attempt");
    let prior_plan_id = context.plan_id().map(str::to_string);
    Phase1StartSeed {
        schema_version: Phase1StartSeed::SCHEMA_VERSION,
        attempt_id: format!("attempt-{command_id}"),
        source_interaction_id: fixture.interaction_id.clone(),
        intent_id: context.intent_id,
        intent_spec_id: context.intent_spec_id,
        intent_spec_json: serde_json::to_string(&context.intent_spec)
            .expect("serialize revision IntentSpec"),
        source_resolution: "modify".to_string(),
        revision_note: Some(revision_note.to_string()),
        checkout: context.checkout,
        prior_plan: Some(context.execution_plan),
        prior_plan_id,
        prior_persisted_plan: context.persisted_plan,
        browser_command_id: Some(command_id.to_string()),
    }
}

fn persist_stale_phase1_thinking_projection(fixture: &LegacyPhase1GateFixture) {
    let mut snapshot = initial_snapshot(
        fixture.workdir.path().to_string_lossy().into_owned(),
        CodeUiProviderInfo {
            provider: "stale-phase1-projection".to_string(),
            model: Some("test".to_string()),
            mode: Some("web-headless".to_string()),
            managed: false,
        },
        headless_capabilities(),
    );
    snapshot.session_id = fixture.session_id.clone();
    snapshot.thread_id = Some(fixture.session_id.clone());
    snapshot.status = CodeUiSessionStatus::Thinking;
    let now = chrono::Utc::now();
    snapshot.transcript.push(CodeUiTranscriptEntry {
        id: "stale-phase1-assistant".to_string(),
        kind: CodeUiTranscriptEntryKind::AssistantMessage,
        title: Some("Planning".to_string()),
        content: Some("partial revision plan".to_string()),
        status: Some("streaming".to_string()),
        streaming: true,
        metadata: serde_json::json!({ "phase": "plan" }),
        created_at: now,
        updated_at: now,
    });
    snapshot.tool_calls.push(CodeUiToolCallSnapshot {
        id: "stale-phase1-tool".to_string(),
        tool_name: "submit_plan_draft".to_string(),
        status: "running".to_string(),
        summary: Some("Submitting revision".to_string()),
        details: None,
        updated_at: now,
    });
    snapshot.transcript.push(CodeUiTranscriptEntry {
        id: "stale-phase1-tool".to_string(),
        kind: CodeUiTranscriptEntryKind::ToolCall,
        title: Some("submit_plan_draft".to_string()),
        content: None,
        status: Some("running".to_string()),
        streaming: false,
        metadata: serde_json::json!({ "phase": "plan" }),
        created_at: now,
        updated_at: now,
    });
    let mut state = fixture
        .store
        .load(&fixture.session_id)
        .expect("load session before injecting stale Phase 1 projection");
    state.metadata.insert(
        "code_ui_snapshot".to_string(),
        serde_json::to_value(snapshot).expect("serialize stale Phase 1 snapshot"),
    );
    state.metadata.insert(
        "code_ui_projection_cursor".to_string(),
        serde_json::json!(0),
    );
    fixture
        .store
        .save(&state)
        .expect("persist stale Phase 1 Thinking projection");
}

fn assert_phase1_projection_is_settled(
    snapshot: &libra::internal::ai::web::code_ui::CodeUiSessionSnapshot,
) {
    assert_eq!(snapshot.status, CodeUiSessionStatus::Idle);
    assert!(
        snapshot
            .interactions
            .iter()
            .all(|interaction| { interaction.status != CodeUiInteractionStatus::Pending })
    );
    assert!(snapshot.transcript.iter().all(|entry| !entry.streaming));
    assert!(
        snapshot
            .transcript
            .iter()
            .all(|entry| entry.status.as_deref() != Some("running"))
    );
    assert!(
        snapshot
            .tool_calls
            .iter()
            .all(|tool_call| tool_call.status != "running")
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn failed_plan_revision_with_seed_repairs_stale_thinking_projection_on_restart() {
    use sha2::Digest as _;

    let fixture = legacy_empty_turn_phase1_gate_fixture(false).await;
    fixture
        .goal_store
        .append_code_workflow_durable(CodeWorkflowEventKind::InteractionResolved {
            interaction_id: fixture.interaction_id.clone(),
            resolution: "modify".to_string(),
            command: None,
            prior_interaction_resolutions: Vec::new(),
            intent_revision_consumption: None,
        })
        .expect("persist source Plan Modify authority");
    let seed = phase1_revision_seed(
        &fixture,
        "failed-plan-revision-with-seed",
        "tighten the verification step",
    );
    persist_phase1_start_seed(&fixture.goal_store, &seed)
        .expect("persist exact failed revision seed");
    let (_, repo_id, principal_id) = fixture.persistence.worker_durability_config();
    let command = CodeCommandIntent::new(
        CodeCommandIdentity::new(
            repo_id,
            &fixture.session_id,
            principal_id,
            phase1_turn_id_from_seed(&seed).expect("derive revision command id"),
        ),
        CODE_UI_WEB_TURN_KIND,
        format!(
            "sha256:{}",
            hex::encode(sha2::Sha256::digest(
                seed.revision_note.as_deref().unwrap().as_bytes()
            ))
        ),
        true,
    );
    assert!(matches!(
        fixture
            .goal_store
            .admit_code_command(command.clone())
            .expect("admit revision command before crash"),
        CodeCommandAdmission::Execute { .. }
    ));
    fixture
        .goal_store
        .complete_code_command_failure(&command.identity, "revision failed before formal write")
        .expect("persist determinate revision failure without retry authority");
    persist_stale_phase1_thinking_projection(&fixture);
    drop(fixture.persistence);

    let state = fixture.store.load(&fixture.session_id).unwrap();
    let persistence = HeadlessSessionPersistence::new(fixture.store.clone(), state)
        .expect("reacquire failed-revision startup lease");
    let restored = build_runtime_with_persistence(
        "basic_chat",
        fixture.workdir.path().to_path_buf(),
        Vec::new(),
        Some(persistence),
    )
    .await
    .0;
    assert_phase1_projection_is_settled(&restored.snapshot().await);
    assert!(
        load_phase1_start_seed(&fixture.goal_store)
            .unwrap()
            .is_none()
    );
    let replay = fixture.goal_store.load_code_workflow_replay().unwrap();
    assert_eq!(
        pending_plan_revision_from_workflow(replay.events.iter().map(|event| &event.event))
            .as_deref(),
        Some(fixture.interaction_id.as_str())
    );
    restored
        .submit_message("retry the retained Plan revision".to_string())
        .await
        .expect("repaired revision projection must accept a new command");
}

#[tokio::test(flavor = "multi_thread")]
async fn plan_revision_cancel_ack_persists_idle_then_restart_remains_revision_ready() {
    let fixture = legacy_empty_turn_phase1_gate_fixture(false).await;
    fixture
        .goal_store
        .append_code_workflow_durable(CodeWorkflowEventKind::InteractionResolved {
            interaction_id: fixture.interaction_id.clone(),
            resolution: "modify".to_string(),
            command: None,
            prior_interaction_resolutions: Vec::new(),
            intent_revision_consumption: None,
        })
        .expect("persist Plan Modify before active revision Cancel");
    let entered = Arc::new(Notify::new());
    let entered_wait = entered.notified();
    tokio::pin!(entered_wait);
    entered_wait.as_mut().enable();
    let runtime = build_pending_completion_runtime(
        fixture.workdir.path().to_path_buf(),
        fixture.persistence,
        Arc::clone(&entered),
    )
    .await;
    runtime
        .submit_message("cancel this active Plan revision".to_string())
        .await
        .expect("pending Plan revision must admit");
    tokio::time::timeout(Duration::from_secs(5), entered_wait)
        .await
        .expect("revision provider did not start before Cancel");
    runtime
        .cancel_turn()
        .await
        .expect("pre-formal Plan revision Cancel must acknowledge determinate success");
    assert_phase1_projection_is_settled(&runtime.snapshot().await);
    assert!(
        load_phase1_start_seed(&fixture.goal_store)
            .unwrap()
            .is_none()
    );
    let persisted = fixture.store.load(&fixture.session_id).unwrap();
    let persisted_snapshot: libra::internal::ai::web::code_ui::CodeUiSessionSnapshot =
        serde_json::from_value(
            persisted
                .metadata
                .get("code_ui_snapshot")
                .cloned()
                .expect("Cancel 2xx must persist a Code UI snapshot"),
        )
        .expect("decode Cancel 2xx snapshot");
    assert_phase1_projection_is_settled(&persisted_snapshot);

    drop(runtime);
    tokio::time::sleep(Duration::from_millis(50)).await;
    let state = fixture.store.load(&fixture.session_id).unwrap();
    let persistence = HeadlessSessionPersistence::new(fixture.store.clone(), state)
        .expect("reacquire cancelled-revision startup lease");
    let restored = build_runtime_with_persistence(
        "basic_chat",
        fixture.workdir.path().to_path_buf(),
        Vec::new(),
        Some(persistence),
    )
    .await
    .0;
    assert_phase1_projection_is_settled(&restored.snapshot().await);
    assert!(
        load_phase1_start_seed(&fixture.goal_store)
            .unwrap()
            .is_none()
    );
    let replay = fixture.goal_store.load_code_workflow_replay().unwrap();
    assert_eq!(
        pending_plan_revision_from_workflow(replay.events.iter().map(|event| &event.event))
            .as_deref(),
        Some(fixture.interaction_id.as_str())
    );
    restored
        .submit_message("retry after durable revision cancellation".to_string())
        .await
        .expect("restart must preserve Plan revision retry authority");
}

async fn assert_graceful_shutdown_restores_exact_gate(
    runtime: Arc<HeadlessCodeRuntime<fake::CompletionModel>>,
    store: &Arc<SessionStore>,
    session_id: &str,
    workdir: &Path,
    interaction_id: &str,
    kind: CodeUiInteractionKind,
    phase: Option<&str>,
) -> Arc<HeadlessCodeRuntime<fake::CompletionModel>> {
    runtime
        .shutdown()
        .await
        .expect("graceful shutdown must settle the live runtime without closing its parked gate");
    drop(runtime);
    tokio::time::sleep(Duration::from_millis(50)).await;

    let restored_state = store
        .load(session_id)
        .expect("load gracefully stopped parked session");
    let persistence = HeadlessSessionPersistence::new(store.clone(), restored_state)
        .expect("reacquire the gracefully stopped session writer lease");
    let restored = build_runtime_with_persistence(
        "basic_chat",
        workdir.to_path_buf(),
        Vec::new(),
        Some(persistence),
    )
    .await
    .0;
    let deadline = std::time::Instant::now() + Duration::from_secs(5);
    loop {
        let snapshot = restored.snapshot().await;
        assert_ne!(
            snapshot.status,
            CodeUiSessionStatus::IndeterminateSideEffect,
            "graceful restart must not fence a durable parked gate"
        );
        if snapshot.interactions.iter().any(|interaction| {
            interaction.id == interaction_id
                && interaction.kind == kind
                && interaction.status == CodeUiInteractionStatus::Pending
                && phase.is_none_or(|expected| {
                    interaction
                        .metadata
                        .get("phase")
                        .and_then(serde_json::Value::as_str)
                        == Some(expected)
                })
        }) {
            break;
        }
        assert!(
            std::time::Instant::now() < deadline,
            "restart did not reattach parked {kind:?} interaction '{interaction_id}'"
        );
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    restored
}

async fn assert_twice_restored_intent_settles_with_risk_audit(decision: &str) {
    let fixture = oversized_phase1_confirm_fixture(None).await;
    let risk_interaction_id = fixture
        .risk_interaction_id
        .clone()
        .expect("the graceful Intent fixture must use a real risk response");
    let replay = fixture.goal_store.load_code_workflow_replay().unwrap();
    assert_eq!(
        open_intent_review_from_workflow(replay.events.iter().map(|event| &event.event))
            .map(|(interaction_id, ..)| interaction_id),
        Some(fixture.source_interaction_id.clone())
    );
    let restored = assert_graceful_shutdown_restores_exact_gate(
        fixture.runtime,
        &fixture.store,
        &fixture.session_id,
        fixture.workdir.path(),
        &fixture.source_interaction_id,
        CodeUiInteractionKind::IntentReviewChoice,
        None,
    )
    .await;
    let second_restore = assert_graceful_shutdown_restores_exact_gate(
        restored,
        &fixture.store,
        &fixture.session_id,
        fixture.workdir.path(),
        &fixture.source_interaction_id,
        CodeUiInteractionKind::IntentReviewChoice,
        None,
    )
    .await;
    second_restore
        .respond_interaction(
            &fixture.source_interaction_id,
            CodeUiInteractionResponse {
                selected_option: Some(decision.to_string()),
                ..Default::default()
            },
        )
        .await
        .unwrap_or_else(|error| {
            panic!("twice-restored Intent {decision} must settle normally: {error:#}")
        });
    let replay = fixture.goal_store.load_code_workflow_replay().unwrap();
    let combined = replay
        .events
        .iter()
        .find_map(|event| match &event.event {
            CodeWorkflowEventKind::CommandTerminalSuccessWithInteractionResolved {
                interaction_id,
                resolution,
                prior_interaction_resolutions,
                ..
            } if interaction_id == &fixture.source_interaction_id => {
                Some((resolution.clone(), prior_interaction_resolutions.clone()))
            }
            _ => None,
        })
        .expect("twice-restored Intent must terminalize in one combined row");
    assert_eq!(combined.0, decision);
    assert!(
        replay.events.iter().any(|event| matches!(
            &event.event,
            CodeWorkflowEventKind::InteractionResolved {
                interaction_id,
                resolution,
                ..
            } if interaction_id == &risk_interaction_id && resolution == "answered"
        )) || combined
            .1
            .iter()
            .any(
                |(interaction_id, resolution)| interaction_id == &risk_interaction_id
                    && resolution == "answered"
            ),
        "full-log replay must retain the risk response across restored gate ownership"
    );
    assert!(
        second_restore
            .snapshot()
            .await
            .interactions
            .iter()
            .all(
                |interaction| interaction.id != fixture.source_interaction_id
                    || interaction.status != CodeUiInteractionStatus::Pending
            )
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn twice_restored_intent_confirm_keeps_real_risk_audit() {
    assert_twice_restored_intent_settles_with_risk_audit("confirm").await;
}

#[tokio::test(flavor = "multi_thread")]
async fn twice_restored_intent_cancel_keeps_real_risk_audit() {
    assert_twice_restored_intent_settles_with_risk_audit("cancel").await;
}

#[tokio::test(flavor = "multi_thread")]
async fn graceful_shutdown_then_restart_reattaches_parked_plan_gate() {
    let (fixture, plan_interaction_id) = oversized_phase1_plan_gate_fixture().await;
    let replay = fixture.goal_store.load_code_workflow_replay().unwrap();
    assert_eq!(
        open_plan_review_from_workflow(replay.events.iter().map(|event| &event.event))
            .map(|(interaction_id, ..)| interaction_id),
        Some(plan_interaction_id.clone())
    );
    let _restored = assert_graceful_shutdown_restores_exact_gate(
        fixture.runtime,
        &fixture.store,
        &fixture.session_id,
        fixture.workdir.path(),
        &plan_interaction_id,
        CodeUiInteractionKind::PostPlanChoice,
        None,
    )
    .await;
}

#[tokio::test(flavor = "multi_thread")]
async fn graceful_shutdown_then_restart_reattaches_parked_network_gate() {
    let (fixture, plan_interaction_id) = oversized_phase1_plan_gate_fixture().await;
    fixture
        .runtime
        .respond_interaction(
            &plan_interaction_id,
            CodeUiInteractionResponse {
                selected_option: Some("execute".to_string()),
                ..Default::default()
            },
        )
        .await
        .expect("Plan Execute must promote the Network gate");
    let network_interaction_id = await_pending_interaction(
        &fixture.runtime,
        CodeUiInteractionKind::PostPlanChoice,
        "Plan Execute did not park its Network gate",
    )
    .await;
    assert_ne!(network_interaction_id, plan_interaction_id);
    let replay = fixture.goal_store.load_code_workflow_replay().unwrap();
    assert_eq!(
        open_network_policy_from_workflow(replay.events.iter().map(|event| &event.event))
            .map(|(interaction_id, ..)| interaction_id),
        Some(network_interaction_id.clone())
    );
    let _restored = assert_graceful_shutdown_restores_exact_gate(
        fixture.runtime,
        &fixture.store,
        &fixture.session_id,
        fixture.workdir.path(),
        &network_interaction_id,
        CodeUiInteractionKind::PostPlanChoice,
        Some("networkPolicy"),
    )
    .await;
}

#[tokio::test(flavor = "current_thread")]
async fn shutdown_after_formal_write_boundary_waits_for_one_plan_then_restores_it() {
    let formal_write = HeadlessRecordUserMessageHook::new();
    let fixture =
        oversized_phase1_confirm_fixture_with_hooks(None, None, Some(formal_write.clone()), false)
            .await;
    fixture
        .runtime
        .respond_interaction(
            &fixture.source_interaction_id,
            CodeUiInteractionResponse {
                selected_option: Some("confirm".to_string()),
                ..Default::default()
            },
        )
        .await
        .expect("first oversized Phase 1 attempt must admit and fail pre-formal");
    let retry_interaction_id = {
        let deadline = std::time::Instant::now() + Duration::from_secs(8);
        loop {
            if let Some(interaction) =
                fixture
                    .runtime
                    .snapshot()
                    .await
                    .interactions
                    .iter()
                    .find(|interaction| {
                        interaction.kind == CodeUiInteractionKind::IntentReviewChoice
                            && interaction.status == CodeUiInteractionStatus::Pending
                            && interaction.id != fixture.source_interaction_id
                    })
            {
                break interaction.id.clone();
            }
            assert!(
                std::time::Instant::now() < deadline,
                "pre-formal failure did not rearm Confirm for the bounded attempt"
            );
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    };
    fixture
        .runtime
        .respond_interaction(
            &retry_interaction_id,
            CodeUiInteractionResponse {
                selected_option: Some("confirm".to_string()),
                ..Default::default()
            },
        )
        .await
        .expect("bounded retry Confirm must admit Phase 1 generation");
    tokio::time::timeout(Duration::from_secs(8), formal_write.wait_until_entered())
        .await
        .expect("Phase 1 must pause after its durable formal-write boundary");
    let before_release = fixture.goal_store.load_code_workflow_replay().unwrap();
    assert_eq!(
        before_release
            .events
            .iter()
            .filter(|event| matches!(
                &event.event,
                CodeWorkflowEventKind::Phase1FormalWriteStarted { .. }
            ))
            .count(),
        1
    );
    assert!(before_release.events.iter().all(|event| !matches!(
        &event.event,
        CodeWorkflowEventKind::PlanReviewRequested { .. }
    )));

    let shutdown_runtime = Arc::clone(&fixture.runtime);
    let mut shutdown = tokio::spawn(async move { shutdown_runtime.shutdown().await });
    assert!(
        tokio::time::timeout(Duration::from_millis(50), &mut shutdown)
            .await
            .is_err(),
        "shutdown must wait for the already-Mutating formal write instead of fencing it"
    );
    formal_write.release();
    tokio::time::timeout(Duration::from_secs(15), shutdown)
        .await
        .expect("formal write must finish within the shutdown bound")
        .expect("shutdown task must not panic")
        .expect("shutdown after the formal boundary must complete cleanly");

    let replay = fixture.goal_store.load_code_workflow_replay().unwrap();
    assert!(replay.events.iter().all(|event| !matches!(
        &event.event,
        CodeWorkflowEventKind::CommandIndeterminateSideEffect { .. }
    )));
    let plan_interaction_ids = replay
        .events
        .iter()
        .filter_map(|event| match &event.event {
            CodeWorkflowEventKind::PlanReviewRequested { interaction_id, .. } => {
                Some(interaction_id.clone())
            }
            _ => None,
        })
        .collect::<Vec<_>>();
    assert_eq!(
        plan_interaction_ids.len(),
        1,
        "the allowed formal write must publish exactly one Plan authority"
    );
    libra::internal::ai::runtime::phase1::validate_single_open_gate_authority(&fixture.goal_store)
        .expect("shutdown must leave one valid Plan authority");

    drop(fixture.runtime);
    tokio::time::sleep(Duration::from_millis(50)).await;
    let restored_state = fixture.store.load(&fixture.session_id).unwrap();
    let persistence = HeadlessSessionPersistence::new(fixture.store.clone(), restored_state)
        .expect("reacquire formal-write session lease after clean shutdown");
    let restored = build_runtime_with_persistence(
        "basic_chat",
        fixture.workdir.path().to_path_buf(),
        Vec::new(),
        Some(persistence),
    )
    .await
    .0;
    let restored_plan = await_pending_interaction(
        &restored,
        CodeUiInteractionKind::PostPlanChoice,
        "restart did not restore the Plan written during shutdown",
    )
    .await;
    assert_eq!(restored_plan, plan_interaction_ids[0]);
}

async fn assert_control_cancel_fsyncs_gate_before_restart(
    runtime: Arc<HeadlessCodeRuntime<fake::CompletionModel>>,
    store: &Arc<SessionStore>,
    goal_store: &SessionJsonlStore,
    session_id: &str,
    workdir: &Path,
    interaction_id: &str,
    expected_answered_priors: usize,
) {
    let runtime_turn_id = runtime
        .runtime_snapshot()
        .await
        .expect("read parked runtime turn before control Cancel")
        .active_turn_id
        .expect("parked gate must retain an active runtime turn");
    runtime
        .cancel_turn()
        .await
        .expect("control Cancel must durably settle the parked gate");
    let replay = goal_store
        .load_code_workflow_replay()
        .expect("read control-cancelled gate workflow");
    let atomic = replay
        .events
        .iter()
        .filter_map(|event| match &event.event {
            CodeWorkflowEventKind::CommandTerminalSuccessWithInteractionResolved {
                command,
                interaction_id: resolved_id,
                resolution,
                prior_interaction_resolutions,
                ..
            } if command.command_id == runtime_turn_id && resolved_id == interaction_id => Some((
                command.clone(),
                resolution.clone(),
                prior_interaction_resolutions.clone(),
            )),
            _ => None,
        })
        .collect::<Vec<_>>();
    assert_eq!(
        atomic.len(),
        1,
        "control Cancel must commit terminal success and gate resolution in one durable row"
    );
    assert_eq!(atomic[0].1, "cancel");
    assert_eq!(
        atomic[0]
            .2
            .iter()
            .filter(|(_, resolution)| resolution == "answered")
            .count(),
        expected_answered_priors,
        "control Cancel must retain every earlier user-input resolution in its single terminal row"
    );
    assert!(
        replay.events.iter().all(|event| !matches!(
            &event.event,
            CodeWorkflowEventKind::InteractionResolved {
                interaction_id: resolved_id,
                ..
            } if resolved_id == interaction_id
        )),
        "control Cancel must not leave a separately tearable interaction resolution"
    );
    assert!(
        replay.events.iter().all(|event| !matches!(
            &event.event,
            CodeWorkflowEventKind::CommandTerminalFailure { command, .. }
                if command == &atomic[0].0
        )),
        "control Cancel must not also terminalize the same command as failure"
    );
    let (_, status) = goal_store
        .code_command_intent_status(&atomic[0].0)
        .expect("load control Cancel command status")
        .expect("control Cancel command must remain indexed");
    assert!(matches!(status, CodeCommandStatus::Succeeded { .. }));
    assert!(
        open_intent_review_from_workflow(replay.events.iter().map(|event| &event.event)).is_none()
            && open_plan_review_from_workflow(replay.events.iter().map(|event| &event.event))
                .is_none()
            && open_network_policy_from_workflow(replay.events.iter().map(|event| &event.event))
                .is_none(),
        "control Cancel must close every effective gate before acknowledging"
    );

    drop(runtime);
    tokio::time::sleep(Duration::from_millis(50)).await;
    let restored_state = store.load(session_id).unwrap();
    let persistence = HeadlessSessionPersistence::new(store.clone(), restored_state)
        .expect("reacquire cancelled session writer lease");
    let restored = build_runtime_with_persistence(
        "basic_chat",
        workdir.to_path_buf(),
        Vec::new(),
        Some(persistence),
    )
    .await
    .0;
    tokio::time::sleep(Duration::from_millis(50)).await;
    let snapshot = restored.snapshot().await;
    assert!(
        snapshot
            .interactions
            .iter()
            .all(|interaction| interaction.status != CodeUiInteractionStatus::Pending),
        "restart must not resurrect a control-cancelled gate"
    );
    assert_eq!(snapshot.status, CodeUiSessionStatus::Idle);
}

#[tokio::test(flavor = "multi_thread")]
async fn control_cancel_initial_intent_fsyncs_terminal_before_restart() {
    let fixture = oversized_phase1_confirm_fixture(None).await;
    assert_control_cancel_fsyncs_gate_before_restart(
        fixture.runtime,
        &fixture.store,
        &fixture.goal_store,
        &fixture.session_id,
        fixture.workdir.path(),
        &fixture.source_interaction_id,
        1,
    )
    .await;
}

#[tokio::test(flavor = "multi_thread")]
async fn control_cancel_restored_intent_fsyncs_terminal_before_restart() {
    let fixture = oversized_phase1_confirm_fixture(None).await;
    let risk_interaction_id = fixture
        .risk_interaction_id
        .clone()
        .expect("restored Intent fixture must use a real risk response");
    let restored = assert_graceful_shutdown_restores_exact_gate(
        fixture.runtime,
        &fixture.store,
        &fixture.session_id,
        fixture.workdir.path(),
        &fixture.source_interaction_id,
        CodeUiInteractionKind::IntentReviewChoice,
        None,
    )
    .await;
    assert_control_cancel_fsyncs_gate_before_restart(
        restored,
        &fixture.store,
        &fixture.goal_store,
        &fixture.session_id,
        fixture.workdir.path(),
        &fixture.source_interaction_id,
        0,
    )
    .await;
    let replay = fixture.goal_store.load_code_workflow_replay().unwrap();
    assert!(replay.events.iter().any(|event| matches!(
        &event.event,
        CodeWorkflowEventKind::InteractionResolved {
            interaction_id,
            resolution,
            ..
        } if interaction_id == &risk_interaction_id && resolution == "answered"
    )));
}

#[tokio::test(flavor = "multi_thread")]
async fn control_cancel_plan_fsyncs_terminal_before_restart() {
    let (fixture, plan_interaction_id) = oversized_phase1_plan_gate_fixture().await;
    assert_control_cancel_fsyncs_gate_before_restart(
        fixture.runtime,
        &fixture.store,
        &fixture.goal_store,
        &fixture.session_id,
        fixture.workdir.path(),
        &plan_interaction_id,
        0,
    )
    .await;
}

#[tokio::test(flavor = "multi_thread")]
async fn control_cancel_network_fsyncs_terminal_before_restart() {
    let (fixture, plan_interaction_id) = oversized_phase1_plan_gate_fixture().await;
    fixture
        .runtime
        .respond_interaction(
            &plan_interaction_id,
            CodeUiInteractionResponse {
                selected_option: Some("execute".to_string()),
                ..Default::default()
            },
        )
        .await
        .expect("Plan Execute must promote the Network gate");
    let network_interaction_id = await_pending_interaction(
        &fixture.runtime,
        CodeUiInteractionKind::PostPlanChoice,
        "Plan Execute did not park its Network gate before control Cancel",
    )
    .await;
    assert_control_cancel_fsyncs_gate_before_restart(
        fixture.runtime,
        &fixture.store,
        &fixture.goal_store,
        &fixture.session_id,
        fixture.workdir.path(),
        &network_interaction_id,
        0,
    )
    .await;
}

async fn assert_control_cancel_terminal_failure_fences_and_survives_restart(
    runtime: Arc<HeadlessCodeRuntime<fake::CompletionModel>>,
    store: &Arc<SessionStore>,
    goal_store: &SessionJsonlStore,
    session_id: &str,
    workdir: &Path,
    interaction_id: &str,
) {
    let runtime_turn_id = runtime
        .runtime_snapshot()
        .await
        .expect("read parked gate before injected control Cancel failure")
        .active_turn_id
        .expect("parked review gate must retain its runtime command");
    goal_store.fail_next_combined_terminal_append_for_test();
    let error = runtime
        .cancel_turn()
        .await
        .expect_err("an unpersistable combined control Cancel must not acknowledge success");
    assert_eq!(
        error.to_string(),
        "Phase 1 Web close-out is indeterminate; restart and reconcile the durable session before retrying",
        "the Cancel error must expose the bounded durable reconciliation boundary"
    );
    assert_eq!(
        runtime.snapshot().await.status,
        CodeUiSessionStatus::IndeterminateSideEffect,
        "Web projection must remain fenced after combined terminal failure"
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
            })
    );
    assert!(matches!(
        runtime
            .runtime_snapshot()
            .await
            .expect("runtime snapshot after injected terminal failure")
            .interaction,
        InteractionState::IndeterminateSideEffect { .. }
    ));

    let replay = goal_store.load_code_workflow_replay().unwrap();
    assert!(replay.events.iter().all(|event| !matches!(
        &event.event,
        CodeWorkflowEventKind::CommandTerminalSuccessWithInteractionResolved { command, .. }
            if command.command_id == runtime_turn_id
    )));
    assert!(replay.events.iter().any(|event| matches!(
        &event.event,
        CodeWorkflowEventKind::CommandIndeterminateSideEffect { command, .. }
            if command.command_id == runtime_turn_id
    )));

    let retry = runtime
        .cancel_turn()
        .await
        .expect_err("UnknownTurn/retry after failed terminal durability must not become success");
    assert!(
        retry.to_string().contains("RECONCILIATION_REQUIRED")
            || retry.to_string().contains("reconciliation")
            || retry.to_string().contains("SESSION_BUSY"),
        "the retry must remain a typed non-success: {retry:#}"
    );
    let submit = runtime
        .submit_message("/must remain blocked after terminal durability failure".to_string())
        .await
        .expect_err("a fenced session must reject new turns");
    assert_code_ui_api_error(&submit, 409, "RECONCILIATION_REQUIRED");

    drop(runtime);
    tokio::time::sleep(Duration::from_millis(50)).await;
    let restored_state = store
        .load(session_id)
        .expect("load control-Cancel durability failure session");
    let persistence = HeadlessSessionPersistence::new(store.clone(), restored_state)
        .expect("reacquire failed control-Cancel session writer lease");
    let restored = build_runtime_with_persistence(
        "basic_chat",
        workdir.to_path_buf(),
        Vec::new(),
        Some(persistence),
    )
    .await
    .0;
    assert_eq!(
        restored.snapshot().await.status,
        CodeUiSessionStatus::IndeterminateSideEffect,
        "restart must fail closed when the combined terminal row never landed"
    );
    restored
        .submit_message("/restart must remain fenced".to_string())
        .await
        .expect_err("restart must not re-admit work after ambiguous control Cancel");
}

#[tokio::test(flavor = "multi_thread")]
async fn initial_intent_control_cancel_terminal_failure_fences_and_survives_restart() {
    let fixture = oversized_phase1_confirm_fixture(None).await;
    assert_control_cancel_terminal_failure_fences_and_survives_restart(
        fixture.runtime,
        &fixture.store,
        &fixture.goal_store,
        &fixture.session_id,
        fixture.workdir.path(),
        &fixture.source_interaction_id,
    )
    .await;
}

#[tokio::test(flavor = "multi_thread")]
async fn plan_control_cancel_terminal_failure_fences_and_survives_restart() {
    let (fixture, interaction_id) = oversized_phase1_plan_gate_fixture().await;
    assert_control_cancel_terminal_failure_fences_and_survives_restart(
        fixture.runtime,
        &fixture.store,
        &fixture.goal_store,
        &fixture.session_id,
        fixture.workdir.path(),
        &interaction_id,
    )
    .await;
}

#[tokio::test(flavor = "multi_thread")]
async fn network_control_cancel_terminal_failure_fences_and_survives_restart() {
    let (fixture, plan_interaction_id) = oversized_phase1_plan_gate_fixture().await;
    fixture
        .runtime
        .respond_interaction(
            &plan_interaction_id,
            CodeUiInteractionResponse {
                selected_option: Some("execute".to_string()),
                ..Default::default()
            },
        )
        .await
        .expect("Plan Execute must promote Network before failure injection");
    let interaction_id = await_pending_interaction(
        &fixture.runtime,
        CodeUiInteractionKind::PostPlanChoice,
        "Plan Execute did not park Network before failure injection",
    )
    .await;
    assert_control_cancel_terminal_failure_fences_and_survives_restart(
        fixture.runtime,
        &fixture.store,
        &fixture.goal_store,
        &fixture.session_id,
        fixture.workdir.path(),
        &interaction_id,
    )
    .await;
}

#[tokio::test(flavor = "multi_thread")]
async fn oversized_phase1_plan_draft_rearms_confirm_then_same_input_produces_one_plan() {
    let fixture = oversized_phase1_retry_fixture().await;

    let snapshot = fixture.runtime.snapshot().await;
    assert_eq!(snapshot.status, CodeUiSessionStatus::AwaitingInteraction);
    assert!(
        snapshot
            .plans
            .iter()
            .all(|plan| plan.id != "oversized-plan-draft"),
        "unbounded arguments must not be cloned into a plan projection"
    );
    let tool_call = snapshot
        .tool_calls
        .iter()
        .find(|call| call.id == "oversized-plan-draft")
        .expect("the rejected tool call remains auditable");
    assert_eq!(tool_call.status, "failed");
    assert!(
        tool_call
            .details
            .as_deref()
            .is_some_and(|details| details.contains("at most")),
        "the tool terminal must explain its enforced step bound: {tool_call:?}"
    );
    assert!(snapshot.transcript.iter().all(|entry| !entry.streaming));
    assert!(
        snapshot.transcript.iter().any(|entry| {
            entry.id == "oversized-plan-draft" && entry.status.as_deref() == Some("failed")
        }),
        "the UI transcript must expose a terminal failed tool row"
    );
    assert!(
        load_phase1_start_seed(&fixture.goal_store)
            .expect("load oversized Phase 1 seed after failure")
            .is_none(),
        "the failed attempt seed is cleared only after fresh Confirm authority is durable"
    );
    let replay = fixture
        .goal_store
        .load_code_workflow_replay()
        .expect("read oversized Phase 1 workflow");
    assert!(replay.events.iter().all(|event| !matches!(
        &event.event,
        CodeWorkflowEventKind::Phase1FormalWriteStarted { .. }
    )));
    assert!(replay.events.iter().all(|event| !matches!(
        &event.event,
        CodeWorkflowEventKind::PlanReviewRequested { .. }
    )));
    assert_eq!(
        phase1_context_file_count(&fixture.store, &fixture.session_id),
        0,
        "no formal review context may be written by the rejected attempt"
    );
    let busy = fixture
        .runtime
        .submit_message("/must remain blocked behind retry authority".to_string())
        .await
        .expect_err("fresh Confirm authority must block unrelated direct turns");
    assert!(
        busy.to_string().contains("running") || busy.to_string().contains("pending"),
        "retry authority conflict must remain actionable: {busy:#}"
    );

    fixture
        .runtime
        .respond_interaction(
            &fixture.retry_interaction_id,
            CodeUiInteractionResponse {
                selected_option: Some("confirm".to_string()),
                ..Default::default()
            },
        )
        .await
        .expect("same Confirm input must admit a fresh Phase 1 attempt");
    let _plan_interaction_id = await_pending_interaction(
        &fixture.runtime,
        CodeUiInteractionKind::PostPlanChoice,
        "the bounded retry did not publish its Plan review",
    )
    .await;

    let replay = fixture
        .goal_store
        .load_code_workflow_replay()
        .expect("read the successful Phase 1 retry workflow");
    let formal_turns = replay
        .events
        .iter()
        .filter_map(|event| match &event.event {
            CodeWorkflowEventKind::Phase1FormalWriteStarted { phase1_turn_id, .. } => {
                Some(phase1_turn_id.clone())
            }
            _ => None,
        })
        .collect::<Vec<_>>();
    assert_eq!(formal_turns.len(), 1);
    assert_eq!(
        replay
            .events
            .iter()
            .filter(|event| {
                matches!(
                    &event.event,
                    CodeWorkflowEventKind::PlanReviewRequested { .. }
                )
            })
            .count(),
        1,
        "retry must publish only one durable Plan review generation"
    );
    let succeeded_command = replay
        .events
        .iter()
        .find_map(|event| match &event.event {
            CodeWorkflowEventKind::CommandTerminalSuccess { command, .. }
                if command.command_id == formal_turns[0] =>
            {
                Some(command.clone())
            }
            _ => None,
        })
        .expect("the bounded retry must complete its Phase 1 command");
    assert_ne!(
        succeeded_command.command_id, fixture.failed_command.command_id,
        "retry attempt identity must not reuse the terminal Failed command"
    );
    let intent_for = |identity: &CodeCommandIdentity| {
        replay.events.iter().find_map(|event| match &event.event {
            CodeWorkflowEventKind::CommandIntentPersisted { command }
                if &command.identity == identity =>
            {
                Some(command.clone())
            }
            _ => None,
        })
    };
    let failed_intent = intent_for(&fixture.failed_command).expect("failed attempt intent");
    let succeeded_intent = intent_for(&succeeded_command).expect("successful retry intent");
    assert_eq!(
        failed_intent.canonical_request_hash, succeeded_intent.canonical_request_hash,
        "same Confirm input must retain the same canonical request text across attempt ids"
    );
    assert_eq!(
        phase1_context_file_count(&fixture.store, &fixture.session_id),
        1,
        "only the successful retry may persist a formal review context"
    );
    libra::internal::ai::runtime::phase1::validate_single_open_gate_authority(&fixture.goal_store)
        .expect("the successful retry must leave one Plan authority");
}

#[tokio::test(flavor = "multi_thread")]
async fn cancelling_rearmed_confirm_clears_retry_authority_without_formal_write() {
    let fixture = oversized_phase1_retry_fixture().await;
    fixture
        .runtime
        .respond_interaction(
            &fixture.retry_interaction_id,
            CodeUiInteractionResponse {
                selected_option: Some("cancel".to_string()),
                ..Default::default()
            },
        )
        .await
        .expect("the developer may explicitly cancel the rearmed Confirm gate");

    let deadline = std::time::Instant::now() + Duration::from_secs(3);
    loop {
        let snapshot = fixture.runtime.snapshot().await;
        if snapshot.status == CodeUiSessionStatus::Idle
            && snapshot
                .interactions
                .iter()
                .all(|interaction| interaction.status != CodeUiInteractionStatus::Pending)
        {
            break;
        }
        assert!(
            std::time::Instant::now() < deadline,
            "explicit Cancel did not clear the rearmed retry authority"
        );
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    assert!(
        load_phase1_start_seed(&fixture.goal_store)
            .expect("load seed after explicit retry Cancel")
            .is_none()
    );
    let replay = fixture.goal_store.load_code_workflow_replay().unwrap();
    assert!(replay.events.iter().all(|event| !matches!(
        &event.event,
        CodeWorkflowEventKind::Phase1FormalWriteStarted { .. }
            | CodeWorkflowEventKind::PlanReviewRequested { .. }
    )));
    assert_eq!(
        phase1_context_file_count(&fixture.store, &fixture.session_id),
        0
    );
    assert_ne!(
        fixture.source_interaction_id, fixture.retry_interaction_id,
        "the cancelled authority must be the fresh retry generation"
    );

    fixture
        .runtime
        .submit_message("/direct turn after explicit Cancel".to_string())
        .await
        .expect("only explicit Cancel releases the retry authority for unrelated direct work");
}

#[tokio::test(flavor = "multi_thread")]
async fn cancelled_embedded_phase1_retry_gate_does_not_reopen_after_restart() {
    let fixture = oversized_phase1_retry_fixture().await;
    let retry_interaction_id = fixture.retry_interaction_id.clone();
    fixture
        .runtime
        .respond_interaction(
            &retry_interaction_id,
            CodeUiInteractionResponse {
                selected_option: Some("cancel".to_string()),
                ..Default::default()
            },
        )
        .await
        .expect("Cancel must durably resolve the embedded retry authority");

    let deadline = std::time::Instant::now() + Duration::from_secs(5);
    loop {
        let replay = fixture.goal_store.load_code_workflow_replay().unwrap();
        let combined_cancel_count = replay
            .events
            .iter()
            .filter(|event| {
                matches!(
                    &event.event,
                    CodeWorkflowEventKind::CommandTerminalSuccessWithInteractionResolved {
                        interaction_id,
                        resolution,
                        ..
                    } if interaction_id == &retry_interaction_id && resolution == "cancel"
                )
            })
            .count();
        if combined_cancel_count == 1
            && load_phase1_start_seed(&fixture.goal_store)
                .expect("load seed after retry Cancel")
                .is_none()
            && fixture.runtime.snapshot().await.status == CodeUiSessionStatus::Idle
        {
            break;
        }
        assert!(
            std::time::Instant::now() < deadline,
            "Cancel ACK did not durably resolve the embedded gate before clearing its seed"
        );
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    let replay = fixture.goal_store.load_code_workflow_replay().unwrap();
    assert!(replay.events.iter().all(|event| !matches!(
        &event.event,
        CodeWorkflowEventKind::InteractionResolved {
            interaction_id,
            resolution,
            ..
        } if interaction_id == &retry_interaction_id && resolution == "cancel"
    )));
    fixture
        .runtime
        .shutdown()
        .await
        .expect("the cancelled retry gate must allow a clean shutdown");
    drop(fixture.runtime);
    tokio::time::sleep(Duration::from_millis(50)).await;

    let restored_state = fixture
        .store
        .load(&fixture.session_id)
        .expect("load cancelled embedded-gate session");
    let persistence = HeadlessSessionPersistence::new(fixture.store.clone(), restored_state)
        .expect("reacquire cancelled embedded-gate session lease");
    let restored = build_runtime_with_persistence(
        "basic_chat",
        fixture.workdir.path().to_path_buf(),
        Vec::new(),
        Some(persistence),
    )
    .await
    .0;
    let replay = fixture.goal_store.load_code_workflow_replay().unwrap();
    assert!(
        open_intent_review_from_workflow(replay.events.iter().map(|event| &event.event)).is_none(),
        "restart must not reopen the cancelled embedded retry authority"
    );
    let snapshot = restored.snapshot().await;
    assert_eq!(snapshot.status, CodeUiSessionStatus::Idle);
    assert!(snapshot.interactions.iter().all(|interaction| {
        interaction.id != retry_interaction_id
            || interaction.status != CodeUiInteractionStatus::Pending
    }));
    assert!(
        load_phase1_start_seed(&fixture.goal_store)
            .expect("load seed after cancelled-gate restart")
            .is_none()
    );
    restored
        .submit_message("/new turn after embedded retry cancellation".to_string())
        .await
        .expect("restart after durable retry Cancel must admit new work");
}

#[tokio::test(flavor = "multi_thread")]
async fn non_pending_intent_confirm_retry_is_pure_ack_and_conflict_is_non_mutating() {
    let fixture = oversized_phase1_retry_fixture().await;
    let before_retry = fixture
        .goal_store
        .load_code_workflow_replay()
        .expect("read workflow before retrying the terminal source Confirm");

    fixture
        .runtime
        .respond_interaction(
            &fixture.source_interaction_id,
            CodeUiInteractionResponse {
                selected_option: Some("confirm".to_string()),
                ..Default::default()
            },
        )
        .await
        .expect("the exact non-Pending Confirm retry must be a pure acknowledgement");
    let after_exact_retry = fixture
        .goal_store
        .load_code_workflow_replay()
        .expect("read workflow after the exact terminal Confirm retry");
    assert_eq!(
        after_exact_retry.events, before_retry.events,
        "an exact retry must not enqueue another Phase 1 attempt or append another resolution"
    );
    assert!(
        fixture
            .runtime
            .snapshot()
            .await
            .interactions
            .iter()
            .any(|interaction| {
                interaction.id == fixture.retry_interaction_id
                    && interaction.status == CodeUiInteractionStatus::Pending
            }),
        "the exact old-generation ACK must leave the fresh retry authority pending"
    );

    let conflict = fixture
        .runtime
        .respond_interaction(
            &fixture.source_interaction_id,
            CodeUiInteractionResponse {
                selected_option: Some("cancel".to_string()),
                ..Default::default()
            },
        )
        .await
        .expect_err("a conflicting retry of a terminal Confirm must fail closed");
    assert!(
        conflict
            .to_string()
            .contains("INTERACTION_ALREADY_RESOLVED")
            || conflict
                .to_string()
                .contains("already resolved as 'confirm'"),
        "the conflict must identify the durable terminal resolution: {conflict:#}"
    );
    assert_eq!(
        fixture
            .goal_store
            .load_code_workflow_replay()
            .expect("read workflow after the conflicting terminal retry")
            .events,
        before_retry.events,
        "a conflicting retry must not mutate the durable source or replacement generation"
    );
}

#[derive(Clone)]
struct DeltaThenErrorModel;

impl CompletionModel for DeltaThenErrorModel {
    type Response = ();

    async fn completion(
        &self,
        request: CompletionRequest,
    ) -> Result<CompletionResponse<Self::Response>, CompletionError> {
        if let Some(stream_events) = request.stream_events {
            let _ = stream_events.send(CompletionStreamEvent::TextDelta {
                request_id: Some("delta-then-error".to_string()),
                delta: "partial provider delta".to_string(),
            });
            tokio::task::yield_now().await;
        }
        Err(CompletionError::ProviderError(
            "injected failure after provider delta".to_string(),
        ))
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn provider_delta_then_error_flushes_observer_worker_without_late_snapshot_update() {
    let workdir = tempfile::tempdir().expect("tempdir for delta/error workdir");
    let registry = Arc::new(
        ToolRegistryBuilder::with_working_dir(workdir.path().to_path_buf())
            .hardening(ToolBoundaryRuntime::system(
                Uuid::new_v4(),
                Arc::new(TracingAuditSink),
            ))
            .build(),
    );
    let (_user_input_tx, user_input_rx) = mpsc::unbounded_channel::<UserInputRequest>();
    let (_exec_approval_tx, exec_approval_rx) = mpsc::unbounded_channel::<ExecApprovalRequest>();
    let capabilities = headless_capabilities();
    let session = CodeUiSession::new(initial_snapshot(
        workdir.path().to_string_lossy().into_owned(),
        CodeUiProviderInfo {
            provider: "delta-then-error".to_string(),
            model: Some("test".to_string()),
            mode: Some("web-headless".to_string()),
            managed: false,
        },
        capabilities.clone(),
    ));
    let runtime = HeadlessCodeRuntime::new_with_persistence(
        session,
        capabilities,
        DeltaThenErrorModel,
        registry,
        user_input_rx,
        exec_approval_rx,
        Arc::new(ToolLoopConfig::default),
        Vec::new(),
        None,
        None,
    )
    .await
    .expect("build the delta-then-error runtime");

    runtime
        .submit_message("/stream once, then fail".to_string())
        .await
        .expect("delta/error turn must admit");
    let deadline = std::time::Instant::now() + Duration::from_secs(3);
    let terminal_entry = loop {
        let snapshot = runtime.snapshot().await;
        if snapshot.status == CodeUiSessionStatus::Error
            && let Some(entry) = snapshot.transcript.iter().find(|entry| {
                entry.kind
                    == libra::internal::ai::web::code_ui::CodeUiTranscriptEntryKind::AssistantMessage
                    && entry.status.as_deref() == Some("error")
            })
        {
            break entry.clone();
        }
        assert!(
            std::time::Instant::now() < deadline,
            "provider delta followed by error did not reach a terminal snapshot"
        );
        tokio::time::sleep(Duration::from_millis(10)).await;
    };
    assert!(!terminal_entry.streaming);
    assert!(
        terminal_entry
            .content
            .as_deref()
            .is_some_and(|content| content.contains("injected failure after provider delta")),
        "terminal error must replace the partial provider delta: {terminal_entry:?}"
    );

    // Give any incorrectly detached observer worker a bounded opportunity to
    // race. A correctly flushed worker is already joined before Error becomes
    // visible, so neither content nor streaming state can change afterwards.
    tokio::time::sleep(Duration::from_millis(100)).await;
    let settled = runtime
        .snapshot()
        .await
        .transcript
        .into_iter()
        .find(|entry| entry.id == terminal_entry.id)
        .expect("terminal assistant entry remains projected");
    assert_eq!(settled.content, terminal_entry.content);
    assert_eq!(settled.status, terminal_entry.status);
    assert!(!settled.streaming);
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

/// Plain (non-`/`) browser chat must admit as Phase 0 plan routing so the
/// default path cannot open the full mutating tool allowlist.
#[tokio::test(flavor = "multi_thread")]
async fn plain_message_admits_as_plan_phase0_turn_mode() {
    let workdir = tempfile::tempdir().expect("tempdir for headless workdir");
    let (runtime, _, _) = build_runtime("basic_chat", workdir.path().to_path_buf()).await;

    runtime
        .submit_message("Add docs to README".to_string())
        .await
        .expect("plain message must still admit");

    let deadline = std::time::Instant::now() + Duration::from_secs(5);
    while std::time::Instant::now() < deadline {
        let snapshot = runtime.snapshot().await;
        if let Some(user) = snapshot.transcript.iter().find(|entry| {
            entry.kind == libra::internal::ai::web::code_ui::CodeUiTranscriptEntryKind::UserMessage
        }) {
            assert_eq!(
                user.metadata.get("webTurnMode").and_then(|v| v.as_str()),
                Some("PlanPhase0"),
                "plain chat must record PlanPhase0 admission metadata"
            );
            return;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    panic!("plain message did not project a user transcript entry with plan mode");
}

/// Plain Phase 0 admission must actually fence mutating tools — metadata alone
/// is not enough. The model fixture asks for `blocking_mutation`; Phase 0's
/// allowlist must reject execution so the mutating handler never starts.
#[tokio::test(flavor = "multi_thread")]
async fn plain_message_phase0_blocks_mutating_tool_execution() {
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
    // Full allowlist in the factory must not leak into PlanPhase0 turns —
    // HeadlessTurnExecutor wraps plain chat with phase0_plan_tool_loop_config.
    let config_factory: Arc<dyn Fn() -> ToolLoopConfig + Send + Sync> =
        Arc::new(|| ToolLoopConfig {
            terminal_tools: Some(vec!["blocking_mutation".to_string()]),
            allowed_tools: Some(vec![
                "read_file".to_string(),
                "blocking_mutation".to_string(),
            ]),
            ..ToolLoopConfig::default()
        });
    let (runtime, _, _) = build_runtime_with_registry_and_config(
        "phase0_blocks_mutation",
        workdir.path().to_path_buf(),
        Vec::new(),
        None,
        registry,
        config_factory,
    )
    .await;

    runtime
        .submit_message("please mutate the workspace now".to_string())
        .await
        .expect("plain Phase 0 message must still admit");

    let deadline = std::time::Instant::now() + Duration::from_secs(5);
    while std::time::Instant::now() < deadline {
        if runtime.snapshot().await.status == CodeUiSessionStatus::Idle {
            break;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    let phase0_status = runtime.snapshot().await.status;
    assert!(
        matches!(
            phase0_status,
            CodeUiSessionStatus::Idle | CodeUiSessionStatus::Error
        ),
        "Phase 0 turn must settle after rejecting the mutating tool, got {phase0_status:?}",
    );

    // If Phase 0 failed to apply the allowlist, the handler would notify and
    // leave the turn blocked on `release`. Prove it never started.
    assert!(
        tokio::time::timeout(Duration::from_millis(50), started.notified())
            .await
            .is_err(),
        "Phase 0 must not execute blocking_mutation for plain browser chat",
    );
    let snapshot = runtime.snapshot().await;
    assert!(
        snapshot
            .tool_calls
            .iter()
            .all(|call| call.id != "phase0-blocked-mutation-1" || call.status != "completed"),
        "mutating tool must not complete under PlanPhase0: {:?}",
        snapshot.tool_calls,
    );
    let user = snapshot
        .transcript
        .iter()
        .find(|entry| {
            entry.kind == libra::internal::ai::web::code_ui::CodeUiTranscriptEntryKind::UserMessage
        })
        .expect("user transcript row");
    assert_eq!(
        user.metadata.get("webTurnMode").and_then(|v| v.as_str()),
        Some("PlanPhase0"),
    );
}

/// After Phase 0 risk selection + `submit_intent_draft`, the browser must see
/// a pending IntentSpec review gate (confirm/modify/cancel) rather than Idle.
#[tokio::test(flavor = "multi_thread")]
async fn plain_message_phase0_parks_intent_review_until_confirm() {
    let workdir = tempfile::tempdir().expect("tempdir for headless workdir");
    let canonical_workdir = initialize_phase1_test_checkout(workdir.path()).await;
    let storage = tempfile::tempdir().expect("tempdir for durable Phase 0 session storage");
    let store = Arc::new(SessionStore::from_storage_path(storage.path()));
    let state = SessionState::new(&canonical_workdir.to_string_lossy());
    let persistence =
        HeadlessSessionPersistence::new(store, state).expect("attach durable Phase 0 session");
    let (runtime, _, _) = build_runtime_with_persistence(
        "phase0_intent_review",
        canonical_workdir,
        Vec::new(),
        Some(persistence),
    )
    .await;

    runtime
        .submit_message("please draft an IntentSpec for README docs".to_string())
        .await
        .expect("plain Phase 0 message must admit");

    let risk_interaction_id = await_pending_interaction(
        &runtime,
        CodeUiInteractionKind::RequestUserInput,
        "Phase 0 must ask for risk_profile before drafting",
    )
    .await;
    runtime
        .respond_interaction(
            &risk_interaction_id,
            CodeUiInteractionResponse {
                selected_option: Some("Low".to_string()),
                ..Default::default()
            },
        )
        .await
        .expect("risk_profile Low must be accepted");

    let interaction_id = await_pending_interaction(
        &runtime,
        CodeUiInteractionKind::IntentReviewChoice,
        "Phase 0 submit_intent_draft must park IntentReviewChoice",
    )
    .await;
    assert_eq!(
        runtime.snapshot().await.status,
        CodeUiSessionStatus::AwaitingInteraction
    );

    runtime
        .respond_interaction(
            &interaction_id,
            CodeUiInteractionResponse {
                selected_option: Some("confirm".to_string()),
                ..Default::default()
            },
        )
        .await
        .expect("confirm must settle the IntentSpec review gate");

    let deadline = std::time::Instant::now() + Duration::from_secs(3);
    while std::time::Instant::now() < deadline {
        if runtime.snapshot().await.status == CodeUiSessionStatus::Idle {
            break;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    let snapshot = runtime.snapshot().await;
    assert_eq!(snapshot.status, CodeUiSessionStatus::Idle);
    assert!(
        snapshot
            .interactions
            .iter()
            .all(|interaction| interaction.id != interaction_id
                || interaction.status != CodeUiInteractionStatus::Pending),
        "confirmed IntentSpec review must leave the pending gate",
    );
}

/// Modify must arm the legacy-interaction-parity revise mode so the next plain message revises
/// the parked IntentSpec instead of silently abandoning the draft.
#[tokio::test(flavor = "multi_thread")]
async fn plain_message_phase0_modify_enters_revision_mode_for_next_turn() {
    let workdir = tempfile::tempdir().expect("tempdir for headless workdir");
    let canonical_workdir = initialize_phase1_test_checkout(workdir.path()).await;
    let storage = tempfile::tempdir().expect("tempdir for durable Phase 0 session storage");
    let store = Arc::new(SessionStore::from_storage_path(storage.path()));
    let state = SessionState::new(&canonical_workdir.to_string_lossy());
    let persistence =
        HeadlessSessionPersistence::new(store, state).expect("attach durable Phase 0 session");
    let (runtime, _, _) = build_runtime_with_persistence(
        "phase0_intent_review",
        canonical_workdir,
        Vec::new(),
        Some(persistence),
    )
    .await;

    runtime
        .submit_message("please draft an IntentSpec for README docs".to_string())
        .await
        .expect("plain Phase 0 message must admit");

    let risk_interaction_id = await_pending_interaction(
        &runtime,
        CodeUiInteractionKind::RequestUserInput,
        "Phase 0 must ask for risk_profile before drafting",
    )
    .await;
    runtime
        .respond_interaction(
            &risk_interaction_id,
            CodeUiInteractionResponse {
                selected_option: Some("Low".to_string()),
                ..Default::default()
            },
        )
        .await
        .expect("risk_profile Low must be accepted");

    let interaction_id = await_pending_interaction(
        &runtime,
        CodeUiInteractionKind::IntentReviewChoice,
        "Phase 0 submit_intent_draft must park IntentReviewChoice",
    )
    .await;

    runtime
        .respond_interaction(
            &interaction_id,
            CodeUiInteractionResponse {
                selected_option: Some("modify".to_string()),
                note: Some("keep docs only".to_string()),
                ..Default::default()
            },
        )
        .await
        .expect("modify must settle into revision mode");

    let deadline = std::time::Instant::now() + Duration::from_secs(3);
    while std::time::Instant::now() < deadline {
        if runtime.snapshot().await.status == CodeUiSessionStatus::Idle {
            break;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    let snapshot = runtime.snapshot().await;
    assert_eq!(snapshot.status, CodeUiSessionStatus::Idle);
    assert!(
        snapshot.transcript.iter().any(|entry| {
            entry.content.as_deref().is_some_and(|content| {
                content.contains("IntentSpec revise mode is active")
                    && content.contains("retained privately for the next Phase 0 revision prompt")
            })
        }),
        "modify must project generic private revise-mode help into the transcript",
    );
    assert!(snapshot.transcript.iter().all(|entry| {
        entry
            .content
            .as_deref()
            .is_none_or(|content| !content.contains("keep docs only"))
    }));

    runtime
        .submit_message("tighten scope to README only".to_string())
        .await
        .expect("revision plain message must admit as PlanPhase0");

    let revise_risk_id = await_pending_interaction(
        &runtime,
        CodeUiInteractionKind::RequestUserInput,
        "revision Phase 0 must ask for risk_profile again",
    )
    .await;
    runtime
        .respond_interaction(
            &revise_risk_id,
            CodeUiInteractionResponse {
                selected_option: Some("Low".to_string()),
                ..Default::default()
            },
        )
        .await
        .expect("revision risk_profile Low must be accepted");

    let _revised_review = await_pending_interaction(
        &runtime,
        CodeUiInteractionKind::IntentReviewChoice,
        "revision submit_intent_draft must park a new IntentReviewChoice",
    )
    .await;
}

/// Modify then process restart must restore revision mode so the next plain
/// message revises the same IntentSpec instead of drafting a fresh one.
#[tokio::test(flavor = "multi_thread")]
async fn resumed_runtime_restores_pending_intent_revision_mode() {
    let workdir = tempfile::tempdir().expect("tempdir for headless workdir");
    let storage = tempfile::tempdir().expect("tempdir for session storage");
    let store = Arc::new(SessionStore::from_storage_path(storage.path()));
    let mut state = SessionState::new(&workdir.path().to_string_lossy());
    let thread_id = state.id.clone();
    state.metadata.insert(
        "thread_id".to_string(),
        serde_json::json!(thread_id.clone()),
    );
    let persistence =
        HeadlessSessionPersistence::new(store.clone(), state.clone()).expect("attach workflow hub");
    let (runtime, _, _) = build_runtime_with_persistence(
        "phase0_intent_review",
        workdir.path().to_path_buf(),
        Vec::new(),
        Some(persistence),
    )
    .await;

    runtime
        .submit_message("please draft an IntentSpec for README docs".to_string())
        .await
        .expect("plain Phase 0 message must admit");

    let risk_interaction_id = await_pending_interaction(
        &runtime,
        CodeUiInteractionKind::RequestUserInput,
        "Phase 0 must ask for risk_profile before drafting",
    )
    .await;
    runtime
        .respond_interaction(
            &risk_interaction_id,
            CodeUiInteractionResponse {
                selected_option: Some("Low".to_string()),
                ..Default::default()
            },
        )
        .await
        .expect("risk_profile Low must be accepted");

    let interaction_id = await_pending_interaction(
        &runtime,
        CodeUiInteractionKind::IntentReviewChoice,
        "Phase 0 submit_intent_draft must park IntentReviewChoice",
    )
    .await;
    runtime
        .respond_interaction(
            &interaction_id,
            CodeUiInteractionResponse {
                selected_option: Some("modify".to_string()),
                note: Some("keep docs only".to_string()),
                ..Default::default()
            },
        )
        .await
        .expect("modify must arm durable revision mode");

    let deadline = std::time::Instant::now() + Duration::from_secs(3);
    while std::time::Instant::now() < deadline {
        if runtime.snapshot().await.status == CodeUiSessionStatus::Idle {
            break;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    assert!(
        store
            .session_root(&thread_id)
            .join("intents")
            .join("pending_revision.json")
            .is_file(),
        "modify must persist intents/pending_revision.json for resume",
    );

    drop(runtime);
    tokio::time::sleep(Duration::from_millis(50)).await;

    let restored_state = store
        .load(&thread_id)
        .expect("parked session must remain loadable");
    let persistence = HeadlessSessionPersistence::new(store.clone(), restored_state)
        .expect("attach workflow hub");
    let (restored, _, _) = build_runtime_with_persistence(
        "phase0_intent_review",
        workdir.path().to_path_buf(),
        Vec::new(),
        Some(persistence),
    )
    .await;

    assert!(
        restored.snapshot().await.transcript.iter().any(|entry| {
            entry.content.as_deref().is_some_and(|content| {
                content.contains("retained privately for the next Phase 0 revision prompt")
            })
        }),
        "resume must re-project revise-mode help",
    );
    assert!(restored.snapshot().await.transcript.iter().all(|entry| {
        entry
            .content
            .as_deref()
            .is_none_or(|content| !content.contains("keep docs only"))
    }));
    assert_raw_note_absent_from_workflow_and_sse(
        &SessionJsonlStore::new(store.session_root(&thread_id)),
        "keep docs only",
    );

    restored
        .submit_message("continue".to_string())
        .await
        .expect("restored revision follow-up must admit");

    let revise_risk_id = await_pending_interaction(
        &restored,
        CodeUiInteractionKind::RequestUserInput,
        "restored revision Phase 0 must ask for risk_profile",
    )
    .await;
    restored
        .respond_interaction(
            &revise_risk_id,
            CodeUiInteractionResponse {
                selected_option: Some("Low".to_string()),
                ..Default::default()
            },
        )
        .await
        .expect("restored revision risk_profile Low must be accepted");

    let _revised_review = await_pending_interaction(
        &restored,
        CodeUiInteractionKind::IntentReviewChoice,
        "restored revision must park a new IntentReviewChoice",
    )
    .await;
}

/// Crash/resume must rehydrate a parked IntentSpec review so confirm works
/// after process restart (Codex W3-03 P1: restore pending Phase 0 reviews).
#[tokio::test(flavor = "multi_thread")]
async fn resumed_runtime_restores_pending_intent_review_gate() {
    let workdir = tempfile::tempdir().expect("tempdir for headless workdir");
    let canonical_workdir = initialize_phase1_test_checkout(workdir.path()).await;
    let storage = tempfile::tempdir().expect("tempdir for session storage");
    let store = Arc::new(SessionStore::from_storage_path(storage.path()));
    let mut state = SessionState::new(&canonical_workdir.to_string_lossy());
    let thread_id = state.id.clone();
    state.metadata.insert(
        "thread_id".to_string(),
        serde_json::json!(thread_id.clone()),
    );
    let persistence =
        HeadlessSessionPersistence::new(store.clone(), state.clone()).expect("attach workflow hub");
    let (runtime, _, _) = build_runtime_with_persistence(
        "phase0_intent_review",
        canonical_workdir.clone(),
        Vec::new(),
        Some(persistence),
    )
    .await;

    runtime
        .submit_message("please draft an IntentSpec for README docs".to_string())
        .await
        .expect("plain Phase 0 message must admit");

    let risk_interaction_id = await_pending_interaction(
        &runtime,
        CodeUiInteractionKind::RequestUserInput,
        "Phase 0 must ask for risk_profile before drafting",
    )
    .await;
    runtime
        .respond_interaction(
            &risk_interaction_id,
            CodeUiInteractionResponse {
                selected_option: Some("Low".to_string()),
                ..Default::default()
            },
        )
        .await
        .expect("risk_profile Low must be accepted");

    let interaction_id = await_pending_interaction(
        &runtime,
        CodeUiInteractionKind::IntentReviewChoice,
        "Phase 0 submit_intent_draft must park IntentReviewChoice",
    )
    .await;

    // Simulate process exit without settling the review: drop the live runtime
    // after the durable IntentReviewRequested + pending snapshot are on disk.
    drop(runtime);
    tokio::time::sleep(Duration::from_millis(50)).await;

    let restored_state = store
        .load(&thread_id)
        .expect("parked session must remain loadable");
    let persistence = HeadlessSessionPersistence::new(store.clone(), restored_state)
        .expect("attach workflow hub");
    let (restored, _, _) = build_runtime_with_persistence(
        "phase0_intent_review",
        canonical_workdir,
        Vec::new(),
        Some(persistence),
    )
    .await;

    let snapshot = restored.snapshot().await;
    assert_eq!(
        snapshot.status,
        CodeUiSessionStatus::AwaitingInteraction,
        "resume must re-project AwaitingInteraction for an open IntentSpec review",
    );
    let restored_gate = snapshot
        .interactions
        .iter()
        .find(|interaction| {
            interaction.id == interaction_id
                && interaction.kind == CodeUiInteractionKind::IntentReviewChoice
                && interaction.status == CodeUiInteractionStatus::Pending
        })
        .expect("resume must keep the pending IntentReviewChoice gate visible");
    let intent_id = restored_gate
        .metadata
        .get("intentId")
        .and_then(|value| value.as_str())
        .expect("restored review must expose durable intentId");
    assert!(
        !intent_id.trim().is_empty(),
        "restored intentId must be non-empty"
    );
    let intent_spec = restored_gate
        .metadata
        .get("intentSpec")
        .and_then(|value| value.as_str())
        .expect("restored review must reload IntentSpec JSON for confirm/modify/cancel");
    assert!(
        intent_spec.contains('"') || intent_spec.contains('{'),
        "restored IntentSpec payload must be non-empty JSON text"
    );
    assert!(
        store
            .session_root(&thread_id)
            .join("intents")
            .join(format!("{intent_id}.json"))
            .is_file(),
        "durable intents/{{intent_id}}.json must remain after resume rebuild"
    );

    restored
        .respond_interaction(
            &interaction_id,
            CodeUiInteractionResponse {
                selected_option: Some("confirm".to_string()),
                ..Default::default()
            },
        )
        .await
        .expect("confirm must settle the restored IntentSpec review gate");

    let deadline = std::time::Instant::now() + Duration::from_secs(3);
    while std::time::Instant::now() < deadline {
        if restored.snapshot().await.status == CodeUiSessionStatus::Idle {
            break;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    assert_eq!(
        restored.snapshot().await.status,
        CodeUiSessionStatus::Idle,
        "restored IntentSpec confirm must reach Idle",
    );
}

/// Crash between IntentReviewRequested and pending-interaction snapshot must not
/// open a blind review gate when the durable IntentSpec file is missing — fence.
#[tokio::test(flavor = "multi_thread")]
async fn resumed_runtime_fences_intent_review_when_durable_spec_missing() {
    let workdir = tempfile::tempdir().expect("tempdir for headless workdir");
    let storage = tempfile::tempdir().expect("tempdir for session storage");
    let store = Arc::new(SessionStore::from_storage_path(storage.path()));
    let mut state = SessionState::new(&workdir.path().to_string_lossy());
    let thread_id = state.id.clone();
    state.metadata.insert(
        "thread_id".to_string(),
        serde_json::json!(thread_id.clone()),
    );
    let persistence =
        HeadlessSessionPersistence::new(store.clone(), state.clone()).expect("attach workflow hub");
    let (runtime, _, _) = build_runtime_with_persistence(
        "phase0_intent_review",
        workdir.path().to_path_buf(),
        Vec::new(),
        Some(persistence),
    )
    .await;

    runtime
        .submit_message("please draft an IntentSpec for README docs".to_string())
        .await
        .expect("plain Phase 0 message must admit");

    let risk_interaction_id = await_pending_interaction(
        &runtime,
        CodeUiInteractionKind::RequestUserInput,
        "Phase 0 must ask for risk_profile before drafting",
    )
    .await;
    runtime
        .respond_interaction(
            &risk_interaction_id,
            CodeUiInteractionResponse {
                selected_option: Some("Low".to_string()),
                ..Default::default()
            },
        )
        .await
        .expect("risk_profile Low must be accepted");

    let _interaction_id = await_pending_interaction(
        &runtime,
        CodeUiInteractionKind::IntentReviewChoice,
        "Phase 0 submit_intent_draft must park IntentReviewChoice",
    )
    .await;

    let intents_dir = store.session_root(&thread_id).join("intents");
    assert!(
        intents_dir.is_dir(),
        "park path must create session intents/ before review"
    );
    for entry in std::fs::read_dir(&intents_dir).expect("read intents dir") {
        let entry = entry.expect("intent dir entry");
        if entry.path().extension().and_then(|ext| ext.to_str()) == Some("json") {
            std::fs::remove_file(entry.path()).expect("delete durable IntentSpec to force fence");
        }
    }

    drop(runtime);
    tokio::time::sleep(Duration::from_millis(50)).await;

    let restored_state = store
        .load(&thread_id)
        .expect("parked session must remain loadable");
    let persistence = HeadlessSessionPersistence::new(store.clone(), restored_state)
        .expect("attach workflow hub");
    let (restored, _, _) = build_runtime_with_persistence(
        "phase0_intent_review",
        workdir.path().to_path_buf(),
        Vec::new(),
        Some(persistence),
    )
    .await;

    assert_eq!(
        restored.snapshot().await.status,
        CodeUiSessionStatus::IndeterminateSideEffect,
        "missing durable IntentSpec must fence resume instead of opening a blind review gate",
    );
}

/// Submitting an explicit direct-chat message (leading `/`) must produce an
/// assistant transcript entry that matches the fake provider's deterministic
/// response, with the snapshot returning to `Idle` once the turn settles.
/// Plain (non-`/`) messages default to Phase 0 plan routing after W3-03.
#[tokio::test(flavor = "multi_thread")]
async fn submit_message_streams_assistant_reply_into_snapshot() {
    let workdir = tempfile::tempdir().expect("tempdir for headless workdir");
    let (runtime, _, _) = build_runtime("basic_chat", workdir.path().to_path_buf()).await;

    runtime
        .submit_message("/hello headless".to_string())
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
        .submit_message("/runtime-owned turn".to_string())
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

    match runtime.cancel_turn().await {
        Ok(()) => {}
        Err(error) => {
            // The worker may complete after the active-state observation but
            // before this independent cancel request reaches admission. That
            // is a valid natural completion, not a failed cancellation path.
            let session_busy = error
                .downcast_ref::<CodeUiApiError>()
                .expect("a naturally completed turn must report typed SESSION_BUSY");
            assert_eq!(session_busy.status, 409);
            assert_eq!(session_busy.code, "SESSION_BUSY");
            assert!(
                runtime
                    .runtime_snapshot()
                    .await
                    .expect("worker snapshot after natural completion")
                    .active_turn_id
                    .is_none(),
                "SESSION_BUSY is valid here only after the observed turn completed"
            );
        }
    }
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
    let storage = tempfile::tempdir().expect("tempdir for session storage");
    let store = Arc::new(SessionStore::from_storage_path(storage.path()));
    let state = SessionState::new(&workdir.path().to_string_lossy());
    let session_id = state.id.clone();
    let persistence =
        HeadlessSessionPersistence::new(store.clone(), state).expect("attach workflow hub");
    let provider_entered = Arc::new(Notify::new());
    let runtime = build_pending_completion_runtime(
        workdir.path().to_path_buf(),
        persistence,
        Arc::clone(&provider_entered),
    )
    .await;

    // Construction may create the session root for Goal/command durability.
    // Break the durable event log *after* launch so submit's persist-before-gate
    // fails without a live transcript mutation (same pattern as approval tests).
    let session_root = store.session_root(&session_id);
    std::fs::create_dir_all(&session_root).expect("ensure session root exists");
    let events_path = session_root.join("events.jsonl");
    if events_path.is_file() {
        std::fs::remove_file(&events_path).expect("remove existing event log");
    } else if events_path.exists() {
        std::fs::remove_dir_all(&events_path).expect("remove unexpected events path");
    }
    std::fs::create_dir(&events_path)
        .expect("replace the durable event file with a directory to force append failure");

    let error = tokio::time::timeout(
        Duration::from_secs(3),
        runtime.submit_message("must not start".to_string()),
    )
    .await
    .expect("unpersistable admission must fail rather than wait indefinitely")
    .expect_err("an unpersistable browser turn must be rejected");
    let error_chain = format!("{error:#}");
    assert!(
        error_chain.contains("events.jsonl") && error_chain.contains("Is a directory"),
        "the error must identify the failed durable preflight: {error_chain}",
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
    match runtime.runtime_snapshot().await {
        Ok(worker) => assert!(
            worker.active_turn_id.is_none(),
            "an existing worker session must remain inactive after failed durable preflight: {worker:#?}",
        ),
        Err(RuntimeWorkerError::UnknownSession {
            session_id: unknown,
        }) => assert_eq!(
            unknown, session_id,
            "pre-worker failure may leave no SessionQueue, but must target this session",
        ),
        Err(other) => panic!("unexpected worker state after failed durable preflight: {other}"),
    }
    assert!(
        std::fs::read_dir(&events_path)
            .expect("failed append target remains inspectable")
            .next()
            .is_none(),
        "failed durable preflight must not leave a command-intent artifact",
    );
    assert!(
        tokio::time::timeout(Duration::from_millis(50), provider_entered.notified())
            .await
            .is_err(),
        "failed durable preflight must not invoke the provider",
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
    let persistence =
        HeadlessSessionPersistence::new(store.clone(), state).expect("attach workflow hub");
    let (runtime, _, _) = build_runtime_with_persistence(
        "basic_chat",
        workdir.path().to_path_buf(),
        Vec::new(),
        Some(persistence),
    )
    .await;

    runtime
        .submit_message("/persist this turn".to_string())
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
    let persistence =
        HeadlessSessionPersistence::new(store.clone(), state).expect("attach workflow hub");
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
            "/persist with stable id".to_string(),
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
            "/persist with stable id".to_string(),
            Some(command_id.clone()),
        )
        .await
        .expect("matching retry must acknowledge without error");

    let conflict = runtime
        .submit_message_with_command_id(
            "/different payload for same command id".to_string(),
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

/// A completed browser retry must not replay provider usage. This exercises the
/// HeadlessDirectTurnExecutor path that replaces the config's local identity
/// with the durable runtime command id, while the tool loop adds the
/// per-model-turn suffix used as the idempotency key. The deterministic fake
/// provider fixture supplies non-zero usage; this test runs only with
/// `--features test-provider`, as does the rest of this target.
#[tokio::test(flavor = "multi_thread")]
async fn durable_command_retry_does_not_double_count_executor_usage() {
    let workdir = tempfile::tempdir().expect("tempdir for headless workdir");
    let storage = tempfile::tempdir().expect("tempdir for session storage");
    let store = Arc::new(SessionStore::from_storage_path(storage.path()));
    let mut state = SessionState::new(&workdir.path().to_string_lossy());
    let thread_id = state.id.clone();
    state
        .metadata
        .insert("thread_id".to_string(), serde_json::json!(thread_id));
    let persistence = HeadlessSessionPersistence::new(store, state).expect("attach workflow hub");

    let conn = Database::connect("sqlite::memory:")
        .await
        .expect("connect usage sqlite");
    run_builtin_migrations(&conn)
        .await
        .expect("run usage migrations");
    let recorder = UsageRecorder::new(conn.clone());
    let usage_context = UsageContext {
        repo_id: Some("headless-usage-repo".to_string()),
        session_id: Some("headless-usage-session".to_string()),
        thread_id: Some("headless-usage-thread".to_string()),
        agent_run_id: None,
        run_id: Some("config-local-run-id".to_string()),
        turn_id: Some("config-local-turn-id".to_string()),
        event_id: Some("config-local-event-id".to_string()),
        provider: "fake".to_string(),
        model: "fake".to_string(),
        request_kind: "completion".to_string(),
        intent: Some("headless-retry-test".to_string()),
        agent_name: None,
    };
    let config_factory: Arc<dyn Fn() -> ToolLoopConfig + Send + Sync> =
        Arc::new(move || ToolLoopConfig {
            usage_recorder: Some(recorder.clone()),
            usage_context: Some(usage_context.clone()),
            ..ToolLoopConfig::default()
        });
    let registry = Arc::new(
        ToolRegistryBuilder::with_working_dir(workdir.path().to_path_buf())
            .hardening(ToolBoundaryRuntime::system(
                Uuid::new_v4(),
                Arc::new(TracingAuditSink),
            ))
            .build(),
    );
    let (runtime, _, _) = build_runtime_with_registry_and_config(
        "basic_chat",
        workdir.path().to_path_buf(),
        Vec::new(),
        Some(persistence),
        registry,
        config_factory,
    )
    .await;
    let command_id = "headless-usage-retry-1".to_string();

    runtime
        .submit_message_with_command_id(
            "/record provider usage".to_string(),
            Some(command_id.clone()),
        )
        .await
        .expect("first durable command should admit");

    let service = RuntimeUsageService::new(UsageRecorder::new(conn.clone()));
    let deadline = std::time::Instant::now() + Duration::from_secs(5);
    while std::time::Instant::now() < deadline {
        let totals = service
            .current_turn(&command_id, UsageQueryFilter::default())
            .await
            .expect("query headless turn usage");
        if totals.request_count == 1 {
            assert_eq!(totals.total_tokens, 10);
            break;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    let before_retry = service
        .current_turn(&command_id, UsageQueryFilter::default())
        .await
        .expect("query first command usage");
    assert_eq!(before_retry.request_count, 1);
    assert_eq!(before_retry.total_tokens, 10);

    runtime
        .submit_message_with_command_id(
            "/record provider usage".to_string(),
            Some(command_id.clone()),
        )
        .await
        .expect("completed retry should acknowledge without re-dispatch");

    let after_retry = service
        .current_turn(&command_id, UsageQueryFilter::default())
        .await
        .expect("query retried command usage");
    assert_eq!(after_retry.request_count, 1);
    assert_eq!(after_retry.total_tokens, 10);

    let rows = conn
        .query_all_raw(Statement::from_string(
            conn.get_database_backend(),
            format!(
                "SELECT turn_id, event_id FROM agent_usage_stats \
                 WHERE event_id = 'runtime-turn:{command_id}:model-turn:1'"
            ),
        ))
        .await
        .expect("read executor usage row");
    assert_eq!(rows.len(), 1, "retry must leave one provider usage row");
    assert_eq!(
        rows[0].try_get_by::<String, _>("turn_id").ok().as_deref(),
        Some(command_id.as_str()),
        "executor must replace config-local turn_id with durable command id"
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
    let persistence =
        HeadlessSessionPersistence::new(store.clone(), state).expect("attach workflow hub");
    let (runtime, _, _) = build_runtime_with_persistence(
        "delayed_chat",
        workdir.path().to_path_buf(),
        Vec::new(),
        Some(persistence),
    )
    .await;
    let command_id = "browser-cmd-inflight-1".to_string();

    runtime
        .submit_message_with_command_id("/slow".to_string(), Some(command_id.clone()))
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
        .submit_message_with_command_id("/slow".to_string(), Some(command_id.clone()))
        .await;
    assert!(
        matching.is_ok(),
        "same commandId + same text while in flight must be idempotent"
    );

    let conflict = runtime
        .submit_message_with_command_id(
            "/different slow text".to_string(),
            Some(command_id.clone()),
        )
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
        .submit_message("/please update the plan".to_string())
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
        .submit_message("/please draft an execution plan".to_string())
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
        .submit_message("/slow".to_string())
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
        .submit_message("/slow shutdown".to_string())
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
        .submit_message("/must not restart during shutdown".to_string())
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
        .submit_message("/slow repeated shutdown".to_string())
        .await
        .expect("submit must start a cancellable turn");

    let (first, second) = tokio::join!(runtime.shutdown(), runtime.shutdown());
    first.expect("the first shutdown caller should see clean completion");
    second.expect("the second shutdown caller must join the same clean completion");

    let snapshot = runtime.snapshot().await;
    assert_eq!(snapshot.status, CodeUiSessionStatus::Idle);
    assert!(snapshot.transcript.iter().any(|entry| {
        entry.kind == libra::internal::ai::web::code_ui::CodeUiTranscriptEntryKind::AssistantMessage
            && entry.status.as_deref() == Some("cancelled")
            && !entry.streaming
    }));
}

/// Shutdown can race with the durable `record_user_message` admission write.
/// The worker must cancel without waiting for that write, and admission must
/// later commit one cancelled projection instead of republishing `Thinking`.
#[tokio::test(flavor = "multi_thread")]
async fn shutdown_during_durable_admission_cannot_republish_thinking() {
    let workdir = tempfile::tempdir().expect("tempdir for headless workdir");
    let storage = tempfile::tempdir().expect("tempdir for session storage");
    let store = Arc::new(SessionStore::from_storage_path(storage.path()));
    let state = SessionState::new(&workdir.path().to_string_lossy());
    let admission_hook = HeadlessRecordUserMessageHook::new();
    let persistence = HeadlessSessionPersistence::new(store, state)
        .expect("attach workflow hub")
        .with_record_user_message_hook(admission_hook.clone());
    let (runtime, _, _) = build_runtime_with_persistence(
        "delayed_chat",
        workdir.path().to_path_buf(),
        Vec::new(),
        Some(persistence),
    )
    .await;

    let submit_runtime = Arc::clone(&runtime);
    let submit = tokio::spawn(async move {
        submit_runtime
            .submit_message("/slow durable-admission shutdown".to_string())
            .await
    });
    tokio::time::timeout(Duration::from_secs(3), admission_hook.wait_until_entered())
        .await
        .expect("submit must pause in the durable admission write");

    let shutdown_runtime = Arc::clone(&runtime);
    let shutdown = tokio::spawn(async move { shutdown_runtime.shutdown().await });
    tokio::time::timeout(Duration::from_secs(3), shutdown)
        .await
        .expect("shutdown must not wait for the paused durable admission write")
        .expect("shutdown task must not panic")
        .expect("shutdown must cancel the unstarted worker turn");

    admission_hook.release();
    tokio::time::timeout(Duration::from_secs(3), submit)
        .await
        .expect("submit must complete after the durable admission pause releases")
        .expect("submit task must not panic")
        .expect("durable admission must complete after the pause releases");

    let snapshot = runtime.snapshot().await;
    assert_eq!(snapshot.status, CodeUiSessionStatus::Idle);
    assert!(snapshot.transcript.iter().any(|entry| {
        entry.kind == libra::internal::ai::web::code_ui::CodeUiTranscriptEntryKind::AssistantMessage
            && entry.status.as_deref() == Some("cancelled")
            && !entry.streaming
    }));
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
    let persistence =
        HeadlessSessionPersistence::new(store.clone(), state).expect("attach workflow hub");
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
        .submit_message("/start blocking mutation".to_string())
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

/// Once a handler that may mutate has begun, cancellation must be accepted
/// cooperatively without hard-aborting its task. The handler stays alive until
/// its own completion, and the turn can then settle normally.
#[tokio::test(flavor = "multi_thread")]
async fn cancel_accepts_started_mutating_headless_tool_without_abort() {
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
        .submit_message("/start blocking mutation".to_string())
        .await
        .expect("the mutation fixture should start a headless turn");
    tokio::time::timeout(Duration::from_secs(3), started.notified())
        .await
        .expect("the blocking mutation handler should begin");

    runtime
        .cancel_turn()
        .await
        .expect("cancellation must be accepted cooperatively after a mutation starts");
    assert_eq!(
        runtime
            .runtime_snapshot()
            .await
            .expect("worker snapshot after cooperative cancellation")
            .interaction,
        InteractionState::Cancelling,
        "the accepted cancellation must be visible in the worker-owned lifecycle before the mutation completes",
    );
    let second_submit = runtime
        .submit_message("/must wait for mutation".to_string())
        .await
        .expect_err("the still-running mutation must retain the turn slot");
    let session_busy = second_submit
        .downcast_ref::<CodeUiApiError>()
        .expect("a started mutation must retain the typed SESSION_BUSY wire error");
    assert_eq!(session_busy.status, 409);
    assert_eq!(session_busy.code, "SESSION_BUSY");

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
    assert!(
        snapshot.transcript.iter().any(|entry| {
            entry.kind
                == libra::internal::ai::web::code_ui::CodeUiTranscriptEntryKind::AssistantMessage
                && entry.status.as_deref() == Some("completed")
                && entry
                    .content
                    .as_deref()
                    .is_some_and(|content| content.contains("mutation completed"))
        }),
        "the preserved mutating tool must publish its completed assistant result; snapshot={snapshot:#?}",
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
            // The Code UI's live assistant row carries `status: "thinking"`
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

/// `respond_interaction` with no active AgentRuntime turn fail-closes (W2-05
/// removed idle private pending maps). Unknown-id delivery requires a live turn.
#[tokio::test(flavor = "multi_thread")]
async fn respond_interaction_unknown_id() {
    let workdir = tempfile::tempdir().expect("tempdir for headless workdir");
    let (runtime, _, _) = build_runtime("basic_chat", workdir.path().to_path_buf()).await;

    let result = runtime
        .respond_interaction("ignored", CodeUiInteractionResponse::default())
        .await;
    let error = result.expect_err("idle respond_interaction must fail closed");
    let error = assert_code_ui_api_error(&error, 409, "INTERACTION_NOT_ACTIVE");
    assert_eq!(error.message, "interaction 'ignored' is not pending");
}

#[tokio::test(flavor = "multi_thread")]
async fn user_input_checkpoint_precedes_continuation_and_failure_keeps_gate_retryable() {
    let checkpoint = HeadlessRecordUserMessageHook::new();
    let fixture = interaction_checkpoint_fixture(checkpoint.clone(), "user-input").await;
    let interaction_id = "checkpoint-user-input".to_string();
    let (response_tx, mut response_rx) = tokio::sync::oneshot::channel::<UserInputResponse>();
    fixture
        .user_input_tx
        .send(UserInputRequest {
            call_id: interaction_id.clone(),
            questions: vec![UserInputQuestion {
                id: "answer".to_string(),
                header: "Answer".to_string(),
                question: "Continue after the durable checkpoint?".to_string(),
                is_other: false,
                is_secret: false,
                options: None,
            }],
            response_tx,
        })
        .expect("user-input request reaches the headless listener");
    assert_eq!(
        await_pending_interaction(
            &fixture.runtime,
            CodeUiInteractionKind::RequestUserInput,
            "checkpoint user-input gate did not become pending",
        )
        .await,
        interaction_id
    );

    fixture
        .goal_store
        .fail_next_pending_interaction_checkpoint_for_test();
    let error = fixture
        .runtime
        .respond_interaction(
            &interaction_id,
            CodeUiInteractionResponse {
                selected_option: Some("continue".to_string()),
                ..Default::default()
            },
        )
        .await
        .expect_err("checkpoint failure must reject the browser acknowledgement");
    assert!(
        error
            .to_string()
            .contains("checkpoint interaction responses"),
        "checkpoint failure must stay typed and actionable, got {error:#}"
    );
    assert!(
        tokio::time::timeout(Duration::from_millis(50), &mut response_rx)
            .await
            .is_err(),
        "a failed checkpoint must not release the user-input continuation"
    );
    assert_eq!(
        fixture
            .runtime
            .runtime_snapshot()
            .await
            .expect("worker snapshot after checkpoint failure")
            .interaction,
        InteractionState::AwaitingUserInput {
            interaction_id: interaction_id.clone(),
        },
        "the exact gate must remain retryable after a pre-delivery failure"
    );
    let web_snapshot = fixture.runtime.snapshot().await;
    assert_ne!(
        web_snapshot.status,
        CodeUiSessionStatus::IndeterminateSideEffect,
        "Headless Web admission must not fence away the worker-owned continuation"
    );
    assert!(web_snapshot.interactions.iter().any(|interaction| {
        interaction.id == interaction_id && interaction.status == CodeUiInteractionStatus::Pending
    }));
    assert!(
        fixture
            .goal_store
            .load_code_workflow_replay()
            .unwrap()
            .events
            .iter()
            .all(|event| !matches!(
                &event.event,
                CodeWorkflowEventKind::InteractionResolved {
                    interaction_id: resolved_id,
                    command: Some(command),
                    ..
                } if resolved_id == &interaction_id && command.command_id == fixture.command_id
            ))
    );

    let retry_runtime = Arc::clone(&fixture.runtime);
    let retry_interaction_id = interaction_id.clone();
    let retry = tokio::spawn(async move {
        retry_runtime
            .respond_interaction(
                &retry_interaction_id,
                CodeUiInteractionResponse {
                    selected_option: Some("continue".to_string()),
                    ..Default::default()
                },
            )
            .await
    });
    checkpoint.wait_until_entered().await;
    assert!(
        tokio::time::timeout(Duration::from_millis(50), &mut response_rx)
            .await
            .is_err(),
        "even a successful checkpoint must fsync before releasing the continuation"
    );
    let replay = fixture.goal_store.load_code_workflow_replay().unwrap();
    assert_eq!(
        replay
            .events
            .iter()
            .filter(|event| matches!(
                &event.event,
                CodeWorkflowEventKind::InteractionResolved {
                    interaction_id: resolved_id,
                    resolution,
                    command: Some(command),
                    prior_interaction_resolutions,
                    ..
                } if resolved_id == &interaction_id
                    && resolution == "answered"
                    && command.command_id == fixture.command_id
                    && prior_interaction_resolutions.is_empty()
            ))
            .count(),
        1,
        "the continuation pause must observe one durable command-bound checkpoint"
    );

    checkpoint.release();
    retry
        .await
        .expect("retry task does not panic")
        .expect("retry succeeds after the durable checkpoint");
    let response = response_rx
        .await
        .expect("the checkpointed user-input continuation is released");
    assert_eq!(
        response.answers.get("answer").map(|answer| &answer.answers),
        Some(&vec!["continue".to_string()])
    );
    fixture
        .runtime
        .cancel_turn()
        .await
        .expect("checkpoint-ordering turn cancels cooperatively");
}

#[tokio::test(flavor = "multi_thread")]
async fn exec_approval_checkpoint_precedes_tool_release_and_failure_keeps_gate_retryable() {
    let checkpoint = HeadlessRecordUserMessageHook::new();
    let fixture = interaction_checkpoint_fixture(checkpoint.clone(), "exec-approval").await;
    let interaction_id = "checkpoint-exec-approval".to_string();
    let (response_tx, mut response_rx) = tokio::sync::oneshot::channel();
    fixture
        .exec_approval_tx
        .send(ExecApprovalRequest {
            call_id: interaction_id.clone(),
            command: "cargo check".to_string(),
            cwd: fixture._workdir.path().to_path_buf(),
            reason: Some("prove approval checkpoint ordering".to_string()),
            is_retry: false,
            sandbox_label: "workspace-write".to_string(),
            network_access: NetworkAccess::Denied,
            writable_roots: Vec::new(),
            cache_disabled_reason: None,
            response_tx,
        })
        .expect("exec approval reaches the headless listener");
    assert_eq!(
        await_pending_interaction(
            &fixture.runtime,
            CodeUiInteractionKind::Approval,
            "checkpoint exec-approval gate did not become pending",
        )
        .await,
        interaction_id
    );

    fixture
        .goal_store
        .fail_next_pending_interaction_checkpoint_for_test();
    let approval = CodeUiInteractionResponse {
        selected_option: Some("approve".to_string()),
        ..Default::default()
    };
    let error = fixture
        .runtime
        .respond_interaction(&interaction_id, approval.clone())
        .await
        .expect_err("checkpoint failure must not approve tool execution");
    assert!(
        error
            .to_string()
            .contains("checkpoint interaction responses")
    );
    assert!(
        tokio::time::timeout(Duration::from_millis(50), &mut response_rx)
            .await
            .is_err(),
        "checkpoint failure must leave the approval receiver blocked"
    );
    assert_eq!(
        fixture
            .runtime
            .runtime_snapshot()
            .await
            .expect("worker snapshot after approval checkpoint failure")
            .interaction,
        InteractionState::AwaitingToolApproval {
            interaction_id: interaction_id.clone(),
            tool_name: "shell".to_string(),
        }
    );
    assert!(
        fixture
            .runtime
            .snapshot()
            .await
            .interactions
            .iter()
            .any(|item| {
                item.id == interaction_id && item.status == CodeUiInteractionStatus::Pending
            })
    );

    let retry_runtime = Arc::clone(&fixture.runtime);
    let retry_interaction_id = interaction_id.clone();
    let retry = tokio::spawn(async move {
        retry_runtime
            .respond_interaction(&retry_interaction_id, approval)
            .await
    });
    checkpoint.wait_until_entered().await;
    assert!(
        tokio::time::timeout(Duration::from_millis(50), &mut response_rx)
            .await
            .is_err(),
        "the tool decision must remain blocked after checkpoint fsync but before release"
    );
    let replay = fixture.goal_store.load_code_workflow_replay().unwrap();
    assert_eq!(
        replay
            .events
            .iter()
            .filter(|event| matches!(
                &event.event,
                CodeWorkflowEventKind::InteractionResolved {
                    interaction_id: resolved_id,
                    resolution,
                    command: Some(command),
                    prior_interaction_resolutions,
                    ..
                } if resolved_id == &interaction_id
                    && resolution == "approved"
                    && command.command_id == fixture.command_id
                    && prior_interaction_resolutions.is_empty()
            ))
            .count(),
        1
    );

    checkpoint.release();
    retry
        .await
        .expect("approval retry task does not panic")
        .expect("approval retry succeeds after checkpoint fsync");
    assert_eq!(
        response_rx
            .await
            .expect("durably checkpointed approval releases the tool continuation"),
        libra::internal::ai::sandbox::ReviewDecision::Approved
    );
    fixture
        .runtime
        .cancel_turn()
        .await
        .expect("approval checkpoint-ordering turn cancels cooperatively");
}

#[tokio::test(flavor = "multi_thread")]
async fn request_user_input_request_is_reflected_and_immediately_responded_to() {
    let workdir = tempfile::tempdir().expect("tempdir for headless workdir");
    let (runtime, user_input_tx, _) =
        build_runtime("delayed_chat", workdir.path().to_path_buf()).await;

    runtime
        .submit_message("/slow turn with input".to_string())
        .await
        .expect("delayed turn starts");
    await_worker_running(&runtime).await;

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

    let pending = runtime
        .snapshot()
        .await
        .interactions
        .into_iter()
        .find(|interaction| interaction.id == interaction_id)
        .expect("pending request_user_input interaction");
    let questions = pending
        .metadata
        .get("questions")
        .and_then(|value| value.as_array())
        .expect("projected questions array");
    assert_eq!(questions.len(), 1);
    assert_eq!(questions[0]["id"], question_id);
    assert_eq!(questions[0]["header"], "Approve");
    assert_eq!(questions[0]["prompt"], "Choose approach");
    assert_eq!(questions[0]["isOther"], false);
    assert_eq!(questions[0]["isSecret"], false);
    assert_eq!(questions[0]["kind"], "text");

    // Reply in the same scheduler turn that first observes the projection.
    // This is the registration-race success path: callers must not need to
    // sleep and retry after the browser has been told an interaction is ready.
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

    runtime
        .cancel_turn()
        .await
        .expect("cancelling the delayed turn is cooperative");
}

/// A stale id can look identical to the narrow registration race at the
/// AgentRuntime boundary. The adapter may retry that one error briefly, but
/// it must return a typed failure within the documented bounded window instead
/// of keeping a browser request open forever.
#[tokio::test(flavor = "multi_thread")]
async fn stale_interaction_id_exhausts_bounded_registration_retry() {
    let workdir = tempfile::tempdir().expect("tempdir for headless workdir");
    let (runtime, user_input_tx, _) =
        build_runtime("delayed_chat", workdir.path().to_path_buf()).await;

    runtime
        .submit_message("/slow turn with stale interaction response".to_string())
        .await
        .expect("delayed turn starts");
    await_worker_running(&runtime).await;

    let live_interaction_id = "registration-window-live".to_string();
    let (response_tx, _response_rx) = tokio::sync::oneshot::channel::<UserInputResponse>();
    user_input_tx
        .send(UserInputRequest {
            call_id: live_interaction_id.clone(),
            questions: vec![UserInputQuestion {
                id: "answer".to_string(),
                header: "Answer".to_string(),
                question: "Provide an answer".to_string(),
                is_other: false,
                is_secret: false,
                options: None,
            }],
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
                interaction.id == live_interaction_id
                    && interaction.status == CodeUiInteractionStatus::Pending
            })
        {
            break;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    assert!(
        runtime
            .snapshot()
            .await
            .interactions
            .iter()
            .any(|interaction| {
                interaction.id == live_interaction_id
                    && interaction.status == CodeUiInteractionStatus::Pending
            }),
        "a live interaction is required to distinguish a stale id from idle delivery",
    );

    let error = tokio::time::timeout(
        Duration::from_secs(2),
        runtime.respond_interaction(
            "registration-window-stale",
            CodeUiInteractionResponse::default(),
        ),
    )
    .await
    .expect("stale interaction delivery must remain bounded")
    .expect_err("stale interaction ids must fail closed after the retry window");
    assert!(
        error.to_string().contains("not pending"),
        "stale delivery must retain AgentRuntime's stale-delivery error, got {error:#}",
    );

    runtime
        .cancel_turn()
        .await
        .expect("cancelling the delayed turn is cooperative");
}

/// A multi-question `request_user_input` response must be complete and keyed
/// only by the questions the tool requested.  In particular, do not silently
/// deliver the first answer and discard the rest: that would let the tool loop
/// proceed with an incomplete human decision.
#[tokio::test(flavor = "multi_thread")]
async fn request_user_input_validates_and_delivers_all_requested_answers() {
    let workdir = tempfile::tempdir().expect("tempdir for headless workdir");
    let (runtime, user_input_tx, _) =
        build_runtime("delayed_chat", workdir.path().to_path_buf()).await;

    runtime
        .submit_message("/slow turn with input".to_string())
        .await
        .expect("delayed turn starts");
    await_worker_running(&runtime).await;

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
    let incomplete = assert_code_ui_api_error(&incomplete, 400, "INVALID_QUERY_PARAM");
    assert_eq!(
        incomplete.message,
        "selectedOption is invalid for this pending interaction"
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

    runtime
        .cancel_turn()
        .await
        .expect("cancelling the delayed turn is cooperative");
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
        .submit_message("/slow turn with input".to_string())
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
        .submit_message("/slow turn with input".to_string())
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
        .submit_message("/slow turn with approval".to_string())
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
    let invalid = assert_code_ui_api_error(&invalid, 400, "INVALID_QUERY_PARAM");
    assert_eq!(
        invalid.message,
        "selectedOption is invalid for this pending interaction"
    );
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
        build_runtime("delayed_chat", workdir.path().to_path_buf()).await;

    runtime
        .submit_message("/slow turn with approval".to_string())
        .await
        .expect("delayed turn starts");
    await_worker_running(&runtime).await;

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
    let invalid_response = assert_code_ui_api_error(&invalid_response, 400, "INVALID_QUERY_PARAM");
    assert_eq!(
        invalid_response.message,
        "selectedOption is invalid for this pending interaction"
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

    runtime
        .cancel_turn()
        .await
        .expect("cancelling the delayed turn is cooperative");
}

/// W2-05: an exec approval without an active AgentRuntime turn must fail
/// closed immediately (deny + drop projection). There is no adapter-private
/// pending map for idle cancellation to drain.
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
            reason: Some("verify idle approval fails closed without a runtime turn".to_string()),
            is_retry: false,
            sandbox_label: "workspace-write".to_string(),
            network_access: NetworkAccess::Denied,
            writable_roots: Vec::new(),
            cache_disabled_reason: None,
            response_tx,
        })
        .expect("exec approval request should enqueue in runtime");

    let decision = tokio::time::timeout(Duration::from_secs(2), response_rx)
        .await
        .expect("idle approval must resolve without waiting for cancel_turn")
        .expect("fail-closed path sends Denied rather than dropping the sender");
    assert_eq!(
        decision,
        libra::internal::ai::sandbox::ReviewDecision::Denied,
        "approvals without a live runtime turn are denied fail-closed"
    );
    let deadline = std::time::Instant::now() + Duration::from_secs(2);
    while std::time::Instant::now() < deadline {
        if runtime
            .snapshot()
            .await
            .interactions
            .iter()
            .all(|interaction| interaction.id != interaction_id)
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
        "fail-closed idle approval must be removed from the browser projection",
    );
}

/// A browser approval is a side-effect boundary. The response must reach
/// durable storage before it reaches the tool loop; a failed checkpoint keeps
/// the same gate pending so the browser can retry after storage is repaired.
#[tokio::test(flavor = "multi_thread")]
async fn unpersistable_approval_response_does_not_release_the_tool_loop() {
    let workdir = tempfile::tempdir().expect("tempdir for headless workdir");
    let storage = tempfile::tempdir().expect("tempdir for session storage");
    let store = Arc::new(SessionStore::from_storage_path(storage.path()));
    let state = SessionState::new(&workdir.path().to_string_lossy());
    let session_id = state.id.clone();
    let persistence =
        HeadlessSessionPersistence::new(store.clone(), state).expect("attach workflow hub");
    let (runtime, _, exec_approval_tx) = build_runtime_with_persistence(
        "delayed_chat",
        workdir.path().to_path_buf(),
        Vec::new(),
        Some(persistence),
    )
    .await;

    runtime
        .submit_message("/slow turn with approval".to_string())
        .await
        .expect("delayed turn starts");
    await_worker_running(&runtime).await;

    let interaction_id = "persisted-exec-approval".to_string();
    let (response_tx, mut response_rx) = tokio::sync::oneshot::channel();
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
    let Some(RuntimeWorkerError::DurabilityFailure(reason)) =
        error.downcast_ref::<RuntimeWorkerError>()
    else {
        panic!("checkpoint failure must retain its typed worker error: {error:#}");
    };
    assert!(
        reason.contains("could not checkpoint interaction responses"),
        "the typed durability failure must identify the pre-delivery checkpoint: {reason}",
    );
    let web_snapshot = runtime.snapshot().await;
    assert_eq!(
        web_snapshot.status,
        CodeUiSessionStatus::AwaitingInteraction,
        "a pre-delivery checkpoint failure must keep the exact gate retryable",
    );
    assert!(web_snapshot.interactions.iter().any(|interaction| {
        interaction.id == interaction_id && interaction.status == CodeUiInteractionStatus::Pending
    }));
    let worker_snapshot = runtime
        .runtime_snapshot()
        .await
        .expect("worker snapshot after failed approval checkpoint");
    assert_eq!(
        worker_snapshot.interaction,
        InteractionState::AwaitingToolApproval {
            interaction_id: interaction_id.clone(),
            tool_name: "shell".to_string(),
        },
        "the worker must retain ownership of the retryable approval gate",
    );
    assert!(
        tokio::time::timeout(Duration::from_millis(200), &mut response_rx)
            .await
            .is_err(),
        "the tool loop must remain blocked until a later response is durably checkpointed",
    );
    let submit_error = runtime
        .submit_message("/blocked after failed approval persistence".to_string())
        .await
        .expect_err("the pending approval gate must continue to serialize the session");
    assert_code_ui_api_error(&submit_error, 409, "SESSION_BUSY");
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
    let persistence =
        HeadlessSessionPersistence::new(store.clone(), state).expect("attach workflow hub");
    // brief_delayed_chat keeps a live turn long enough to register approval,
    // then completes quickly so post-terminal InteractionResolved is visible.
    let (runtime, _, exec_approval_tx) = build_runtime_with_persistence(
        "brief_delayed_chat",
        workdir.path().to_path_buf(),
        Vec::new(),
        Some(persistence),
    )
    .await;

    runtime
        .submit_message("/slow turn with approval".to_string())
        .await
        .expect("delayed turn starts");
    await_worker_running(&runtime).await;

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

    // InteractionResolved is appended with the terminal command outcome after
    // the live turn finishes (persist_interaction_resolved_after_terminal).
    let deadline = std::time::Instant::now() + Duration::from_secs(15);
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
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    assert!(
        runtime
            .runtime_snapshot()
            .await
            .expect("worker snapshot")
            .active_turn_id
            .is_none(),
        "delayed turn must finish so InteractionResolved can be persisted",
    );

    let replay = SessionJsonlStore::new(store.session_root(&session_id))
        .load_code_workflow_replay()
        .expect("durable interaction audit event can be replayed");
    let saw_resolution = replay.events.iter().any(|event| match &event.event {
        CodeWorkflowEventKind::InteractionResolved {
            interaction_id: event_interaction_id,
            resolution,
            ..
        }
        | CodeWorkflowEventKind::CommandTerminalSuccessWithInteractionResolved {
            interaction_id: event_interaction_id,
            resolution,
            ..
        } => event_interaction_id == &interaction_id && resolution == "approved",
        _ => false,
    });
    assert!(
        saw_resolution,
        "approval delivery must have a durable interaction-resolution audit event; events={:?}",
        replay
            .events
            .iter()
            .map(|event| &event.event)
            .collect::<Vec<_>>(),
    );
}

/// A terminal executor result that races a failed approval checkpoint must be
/// cached behind the still-pending worker gate instead of silently consuming
/// the unpersisted response. This out-of-band approval fixture does not model
/// the provider awaiting that sender, so it deliberately makes no assertion
/// about the Web slot after the provider future itself completes.
#[tokio::test(flavor = "multi_thread")]
async fn late_executor_completion_waits_for_retryable_approval_checkpoint() {
    let workdir = tempfile::tempdir().expect("tempdir for headless workdir");
    let storage = tempfile::tempdir().expect("tempdir for session storage");
    let store = Arc::new(SessionStore::from_storage_path(storage.path()));
    let state = SessionState::new(&workdir.path().to_string_lossy());
    let persistence =
        HeadlessSessionPersistence::new(store.clone(), state).expect("attach workflow hub");
    let goal_store = persistence.goal_event_store();
    let (runtime, _, exec_approval_tx) = build_runtime_with_persistence(
        "brief_delayed_chat",
        workdir.path().to_path_buf(),
        Vec::new(),
        Some(persistence),
    )
    .await;

    runtime
        .submit_message("/slow interaction persistence failure".to_string())
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
    let (response_tx, mut response_rx) = tokio::sync::oneshot::channel();
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
    let registered = runtime
        .runtime_snapshot()
        .await
        .expect("worker snapshot after approval registration");
    assert_eq!(
        registered.interaction,
        InteractionState::AwaitingToolApproval {
            interaction_id: interaction_id.clone(),
            tool_name: "shell".to_string(),
        },
        "the live approval must be registered before its persistence failure is induced",
    );
    let runtime_turn_id = registered
        .active_turn_id
        .expect("registered approval must retain its active runtime turn");

    goal_store.fail_next_pending_interaction_checkpoint_for_test();

    let error = runtime
        .respond_interaction(
            &interaction_id,
            CodeUiInteractionResponse {
                selected_option: Some("approve".to_string()),
                ..Default::default()
            },
        )
        .await
        .expect_err("the injected approval checkpoint failure must reject its acknowledgement");
    let Some(RuntimeWorkerError::DurabilityFailure(reason)) =
        error.downcast_ref::<RuntimeWorkerError>()
    else {
        panic!("checkpoint failure must retain its typed worker error: {error:#}");
    };
    assert!(
        reason.contains("could not checkpoint interaction responses"),
        "the typed durability failure must identify the pre-delivery checkpoint: {reason}",
    );
    assert!(
        tokio::time::timeout(Duration::from_millis(200), &mut response_rx)
            .await
            .is_err(),
        "the original tool loop must remain blocked after an unpersisted approval",
    );

    // The brief executor completes after one second. Its result must remain
    // cached behind the approval instead of terminalizing the command or
    // dropping the only live continuation.
    tokio::time::sleep(Duration::from_millis(1_300)).await;
    let waiting = runtime
        .runtime_snapshot()
        .await
        .expect("worker snapshot after the executor result raced the pending gate");
    assert_eq!(
        waiting.active_turn_id.as_deref(),
        Some(runtime_turn_id.as_str()),
        "the cached executor result must not release the serialized runtime owner",
    );
    assert_eq!(
        waiting.interaction,
        InteractionState::AwaitingToolApproval {
            interaction_id: interaction_id.clone(),
            tool_name: "shell".to_string(),
        },
    );
    assert!(
        tokio::time::timeout(Duration::from_millis(50), &mut response_rx)
            .await
            .is_err(),
        "a cached terminal result must not manufacture an approval decision",
    );
    let before_retry = goal_store
        .load_code_workflow_replay()
        .expect("read workflow before retrying the approval checkpoint");
    assert!(before_retry.events.iter().all(|event| !matches!(
        &event.event,
        CodeWorkflowEventKind::CommandTerminalSuccess { command, .. }
            | CodeWorkflowEventKind::CommandTerminalSuccessWithInteractionResolved {
                command,
                ..
            }
            | CodeWorkflowEventKind::CommandTerminalFailure { command, .. }
            | CodeWorkflowEventKind::CommandIndeterminateSideEffect { command, .. }
                if command.command_id == runtime_turn_id
    )));
}
