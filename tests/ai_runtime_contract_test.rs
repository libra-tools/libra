//! Wave 1A runtime contract tests.
//!
//! Pin the `TaskExecutor` trait contract so any provider that implements
//! `CompletionModel` can be plugged into the runtime by wrapping it in a thin
//! adapter. Verifies the runtime can build a task prompt, dispatch through a
//! `TaskExecutor`, and surface the response back as a `TaskExecutionResult`.
//!
//! **Layer:** L1 — uses `MockCompletionModel`, no external dependencies.

mod helpers;

use std::path::PathBuf;

use async_trait::async_trait;
use helpers::mock_completion_model::MockCompletionModel;
use libra::internal::ai::{
    completion::{AssistantContent, CompletionModel, CompletionRequest},
    runtime::{
        Runtime, RuntimeConfig,
        contracts::{
            ApprovalMediationState, TaskExecutionContext, TaskExecutionError, TaskExecutionResult,
            TaskExecutionStatus, TaskExecutor,
        },
    },
};
use uuid::Uuid;

/// W2-06: Code's runtime control path must consume the A0-07 projection and
/// curated registry directly. This pins both the source boundary (no second
/// skill discovery store) and the behavior of search/activation.
#[test]
fn skill_search_activation_uses_a0_projection() {
    use libra::internal::ai::{
        observed_agents::{
            AgentKind, SkillEvent, SkillEventProjection,
            capability::{SkillEventSignal, SkillEventSource, SkillEventType, SkillRef},
        },
        runtime::{CodeSkillActivation, CodeSkillSearch, ExecutionControlService},
    };

    let service = ExecutionControlService::new("contract-session", None, None)
        .expect("in-memory runtime control service");
    let mut projection = SkillEventProjection::new();
    projection.ingest(
        "contract-session",
        Some("checkpoint-1"),
        "codex",
        vec![SkillEvent {
            id: "turn-1:/review".to_string(),
            event_type: SkillEventType::PromptInvocation,
            skill: SkillRef {
                name: "/review".to_string(),
            },
            source: SkillEventSource {
                agent: "codex".to_string(),
                signal: SkillEventSignal::InputSlashCommand,
                confidence: 1.0,
            },
            turn_id: "turn-1".to_string(),
            timestamp: "2026-07-15T00:00:00Z".to_string(),
            transcript_anchor: None,
            native: false,
            collapse: false,
        }],
    );
    let matched = service.skill_search(
        &projection,
        &CodeSkillSearch {
            provider: Some("codex".to_string()),
            skill: Some("/review".to_string()),
            ..Default::default()
        },
    );
    assert_eq!(matched.len(), 1);
    assert_eq!(matched[0].event.skill.name, "/review");
    service
        .skill_activate(&CodeSkillActivation {
            provider: AgentKind::Codex.as_cli_slug().to_string(),
            name: "/review".to_string(),
        })
        .expect("activation must use the curated Codex A0-07 registry");

    let control_source = include_str!("../src/internal/ai/runtime/execution_control.rs");
    let projection_source = include_str!("../src/internal/ai/observed_agents/skill_projection.rs");
    let extract_source = include_str!("../src/internal/ai/observed_agents/extract.rs");
    assert!(control_source.contains("SkillEventProjection"));
    assert!(control_source.contains("discover_skills"));
    assert!(projection_source.contains("skill_registry_for"));
    assert!(extract_source.contains("pub fn skill_registry_for"));
    assert!(!control_source.contains("SkillDispatcher"));
}

/// Generic adapter that turns any `CompletionModel` into a `TaskExecutor`.
///
/// Demonstrates the wiring an integrator would write to plug a custom provider into
/// the runtime: forward the prompt messages, capture the first text response as the
/// summary, fabricate a `run_id` if one was not supplied, and report
/// `TaskExecutionStatus::Completed`.
#[derive(Clone)]
struct CompletionBackedTaskExecutor<M> {
    model: M,
}

#[async_trait]
impl<M> TaskExecutor for CompletionBackedTaskExecutor<M>
where
    M: CompletionModel + Clone + Send + Sync,
{
    async fn execute_task_attempt(
        &self,
        context: TaskExecutionContext,
    ) -> Result<TaskExecutionResult, TaskExecutionError> {
        let response = self
            .model
            .completion(CompletionRequest::new(
                context
                    .prompt
                    .messages
                    .into_iter()
                    .map(Into::into)
                    .collect(),
            ))
            .await
            .map_err(|err| TaskExecutionError::Provider(err.to_string()))?;
        let summary = response.content.first().and_then(|content| match content {
            AssistantContent::Text(text) => Some(text.text.clone()),
            AssistantContent::ToolCall(_) => None,
        });

        Ok(TaskExecutionResult {
            task_id: context.task_id,
            run_id: context.run_id.unwrap_or_else(Uuid::new_v4),
            status: TaskExecutionStatus::Completed,
            evidence: vec![],
            summary,
        })
    }
}

/// Scenario: build the runtime's task prompt with a fixture provider/model pair,
/// dispatch a single attempt through `CompletionBackedTaskExecutor` backed by
/// `MockCompletionModel::text("attempt complete")`, and assert the result preserves
/// the supplied `task_id`, marks the attempt completed, and surfaces the model's
/// text as the summary. Acts as the contract pin proving the runtime actually
/// integrates a generic provider via the `TaskExecutor` trait alone.
#[tokio::test]
async fn generic_provider_can_execute_through_task_executor_contract() {
    let runtime = Runtime::new(RuntimeConfig {
        principal: "contract-test".into(),
    });
    let prompt = runtime
        .task_prompt_builder("mock", "scripted")
        .task("write tests", "prove the runtime contract")
        .build();
    let task_id = Uuid::new_v4();
    let executor = CompletionBackedTaskExecutor {
        model: MockCompletionModel::text("attempt complete"),
    };

    let result = executor
        .execute_task_attempt(TaskExecutionContext {
            thread_id: Uuid::new_v4(),
            task_id,
            run_id: None,
            working_dir: PathBuf::from("."),
            prompt,
            approval: ApprovalMediationState::RuntimeMediatedInteractive,
        })
        .await
        .expect("task attempt");

    assert_eq!(result.task_id, task_id);
    assert_eq!(result.status, TaskExecutionStatus::Completed);
    assert_eq!(result.summary.as_deref(), Some("attempt complete"));
}

/// W1-01: the runtime worker, rather than a UI adapter, owns per-session
/// serialized turn execution.  The executor deliberately waits so the test
/// can prove the second mutating turn cannot begin until the first exits.
#[tokio::test]
async fn agent_runtime_state_machine() {
    use std::sync::Arc;

    use libra::internal::ai::runtime::{
        AgentRuntimeWorker, AgentRuntimeWorkerConfig, InMemoryAuditSink, PrincipalContext,
        PrincipalRole, RuntimeExecutionContext, RuntimeTurnExecution, RuntimeTurnExecutor,
        RuntimeWorkerError, SecretRedactor, ToolBoundaryPolicy, ToolBoundaryRuntime, TurnRequest,
    };
    use tokio::{
        sync::{Mutex, Notify},
        time::{Duration, timeout},
    };

    struct BlockingExecutor {
        starts: Arc<Mutex<Vec<String>>>,
        started: Arc<Notify>,
        release: Arc<Notify>,
    }

    #[async_trait]
    impl RuntimeTurnExecutor for BlockingExecutor {
        async fn execute(
            &self,
            request: TurnRequest,
            context: RuntimeExecutionContext,
        ) -> Result<RuntimeTurnExecution, RuntimeWorkerError> {
            self.starts.lock().await.push(request.turn_id);
            self.started.notify_one();
            let cancellation = context.cancellation();
            tokio::select! {
                _ = self.release.notified() => Ok(RuntimeTurnExecution::Completed { summary: "done".to_string() }),
                _ = cancellation.cancelled() => Err(RuntimeWorkerError::Cancelled),
            }
        }
    }

    let starts = Arc::new(Mutex::new(Vec::new()));
    let started = Arc::new(Notify::new());
    let release = Arc::new(Notify::new());
    let executor = Arc::new(BlockingExecutor {
        starts: starts.clone(),
        started: started.clone(),
        release: release.clone(),
    });
    let tool_boundary = ToolBoundaryRuntime::new(
        Uuid::new_v4(),
        PrincipalContext {
            principal_id: "runtime-contract-test".to_string(),
            role: PrincipalRole::Contributor,
        },
        ToolBoundaryPolicy::default_runtime(),
        SecretRedactor::default_runtime(),
        Arc::new(InMemoryAuditSink::default()),
    );
    let (handle, worker) =
        AgentRuntimeWorker::spawn(AgentRuntimeWorkerConfig::new(executor, tool_boundary));

    handle
        .submit(TurnRequest::new("session", "first", "first input", true))
        .await
        .expect("first turn accepted");
    handle
        .submit(TurnRequest::new("session", "second", "second input", true))
        .await
        .expect("second turn accepted");
    timeout(Duration::from_secs(1), started.notified())
        .await
        .expect("first turn started");
    assert_eq!(starts.lock().await.as_slice(), ["first"]);
    let snapshot = handle.snapshot("session").await.expect("runtime snapshot");
    assert_eq!(snapshot.active_turn_id.as_deref(), Some("first"));
    assert_eq!(snapshot.queued_turns, 1);

    release.notify_one();
    timeout(Duration::from_secs(1), started.notified())
        .await
        .expect("second turn started after first completed");
    assert_eq!(starts.lock().await.as_slice(), ["first", "second"]);
    release.notify_one();
    worker.abort();
}

#[test]
fn durable_intent_precedes_mutation_in_four_crash_windows() {
    use std::sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    };

    use libra::internal::ai::{
        runtime::{
            DurableCommandCrashPoint, RuntimeCommandDurability, RuntimeCommandDurabilityError,
        },
        session::{
            CodeCommandIdentity, CodeCommandIntent, CodeCommandRecovery, CodeCommandStatus,
            CodeCommandStoreError, SessionJsonlStore,
        },
    };

    fn intent(command_id: &str, mutating: bool) -> CodeCommandIntent {
        CodeCommandIntent::new(
            CodeCommandIdentity::new("repo", "session", "contract-test", command_id),
            if mutating {
                "apply_patch"
            } else {
                "search_files"
            },
            format!("sha256:{command_id}"),
            mutating,
        )
    }

    for crash_point in [
        DurableCommandCrashPoint::BeforeIntentFsync,
        DurableCommandCrashPoint::AfterIntentFsyncBeforeDispatch,
        DurableCommandCrashPoint::AfterDispatchBeforeTerminalFsync,
        DurableCommandCrashPoint::AfterTerminalFsync,
    ] {
        let temp = tempfile::TempDir::new().expect("temporary session root");
        let durability =
            RuntimeCommandDurability::new(SessionJsonlStore::new(temp.path().join("session")));
        let command = intent("mutating", true);
        let dispatches = Arc::new(AtomicUsize::new(0));
        let dispatches_for_call = Arc::clone(&dispatches);

        let error = durability
            .execute(command.clone(), Some(crash_point), move || {
                dispatches_for_call.fetch_add(1, Ordering::SeqCst);
                Ok("mutation applied".to_string())
            })
            .expect_err("each injected crash must stop the caller");
        assert!(matches!(
            error,
            RuntimeCommandDurabilityError::InjectedCrash(point) if point == crash_point
        ));

        match crash_point {
            DurableCommandCrashPoint::BeforeIntentFsync => {
                assert_eq!(dispatches.load(Ordering::SeqCst), 0);
                assert!(matches!(
                    durability.recover(&command),
                    Err(RuntimeCommandDurabilityError::Store(
                        CodeCommandStoreError::MissingIntent { .. }
                    ))
                ));
            }
            DurableCommandCrashPoint::AfterIntentFsyncBeforeDispatch => {
                assert_eq!(dispatches.load(Ordering::SeqCst), 0);
                assert!(matches!(
                    durability
                        .recover(&command)
                        .expect("recover persisted intent"),
                    CodeCommandRecovery::Existing {
                        status: CodeCommandStatus::Indeterminate { .. }
                    }
                ));
            }
            DurableCommandCrashPoint::AfterDispatchBeforeTerminalFsync => {
                assert_eq!(dispatches.load(Ordering::SeqCst), 1);
                assert!(matches!(
                    durability
                        .recover(&command)
                        .expect("recover dispatched mutation"),
                    CodeCommandRecovery::Existing {
                        status: CodeCommandStatus::Indeterminate { .. }
                    }
                ));
            }
            DurableCommandCrashPoint::AfterTerminalFsync => {
                assert_eq!(dispatches.load(Ordering::SeqCst), 1);
                assert!(matches!(
                    durability
                        .recover(&command)
                        .expect("recover durable terminal result"),
                    CodeCommandRecovery::Existing {
                        status: CodeCommandStatus::Succeeded { .. }
                    }
                ));
            }
        }
    }

    let temp = tempfile::TempDir::new().expect("temporary read-only session root");
    let durability =
        RuntimeCommandDurability::new(SessionJsonlStore::new(temp.path().join("session")));
    let read_only = intent("read-only", false);
    let error = durability
        .execute(
            read_only.clone(),
            Some(DurableCommandCrashPoint::AfterDispatchBeforeTerminalFsync),
            || Ok("three matches".to_string()),
        )
        .expect_err("injected crash");
    assert!(matches!(
        error,
        RuntimeCommandDurabilityError::InjectedCrash(
            DurableCommandCrashPoint::AfterDispatchBeforeTerminalFsync
        )
    ));
    assert!(matches!(
        durability
            .recover(&read_only)
            .expect("recover read-only command"),
        CodeCommandRecovery::RetryReadOnly { .. }
    ));
    assert!(matches!(
        durability
            .retry_recovered_read_only(read_only, || Ok("three matches".to_string()))
            .expect("retry recovered read-only command"),
        CodeCommandStatus::Succeeded { .. }
    ));
}

#[tokio::test]
async fn recovered_mutating_command_fences_worker_session_before_admission() {
    use std::sync::Arc;

    use libra::internal::ai::{
        runtime::{
            AgentRuntimeWorker, AgentRuntimeWorkerConfig, ExternalTurnTrackingExecutor,
            InMemoryAuditSink, RuntimeCommandDurability, RuntimeWorkerError, ToolBoundaryRuntime,
            TurnRequest,
        },
        session::{CodeCommandIdentity, CodeCommandIntent, CodeCommandStatus, SessionJsonlStore},
    };

    let temp = tempfile::TempDir::new().expect("temporary session root");
    let durability =
        RuntimeCommandDurability::new(SessionJsonlStore::new(temp.path().join("session")));
    let intent = CodeCommandIntent::new(
        CodeCommandIdentity::new("repo", "session", "principal", "interrupted-turn"),
        "agent_runtime_turn",
        "sha256:request",
        true,
    );
    durability
        .admit(intent)
        .expect("persist interrupted intent");
    assert_eq!(
        durability
            .recover_pending_mutations()
            .expect("recover pending mutations"),
        vec![CodeCommandIdentity::new(
            "repo",
            "session",
            "principal",
            "interrupted-turn"
        )]
    );
    assert!(matches!(
        durability
            .session_store()
            .recover_code_command(&CodeCommandIdentity::new(
                "repo",
                "session",
                "principal",
                "interrupted-turn"
            ))
            .expect("read recovered command"),
        libra::internal::ai::session::CodeCommandRecovery::Existing {
            status: CodeCommandStatus::Indeterminate { .. }
        }
    ));

    let config = AgentRuntimeWorkerConfig::new(
        Arc::new(ExternalTurnTrackingExecutor),
        ToolBoundaryRuntime::system(Uuid::new_v4(), Arc::new(InMemoryAuditSink::default())),
    )
    .with_durability(durability, "repo", "principal")
    .with_recovered_reconciliation_session("session");
    let (handle, worker) = AgentRuntimeWorker::spawn(config);

    assert!(matches!(
        handle
            .submit(TurnRequest::new("session", "next-turn", "next input", true))
            .await,
        Err(RuntimeWorkerError::ReconciliationRequired { session_id }) if session_id == "session"
    ));
    worker.abort();
}

#[tokio::test]
async fn second_restart_keeps_reconciliation_fence_for_indeterminate_mutation() {
    use std::sync::Arc;

    use libra::internal::ai::{
        runtime::{
            AgentRuntimeWorker, AgentRuntimeWorkerConfig, ExternalTurnTrackingExecutor,
            InMemoryAuditSink, RuntimeCommandDurability, RuntimeWorkerError, ToolBoundaryRuntime,
            TurnRequest,
        },
        session::{CodeCommandIdentity, CodeCommandIntent, SessionJsonlStore},
    };

    let temp = tempfile::TempDir::new().expect("temporary session root");
    let session_root = temp.path().join("session");
    let first = RuntimeCommandDurability::new(SessionJsonlStore::new(session_root.clone()));
    let intent = CodeCommandIntent::new(
        CodeCommandIdentity::new("repo", "session", "principal", "interrupted-turn"),
        "agent_runtime_turn",
        "sha256:request",
        true,
    );
    first.admit(intent).expect("persist interrupted intent");
    assert_eq!(
        first
            .recover_pending_mutations()
            .expect("first restart fences pending mutation")
            .len(),
        1,
        "first restart must fence the pending mutating command"
    );

    // A later process opens the same durable log with no Pending rows left.
    // Recovery must still report the indeterminate mutation so callers keep the
    // in-memory reconciliation fence.
    let second = RuntimeCommandDurability::new(SessionJsonlStore::new(session_root));
    let recovered = second
        .recover_pending_mutations()
        .expect("second restart must still surface reconciliation");
    assert_eq!(
        recovered,
        vec![CodeCommandIdentity::new(
            "repo",
            "session",
            "principal",
            "interrupted-turn"
        )]
    );

    let config = AgentRuntimeWorkerConfig::new(
        Arc::new(ExternalTurnTrackingExecutor),
        ToolBoundaryRuntime::system(Uuid::new_v4(), Arc::new(InMemoryAuditSink::default())),
    )
    .with_durability(second, "repo", "principal")
    .with_recovered_reconciliation_session("session");
    let (handle, worker) = AgentRuntimeWorker::spawn(config);
    assert!(matches!(
        handle
            .submit(TurnRequest::new("session", "next-turn", "next input", true))
            .await,
        Err(RuntimeWorkerError::ReconciliationRequired { session_id }) if session_id == "session"
    ));
    worker.abort();
}

#[tokio::test]
async fn cancel_during_mutation_requires_reconciliation() {
    use std::sync::Arc;

    use libra::internal::ai::{
        runtime::{
            AgentRuntimeWorker, AgentRuntimeWorkerConfig, InMemoryAuditSink, InteractionState,
            PrincipalContext, PrincipalRole, RuntimeCommandDurability, RuntimeExecutionContext,
            RuntimeTurnExecution, RuntimeTurnExecutor, RuntimeWorkerError, SecretRedactor,
            ToolBoundaryPolicy, ToolBoundaryRuntime, TurnRequest,
        },
        session::{CodeCommandIdentity, CodeCommandRecovery, CodeCommandStatus, SessionJsonlStore},
    };
    use tokio::{
        sync::Notify,
        time::{Duration, timeout},
    };

    struct MutationExecutor {
        mutation_started: Arc<Notify>,
        release_first: Arc<Notify>,
        second_started: Arc<Notify>,
    }

    #[async_trait]
    impl RuntimeTurnExecutor for MutationExecutor {
        async fn execute(
            &self,
            request: TurnRequest,
            context: RuntimeExecutionContext,
        ) -> Result<RuntimeTurnExecution, RuntimeWorkerError> {
            if request.turn_id == "first" {
                context.mark_mutation_started();
                self.mutation_started.notify_one();
                self.release_first.notified().await;
                // A real adapter must report an actual terminal tool result.
                // This intentionally ambiguous result exercises the worker's
                // fail-closed reconciliation branch.
                return Err(RuntimeWorkerError::Cancelled);
            }
            self.second_started.notify_one();
            Ok(RuntimeTurnExecution::Completed {
                summary: "second completed".to_string(),
            })
        }
    }

    let mutation_started = Arc::new(Notify::new());
    let release_first = Arc::new(Notify::new());
    let second_started = Arc::new(Notify::new());
    let executor = Arc::new(MutationExecutor {
        mutation_started: Arc::clone(&mutation_started),
        release_first: Arc::clone(&release_first),
        second_started: Arc::clone(&second_started),
    });
    let boundary = ToolBoundaryRuntime::new(
        Uuid::new_v4(),
        PrincipalContext {
            principal_id: "runtime-cancel-test".to_string(),
            role: PrincipalRole::Contributor,
        },
        ToolBoundaryPolicy::default_runtime(),
        SecretRedactor::default_runtime(),
        Arc::new(InMemoryAuditSink::default()),
    );
    let temp = tempfile::TempDir::new().expect("temporary session JSONL root");
    let durability =
        RuntimeCommandDurability::new(SessionJsonlStore::new(temp.path().join("session")));
    let (handle, worker) = AgentRuntimeWorker::spawn(
        AgentRuntimeWorkerConfig::new(executor, boundary).with_durability(
            durability.clone(),
            "runtime-contract-repo",
            "runtime-cancel-test",
        ),
    );

    handle
        .submit(TurnRequest::new("session", "first", "apply patch", true))
        .await
        .expect("first turn accepted");
    timeout(Duration::from_secs(1), mutation_started.notified())
        .await
        .expect("mutation began");
    handle
        .submit(TurnRequest::new("session", "second", "next patch", true))
        .await
        .expect("second turn queued");

    handle
        .cancel("session", "first")
        .await
        .expect("cancel request accepted");
    assert_eq!(
        handle
            .snapshot("session")
            .await
            .expect("cancelling snapshot")
            .interaction,
        InteractionState::Cancelling,
        "a cancellation after mutation start must wait for executor reconciliation",
    );

    release_first.notify_one();
    timeout(Duration::from_secs(1), async {
        loop {
            let snapshot = handle.snapshot("session").await.expect("snapshot");
            if matches!(
                snapshot.interaction,
                InteractionState::IndeterminateSideEffect { .. }
            ) {
                return;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("ambiguous mutation result becomes indeterminate");
    assert!(matches!(
        durability
            .session_store()
            .recover_code_command(&CodeCommandIdentity::new(
                "runtime-contract-repo",
                "session",
                "runtime-cancel-test",
                "first",
            ))
            .expect("read durable cancellation result"),
        CodeCommandRecovery::Existing {
            status: CodeCommandStatus::Indeterminate { .. }
        }
    ));
    assert!(
        timeout(Duration::from_millis(100), second_started.notified())
            .await
            .is_err(),
        "the queued mutation must not begin while reconciliation is required"
    );
    assert!(matches!(
        handle
            .submit(TurnRequest::new("session", "third", "another patch", true))
            .await,
        Err(RuntimeWorkerError::ReconciliationRequired { .. })
    ));
    if let Err(error) = handle
        .submit(TurnRequest::new(
            "session",
            "fourth",
            "must not replay",
            true,
        ))
        .await
    {
        assert!(
            libra::internal::ai::runtime::runtime_worker_adapter_message(error)
                .contains("RECONCILIATION_REQUIRED"),
            "resubmit after cancel-indeterminate must expose a stable reconciliation code"
        );
    } else {
        panic!("resubmit after cancel-indeterminate must be rejected, not replayed");
    }
    worker.abort();
}

/// W2-02 AC3/AC4: while a Phase 0 IntentSpec review is pending
/// (`InteractionState::AwaitingIntentReview`), the worker — not
/// `pending_intent_review` in the retired TUI — is the durable owner of the mutation
/// fence. A follow-on turn on the same session (e.g. a stray mutating tool
/// call outside the `phase0_plan_tool_loop_config` allowlist) must queue
/// rather than execute, because the tracked Phase 0 turn stays active until
/// `IntentReviewAckDelivery` resolves it via `respond`. Only `confirm`
/// releases the fence for the queued turn to start.
#[tokio::test]
async fn intentspec_review_blocks_mutation() {
    use std::sync::{
        Arc,
        atomic::{AtomicBool, AtomicUsize, Ordering},
    };

    use libra::internal::ai::runtime::{
        AgentRuntimeWorker, AgentRuntimeWorkerConfig, InMemoryAuditSink, InteractionResponse,
        InteractionState, PrincipalContext, PrincipalRole, RuntimeExecutionContext,
        RuntimeTurnExecution, RuntimeTurnExecutor, RuntimeWorkerError, SecretRedactor,
        ToolBoundaryPolicy, ToolBoundaryRuntime, TurnRequest,
        phase0::{IntentReviewAckDelivery, IntentReviewDecision},
    };
    use tokio::{
        sync::Notify,
        time::{Duration, timeout},
    };
    use tokio_util::sync::CancellationToken;

    struct MutatingExecutor {
        started: Arc<AtomicUsize>,
        notify: Arc<Notify>,
    }

    #[async_trait]
    impl RuntimeTurnExecutor for MutatingExecutor {
        async fn execute(
            &self,
            _request: TurnRequest,
            _context: RuntimeExecutionContext,
        ) -> Result<RuntimeTurnExecution, RuntimeWorkerError> {
            self.started.fetch_add(1, Ordering::SeqCst);
            self.notify.notify_one();
            Ok(RuntimeTurnExecution::Completed {
                summary: "mutation applied".to_string(),
            })
        }
    }

    let started = Arc::new(AtomicUsize::new(0));
    let notify = Arc::new(Notify::new());
    let executor = Arc::new(MutatingExecutor {
        started: Arc::clone(&started),
        notify: Arc::clone(&notify),
    });
    let boundary = ToolBoundaryRuntime::new(
        Uuid::new_v4(),
        PrincipalContext {
            principal_id: "intentspec-review-fence-test".to_string(),
            role: PrincipalRole::Contributor,
        },
        ToolBoundaryPolicy::default_runtime(),
        SecretRedactor::default_runtime(),
        Arc::new(InMemoryAuditSink::default()),
    );
    let (handle, worker) =
        AgentRuntimeWorker::spawn(AgentRuntimeWorkerConfig::new(executor, boundary));

    // Track the Phase 0 turn the way the Web adapter does once
    // `submit_intent_draft` fires: `track_external_turn` keeps it active
    // while the tool loop itself has already exited.
    handle
        .track_external_turn(
            TurnRequest::new("session", "phase0-turn", "plan workflow", true),
            CancellationToken::new(),
            Arc::new(AtomicBool::new(false)),
        )
        .await
        .expect("phase0 turn tracked");
    handle
        .register_interaction_with_delivery(
            "session",
            "phase0-turn",
            InteractionState::AwaitingIntentReview {
                interaction_id: "intent-1".to_string(),
            },
            Box::new(IntentReviewAckDelivery::new()),
        )
        .await
        .expect("worker owns the IntentSpec review interaction");

    // A follow-on turn must queue, not run, while the review is pending.
    handle
        .submit(TurnRequest::new(
            "session",
            "mutating-turn",
            "apply_patch",
            true,
        ))
        .await
        .expect("mutating turn is accepted into the queue");
    assert_eq!(
        started.load(Ordering::SeqCst),
        0,
        "no mutating tool call may execute before the IntentSpec review is confirmed"
    );
    let snapshot = handle
        .snapshot("session")
        .await
        .expect("snapshot while awaiting review");
    assert_eq!(
        snapshot.interaction,
        InteractionState::AwaitingIntentReview {
            interaction_id: "intent-1".to_string(),
        }
    );
    assert_eq!(snapshot.queued_turns, 1);

    // Confirming the review resolves the Phase 0 turn and releases the fence.
    handle
        .respond(
            "session",
            "phase0-turn",
            InteractionResponse::new("intent-1", IntentReviewDecision::Confirm.wire_id()),
        )
        .await
        .expect("confirm resolves the review");
    timeout(Duration::from_secs(1), notify.notified())
        .await
        .expect("queued mutating turn starts once the review is resolved");
    assert_eq!(started.load(Ordering::SeqCst), 1);
    worker.abort();
}

/// W2-02 recovery: Phase 0 must be durably terminalized before the review
/// gate parks, and the gate itself must be non-mutating so a crash cannot
/// reopen an indeterminate reconciliation fence.
#[tokio::test]
async fn intentspec_review_gate_is_non_mutating_after_phase0_terminalizes() {
    use std::sync::{
        Arc,
        atomic::{AtomicBool, AtomicUsize, Ordering},
    };

    use libra::internal::ai::runtime::{
        AgentRuntimeWorker, AgentRuntimeWorkerConfig, InMemoryAuditSink, InteractionResponse,
        InteractionState, PrincipalContext, PrincipalRole, RuntimeExecutionContext,
        RuntimeTurnExecution, RuntimeTurnExecutor, RuntimeWorkerError, SecretRedactor,
        ToolBoundaryPolicy, ToolBoundaryRuntime, TurnRequest,
        phase0::{IntentReviewAckDelivery, IntentReviewDecision},
    };
    use tokio::time::{Duration, sleep};
    use tokio_util::sync::CancellationToken;

    struct MutatingExecutor {
        started: Arc<AtomicUsize>,
    }

    #[async_trait]
    impl RuntimeTurnExecutor for MutatingExecutor {
        async fn execute(
            &self,
            _request: TurnRequest,
            _context: RuntimeExecutionContext,
        ) -> Result<RuntimeTurnExecution, RuntimeWorkerError> {
            self.started.fetch_add(1, Ordering::SeqCst);
            Ok(RuntimeTurnExecution::Completed {
                summary: "mutation applied".to_string(),
            })
        }
    }

    let started = Arc::new(AtomicUsize::new(0));
    let executor = Arc::new(MutatingExecutor {
        started: Arc::clone(&started),
    });
    let boundary = ToolBoundaryRuntime::new(
        Uuid::new_v4(),
        PrincipalContext {
            principal_id: "intentspec-review-hold-queued".to_string(),
            role: PrincipalRole::Contributor,
        },
        ToolBoundaryPolicy::default_runtime(),
        SecretRedactor::default_runtime(),
        Arc::new(InMemoryAuditSink::default()),
    );
    let (handle, worker) =
        AgentRuntimeWorker::spawn(AgentRuntimeWorkerConfig::new(executor, boundary));

    handle
        .track_external_turn(
            TurnRequest::new("session", "phase0-turn", "plan workflow", true),
            CancellationToken::new(),
            Arc::new(AtomicBool::new(true)),
        )
        .await
        .expect("phase0 turn tracked");
    handle
        .submit(TurnRequest::new(
            "session",
            "mutating-turn",
            "apply_patch",
            true,
        ))
        .await
        .expect("mutating turn queues behind phase0");
    assert_eq!(started.load(Ordering::SeqCst), 0);

    // Mirror the legacy register path: terminalize Phase 0 without releasing the
    // queue, then park a non-mutating review gate in front of it.
    handle
        .finish_external_turn(
            "session",
            "phase0-turn",
            Ok(RuntimeTurnExecution::CompletedHoldQueued {
                summary: "IntentSpec draft persisted; awaiting review".to_string(),
            }),
        )
        .await
        .expect("phase0 terminalizes without releasing the queue");
    assert_eq!(
        started.load(Ordering::SeqCst),
        0,
        "CompletedHoldQueued must not start queued mutations"
    );

    handle
        .track_external_turn(
            TurnRequest::new("session", "intent-review-turn", "IntentSpec review", false),
            CancellationToken::new(),
            Arc::new(AtomicBool::new(false)),
        )
        .await
        .expect("non-mutating review gate can park in front of the queue");
    handle
        .register_interaction_with_delivery(
            "session",
            "intent-review-turn",
            InteractionState::AwaitingIntentReview {
                interaction_id: "intent-1".to_string(),
            },
            Box::new(IntentReviewAckDelivery::new()),
        )
        .await
        .expect("review gate owns AwaitingIntentReview");

    sleep(Duration::from_millis(50)).await;
    assert_eq!(started.load(Ordering::SeqCst), 0);
    assert_eq!(
        handle
            .snapshot("session")
            .await
            .expect("snapshot")
            .queued_turns,
        1
    );

    handle
        .respond(
            "session",
            "intent-review-turn",
            InteractionResponse::new("intent-1", IntentReviewDecision::Confirm.wire_id()),
        )
        .await
        .expect("confirm resolves the non-mutating gate");
    sleep(Duration::from_millis(100)).await;
    assert_eq!(
        started.load(Ordering::SeqCst),
        1,
        "confirm releases the held queue"
    );
    worker.abort();
}

/// W2-02 AC3: revise/cancel must discard turns queued under the IntentSpec
/// review fence. Completing the Phase 0 turn alone is not enough — those
/// queued mutations never received a confirmed IntentSpec.
#[tokio::test]
async fn intentspec_review_non_confirm_discards_queued_mutations() {
    use std::sync::{
        Arc,
        atomic::{AtomicBool, AtomicUsize, Ordering},
    };

    use libra::internal::ai::runtime::{
        AgentRuntimeWorker, AgentRuntimeWorkerConfig, InMemoryAuditSink, InteractionResponse,
        InteractionState, PrincipalContext, PrincipalRole, RuntimeExecutionContext,
        RuntimeTurnExecution, RuntimeTurnExecutor, RuntimeWorkerError, SecretRedactor,
        ToolBoundaryPolicy, ToolBoundaryRuntime, TurnRequest,
        phase0::{IntentReviewAckDelivery, IntentReviewDecision},
    };
    use tokio::time::{Duration, sleep};
    use tokio_util::sync::CancellationToken;

    struct MutatingExecutor {
        started: Arc<AtomicUsize>,
    }

    #[async_trait]
    impl RuntimeTurnExecutor for MutatingExecutor {
        async fn execute(
            &self,
            _request: TurnRequest,
            _context: RuntimeExecutionContext,
        ) -> Result<RuntimeTurnExecution, RuntimeWorkerError> {
            self.started.fetch_add(1, Ordering::SeqCst);
            Ok(RuntimeTurnExecution::Completed {
                summary: "mutation applied".to_string(),
            })
        }
    }

    for decision in [IntentReviewDecision::Revise, IntentReviewDecision::Cancel] {
        let started = Arc::new(AtomicUsize::new(0));
        let executor = Arc::new(MutatingExecutor {
            started: Arc::clone(&started),
        });
        let boundary = ToolBoundaryRuntime::new(
            Uuid::new_v4(),
            PrincipalContext {
                principal_id: format!("intentspec-review-{}-fence", decision.wire_id()),
                role: PrincipalRole::Contributor,
            },
            ToolBoundaryPolicy::default_runtime(),
            SecretRedactor::default_runtime(),
            Arc::new(InMemoryAuditSink::default()),
        );
        let (handle, worker) =
            AgentRuntimeWorker::spawn(AgentRuntimeWorkerConfig::new(executor, boundary));

        handle
            .track_external_turn(
                TurnRequest::new("session", "phase0-turn", "plan workflow", true),
                CancellationToken::new(),
                Arc::new(AtomicBool::new(false)),
            )
            .await
            .expect("phase0 turn tracked");
        handle
            .register_interaction_with_delivery(
                "session",
                "phase0-turn",
                InteractionState::AwaitingIntentReview {
                    interaction_id: "intent-1".to_string(),
                },
                Box::new(IntentReviewAckDelivery::new()),
            )
            .await
            .expect("worker owns the IntentSpec review interaction");
        handle
            .submit(TurnRequest::new(
                "session",
                "mutating-turn",
                "apply_patch",
                true,
            ))
            .await
            .expect("mutating turn is accepted into the queue");
        assert_eq!(started.load(Ordering::SeqCst), 0);

        handle
            .respond(
                "session",
                "phase0-turn",
                InteractionResponse::new("intent-1", decision.wire_id()),
            )
            .await
            .expect("non-confirm decision resolves the review");
        // Give the worker a moment to (incorrectly) start the queued turn if
        // the fence regresses; then assert it never ran.
        sleep(Duration::from_millis(50)).await;
        assert_eq!(
            started.load(Ordering::SeqCst),
            0,
            "{} must discard queued mutations, not release them",
            decision.wire_id()
        );
        let snapshot = handle
            .snapshot("session")
            .await
            .expect("snapshot after non-confirm");
        assert_eq!(snapshot.interaction, InteractionState::Completed);
        assert_eq!(
            snapshot.queued_turns,
            0,
            "{} must empty the queue of fenced mutations",
            decision.wire_id()
        );
        worker.abort();
    }
}

/// W2-03: Plan Execute must not release mutating work until the network-policy
/// human gate resolves. Revise/Cancel discard queued mutations (same fence
/// contract as IntentSpec review).
#[tokio::test]
async fn plan_review_network_policy_gate() {
    use std::sync::{
        Arc,
        atomic::{AtomicBool, AtomicUsize, Ordering},
    };

    use libra::internal::ai::runtime::{
        AgentRuntimeWorker, AgentRuntimeWorkerConfig, InMemoryAuditSink, InteractionResponse,
        InteractionState, PrincipalContext, PrincipalRole, RuntimeExecutionContext,
        RuntimeTurnExecution, RuntimeTurnExecutor, RuntimeWorkerError, SecretRedactor,
        ToolBoundaryPolicy, ToolBoundaryRuntime, TurnRequest,
        phase1::{
            NetworkPolicyAckDelivery, NetworkPolicyDecision, PlanReviewAckDelivery,
            PlanReviewDecision,
        },
    };
    use tokio::{
        sync::Notify,
        time::{Duration, timeout},
    };
    use tokio_util::sync::CancellationToken;

    struct MutatingExecutor {
        started: Arc<AtomicUsize>,
        notify: Arc<Notify>,
    }

    #[async_trait]
    impl RuntimeTurnExecutor for MutatingExecutor {
        async fn execute(
            &self,
            _request: TurnRequest,
            _context: RuntimeExecutionContext,
        ) -> Result<RuntimeTurnExecution, RuntimeWorkerError> {
            self.started.fetch_add(1, Ordering::SeqCst);
            self.notify.notify_one();
            Ok(RuntimeTurnExecution::Completed {
                summary: "mutation applied".to_string(),
            })
        }
    }

    let started = Arc::new(AtomicUsize::new(0));
    let notify = Arc::new(Notify::new());
    let executor = Arc::new(MutatingExecutor {
        started: Arc::clone(&started),
        notify: Arc::clone(&notify),
    });
    let boundary = ToolBoundaryRuntime::new(
        Uuid::new_v4(),
        PrincipalContext {
            principal_id: "plan-review-network-policy-gate".to_string(),
            role: PrincipalRole::Contributor,
        },
        ToolBoundaryPolicy::default_runtime(),
        SecretRedactor::default_runtime(),
        Arc::new(InMemoryAuditSink::default()),
    );
    let (handle, worker) =
        AgentRuntimeWorker::spawn(AgentRuntimeWorkerConfig::new(executor, boundary));

    handle
        .track_external_turn(
            TurnRequest::new("session", "plan-review-turn", "plan review", false),
            CancellationToken::new(),
            Arc::new(AtomicBool::new(false)),
        )
        .await
        .expect("plan review turn tracked");
    handle
        .register_interaction_with_delivery(
            "session",
            "plan-review-turn",
            InteractionState::AwaitingPlanReview {
                interaction_id: "plan-1".to_string(),
            },
            Box::new(PlanReviewAckDelivery::new()),
        )
        .await
        .expect("worker owns the Plan review interaction");

    handle
        .submit(TurnRequest::new(
            "session",
            "mutating-turn",
            "apply_patch",
            true,
        ))
        .await
        .expect("mutating turn is accepted into the queue");
    assert_eq!(
        started.load(Ordering::SeqCst),
        0,
        "no mutating tool may run before Plan review + network policy resolve"
    );

    handle
        .respond(
            "session",
            "plan-review-turn",
            InteractionResponse::new("plan-1", PlanReviewDecision::Execute.wire_id()),
        )
        .await
        .expect("Execute advances to network policy");
    assert_eq!(
        started.load(Ordering::SeqCst),
        0,
        "Execute must HoldQueued — network policy still required"
    );

    handle
        .track_external_turn(
            TurnRequest::new("session", "network-policy-turn", "network policy", false),
            CancellationToken::new(),
            Arc::new(AtomicBool::new(false)),
        )
        .await
        .expect("network policy turn tracked");
    handle
        .register_interaction_with_delivery(
            "session",
            "network-policy-turn",
            InteractionState::AwaitingNetworkPolicy {
                interaction_id: "plan-1:network-policy".to_string(),
            },
            Box::new(NetworkPolicyAckDelivery::new()),
        )
        .await
        .expect("worker owns the network policy interaction");

    let snapshot = handle
        .snapshot("session")
        .await
        .expect("snapshot while awaiting network policy");
    assert_eq!(
        snapshot.interaction,
        InteractionState::AwaitingNetworkPolicy {
            interaction_id: "plan-1:network-policy".to_string(),
        }
    );
    assert_eq!(started.load(Ordering::SeqCst), 0);

    handle
        .respond(
            "session",
            "network-policy-turn",
            InteractionResponse::new(
                "plan-1:network-policy",
                NetworkPolicyDecision::Allow.wire_id(),
            ),
        )
        .await
        .expect("network allow releases the fence");

    timeout(Duration::from_secs(2), notify.notified())
        .await
        .expect("queued mutation should start after network allow");
    assert_eq!(
        started.load(Ordering::SeqCst),
        1,
        "network allow releases the held mutating queue"
    );
    worker.abort();
}

/// W2-03 end-to-end fence, mirroring `intentspec_review_blocks_mutation` but
/// for the full Phase 1 sequence the runtime drives: a mutating Phase 1 turn
/// terminalizes with `CompletedHoldQueued`, a non-mutating Plan review gate
/// parks in front of the held queue, Execute hands off to a non-mutating
/// network-policy gate, and only `network-allow` releases the mutation.
#[tokio::test]
async fn plan_review_gate_holds_phase1_queue_until_network_policy_resolves() {
    use std::sync::{
        Arc,
        atomic::{AtomicBool, AtomicUsize, Ordering},
    };

    use libra::internal::ai::runtime::{
        AgentRuntimeWorker, AgentRuntimeWorkerConfig, InMemoryAuditSink, InteractionResponse,
        InteractionState, PrincipalContext, PrincipalRole, RuntimeExecutionContext,
        RuntimeTurnExecution, RuntimeTurnExecutor, RuntimeWorkerError, SecretRedactor,
        ToolBoundaryPolicy, ToolBoundaryRuntime, TurnRequest,
        phase1::{
            NetworkPolicyAckDelivery, NetworkPolicyDecision, PlanReviewAckDelivery,
            PlanReviewDecision, network_policy_interaction_id,
        },
    };
    use tokio::{
        sync::Notify,
        time::{Duration, sleep, timeout},
    };
    use tokio_util::sync::CancellationToken;

    struct MutatingExecutor {
        started: Arc<AtomicUsize>,
        notify: Arc<Notify>,
    }

    #[async_trait]
    impl RuntimeTurnExecutor for MutatingExecutor {
        async fn execute(
            &self,
            _request: TurnRequest,
            _context: RuntimeExecutionContext,
        ) -> Result<RuntimeTurnExecution, RuntimeWorkerError> {
            self.started.fetch_add(1, Ordering::SeqCst);
            self.notify.notify_one();
            Ok(RuntimeTurnExecution::Completed {
                summary: "mutation applied".to_string(),
            })
        }
    }

    let started = Arc::new(AtomicUsize::new(0));
    let notify = Arc::new(Notify::new());
    let executor = Arc::new(MutatingExecutor {
        started: Arc::clone(&started),
        notify: Arc::clone(&notify),
    });
    let boundary = ToolBoundaryRuntime::new(
        Uuid::new_v4(),
        PrincipalContext {
            principal_id: "plan-review-phase1-hold-queued".to_string(),
            role: PrincipalRole::Contributor,
        },
        ToolBoundaryPolicy::default_runtime(),
        SecretRedactor::default_runtime(),
        Arc::new(InMemoryAuditSink::default()),
    );
    let (handle, worker) =
        AgentRuntimeWorker::spawn(AgentRuntimeWorkerConfig::new(executor, boundary));

    // The mutating Phase 1 turn that wrote the plan draft is still tracked when
    // `PlanWorkflowComplete` lands, exactly as `track_external_turn` leaves it.
    handle
        .track_external_turn(
            TurnRequest::new("session", "phase1-turn", "plan workflow", true),
            CancellationToken::new(),
            Arc::new(AtomicBool::new(true)),
        )
        .await
        .expect("phase1 turn tracked");
    handle
        .submit(TurnRequest::new(
            "session",
            "mutating-turn",
            "apply_patch",
            true,
        ))
        .await
        .expect("mutating turn queues behind phase1");
    assert_eq!(started.load(Ordering::SeqCst), 0);

    handle
        .finish_external_turn(
            "session",
            "phase1-turn",
            Ok(RuntimeTurnExecution::CompletedHoldQueued {
                summary: "Execution plan draft persisted; awaiting review".to_string(),
            }),
        )
        .await
        .expect("phase1 terminalizes without releasing the queue");
    assert_eq!(
        started.load(Ordering::SeqCst),
        0,
        "CompletedHoldQueued must not start queued mutations"
    );

    handle
        .track_external_turn(
            TurnRequest::new("session", "plan-review-turn", "Plan review", false),
            CancellationToken::new(),
            Arc::new(AtomicBool::new(false)),
        )
        .await
        .expect("non-mutating plan review gate parks in front of the queue");
    handle
        .register_interaction_with_delivery(
            "session",
            "plan-review-turn",
            InteractionState::AwaitingPlanReview {
                interaction_id: "plan-7".to_string(),
            },
            Box::new(PlanReviewAckDelivery::new()),
        )
        .await
        .expect("review gate owns AwaitingPlanReview");

    sleep(Duration::from_millis(50)).await;
    let snapshot = handle
        .snapshot("session")
        .await
        .expect("snapshot while awaiting plan review");
    assert_eq!(
        snapshot.interaction,
        InteractionState::AwaitingPlanReview {
            interaction_id: "plan-7".to_string(),
        }
    );
    assert_eq!(snapshot.queued_turns, 1);
    assert_eq!(started.load(Ordering::SeqCst), 0);

    handle
        .respond(
            "session",
            "plan-review-turn",
            InteractionResponse::new("plan-7", PlanReviewDecision::Execute.wire_id()),
        )
        .await
        .expect("Execute resolves the plan review gate");
    sleep(Duration::from_millis(50)).await;
    assert_eq!(
        started.load(Ordering::SeqCst),
        0,
        "Execute alone must not admit mutating work — network policy is still unanswered"
    );

    let network_interaction_id = network_policy_interaction_id(Some("plan-7"));
    assert_eq!(network_interaction_id, "plan-7:network-policy");
    handle
        .track_external_turn(
            TurnRequest::new(
                "session",
                "network-policy-turn",
                "network policy (default: deny)",
                false,
            ),
            CancellationToken::new(),
            Arc::new(AtomicBool::new(false)),
        )
        .await
        .expect("network policy gate parks after Execute");
    handle
        .register_interaction_with_delivery(
            "session",
            "network-policy-turn",
            InteractionState::AwaitingNetworkPolicy {
                interaction_id: network_interaction_id.clone(),
            },
            Box::new(NetworkPolicyAckDelivery::new()),
        )
        .await
        .expect("network policy gate owns AwaitingNetworkPolicy");
    sleep(Duration::from_millis(50)).await;
    assert_eq!(
        started.load(Ordering::SeqCst),
        0,
        "a preselected network default must not skip the human gate"
    );

    handle
        .respond(
            "session",
            "network-policy-turn",
            InteractionResponse::new(
                network_interaction_id.as_str(),
                NetworkPolicyDecision::Allow.wire_id(),
            ),
        )
        .await
        .expect("network allow resolves the last gate");
    timeout(Duration::from_secs(2), notify.notified())
        .await
        .expect("queued mutation starts once both Phase 1 gates resolve");
    assert_eq!(started.load(Ordering::SeqCst), 1);
    let snapshot = handle
        .snapshot("session")
        .await
        .expect("snapshot after both gates resolved");
    assert_eq!(snapshot.queued_turns, 0);
    worker.abort();
}

/// W2-03 recovery: after Plan `Execute` durably resolves the review, the
/// `NetworkPolicyRequested` marker keeps the mandatory network human gate
/// recoverable across restart. Markers observed without a prior Execute
/// resolution must not reopen (otherwise an unapproved plan could execute).
#[test]
fn plan_review_execute_leaves_network_policy_gate_recoverable() {
    use libra::internal::ai::{
        runtime::phase1::{
            network_policy_interaction_id, open_network_policy_from_workflow,
            open_plan_review_from_workflow,
        },
        session::{CodeWorkflowEventKind, jsonl::SessionJsonlStore},
    };

    let temp = tempfile::tempdir().expect("temp dir");
    let session_root = temp.path().join("session");
    let store = SessionJsonlStore::new(session_root.clone());
    let network_interaction_id = network_policy_interaction_id(Some("plan-42"));

    store
        .append_code_workflow_durable(CodeWorkflowEventKind::PlanReviewRequested {
            interaction_id: "review-42".to_string(),
            plan_id: "plan-42".to_string(),
            turn_id: "plan-review-turn".to_string(),
            phase1_turn_id: "phase1-turn".to_string(),
            context_id: "review-42".to_string(),
            revision_of: None,
            prepared_from_network: None,
        })
        .expect("plan review marker persists");
    // Premature marker (before Execute) must not restore.
    store
        .append_code_workflow_durable(CodeWorkflowEventKind::NetworkPolicyRequested {
            interaction_id: network_interaction_id.clone(),
            plan_id: "plan-42".to_string(),
            turn_id: "network-policy-turn".to_string(),
            default_allow: true,
        })
        .expect("premature network policy marker persists");
    {
        let premature = SessionJsonlStore::new(session_root.clone());
        let events: Vec<_> = premature
            .load_code_workflow_replay()
            .expect("replay")
            .events
            .into_iter()
            .map(|e| e.event)
            .collect();
        assert!(
            open_network_policy_from_workflow(events.iter()).is_none(),
            "network marker before Plan Execute must not be restorable"
        );
    }
    store
        .append_code_workflow_durable(CodeWorkflowEventKind::InteractionResolved {
            interaction_id: "review-42".to_string(),
            resolution: "execute".to_string(),
            command: None,
            prior_interaction_resolutions: Vec::new(),
            intent_revision_consumption: None,
        })
        .expect("plan review resolution persists");

    // Restart: a fresh store over the same session root.
    let reloaded = SessionJsonlStore::new(session_root);
    let events = |store: &SessionJsonlStore| {
        store
            .load_code_workflow_replay()
            .expect("workflow replay")
            .events
            .into_iter()
            .map(|event| event.event)
            .collect::<Vec<_>>()
    };
    let replayed = events(&reloaded);
    assert_eq!(
        open_plan_review_from_workflow(replayed.iter()),
        None,
        "the plan review is durably resolved, so its restore must no-op"
    );
    assert_eq!(
        open_network_policy_from_workflow(replayed.iter()),
        Some((
            network_interaction_id.clone(),
            "plan-42".to_string(),
            "network-policy-turn".to_string(),
            true,
        )),
        "after Execute, the unanswered network-policy gate must survive restart"
    );

    reloaded
        .append_code_workflow_durable(CodeWorkflowEventKind::InteractionResolved {
            interaction_id: network_interaction_id,
            resolution: "network-allow".to_string(),
            command: None,
            prior_interaction_resolutions: Vec::new(),
            intent_revision_consumption: None,
        })
        .expect("network policy resolution persists");
    assert!(
        open_network_policy_from_workflow(events(&reloaded).iter()).is_none(),
        "answering the gate must clear the durable marker"
    );
}

/// W2-03 r6: `PlanReviewAckDelivery` Execute returns `CompletedHoldQueued`,
/// which must still append durable `InteractionResolved` so network-policy
/// recovery can prove the plan was approved.
#[tokio::test]
async fn plan_review_execute_hold_queued_persists_interaction_resolved() {
    use std::sync::{Arc, atomic::AtomicBool};

    use libra::internal::ai::{
        runtime::{
            AgentRuntimeWorker, AgentRuntimeWorkerConfig, InMemoryAuditSink, InteractionResponse,
            InteractionState, PrincipalContext, PrincipalRole, RuntimeCommandDurability,
            SecretRedactor, ToolBoundaryPolicy, ToolBoundaryRuntime, TurnRequest,
            phase1::{
                PlanReviewAckDelivery, PlanReviewDecision, network_policy_interaction_id,
                open_network_policy_from_workflow, open_plan_review_from_workflow,
            },
        },
        session::{CodeWorkflowEventKind, SessionJsonlStore},
    };
    use tokio_util::sync::CancellationToken;

    let temp = tempfile::tempdir().expect("temp dir");
    let session_root = temp.path().join("session");
    let store = SessionJsonlStore::new(session_root.clone());
    let durability = RuntimeCommandDurability::new(SessionJsonlStore::new(session_root.clone()));
    let boundary = ToolBoundaryRuntime::new(
        Uuid::new_v4(),
        PrincipalContext {
            principal_id: "plan-review-hold-persist".to_string(),
            role: PrincipalRole::Contributor,
        },
        ToolBoundaryPolicy::default_runtime(),
        SecretRedactor::default_runtime(),
        Arc::new(InMemoryAuditSink::default()),
    );
    let (handle, worker) = AgentRuntimeWorker::spawn(
        AgentRuntimeWorkerConfig::new(
            Arc::new(libra::internal::ai::runtime::ExternalTurnTrackingExecutor),
            boundary,
        )
        .with_durability(durability, "repo", "principal")
        .with_durability_command_kind("tui_local_turn"),
    );

    store
        .append_code_workflow_durable(CodeWorkflowEventKind::PlanReviewRequested {
            interaction_id: "review-hold".to_string(),
            plan_id: "plan-hold".to_string(),
            turn_id: "plan-review-turn".to_string(),
            phase1_turn_id: "phase1-turn".to_string(),
            context_id: "review-hold".to_string(),
            revision_of: None,
            prepared_from_network: None,
        })
        .expect("plan review marker");
    store
        .append_code_workflow_durable(CodeWorkflowEventKind::NetworkPolicyRequested {
            interaction_id: network_policy_interaction_id(Some("plan-hold")),
            plan_id: "plan-hold".to_string(),
            turn_id: "network-policy-turn".to_string(),
            default_allow: true,
        })
        .expect("network policy marker before Execute");

    handle
        .track_external_turn(
            TurnRequest::new("session", "plan-review-turn", "Plan review", false),
            CancellationToken::new(),
            Arc::new(AtomicBool::new(false)),
        )
        .await
        .expect("plan review gate tracked");
    handle
        .register_interaction_with_delivery(
            "session",
            "plan-review-turn",
            InteractionState::AwaitingPlanReview {
                interaction_id: "review-hold".to_string(),
            },
            Box::new(PlanReviewAckDelivery::new()),
        )
        .await
        .expect("plan review delivery registered");
    handle
        .respond(
            "session",
            "plan-review-turn",
            InteractionResponse::new("review-hold", PlanReviewDecision::Execute.wire_id()),
        )
        .await
        .expect("Execute settles via CompletedHoldQueued");

    let events: Vec<_> = store
        .load_code_workflow_replay()
        .expect("replay")
        .events
        .into_iter()
        .map(|event| event.event)
        .collect();
    assert!(
        events.iter().any(|event| match event {
            CodeWorkflowEventKind::InteractionResolved {
                interaction_id,
                resolution,
                ..
            }
            | CodeWorkflowEventKind::CommandTerminalSuccessWithInteractionResolved {
                interaction_id,
                resolution,
                ..
            } => {
                interaction_id == "review-hold" && resolution.eq_ignore_ascii_case("execute")
            }
            _ => false,
        }),
        "CompletedHoldQueued Execute must append a durable execute resolution: {events:?}"
    );
    assert_eq!(
        open_plan_review_from_workflow(events.iter()),
        None,
        "plan review must be closed after durable Execute"
    );
    assert_eq!(
        open_network_policy_from_workflow(events.iter()),
        Some((
            network_policy_interaction_id(Some("plan-hold")),
            "plan-hold".to_string(),
            "network-policy-turn".to_string(),
            true,
        )),
        "network gate must become restorable once Execute is durably resolved"
    );
    worker.abort();
}

/// W2-03 r9: after Plan `Execute` returns `CompletedHoldQueued`, a concurrent
/// `submit` must not drain the held queue before the network-policy gate is
/// tracked — otherwise a mutation can start without an explicit network choice.
#[tokio::test]
async fn plan_review_execute_hold_blocks_submit_until_network_gate_parks() {
    use std::sync::{
        Arc,
        atomic::{AtomicBool, AtomicUsize, Ordering},
    };

    use libra::internal::ai::runtime::{
        AgentRuntimeWorker, AgentRuntimeWorkerConfig, InMemoryAuditSink, InteractionResponse,
        InteractionState, PrincipalContext, PrincipalRole, RuntimeExecutionContext,
        RuntimeTurnExecution, RuntimeTurnExecutor, RuntimeWorkerError, SecretRedactor,
        ToolBoundaryPolicy, ToolBoundaryRuntime, TurnRequest,
        phase1::{
            NetworkPolicyAckDelivery, NetworkPolicyDecision, PlanReviewAckDelivery,
            PlanReviewDecision, network_policy_interaction_id,
        },
    };
    use tokio::{
        sync::Notify,
        time::{Duration, sleep, timeout},
    };
    use tokio_util::sync::CancellationToken;

    struct MutatingExecutor {
        started: Arc<AtomicUsize>,
        notify: Arc<Notify>,
    }

    #[async_trait]
    impl RuntimeTurnExecutor for MutatingExecutor {
        async fn execute(
            &self,
            _request: TurnRequest,
            _context: RuntimeExecutionContext,
        ) -> Result<RuntimeTurnExecution, RuntimeWorkerError> {
            self.started.fetch_add(1, Ordering::SeqCst);
            self.notify.notify_one();
            Ok(RuntimeTurnExecution::Completed {
                summary: "mutation applied".to_string(),
            })
        }
    }

    let started = Arc::new(AtomicUsize::new(0));
    let notify = Arc::new(Notify::new());
    let executor = Arc::new(MutatingExecutor {
        started: Arc::clone(&started),
        notify: Arc::clone(&notify),
    });
    let boundary = ToolBoundaryRuntime::new(
        Uuid::new_v4(),
        PrincipalContext {
            principal_id: "plan-review-hold-submit-race".to_string(),
            role: PrincipalRole::Contributor,
        },
        ToolBoundaryPolicy::default_runtime(),
        SecretRedactor::default_runtime(),
        Arc::new(InMemoryAuditSink::default()),
    );
    let (handle, worker) =
        AgentRuntimeWorker::spawn(AgentRuntimeWorkerConfig::new(executor, boundary));

    handle
        .track_external_turn(
            TurnRequest::new("session", "phase1-turn", "plan workflow", true),
            CancellationToken::new(),
            Arc::new(AtomicBool::new(true)),
        )
        .await
        .expect("phase1 turn tracked");
    handle
        .submit(TurnRequest::new(
            "session",
            "mutating-before-execute",
            "apply_patch",
            true,
        ))
        .await
        .expect("mutation queues behind phase1");
    handle
        .finish_external_turn(
            "session",
            "phase1-turn",
            Ok(RuntimeTurnExecution::CompletedHoldQueued {
                summary: "plan draft ready".to_string(),
            }),
        )
        .await
        .expect("phase1 hold");
    handle
        .track_external_turn(
            TurnRequest::new("session", "plan-review-turn", "Plan review", false),
            CancellationToken::new(),
            Arc::new(AtomicBool::new(false)),
        )
        .await
        .expect("plan review parks");
    handle
        .register_interaction_with_delivery(
            "session",
            "plan-review-turn",
            InteractionState::AwaitingPlanReview {
                interaction_id: "review-race".to_string(),
            },
            Box::new(PlanReviewAckDelivery::new()),
        )
        .await
        .expect("plan review registered");
    handle
        .respond(
            "session",
            "plan-review-turn",
            InteractionResponse::new("review-race", PlanReviewDecision::Execute.wire_id()),
        )
        .await
        .expect("Execute holds for network gate");

    // Race window: a late submit after Execute must queue without starting the
    // held mutation or this new one before the network gate is parked.
    handle
        .submit(TurnRequest::new(
            "session",
            "mutating-after-execute",
            "apply_patch",
            true,
        ))
        .await
        .expect("post-Execute submit is accepted into the held queue");
    sleep(Duration::from_millis(50)).await;
    assert_eq!(
        started.load(Ordering::SeqCst),
        0,
        "submit after Execute must not start mutations before the network gate parks"
    );

    let network_id = network_policy_interaction_id(Some("plan-race"));
    handle
        .track_external_turn(
            TurnRequest::new("session", "network-policy-turn", "network policy", false),
            CancellationToken::new(),
            Arc::new(AtomicBool::new(false)),
        )
        .await
        .expect("network gate parks");
    handle
        .register_interaction_with_delivery(
            "session",
            "network-policy-turn",
            InteractionState::AwaitingNetworkPolicy {
                interaction_id: network_id.clone(),
            },
            Box::new(NetworkPolicyAckDelivery::new()),
        )
        .await
        .expect("network delivery registered");
    sleep(Duration::from_millis(50)).await;
    assert_eq!(
        started.load(Ordering::SeqCst),
        0,
        "network gate must keep mutations fenced"
    );

    handle
        .respond(
            "session",
            "network-policy-turn",
            InteractionResponse::new(&network_id, NetworkPolicyDecision::Allow.wire_id()),
        )
        .await
        .expect("network allow");
    timeout(Duration::from_secs(2), notify.notified())
        .await
        .expect("held mutations eventually start");
    assert!(started.load(Ordering::SeqCst) >= 1);
    worker.abort();
}

/// W2-03 r12: end-to-end crash/restart for the App-owned Execute → network
/// marker ordering. Simulates the durable rows `App` writes, drops the live
/// worker before `register_local_runtime_network_policy`, then recovers the
/// gate from the session store and proves mutations stay fenced until Allow.
#[tokio::test]
async fn plan_review_network_gate_survives_worker_restart_after_execute() {
    use std::sync::{
        Arc,
        atomic::{AtomicBool, AtomicUsize, Ordering},
    };

    use libra::internal::ai::{
        runtime::{
            AgentRuntimeWorker, AgentRuntimeWorkerConfig, InMemoryAuditSink, InteractionResponse,
            InteractionState, PrincipalContext, PrincipalRole, RuntimeCommandDurability,
            RuntimeExecutionContext, RuntimeTurnExecution, RuntimeTurnExecutor, RuntimeWorkerError,
            SecretRedactor, ToolBoundaryPolicy, ToolBoundaryRuntime, TurnRequest,
            phase1::{
                NetworkPolicyAckDelivery, NetworkPolicyDecision, PlanReviewAckDelivery,
                PlanReviewDecision, network_policy_interaction_id,
                open_network_policy_from_workflow, open_plan_review_from_workflow,
            },
        },
        session::{CodeWorkflowEventKind, SessionJsonlStore},
    };
    use tokio::{
        sync::Notify,
        time::{Duration, sleep, timeout},
    };
    use tokio_util::sync::CancellationToken;

    struct MutatingExecutor {
        started: Arc<AtomicUsize>,
        notify: Arc<Notify>,
    }

    #[async_trait]
    impl RuntimeTurnExecutor for MutatingExecutor {
        async fn execute(
            &self,
            _request: TurnRequest,
            _context: RuntimeExecutionContext,
        ) -> Result<RuntimeTurnExecution, RuntimeWorkerError> {
            self.started.fetch_add(1, Ordering::SeqCst);
            self.notify.notify_one();
            Ok(RuntimeTurnExecution::Completed {
                summary: "mutation applied".to_string(),
            })
        }
    }

    let temp = tempfile::tempdir().expect("temp dir");
    let session_root = temp.path().join("session");
    let store = SessionJsonlStore::new(session_root.clone());
    let started = Arc::new(AtomicUsize::new(0));
    let notify = Arc::new(Notify::new());
    let boundary = || {
        ToolBoundaryRuntime::new(
            Uuid::new_v4(),
            PrincipalContext {
                principal_id: "plan-review-restart".to_string(),
                role: PrincipalRole::Contributor,
            },
            ToolBoundaryPolicy::default_runtime(),
            SecretRedactor::default_runtime(),
            Arc::new(InMemoryAuditSink::default()),
        )
    };

    // Process 1: Phase 1 hold → Plan review → App-ordered network marker → Execute.
    let durability = RuntimeCommandDurability::new(SessionJsonlStore::new(session_root.clone()));
    let (handle, worker) = AgentRuntimeWorker::spawn(
        AgentRuntimeWorkerConfig::new(
            Arc::new(MutatingExecutor {
                started: Arc::clone(&started),
                notify: Arc::clone(&notify),
            }),
            boundary(),
        )
        .with_durability(durability, "repo", "principal")
        .with_durability_command_kind("tui_local_turn"),
    );
    handle
        .track_external_turn(
            TurnRequest::new("session", "phase1-turn", "plan workflow", true),
            CancellationToken::new(),
            Arc::new(AtomicBool::new(true)),
        )
        .await
        .expect("phase1 admitted");
    handle
        .submit(TurnRequest::new(
            "session",
            "mutating-turn",
            "apply_patch",
            true,
        ))
        .await
        .expect("mutation queued");
    handle
        .finish_external_turn(
            "session",
            "phase1-turn",
            Ok(RuntimeTurnExecution::CompletedHoldQueued {
                summary: "plan draft ready".to_string(),
            }),
        )
        .await
        .expect("phase1 hold");
    store
        .append_code_workflow_durable(CodeWorkflowEventKind::PlanReviewRequested {
            interaction_id: "review-restart".to_string(),
            plan_id: "plan-restart".to_string(),
            turn_id: "plan-review-turn".to_string(),
            phase1_turn_id: "phase1-turn".to_string(),
            context_id: "review-restart".to_string(),
            revision_of: None,
            prepared_from_network: None,
        })
        .expect("plan review marker");
    handle
        .track_external_turn(
            TurnRequest::new("session", "plan-review-turn", "Plan review", false),
            CancellationToken::new(),
            Arc::new(AtomicBool::new(false)),
        )
        .await
        .expect("plan review parks");
    handle
        .register_interaction_with_delivery(
            "session",
            "plan-review-turn",
            InteractionState::AwaitingPlanReview {
                interaction_id: "review-restart".to_string(),
            },
            Box::new(PlanReviewAckDelivery::new()),
        )
        .await
        .expect("plan review registered");

    let network_id = network_policy_interaction_id(Some("plan-restart"));
    // App writes the network marker *before* Execute settles.
    store
        .append_code_workflow_durable(CodeWorkflowEventKind::NetworkPolicyRequested {
            interaction_id: network_id.clone(),
            plan_id: "plan-restart".to_string(),
            turn_id: "network-policy-turn".to_string(),
            default_allow: false,
        })
        .expect("network marker before Execute");
    handle
        .respond(
            "session",
            "plan-review-turn",
            InteractionResponse::new("review-restart", PlanReviewDecision::Execute.wire_id()),
        )
        .await
        .expect("Execute settles");
    // Crash before register_local_runtime_network_policy.
    worker.abort();
    sleep(Duration::from_millis(20)).await;
    assert_eq!(started.load(Ordering::SeqCst), 0);

    let events = |root: &std::path::Path| {
        SessionJsonlStore::new(root.to_path_buf())
            .load_code_workflow_replay()
            .expect("replay")
            .events
            .into_iter()
            .map(|event| event.event)
            .collect::<Vec<_>>()
    };
    let replayed = events(&session_root);
    assert_eq!(
        open_plan_review_from_workflow(replayed.iter()),
        None,
        "Execute must have closed the plan review on disk"
    );
    assert_eq!(
        open_network_policy_from_workflow(replayed.iter()),
        Some((
            network_id.clone(),
            "plan-restart".to_string(),
            "network-policy-turn".to_string(),
            false,
        )),
        "network gate must survive restart after Execute"
    );

    // Process 2: restore gate and keep mutations fenced until Allow.
    let durability = RuntimeCommandDurability::new(SessionJsonlStore::new(session_root.clone()));
    let (handle, worker) = AgentRuntimeWorker::spawn(
        AgentRuntimeWorkerConfig::new(
            Arc::new(MutatingExecutor {
                started: Arc::clone(&started),
                notify: Arc::clone(&notify),
            }),
            boundary(),
        )
        .with_durability(durability, "repo", "principal")
        .with_durability_command_kind("tui_local_turn"),
    );
    handle
        .track_external_turn(
            TurnRequest::new("session", "network-policy-turn", "network policy", false),
            CancellationToken::new(),
            Arc::new(AtomicBool::new(false)),
        )
        .await
        .expect("restored network gate parks");
    handle
        .register_interaction_with_delivery(
            "session",
            "network-policy-turn",
            InteractionState::AwaitingNetworkPolicy {
                interaction_id: network_id.clone(),
            },
            Box::new(NetworkPolicyAckDelivery::new()),
        )
        .await
        .expect("network delivery registered");
    handle
        .submit(TurnRequest::new(
            "session",
            "mutating-after-restore",
            "apply_patch",
            true,
        ))
        .await
        .expect("post-restore mutation queues");
    sleep(Duration::from_millis(50)).await;
    assert_eq!(
        started.load(Ordering::SeqCst),
        0,
        "restored network gate must fence execution"
    );
    handle
        .respond(
            "session",
            "network-policy-turn",
            InteractionResponse::new(&network_id, NetworkPolicyDecision::Allow.wire_id()),
        )
        .await
        .expect("network allow after restore");
    timeout(Duration::from_secs(2), notify.notified())
        .await
        .expect("mutations start after restored Allow");
    assert!(started.load(Ordering::SeqCst) >= 1);
    worker.abort();
}

/// W2-03: Modify/Cancel on the Plan review must discard the mutations queued
/// under the review fence rather than release them — the developer never
/// approved the plan those turns would execute.
#[tokio::test]
async fn plan_review_non_execute_discards_queued_mutations() {
    use std::sync::{
        Arc,
        atomic::{AtomicBool, AtomicUsize, Ordering},
    };

    use libra::internal::ai::runtime::{
        AgentRuntimeWorker, AgentRuntimeWorkerConfig, InMemoryAuditSink, InteractionResponse,
        InteractionState, PrincipalContext, PrincipalRole, RuntimeExecutionContext,
        RuntimeTurnExecution, RuntimeTurnExecutor, RuntimeWorkerError, SecretRedactor,
        ToolBoundaryPolicy, ToolBoundaryRuntime, TurnRequest,
        phase1::{PlanReviewAckDelivery, PlanReviewDecision},
    };
    use tokio::time::{Duration, sleep};
    use tokio_util::sync::CancellationToken;

    struct MutatingExecutor {
        started: Arc<AtomicUsize>,
    }

    #[async_trait]
    impl RuntimeTurnExecutor for MutatingExecutor {
        async fn execute(
            &self,
            _request: TurnRequest,
            _context: RuntimeExecutionContext,
        ) -> Result<RuntimeTurnExecution, RuntimeWorkerError> {
            self.started.fetch_add(1, Ordering::SeqCst);
            Ok(RuntimeTurnExecution::Completed {
                summary: "mutation applied".to_string(),
            })
        }
    }

    for decision in [PlanReviewDecision::Revise, PlanReviewDecision::Cancel] {
        let started = Arc::new(AtomicUsize::new(0));
        let executor = Arc::new(MutatingExecutor {
            started: Arc::clone(&started),
        });
        let boundary = ToolBoundaryRuntime::new(
            Uuid::new_v4(),
            PrincipalContext {
                principal_id: format!("plan-review-{}-fence", decision.wire_id()),
                role: PrincipalRole::Contributor,
            },
            ToolBoundaryPolicy::default_runtime(),
            SecretRedactor::default_runtime(),
            Arc::new(InMemoryAuditSink::default()),
        );
        let (handle, worker) =
            AgentRuntimeWorker::spawn(AgentRuntimeWorkerConfig::new(executor, boundary));

        handle
            .track_external_turn(
                TurnRequest::new("session", "plan-review-turn", "Plan review", false),
                CancellationToken::new(),
                Arc::new(AtomicBool::new(false)),
            )
            .await
            .expect("plan review gate tracked");
        handle
            .register_interaction_with_delivery(
                "session",
                "plan-review-turn",
                InteractionState::AwaitingPlanReview {
                    interaction_id: "plan-7".to_string(),
                },
                Box::new(PlanReviewAckDelivery::new()),
            )
            .await
            .expect("worker owns the Plan review interaction");
        handle
            .submit(TurnRequest::new(
                "session",
                "mutating-turn",
                "apply_patch",
                true,
            ))
            .await
            .expect("mutating turn is accepted into the queue");
        assert_eq!(started.load(Ordering::SeqCst), 0);

        handle
            .respond(
                "session",
                "plan-review-turn",
                InteractionResponse::new("plan-7", decision.wire_id()),
            )
            .await
            .expect("non-execute decision resolves the review");
        sleep(Duration::from_millis(50)).await;
        assert_eq!(
            started.load(Ordering::SeqCst),
            0,
            "{} must discard queued mutations, not release them",
            decision.wire_id()
        );
        let snapshot = handle
            .snapshot("session")
            .await
            .expect("snapshot after non-execute decision");
        assert_eq!(snapshot.interaction, InteractionState::Completed);
        assert_eq!(
            snapshot.queued_turns,
            0,
            "{} must empty the queue of fenced mutations",
            decision.wire_id()
        );
        worker.abort();
    }
}

/// W1-04: the legacy TUI tool loop is externally executed, but its cancel
/// request must still enter the runtime before the adapter signals its local
/// cooperative token. A mutation marker shared with the runtime turns an
/// ambiguous local cancellation into the durable reconciliation fence.
#[tokio::test]
async fn external_legacy_turn_cancel_uses_runtime_reconciliation() {
    use std::sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    };

    use libra::internal::ai::{
        runtime::{
            AgentRuntimeWorker, AgentRuntimeWorkerConfig, ExternalTurnTrackingExecutor,
            InMemoryAuditSink, InteractionState, PrincipalContext, PrincipalRole,
            RuntimeCommandDurability, RuntimeWorkerError, SecretRedactor, ToolBoundaryPolicy,
            ToolBoundaryRuntime, TurnRequest,
        },
        session::{CodeCommandIdentity, CodeCommandRecovery, CodeCommandStatus, SessionJsonlStore},
    };
    use tokio_util::sync::CancellationToken;

    let boundary = ToolBoundaryRuntime::new(
        Uuid::new_v4(),
        PrincipalContext {
            principal_id: "tui-runtime-contract-test".to_string(),
            role: PrincipalRole::Contributor,
        },
        ToolBoundaryPolicy::default_runtime(),
        SecretRedactor::default_runtime(),
        Arc::new(InMemoryAuditSink::default()),
    );
    let temp = tempfile::TempDir::new().expect("temporary legacy session JSONL root");
    let durability =
        RuntimeCommandDurability::new(SessionJsonlStore::new(temp.path().join("session")));
    let (handle, worker) = AgentRuntimeWorker::spawn(
        AgentRuntimeWorkerConfig::new(Arc::new(ExternalTurnTrackingExecutor), boundary)
            .with_durability(durability.clone(), "tui-contract-repo", "tui-local")
            .with_durability_command_kind("tui_local_turn"),
    );
    let cancellation = CancellationToken::new();
    let mutation_started = Arc::new(AtomicBool::new(true));
    handle
        .track_external_turn(
            TurnRequest::new("session", "turn-1", "apply patch", true),
            cancellation.clone(),
            Arc::clone(&mutation_started),
        )
        .await
        .expect("external legacy turn admitted");

    handle
        .cancel("session", "turn-1")
        .await
        .expect("runtime accepted legacy cancellation");
    assert!(
        !cancellation.is_cancelled(),
        "a started mutation must not be locally aborted by runtime cancellation"
    );
    assert_eq!(
        handle
            .snapshot("session")
            .await
            .expect("runtime snapshot")
            .interaction,
        InteractionState::Cancelling
    );

    mutation_started.store(false, Ordering::Release);
    let finalization = handle
        .finish_external_turn("session", "turn-1", Err(RuntimeWorkerError::Cancelled))
        .await;
    assert!(
        matches!(
            finalization,
            Err(RuntimeWorkerError::ReconciliationRequired { .. })
        ),
        "the adapter must observe the worker's durable indeterminate fence"
    );
    assert!(matches!(
        handle
            .snapshot("session")
            .await
            .expect("reconciled snapshot")
            .interaction,
        InteractionState::IndeterminateSideEffect { .. }
    ));
    assert!(matches!(
        durability
            .session_store()
            .recover_code_command(&CodeCommandIdentity::new(
                "tui-contract-repo",
                "session",
                "tui-local",
                "turn-1",
            ))
            .expect("durable legacy cancellation result"),
        CodeCommandRecovery::Existing {
            status: CodeCommandStatus::Indeterminate { .. }
        }
    ));
    worker.abort();
}

/// W1-08: runtime shutdown is a structured, idempotent lifecycle operation.
/// It stops admission before cancelling a read-only turn, drains queued work,
/// and makes concurrent callers observe one terminal outcome. A non-cooperative
/// executor must return an actionable timeout rather than leaving shutdown
/// indefinitely pending.
#[tokio::test]
async fn runtime_shutdown_releases_resources() {
    use std::sync::Arc;

    use libra::internal::ai::runtime::{
        AgentRuntimeWorker, AgentRuntimeWorkerConfig, InMemoryAuditSink, PrincipalContext,
        PrincipalRole, RuntimeExecutionContext, RuntimeShutdownError, RuntimeTurnExecution,
        RuntimeTurnExecutor, RuntimeWorkerError, SecretRedactor, ToolBoundaryPolicy,
        ToolBoundaryRuntime, TurnRequest,
    };
    use tokio::{
        sync::Notify,
        time::{Duration, timeout},
    };

    fn boundary() -> ToolBoundaryRuntime {
        ToolBoundaryRuntime::new(
            Uuid::new_v4(),
            PrincipalContext {
                principal_id: "runtime-shutdown-test".to_string(),
                role: PrincipalRole::Contributor,
            },
            ToolBoundaryPolicy::default_runtime(),
            SecretRedactor::default_runtime(),
            Arc::new(InMemoryAuditSink::default()),
        )
    }

    struct CooperativeExecutor {
        started: Arc<Notify>,
        cancellation_seen: Arc<Notify>,
        release: Arc<Notify>,
    }

    #[async_trait]
    impl RuntimeTurnExecutor for CooperativeExecutor {
        async fn execute(
            &self,
            _request: TurnRequest,
            context: RuntimeExecutionContext,
        ) -> Result<RuntimeTurnExecution, RuntimeWorkerError> {
            self.started.notify_one();
            context.cancellation().cancelled().await;
            self.cancellation_seen.notify_one();
            self.release.notified().await;
            Err(RuntimeWorkerError::Cancelled)
        }
    }

    let started = Arc::new(Notify::new());
    let cancellation_seen = Arc::new(Notify::new());
    let release = Arc::new(Notify::new());
    let executor = Arc::new(CooperativeExecutor {
        started: Arc::clone(&started),
        cancellation_seen: Arc::clone(&cancellation_seen),
        release: Arc::clone(&release),
    });
    let mut config = AgentRuntimeWorkerConfig::new(executor, boundary());
    config.shutdown_timeout = Duration::from_secs(1);
    let (handle, worker) = AgentRuntimeWorker::spawn(config);

    handle
        .submit(TurnRequest::new("session", "active", "read", false))
        .await
        .expect("active turn accepted");
    handle
        .submit(TurnRequest::new("session", "queued", "later", false))
        .await
        .expect("queued turn accepted");
    timeout(Duration::from_secs(1), started.notified())
        .await
        .expect("active turn started");

    let first_shutdown = {
        let handle = handle.clone();
        tokio::spawn(async move { handle.shutdown().await })
    };
    timeout(Duration::from_secs(1), cancellation_seen.notified())
        .await
        .expect("shutdown cooperatively cancels the active turn");
    assert!(matches!(
        handle
            .submit(TurnRequest::new(
                "session",
                "rejected",
                "after shutdown",
                false
            ))
            .await,
        Err(RuntimeWorkerError::ShuttingDown)
    ));

    let second_shutdown = {
        let handle = handle.clone();
        tokio::spawn(async move { handle.shutdown().await })
    };
    tokio::task::yield_now().await;
    release.notify_one();
    assert!(first_shutdown.await.expect("first shutdown task").is_ok());
    assert!(second_shutdown.await.expect("second shutdown task").is_ok());
    timeout(Duration::from_secs(1), worker)
        .await
        .expect("worker exits after shutdown")
        .expect("worker task does not panic");
    assert!(
        handle.shutdown().await.is_ok(),
        "a completed shutdown must stay idempotent after the worker task exits"
    );

    let drop_started = Arc::new(Notify::new());
    let drop_cancellation_seen = Arc::new(Notify::new());
    let drop_release = Arc::new(Notify::new());
    let drop_executor = Arc::new(CooperativeExecutor {
        started: Arc::clone(&drop_started),
        cancellation_seen: Arc::clone(&drop_cancellation_seen),
        release: Arc::clone(&drop_release),
    });
    let (drop_handle, drop_worker) =
        AgentRuntimeWorker::spawn(AgentRuntimeWorkerConfig::new(drop_executor, boundary()));
    drop_handle
        .submit(TurnRequest::new("dropped", "active", "read", false))
        .await
        .expect("last-handle-drop turn accepted");
    timeout(Duration::from_secs(1), drop_started.notified())
        .await
        .expect("last-handle-drop turn started");
    drop(drop_handle);
    timeout(Duration::from_secs(1), drop_cancellation_seen.notified())
        .await
        .expect("dropping the last runtime handle starts the same shutdown path");
    drop_release.notify_one();
    timeout(Duration::from_secs(1), drop_worker)
        .await
        .expect("worker exits after last handle drop")
        .expect("last-handle-drop worker task does not panic");

    struct MutatingExecutor {
        started: Arc<Notify>,
        cancellation_seen: Arc<Notify>,
        release: Arc<Notify>,
    }

    #[async_trait]
    impl RuntimeTurnExecutor for MutatingExecutor {
        async fn execute(
            &self,
            _request: TurnRequest,
            context: RuntimeExecutionContext,
        ) -> Result<RuntimeTurnExecution, RuntimeWorkerError> {
            context.mark_mutation_started();
            self.started.notify_one();
            let cancellation = context.cancellation();
            tokio::select! {
                _ = cancellation.cancelled() => {
                    self.cancellation_seen.notify_one();
                    Err(RuntimeWorkerError::Cancelled)
                }
                _ = self.release.notified() => Ok(RuntimeTurnExecution::Completed {
                    summary: "mutation completed determinately".to_string(),
                }),
            }
        }
    }

    let mutation_started = Arc::new(Notify::new());
    let mutation_cancellation_seen = Arc::new(Notify::new());
    let mutation_release = Arc::new(Notify::new());
    let mutating_executor = Arc::new(MutatingExecutor {
        started: Arc::clone(&mutation_started),
        cancellation_seen: Arc::clone(&mutation_cancellation_seen),
        release: Arc::clone(&mutation_release),
    });
    let (mutating_handle, mutating_worker) =
        AgentRuntimeWorker::spawn(AgentRuntimeWorkerConfig::new(mutating_executor, boundary()));
    mutating_handle
        .submit(TurnRequest::new("mutating", "active", "write", true))
        .await
        .expect("mutating turn accepted");
    timeout(Duration::from_secs(1), mutation_started.notified())
        .await
        .expect("mutating turn started");
    let mutating_shutdown = {
        let handle = mutating_handle.clone();
        tokio::spawn(async move { handle.shutdown().await })
    };
    assert!(
        timeout(
            Duration::from_millis(100),
            mutation_cancellation_seen.notified()
        )
        .await
        .is_err(),
        "shutdown must not cancel a mutation after its side effect begins"
    );
    mutation_release.notify_one();
    assert!(
        mutating_shutdown
            .await
            .expect("mutating shutdown task")
            .is_ok(),
        "a determinate mutation completion releases shutdown"
    );
    timeout(Duration::from_secs(1), mutating_worker)
        .await
        .expect("worker exits after determinate mutation")
        .expect("mutating worker task does not panic");

    struct StuckExecutor;

    #[async_trait]
    impl RuntimeTurnExecutor for StuckExecutor {
        async fn execute(
            &self,
            _request: TurnRequest,
            _context: RuntimeExecutionContext,
        ) -> Result<RuntimeTurnExecution, RuntimeWorkerError> {
            std::future::pending().await
        }
    }

    let mut stuck_config = AgentRuntimeWorkerConfig::new(Arc::new(StuckExecutor), boundary());
    stuck_config.shutdown_timeout = Duration::from_millis(20);
    let (stuck_handle, stuck_worker) = AgentRuntimeWorker::spawn(stuck_config);
    stuck_handle
        .submit(TurnRequest::new("stuck", "active", "read", false))
        .await
        .expect("stuck turn accepted");
    let timeout_error = stuck_handle
        .shutdown()
        .await
        .expect_err("non-cooperative executor must hit the shutdown deadline");
    assert!(matches!(
        &timeout_error,
        RuntimeShutdownError::TimedOut { unreleased_resources }
            if unreleased_resources == &vec!["runtime_turn".to_string()]
    ));
    timeout(Duration::from_secs(1), stuck_worker)
        .await
        .expect("timed-out worker exits")
        .expect("timed-out worker task does not panic");
    assert_eq!(
        stuck_handle.shutdown().await,
        Err(timeout_error),
        "a completed failed shutdown must preserve its diagnostic for repeat callers"
    );

    struct StuckMutatingExecutor {
        started: Arc<Notify>,
    }

    #[async_trait]
    impl RuntimeTurnExecutor for StuckMutatingExecutor {
        async fn execute(
            &self,
            _request: TurnRequest,
            context: RuntimeExecutionContext,
        ) -> Result<RuntimeTurnExecution, RuntimeWorkerError> {
            context.mark_mutation_started();
            self.started.notify_one();
            std::future::pending().await
        }
    }

    let stuck_mutation_started = Arc::new(Notify::new());
    let mutating_stuck_executor = Arc::new(StuckMutatingExecutor {
        started: Arc::clone(&stuck_mutation_started),
    });
    let mutating_stuck_config = AgentRuntimeWorkerConfig::new(mutating_stuck_executor, boundary());
    let (mutating_stuck_handle, mutating_stuck_worker) =
        AgentRuntimeWorker::spawn(AgentRuntimeWorkerConfig {
            shutdown_timeout: Duration::from_millis(20),
            ..mutating_stuck_config
        });
    mutating_stuck_handle
        .submit(TurnRequest::new("stuck-mutation", "active", "write", true))
        .await
        .expect("stuck mutating turn accepted");
    timeout(Duration::from_secs(1), stuck_mutation_started.notified())
        .await
        .expect("stuck mutation reached dispatch boundary");
    assert!(matches!(
        mutating_stuck_handle.shutdown().await,
        Err(RuntimeShutdownError::TimedOut { unreleased_resources })
            if unreleased_resources == vec!["mutating_runtime_turn_reconciliation".to_string()]
    ));
    timeout(Duration::from_secs(1), mutating_stuck_worker)
        .await
        .expect("timed-out mutating worker exits")
        .expect("timed-out mutating worker task does not panic");
}

/// W1-08: SIGINT/SIGTERM-shaped process shutdown and half-initialized startup
/// failure share one [`LifecycleShutdownOwner`] deadline/result contract.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn runtime_shutdown_on_signal_and_startup_failure() {
    use std::sync::Arc;

    use libra::internal::ai::runtime::{
        LifecycleShutdownError, LifecycleShutdownOwner, LifecycleStepError, lifecycle_resource,
    };
    use tokio::{
        sync::Notify,
        time::{Duration, timeout},
    };

    // Signal path: runtime finishes, but a stuck listener reports its category
    // under the shared owner deadline (same contract as SIGINT/SIGTERM cleanup).
    let signal_owner = LifecycleShutdownOwner::with_timeout(Duration::from_millis(40));
    signal_owner
        .push_step(lifecycle_resource::RUNTIME_TURN, async { Ok(()) })
        .await;
    signal_owner
        .push_step(lifecycle_resource::CONTROLLER_LEASE, async { Ok(()) })
        .await;
    signal_owner
        .push_step(lifecycle_resource::MCP_SERVER, async { Ok(()) })
        .await;
    signal_owner
        .push_step(lifecycle_resource::CONTROL_LOCK, async { Ok(()) })
        .await;
    signal_owner
        .push_step(lifecycle_resource::WEB_SERVER, async {
            tokio::time::sleep(Duration::from_secs(2)).await;
            Ok(())
        })
        .await;

    let signal_first = {
        let owner = signal_owner.clone();
        tokio::spawn(async move { owner.shutdown().await })
    };
    let signal_second = {
        let owner = signal_owner.clone();
        tokio::spawn(async move { owner.shutdown().await })
    };
    let signal_a = signal_first.await.expect("join signal shutdown");
    let signal_b = signal_second.await.expect("join repeated signal shutdown");
    assert_eq!(signal_a, signal_b);
    assert!(matches!(
        signal_a,
        Err(LifecycleShutdownError::TimedOut {
            unreleased_resources
        }) if unreleased_resources == vec![lifecycle_resource::WEB_SERVER.to_string()]
    ));

    // Startup-failure path: only the resources that were actually started are
    // registered; a stuck managed child still surfaces under the same owner.
    let started = Arc::new(Notify::new());
    let startup_owner = LifecycleShutdownOwner::with_timeout(Duration::from_millis(30));
    startup_owner
        .push_step(lifecycle_resource::RUNTIME_TURN, async { Ok(()) })
        .await;
    startup_owner
        .push_step(lifecycle_resource::TEMP_FILE, async { Ok(()) })
        .await;
    {
        let started = Arc::clone(&started);
        startup_owner
            .push_step(lifecycle_resource::MANAGED_CODEX_CHILD, async move {
                started.notify_one();
                tokio::time::sleep(Duration::from_secs(2)).await;
                Ok(())
            })
            .await;
    }

    let startup = {
        let owner = startup_owner.clone();
        tokio::spawn(async move { owner.shutdown().await })
    };
    timeout(Duration::from_secs(1), started.notified())
        .await
        .expect("startup-failure cleanup reached the managed child step");
    let startup_result = startup.await.expect("join startup shutdown");
    assert!(matches!(
        &startup_result,
        Err(LifecycleShutdownError::TimedOut {
            unreleased_resources
        }) if unreleased_resources.as_slice()
            == [lifecycle_resource::MANAGED_CODEX_CHILD]
    ));
    assert_eq!(
        startup_owner.shutdown().await,
        startup_result,
        "startup-failure shutdown must stay idempotent"
    );

    // Nested runtime timeout categories propagate through a lifecycle step.
    let nested = LifecycleShutdownOwner::with_timeout(Duration::from_millis(50));
    nested
        .push_step(lifecycle_resource::RUNTIME_TURN, async {
            Err(LifecycleStepError::timed_out_with([
                lifecycle_resource::MUTATING_RUNTIME_TURN_RECONCILIATION,
            ]))
        })
        .await;
    nested
        .push_step(lifecycle_resource::WEB_SERVER, async { Ok(()) })
        .await;
    assert!(matches!(
        nested.shutdown().await,
        Err(LifecycleShutdownError::TimedOut {
            unreleased_resources
        }) if unreleased_resources
            == vec![lifecycle_resource::MUTATING_RUNTIME_TURN_RECONCILIATION.to_string()]
    ));
}

/// W2-11: failure classification, retry bounds, repair interaction, and
/// wire-safe evidence are runtime-owned. Re-admission remains covered by the
/// adjacent W2-04 queue/gate contract.
#[test]
fn plan_execution_repair_loop() {
    use libra::internal::ai::{
        runtime::{
            ExecutionFailureRevision, InteractionState, MAX_AUTOMATIC_PLAN_REPAIR_ATTEMPTS,
            PlanExecutionRepairService, PlanExecutionRepairState,
            open_plan_execution_repair_from_workflow,
        },
        session::{CodeWorkflowEventKind, jsonl::SessionJsonlStore},
    };

    let service = PlanExecutionRepairService;
    assert_eq!(
        PlanExecutionRepairService::classify_failure_signals(true, false, false),
        ExecutionFailureRevision::PlanRevision
    );
    assert_eq!(
        PlanExecutionRepairService::classify_failure_signals(true, true, false),
        ExecutionFailureRevision::IntentSpecRevision
    );
    assert_eq!(
        PlanExecutionRepairService::classify_failure_signals(false, false, false),
        ExecutionFailureRevision::ManualAction
    );

    let waiting = service.after_failure("repair-1", None, Some("orchestrator unavailable"), 2, 2);
    assert!(matches!(
        waiting,
        PlanExecutionRepairState::ManualAction { .. }
    ));

    // The plan route starts automatically while it remains below the requested
    // limit, then exposes a runtime interaction that can be continued or
    // cancelled without an adapter-owned transition.
    let automatic = PlanExecutionRepairState::AutomaticRepair {
        route: ExecutionFailureRevision::PlanRevision,
        evidence: libra::internal::ai::runtime::ExecutionFailureEvidence {
            output: "Decision: Abandon.".to_string(),
            diagnostics: vec!["test failed".to_string()],
            attempt: 1,
            max_attempts: 2,
        },
    };
    assert_eq!(automatic.evidence().diagnostics, ["test failed"]);
    let waiting = PlanExecutionRepairState::AwaitingUser {
        interaction_id: "repair-1".to_string(),
        route: ExecutionFailureRevision::PlanRevision,
        evidence: libra::internal::ai::runtime::ExecutionFailureEvidence {
            output: "Decision: Abandon.".to_string(),
            diagnostics: vec!["test failed".to_string()],
            attempt: 2,
            max_attempts: 2,
        },
    };
    assert!(matches!(
        waiting.interaction_state(),
        Some(InteractionState::AwaitingPlanRepair { .. })
    ));
    let configured_limit_continue = service.respond(waiting.clone(), "continue", None);
    assert!(
        matches!(
            configured_limit_continue,
            PlanExecutionRepairState::AwaitingUser {
                evidence,
                ..
            } if evidence.attempt == 2 && evidence.max_attempts == 2
        ),
        "Continue without an explicit higher limit must preserve the configured retry cap"
    );
    assert!(matches!(
        service.respond(waiting.clone(), "continue", Some(3)),
        PlanExecutionRepairState::AutomaticRepair { .. }
    ));
    assert!(matches!(
        service.respond(waiting, "cancel", None),
        PlanExecutionRepairState::Cancelled { .. }
    ));
    assert!(!PlanExecutionRepairService::should_auto_repair(
        ExecutionFailureRevision::PlanRevision,
        MAX_AUTOMATIC_PLAN_REPAIR_ATTEMPTS,
        MAX_AUTOMATIC_PLAN_REPAIR_ATTEMPTS
    ));
    let hard_cap_waiting = PlanExecutionRepairState::AwaitingUser {
        interaction_id: "repair-hard-cap".to_string(),
        route: ExecutionFailureRevision::PlanRevision,
        evidence: libra::internal::ai::runtime::ExecutionFailureEvidence {
            output: "Decision: Abandon.".to_string(),
            diagnostics: vec!["verification failed".to_string()],
            attempt: MAX_AUTOMATIC_PLAN_REPAIR_ATTEMPTS,
            max_attempts: MAX_AUTOMATIC_PLAN_REPAIR_ATTEMPTS,
        },
    };
    let hard_cap_continue = service.respond(
        hard_cap_waiting,
        "continue",
        Some(MAX_AUTOMATIC_PLAN_REPAIR_ATTEMPTS),
    );
    assert!(
        matches!(
            hard_cap_continue.interaction_state(),
            Some(InteractionState::AwaitingPlanRepair { interaction_id })
                if interaction_id == "repair-hard-cap"
        ),
        "a Continue rejected at the hard cap must leave its repair gate actionable"
    );

    let evidence = PlanExecutionRepairService::failure_evidence(
        None,
        Some("Orchestrator failed: token: top-secret"),
        1,
        2,
    );
    assert!(
        !evidence.output.contains("top-secret"),
        "remote repair evidence must use the runtime redactor"
    );

    // The worker snapshot is intentionally ephemeral. A replacement
    // SessionQueue must recover this durable marker and re-park the same
    // Continue/Cancel interaction before admitting any repaired execution.
    let temp = tempfile::tempdir().expect("temp session root");
    let store = SessionJsonlStore::new(temp.path().to_path_buf());
    store
        .append_code_workflow_durable(CodeWorkflowEventKind::PlanExecutionRepairRequested {
            interaction_id: "repair-after-restart".to_string(),
            turn_id: "plan-repair-gate-1".to_string(),
            predecessor_interaction_id: String::new(),
            supersedes_predecessor: false,
            repair: PlanExecutionRepairState::AwaitingUser {
                interaction_id: "repair-after-restart".to_string(),
                route: ExecutionFailureRevision::PlanRevision,
                evidence: libra::internal::ai::runtime::ExecutionFailureEvidence {
                    output: "Decision: Abandon.".to_string(),
                    diagnostics: vec!["verification failed".to_string()],
                    attempt: 2,
                    max_attempts: 2,
                },
            },
        })
        .expect("persist repair gate");
    assert!(
        temp.path().exists(),
        "temporary session root must remain alive"
    );
    let replay = store
        .load_code_workflow_replay()
        .expect("reload repair gate after restart");
    let recovered =
        open_plan_execution_repair_from_workflow(replay.events.iter().map(|event| &event.event));
    assert!(
        matches!(
            &recovered,
            Some((
                PlanExecutionRepairState::AwaitingUser {
                    interaction_id,
                    evidence,
                    ..
                },
                turn_id
            )) if interaction_id == "repair-after-restart"
                && turn_id == "plan-repair-gate-1"
                && evidence.output == "Decision: Abandon."
                && evidence.diagnostics == ["verification failed"]
        ),
        "recovered repair gate must preserve its complete redacted evidence: {recovered:?}"
    );
    store
        .append_code_workflow_durable(CodeWorkflowEventKind::InteractionResolved {
            interaction_id: "repair-after-restart".to_string(),
            resolution: "continue".to_string(),
            command: None,
            prior_interaction_resolutions: Vec::new(),
            intent_revision_consumption: None,
        })
        .expect("resolve repair gate");
    let replay = store
        .load_code_workflow_replay()
        .expect("reload resolved repair gate");
    assert!(
        open_plan_execution_repair_from_workflow(replay.events.iter().map(|event| &event.event))
            .is_none(),
        "a resolved repair gate must not be restored"
    );
}

/// W2-11 r18: an authorized retry-limit increase must be part of the
/// pre-ack continuation marker so a crash before repaired-execution admission
/// can still resume through Continue.
#[test]
fn plan_execution_repair_continuation_preserves_authorized_retry_cap() {
    use libra::internal::ai::{
        runtime::{
            ExecutionFailureEvidence, ExecutionFailureRevision, PlanExecutionRepairService,
            PlanExecutionRepairState, open_plan_execution_repair_from_workflow,
            persist_plan_execution_repair_gate,
        },
        session::SessionJsonlStore,
    };

    let exhausted_repair = PlanExecutionRepairState::AwaitingUser {
        interaction_id: "repair-exhausted".to_string(),
        route: ExecutionFailureRevision::PlanRevision,
        evidence: ExecutionFailureEvidence {
            output: "verification failed".to_string(),
            diagnostics: vec!["test failure".to_string()],
            attempt: 2,
            max_attempts: 2,
        },
    };
    let PlanExecutionRepairState::AutomaticRepair { route, evidence } =
        PlanExecutionRepairService.respond(exhausted_repair, "continue", Some(3))
    else {
        panic!("an explicit higher retry limit must authorize another repair");
    };
    let continuation = PlanExecutionRepairState::AwaitingUser {
        interaction_id: "repair-raised-cap-continuation".to_string(),
        route,
        evidence,
    };

    let temp = tempfile::tempdir().expect("temp session root");
    let store = SessionJsonlStore::new(temp.path().to_path_buf());
    persist_plan_execution_repair_gate(&store, &continuation, "repair-raised-cap-turn")
        .expect("persist pre-ack continuation with raised retry cap");

    let replay = store
        .load_code_workflow_replay()
        .expect("reload continuation after crash");
    let (recovered, turn_id) =
        open_plan_execution_repair_from_workflow(replay.events.iter().map(|event| &event.event))
            .expect("crash recovery retains the continuation");
    assert_eq!(turn_id, "repair-raised-cap-turn");
    assert_eq!(recovered.evidence().max_attempts, 3);
    assert!(
        matches!(
            recovered,
            PlanExecutionRepairState::AwaitingUser {
                interaction_id,
                evidence,
                ..
            } if interaction_id == "repair-raised-cap-continuation"
                && evidence.attempt == 3
                && evidence.max_attempts == 3
        ),
        "recovery must preserve the authorized retry cap"
    );
}

/// W2-11 r22: local manual revision guidance must retain its replacement
/// repair gate until repaired execution is admitted. A crash after Phase 1
/// has produced a revised plan but before execution enters the queue must
/// therefore restore Continue/Cancel rather than require reconciliation only.
#[test]
fn plan_execution_repair_manual_revision_continuation_survives_phase1_admit_crash() {
    use libra::internal::ai::{
        runtime::{
            ExecutionFailureEvidence, ExecutionFailureRevision, PlanExecutionRepairState,
            open_plan_execution_repair_from_workflow, persist_plan_execution_repair_gate,
        },
        session::{CodeWorkflowEventKind, SessionJsonlStore},
    };

    let original = PlanExecutionRepairState::AwaitingUser {
        interaction_id: "repair-manual-guidance".to_string(),
        route: ExecutionFailureRevision::PlanRevision,
        evidence: ExecutionFailureEvidence {
            output: "verification failed".to_string(),
            diagnostics: vec!["test failure".to_string()],
            attempt: 2,
            max_attempts: 2,
        },
    };
    let continuation = PlanExecutionRepairState::AwaitingUser {
        interaction_id: "repair-manual-guidance-continuation".to_string(),
        route: ExecutionFailureRevision::PlanRevision,
        evidence: original.evidence().clone(),
    };
    let temp = tempfile::tempdir().expect("temp session root");
    let store = SessionJsonlStore::new(temp.path().to_path_buf());

    persist_plan_execution_repair_gate(&store, &original, "repair-manual-guidance-turn")
        .expect("persist original repair gate");
    persist_plan_execution_repair_gate(
        &store,
        &continuation,
        "repair-manual-guidance-continuation-turn",
    )
    .expect("persist continuation before local acknowledgement");
    store
        .append_code_workflow_durable(CodeWorkflowEventKind::InteractionResolved {
            interaction_id: "repair-manual-guidance".to_string(),
            resolution: "continue".to_string(),
            command: None,
            prior_interaction_resolutions: Vec::new(),
            intent_revision_consumption: None,
        })
        .expect("acknowledge original gate");
    store
        .append_code_workflow_durable(CodeWorkflowEventKind::PlanReviewRequested {
            interaction_id: "revised-plan-review".to_string(),
            plan_id: "revised-plan".to_string(),
            turn_id: "revised-plan-review-turn".to_string(),
            phase1_turn_id: "revised-phase1-turn".to_string(),
            context_id: "revised-plan-review".to_string(),
            revision_of: None,
            prepared_from_network: None,
        })
        .expect("record revised plan review after Phase 1 admission");

    let replay = store
        .load_code_workflow_replay()
        .expect("reload after Phase 1 admission crash");
    assert_eq!(
        open_plan_execution_repair_from_workflow(replay.events.iter().map(|event| &event.event)),
        Some((
            continuation,
            "repair-manual-guidance-continuation-turn".to_string()
        )),
        "manual guidance must leave Continue/Cancel recoverable until repaired execution is admitted"
    );
}

/// W2-11 r25: cancelling a regenerated plan must retire the continuation that
/// was created when manual guidance acknowledged the original repair gate.
/// Otherwise restart reopens a repair interaction for a plan the user already
/// declined.
#[test]
fn plan_execution_repair_manual_revision_cancel_retires_continuation() {
    use libra::internal::ai::{
        runtime::{
            ExecutionFailureEvidence, ExecutionFailureRevision, PlanExecutionRepairState,
            open_plan_execution_repair_from_workflow, persist_plan_execution_repair_gate,
        },
        session::{CodeWorkflowEventKind, SessionJsonlStore},
    };

    let original = PlanExecutionRepairState::AwaitingUser {
        interaction_id: "repair-manual-guidance".to_string(),
        route: ExecutionFailureRevision::PlanRevision,
        evidence: ExecutionFailureEvidence {
            output: "verification failed".to_string(),
            diagnostics: vec!["test failure".to_string()],
            attempt: 2,
            max_attempts: 2,
        },
    };
    let continuation = PlanExecutionRepairState::AwaitingUser {
        interaction_id: "repair-manual-guidance-continuation".to_string(),
        route: ExecutionFailureRevision::PlanRevision,
        evidence: original.evidence().clone(),
    };
    let temp = tempfile::tempdir().expect("temp session root");
    let store = SessionJsonlStore::new(temp.path().to_path_buf());

    persist_plan_execution_repair_gate(&store, &original, "repair-manual-guidance-turn")
        .expect("persist original repair gate");
    persist_plan_execution_repair_gate(
        &store,
        &continuation,
        "repair-manual-guidance-continuation-turn",
    )
    .expect("persist continuation before local acknowledgement");
    store
        .append_code_workflow_durable(CodeWorkflowEventKind::InteractionResolved {
            interaction_id: "repair-manual-guidance".to_string(),
            resolution: "continue".to_string(),
            command: None,
            prior_interaction_resolutions: Vec::new(),
            intent_revision_consumption: None,
        })
        .expect("acknowledge original gate");
    store
        .append_code_workflow_durable(CodeWorkflowEventKind::PlanReviewRequested {
            interaction_id: "revised-plan-review".to_string(),
            plan_id: "revised-plan".to_string(),
            turn_id: "revised-plan-review-turn".to_string(),
            phase1_turn_id: "revised-phase1-turn".to_string(),
            context_id: "revised-plan-review".to_string(),
            revision_of: None,
            prepared_from_network: None,
        })
        .expect("record regenerated plan review");

    // This is the durable write made by both explicit Cancel and Esc before
    // discarding the regenerated-plan review.
    store
        .append_code_workflow_durable(CodeWorkflowEventKind::InteractionResolved {
            interaction_id: "repair-manual-guidance-continuation".to_string(),
            resolution: "repaired execution plan cancelled".to_string(),
            command: None,
            prior_interaction_resolutions: Vec::new(),
            intent_revision_consumption: None,
        })
        .expect("retire continuation on regenerated plan cancellation");

    let replay = store
        .load_code_workflow_replay()
        .expect("reload after regenerated plan cancellation");
    assert!(
        open_plan_execution_repair_from_workflow(replay.events.iter().map(|event| &event.event))
            .is_none(),
        "restart must not reopen the repair gate after cancelling the regenerated plan"
    );
}

/// W2-11 r23b: cancelling Phase 1 after manual revision guidance must
/// immediately re-park the already-durable continuation. The live runtime may
/// not become Idle while restart recovery would still restore Continue/Cancel.
#[tokio::test]
async fn plan_execution_repair_phase1_cancellation_reparks_manual_continuation() {
    use std::sync::Arc;

    use libra::internal::ai::{
        runtime::{
            AgentRuntimeWorker, AgentRuntimeWorkerConfig, ExecutionFailureEvidence,
            ExecutionFailureRevision, InMemoryAuditSink, InteractionResponse, InteractionState,
            PlanExecutionRepairState, PrincipalContext, PrincipalRole, RuntimeCommandDurability,
            SecretRedactor, ToolBoundaryPolicy, ToolBoundaryRuntime,
            open_plan_execution_repair_from_workflow, park_plan_execution_repair_gate,
            persist_plan_execution_repair_gate,
        },
        session::SessionJsonlStore,
    };

    let temp = tempfile::tempdir().expect("temp session root");
    let session_root = temp.path().join("session");
    let store = SessionJsonlStore::new(session_root.clone());
    let boundary = ToolBoundaryRuntime::new(
        Uuid::new_v4(),
        PrincipalContext {
            principal_id: "repair-phase1-cancel".to_string(),
            role: PrincipalRole::Contributor,
        },
        ToolBoundaryPolicy::default_runtime(),
        SecretRedactor::default_runtime(),
        Arc::new(InMemoryAuditSink::default()),
    );
    let (handle, worker) = AgentRuntimeWorker::spawn(
        AgentRuntimeWorkerConfig::new(
            Arc::new(libra::internal::ai::runtime::ExternalTurnTrackingExecutor),
            boundary,
        )
        .with_durability(
            RuntimeCommandDurability::new(SessionJsonlStore::new(session_root)),
            "repo",
            "principal",
        )
        .with_durability_command_kind("tui_local_turn"),
    );
    let original = PlanExecutionRepairState::AwaitingUser {
        interaction_id: "repair-manual-guidance".to_string(),
        route: ExecutionFailureRevision::PlanRevision,
        evidence: ExecutionFailureEvidence {
            output: "verification failed".to_string(),
            diagnostics: vec!["test failure".to_string()],
            attempt: 2,
            max_attempts: 2,
        },
    };
    let continuation = PlanExecutionRepairState::AwaitingUser {
        interaction_id: "repair-manual-guidance-continuation".to_string(),
        route: ExecutionFailureRevision::PlanRevision,
        evidence: original.evidence().clone(),
    };
    persist_plan_execution_repair_gate(&store, &original, "repair-original-turn")
        .expect("persist original repair gate");
    park_plan_execution_repair_gate(
        &handle,
        "session".to_string(),
        "repair-manual-guidance",
        "repair-original-turn".to_string(),
    )
    .await
    .expect("park original repair gate");
    persist_plan_execution_repair_gate(&store, &continuation, "repair-continuation-turn")
        .expect("persist continuation before manual guidance acknowledges original");
    handle
        .respond(
            "session",
            "repair-original-turn",
            InteractionResponse::new("repair-manual-guidance", "continue"),
        )
        .await
        .expect("manual guidance acknowledges original repair gate");

    // Phase 1 is cancelled before repaired execution admission. Re-parking
    // must put the durable continuation back into the live runtime, not Idle.
    park_plan_execution_repair_gate(
        &handle,
        "session".to_string(),
        "repair-manual-guidance-continuation",
        "repair-continuation-turn".to_string(),
    )
    .await
    .expect("Phase 1 cancellation re-parks continuation");
    assert!(matches!(
        handle
            .snapshot("session")
            .await
            .expect("snapshot after Phase 1 cancellation")
            .interaction,
        InteractionState::AwaitingPlanRepair { ref interaction_id }
            if interaction_id == "repair-manual-guidance-continuation"
    ));
    let replay = store
        .load_code_workflow_replay()
        .expect("reload continuation after Phase 1 cancellation");
    assert_eq!(
        open_plan_execution_repair_from_workflow(replay.events.iter().map(|event| &event.event)),
        Some((continuation, "repair-continuation-turn".to_string())),
        "the live repair gate and durable marker must agree after cancellation"
    );

    worker.abort();
}

/// W2-11 r21: if recovery sees a raised-limit continuation persisted before
/// its predecessor was acknowledged, it restores the predecessor and retires
/// the explicitly linked speculative copy. A later successful repair must not
/// leave that stale continuation to fence the next restart.
#[test]
fn plan_execution_repair_recovery_retires_raised_limit_pre_ack_continuation() {
    use libra::internal::ai::{
        runtime::{
            ExecutionFailureEvidence, ExecutionFailureRevision, PlanExecutionRepairService,
            PlanExecutionRepairState, open_plan_execution_repair_from_workflow,
            persist_plan_execution_repair_gate,
            persist_plan_execution_repair_gate_with_predecessor,
            speculative_plan_execution_repair_continuations_from_workflow,
        },
        session::{CodeWorkflowEventKind, SessionJsonlStore},
    };

    let predecessor = PlanExecutionRepairState::AwaitingUser {
        interaction_id: "repair-crash-predecessor".to_string(),
        route: ExecutionFailureRevision::PlanRevision,
        evidence: ExecutionFailureEvidence {
            output: "verification failed".to_string(),
            diagnostics: vec!["test failure".to_string()],
            attempt: 2,
            max_attempts: 2,
        },
    };
    let PlanExecutionRepairState::AutomaticRepair { route, evidence } =
        PlanExecutionRepairService.respond(predecessor.clone(), "continue", Some(3))
    else {
        panic!("raised retry limit must authorize another automatic repair");
    };
    let continuation = PlanExecutionRepairState::AwaitingUser {
        interaction_id: "repair-crash-continuation".to_string(),
        route,
        evidence,
    };
    let temp = tempfile::tempdir().expect("temp session root");
    let store = SessionJsonlStore::new(temp.path().to_path_buf());
    persist_plan_execution_repair_gate(&store, &predecessor, "repair-crash-predecessor-turn")
        .expect("persist predecessor repair gate");
    persist_plan_execution_repair_gate_with_predecessor(
        &store,
        &continuation,
        "repair-crash-continuation-turn",
        Some("repair-crash-predecessor"),
    )
    .expect("persist raised-limit pre-ack continuation");

    let replay = store
        .load_code_workflow_replay()
        .expect("reload dual unresolved repair markers");
    assert_eq!(
        open_plan_execution_repair_from_workflow(replay.events.iter().map(|event| &event.event)),
        Some((
            predecessor.clone(),
            "repair-crash-predecessor-turn".to_string()
        )),
        "recovery restores the original unresolved repair gate"
    );
    assert_eq!(
        speculative_plan_execution_repair_continuations_from_workflow(
            replay.events.iter().map(|event| &event.event)
        ),
        vec!["repair-crash-continuation".to_string()],
        "the raised-limit pre-ack continuation is retired before re-parking the predecessor"
    );
    store
        .append_code_workflow_durable(CodeWorkflowEventKind::InteractionResolved {
            interaction_id: "repair-crash-continuation".to_string(),
            resolution: "repair continuation retired while restoring its unresolved predecessor"
                .to_string(),
            command: None,
            prior_interaction_resolutions: Vec::new(),
            intent_revision_consumption: None,
        })
        .expect("retire speculative continuation");
    store
        .append_code_workflow_durable(CodeWorkflowEventKind::InteractionResolved {
            interaction_id: "repair-crash-predecessor".to_string(),
            resolution: "continue".to_string(),
            command: None,
            prior_interaction_resolutions: Vec::new(),
            intent_revision_consumption: None,
        })
        .expect("resolve restored predecessor");

    let replay = store
        .load_code_workflow_replay()
        .expect("reload after recovery resolves predecessor");
    assert!(
        open_plan_execution_repair_from_workflow(replay.events.iter().map(|event| &event.event))
            .is_none(),
        "resolving the restored predecessor must not revive its stale continuation"
    );
}

/// W2-11 r25: a re-plan failure replacement supersedes its continuation before
/// the continuation can be retired. Restart must preserve the new failure
/// evidence rather than restoring the stale continuation.
#[test]
fn plan_execution_repair_recovery_prefers_replan_failure_replacement_before_retirement() {
    use libra::internal::ai::{
        runtime::{
            ExecutionFailureEvidence, ExecutionFailureRevision, PlanExecutionRepairState,
            open_plan_execution_repair_from_workflow, persist_plan_execution_repair_gate,
            persist_plan_execution_repair_gate_superseding,
        },
        session::SessionJsonlStore,
    };

    let continuation = PlanExecutionRepairState::AwaitingUser {
        interaction_id: "repair-stale-continuation".to_string(),
        route: ExecutionFailureRevision::PlanRevision,
        evidence: ExecutionFailureEvidence {
            output: "initial execution verification failed".to_string(),
            diagnostics: vec!["initial failure".to_string()],
            attempt: 2,
            max_attempts: 2,
        },
    };
    let replacement = PlanExecutionRepairState::AwaitingUser {
        interaction_id: "repair-replan-failure".to_string(),
        route: ExecutionFailureRevision::PlanRevision,
        evidence: ExecutionFailureEvidence {
            output: "Phase 1 re-planning failed".to_string(),
            diagnostics: vec!["new re-plan failure evidence".to_string()],
            attempt: 2,
            max_attempts: 2,
        },
    };
    let temp = tempfile::tempdir().expect("temp session root");
    let store = SessionJsonlStore::new(temp.path().to_path_buf());
    persist_plan_execution_repair_gate(&store, &continuation, "repair-stale-continuation-turn")
        .expect("persist stale continuation");
    persist_plan_execution_repair_gate_superseding(
        &store,
        &replacement,
        "repair-replan-failure-turn",
        "repair-stale-continuation",
    )
    .expect("persist replacement before retiring predecessor");

    let replay = store
        .load_code_workflow_replay()
        .expect("restart reloads dual unresolved repair gates");
    assert_eq!(
        open_plan_execution_repair_from_workflow(replay.events.iter().map(|event| &event.event)),
        Some((replacement, "repair-replan-failure-turn".to_string())),
        "restart must select the replacement gate after a crash before stale continuation retirement"
    );
}

/// W2-11: the Web/control adapter parks this delivery on the runtime handle.
/// A public interaction response must settle the worker gate and its durable
/// marker together; otherwise resume would strand the session at the repair
/// prompt even after Cancel was acknowledged.
#[tokio::test]
async fn plan_execution_repair_cancel_resolves_runtime_gate_and_durable_marker() {
    use std::sync::{Arc, atomic::AtomicBool};

    use libra::internal::ai::{
        runtime::{
            AgentRuntimeWorker, AgentRuntimeWorkerConfig, ExecutionFailureEvidence,
            ExecutionFailureRevision, InMemoryAuditSink, InteractionResponse, InteractionState,
            PlanExecutionRepairAckDelivery, PlanExecutionRepairState, PrincipalContext,
            PrincipalRole, RuntimeCommandDurability, SecretRedactor, ToolBoundaryPolicy,
            ToolBoundaryRuntime, TurnRequest, open_plan_execution_repair_from_workflow,
        },
        session::{CodeWorkflowEventKind, SessionJsonlStore},
    };
    use tokio_util::sync::CancellationToken;

    let temp = tempfile::tempdir().expect("temp session root");
    let session_root = temp.path().join("session");
    let store = SessionJsonlStore::new(session_root.clone());
    let repair = PlanExecutionRepairState::AwaitingUser {
        interaction_id: "repair-control-cancel".to_string(),
        route: ExecutionFailureRevision::PlanRevision,
        evidence: ExecutionFailureEvidence {
            output: "Decision: Abandon.".to_string(),
            diagnostics: vec!["verification failed".to_string()],
            attempt: 2,
            max_attempts: 2,
        },
    };
    store
        .append_code_workflow_durable(CodeWorkflowEventKind::PlanExecutionRepairRequested {
            interaction_id: "repair-control-cancel".to_string(),
            turn_id: "repair-control-turn".to_string(),
            predecessor_interaction_id: String::new(),
            supersedes_predecessor: false,
            repair,
        })
        .expect("persist repair gate before parking it");

    let boundary = ToolBoundaryRuntime::new(
        Uuid::new_v4(),
        PrincipalContext {
            principal_id: "repair-control".to_string(),
            role: PrincipalRole::Contributor,
        },
        ToolBoundaryPolicy::default_runtime(),
        SecretRedactor::default_runtime(),
        Arc::new(InMemoryAuditSink::default()),
    );
    let (handle, worker) = AgentRuntimeWorker::spawn(
        AgentRuntimeWorkerConfig::new(
            Arc::new(libra::internal::ai::runtime::ExternalTurnTrackingExecutor),
            boundary,
        )
        .with_durability(
            RuntimeCommandDurability::new(SessionJsonlStore::new(session_root)),
            "repo",
            "principal",
        )
        .with_durability_command_kind("tui_local_turn"),
    );

    handle
        .track_external_turn(
            TurnRequest::new("session", "repair-control-turn", "Plan repair", false),
            CancellationToken::new(),
            Arc::new(AtomicBool::new(false)),
        )
        .await
        .expect("repair gate turn tracked");
    handle
        .register_interaction_with_delivery(
            "session",
            "repair-control-turn",
            InteractionState::AwaitingPlanRepair {
                interaction_id: "repair-control-cancel".to_string(),
            },
            Box::new(PlanExecutionRepairAckDelivery),
        )
        .await
        .expect("repair gate registered through runtime handle");
    assert!(matches!(
        handle
            .snapshot("session")
            .await
            .expect("snapshot while repair gate is pending")
            .interaction,
        InteractionState::AwaitingPlanRepair { ref interaction_id }
            if interaction_id == "repair-control-cancel"
    ));

    handle
        .respond(
            "session",
            "repair-control-turn",
            InteractionResponse::new("repair-control-cancel", "cancel"),
        )
        .await
        .expect("control response cancels repair gate");

    let snapshot = handle
        .snapshot("session")
        .await
        .expect("snapshot after repair cancellation");
    assert_eq!(snapshot.active_turn_id, None);
    assert_eq!(snapshot.interaction, InteractionState::Completed);
    let events: Vec<_> = store
        .load_code_workflow_replay()
        .expect("replay resolved repair gate")
        .events
        .into_iter()
        .map(|event| event.event)
        .collect();
    assert!(
        open_plan_execution_repair_from_workflow(events.iter()).is_none(),
        "a Cancel response must close the durable repair marker: {events:?}"
    );

    worker.abort();
}

/// W2-11: exercise the same durable registration sequence used after
/// `ExecuteWorkflowComplete` sees an exhausted failed execution. A restarted
/// runtime must re-park the marker before Continue can re-admit repair.
#[tokio::test]
async fn plan_execution_repair_failure_registration_recovers_and_continues() {
    use std::sync::Arc;

    use libra::internal::ai::{
        orchestrator::{
            run_state::RunStateSnapshot,
            types::{
                DecisionOutcome, ExecutionPlanSpec, GateReport, OrchestratorResult, SystemReport,
            },
        },
        runtime::{
            AgentRuntimeWorker, AgentRuntimeWorkerConfig, ExecutionFailureRevision,
            InMemoryAuditSink, InteractionResponse, InteractionState, PlanExecutionRepairService,
            PlanExecutionRepairState, PrincipalContext, PrincipalRole, RuntimeCommandDurability,
            SecretRedactor, ToolBoundaryPolicy, ToolBoundaryRuntime,
            open_plan_execution_repair_from_workflow, park_plan_execution_repair_gate,
            persist_and_park_plan_execution_repair_gate,
        },
        session::SessionJsonlStore,
    };

    let temp = tempfile::tempdir().expect("temp session root");
    let session_root = temp.path().join("session");
    let store = SessionJsonlStore::new(session_root.clone());
    let failed_execution = OrchestratorResult {
        decision: DecisionOutcome::Abandon,
        execution_plan_spec: ExecutionPlanSpec {
            intent_spec_id: "repair-intent".to_string(),
            revision: 1,
            parent_revision: None,
            replan_reason: None,
            tasks: Vec::new(),
            max_parallel: 1,
            checkpoints: Vec::new(),
        },
        plan_revision_specs: Vec::new(),
        run_state: RunStateSnapshot::default(),
        task_results: Vec::new(),
        system_report: SystemReport {
            integration: GateReport::empty(),
            security: GateReport::empty(),
            release: GateReport::empty(),
            review_passed: false,
            review_findings: Vec::new(),
            artifacts_complete: true,
            missing_artifacts: Vec::new(),
            overall_passed: false,
        },
        intent_spec_id: "repair-intent".to_string(),
        lifecycle_change_log: Vec::new(),
        replan_count: 0,
        persistence: None,
    };
    let first_automatic_repair = PlanExecutionRepairService.after_failure(
        "repair-first-automatic-attempt",
        Some(&failed_execution),
        Some("orchestrator exhausted the execution plan"),
        0,
        1,
    );
    assert!(
        matches!(
            first_automatic_repair,
            PlanExecutionRepairState::AutomaticRepair { evidence, .. }
                if evidence.attempt == 1 && evidence.max_attempts == 1
        ),
        "entering AutomaticRepair must advance the runtime-owned evidence counter"
    );
    let repair = PlanExecutionRepairService.after_failure(
        "repair-after-execute-failure",
        Some(&failed_execution),
        Some("orchestrator exhausted the execution plan"),
        1,
        1,
    );
    let PlanExecutionRepairState::AwaitingUser { interaction_id, .. } = &repair else {
        panic!("exhausted abandoned execution must require a repair gate: {repair:?}");
    };
    assert_eq!(
        repair.interaction_state(),
        Some(InteractionState::AwaitingPlanRepair {
            interaction_id: interaction_id.clone()
        })
    );

    let boundary = || {
        ToolBoundaryRuntime::new(
            Uuid::new_v4(),
            PrincipalContext {
                principal_id: "repair-restart".to_string(),
                role: PrincipalRole::Contributor,
            },
            ToolBoundaryPolicy::default_runtime(),
            SecretRedactor::default_runtime(),
            Arc::new(InMemoryAuditSink::default()),
        )
    };
    let spawn_runtime = |boundary| {
        AgentRuntimeWorker::spawn(
            AgentRuntimeWorkerConfig::new(
                Arc::new(libra::internal::ai::runtime::ExternalTurnTrackingExecutor),
                boundary,
            )
            .with_durability(
                RuntimeCommandDurability::new(SessionJsonlStore::new(session_root.clone())),
                "repo",
                "principal",
            )
            .with_durability_command_kind("tui_local_turn"),
        )
    };

    // This is the production restart adapter sequence: persist the durable
    // marker, then park the runtime interaction before projecting the repaired
    // state back to the Code UI.
    let first_gate_turn = "plan-repair-after-execute-failure";
    let (first_handle, first_worker) = spawn_runtime(boundary());
    persist_and_park_plan_execution_repair_gate(
        &store,
        &first_handle,
        "session".to_string(),
        &repair,
        first_gate_turn.to_string(),
    )
    .await
    .expect("failed execution registers the durable repair interaction");
    assert!(matches!(
        first_handle.snapshot("session").await.expect("pending snapshot").interaction,
        InteractionState::AwaitingPlanRepair { ref interaction_id }
            if interaction_id == "repair-after-execute-failure"
    ));

    // Simulate a process restart before the developer has answered.
    first_worker.abort();
    let replay = store
        .load_code_workflow_replay()
        .expect("restart reloads repair marker");
    let (recovered, _) =
        open_plan_execution_repair_from_workflow(replay.events.iter().map(|event| &event.event))
            .expect("restart must retain unresolved repair marker");
    assert_eq!(recovered, repair);

    let (restored_handle, restored_worker) = spawn_runtime(boundary());
    park_plan_execution_repair_gate(
        &restored_handle,
        "session".to_string(),
        interaction_id,
        first_gate_turn.to_string(),
    )
    .await
    .expect("restart reattaches the durable repair interaction without a new turn");
    assert!(matches!(
        restored_handle
            .snapshot("session")
            .await
            .expect("reattached repair snapshot")
            .interaction,
        InteractionState::AwaitingPlanRepair { ref interaction_id }
            if interaction_id == "repair-after-execute-failure"
    ));
    restored_handle
        .respond(
            "session",
            first_gate_turn,
            InteractionResponse::new(interaction_id, "continue"),
        )
        .await
        .expect("Continue settles the recovered runtime gate");

    assert!(matches!(
        PlanExecutionRepairService.respond(recovered, "continue", Some(2)),
        PlanExecutionRepairState::AutomaticRepair {
            route: ExecutionFailureRevision::PlanRevision,
            ..
        }
    ));
    let replay = store
        .load_code_workflow_replay()
        .expect("Continue persists repair resolution");
    assert!(
        open_plan_execution_repair_from_workflow(replay.events.iter().map(|event| &event.event))
            .is_none(),
        "Continue must clear every durable repair marker after restart"
    );

    restored_worker.abort();
}

/// W2-11 r13: Continue resolves its current marker before Phase 1 re-planning
/// begins. If that re-plan fails, the adapter must open a fresh durable repair
/// interaction so restart and Code UI still have an actionable gate.
#[tokio::test]
async fn plan_execution_repair_reopens_gate_when_phase1_replan_fails() {
    use std::sync::Arc;

    use libra::internal::ai::{
        runtime::{
            AgentRuntimeWorker, AgentRuntimeWorkerConfig, ExecutionFailureEvidence,
            ExecutionFailureRevision, InMemoryAuditSink, InteractionResponse, InteractionState,
            PlanExecutionRepairState, PrincipalContext, PrincipalRole, RuntimeCommandDurability,
            SecretRedactor, ToolBoundaryPolicy, ToolBoundaryRuntime,
            open_plan_execution_repair_from_workflow, park_plan_execution_repair_gate,
            persist_and_park_plan_execution_repair_gate, persist_plan_execution_repair_gate,
            redacted_failure_summary,
        },
        session::SessionJsonlStore,
    };

    let temp = tempfile::tempdir().expect("temp session root");
    let session_root = temp.path().join("session");
    let store = SessionJsonlStore::new(session_root.clone());
    let boundary = ToolBoundaryRuntime::new(
        Uuid::new_v4(),
        PrincipalContext {
            principal_id: "repair-replan-failure".to_string(),
            role: PrincipalRole::Contributor,
        },
        ToolBoundaryPolicy::default_runtime(),
        SecretRedactor::default_runtime(),
        Arc::new(InMemoryAuditSink::default()),
    );
    let (handle, worker) = AgentRuntimeWorker::spawn(
        AgentRuntimeWorkerConfig::new(
            Arc::new(libra::internal::ai::runtime::ExternalTurnTrackingExecutor),
            boundary,
        )
        .with_durability(
            RuntimeCommandDurability::new(SessionJsonlStore::new(session_root)),
            "repo",
            "principal",
        )
        .with_durability_command_kind("tui_local_turn"),
    );

    let initial_repair = PlanExecutionRepairState::AwaitingUser {
        interaction_id: "repair-before-replan".to_string(),
        route: ExecutionFailureRevision::PlanRevision,
        evidence: ExecutionFailureEvidence {
            output: "execution verification failed".to_string(),
            diagnostics: vec!["test failure".to_string()],
            attempt: 1,
            max_attempts: 2,
        },
    };
    persist_and_park_plan_execution_repair_gate(
        &store,
        &handle,
        "session".to_string(),
        &initial_repair,
        "repair-before-replan-turn".to_string(),
    )
    .await
    .expect("initial repair gate is parked");
    // Production writes this handoff before acknowledging Continue. A
    // successfully replanned repair must retain it through execute setup and
    // queue submission: a rejected repaired execution is still recoverable
    // through Continue/Cancel after restart.
    let continuation = PlanExecutionRepairState::AwaitingUser {
        interaction_id: "repair-continuation-after-ack".to_string(),
        route: ExecutionFailureRevision::PlanRevision,
        evidence: initial_repair.evidence().clone(),
    };
    persist_plan_execution_repair_gate(&store, &continuation, "repair-continuation-turn")
        .expect("continuation handoff persists before Continue acknowledgement");
    handle
        .respond(
            "session",
            "repair-before-replan-turn",
            InteractionResponse::new("repair-before-replan", "continue"),
        )
        .await
        .expect("Continue resolves the initial repair gate");
    // Simulate a successful replan followed by `start_execute_workflow`
    // rejecting setup or queue submission. Neither pre-admission failure path
    // records `InteractionResolved` for the continuation.
    let replay = store
        .load_code_workflow_replay()
        .expect("restart reloads continuation handoff");
    assert_eq!(
        open_plan_execution_repair_from_workflow(replay.events.iter().map(|event| &event.event)),
        Some((continuation, "repair-continuation-turn".to_string())),
        "failed repaired-execution admission must retain the repair continuation"
    );
    // The live adapter must immediately re-park the retained continuation
    // rather than becoming Idle and waiting for a restart to recover it.
    park_plan_execution_repair_gate(
        &handle,
        "session".to_string(),
        "repair-continuation-after-ack",
        "repair-continuation-turn".to_string(),
    )
    .await
    .expect("rejected repaired execution re-parks its continuation gate");
    assert!(matches!(
        handle
            .snapshot("session")
            .await
            .expect("repaired execution rejection snapshot")
            .interaction,
        InteractionState::AwaitingPlanRepair { ref interaction_id }
            if interaction_id == "repair-continuation-after-ack"
    ));
    handle
        .respond(
            "session",
            "repair-continuation-turn",
            InteractionResponse::new("repair-continuation-after-ack", "continue"),
        )
        .await
        .expect("re-parked continuation remains actionable");
    // Simulate Phase 1 rejecting the revised plan. Re-plan failures are
    // supplemental runtime evidence, so they must be redacted and bounded
    // before landing in the restart-recoverable marker.
    let raw_replan_error = format!(
        "provider rejected token: top-secret {}",
        "revised plan detail ".repeat(40)
    );
    let redacted_replan_error = redacted_failure_summary(&raw_replan_error);
    assert!(!redacted_replan_error.contains("top-secret"));
    assert!(redacted_replan_error.chars().count() <= 513);
    let reopened_repair = PlanExecutionRepairState::AwaitingUser {
        interaction_id: "repair-after-replan-failure".to_string(),
        route: ExecutionFailureRevision::PlanRevision,
        evidence: ExecutionFailureEvidence {
            output: "Phase 1 re-planning failed: invalid IntentSpec".to_string(),
            diagnostics: vec![format!(
                "Automatic plan repair re-planning failed: {redacted_replan_error}"
            )],
            attempt: 2,
            max_attempts: 2,
        },
    };
    persist_and_park_plan_execution_repair_gate(
        &store,
        &handle,
        "session".to_string(),
        &reopened_repair,
        "repair-after-replan-failure-turn".to_string(),
    )
    .await
    .expect("re-plan failure reopens a repair gate");
    // The replacement must be durable before the pre-ack continuation is
    // retired. Otherwise a crash could lose all actionable repair evidence;
    // leaving it open afterward would make recovery select stale evidence.
    store
        .append_code_workflow_durable(
            libra::internal::ai::session::CodeWorkflowEventKind::InteractionResolved {
                interaction_id: "repair-continuation-after-ack".to_string(),
                resolution: "repair continuation superseded by replan failure".to_string(),
                command: None,
                prior_interaction_resolutions: Vec::new(),
                intent_revision_consumption: None,
            },
        )
        .expect("retire continuation after replacement gate persists");

    assert!(matches!(
        handle
            .snapshot("session")
            .await
            .expect("snapshot after re-plan failure")
            .interaction,
        InteractionState::AwaitingPlanRepair { ref interaction_id }
            if interaction_id == "repair-after-replan-failure"
    ));
    let replay = store
        .load_code_workflow_replay()
        .expect("reload reopened repair marker");
    assert_eq!(
        open_plan_execution_repair_from_workflow(replay.events.iter().map(|event| &event.event)),
        Some((
            reopened_repair,
            "repair-after-replan-failure-turn".to_string()
        )),
        "restart must select the replacement re-plan failure evidence, not the stale continuation"
    );
    let recovered =
        open_plan_execution_repair_from_workflow(replay.events.iter().map(|event| &event.event))
            .expect("reopen retains redacted re-plan evidence");
    assert!(
        recovered
            .0
            .evidence()
            .diagnostics
            .iter()
            .any(|diagnostic| diagnostic.contains("[REDACTED]") && diagnostic.contains('…')),
        "restart recovery must receive bounded redacted re-plan evidence"
    );

    worker.abort();
}

/// W2-04: confirmed plan execution enters the serialized worker queue, and a
/// mutating tool is refused when the shared hardening boundary denies it.
#[tokio::test]
async fn plan_execution_enters_runtime_queue() {
    use std::sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    };

    use libra::internal::ai::runtime::{
        AgentRuntimeWorker, AgentRuntimeWorkerConfig, DeferredPlanExecutionExecutor,
        InMemoryAuditSink, InteractionState, PLAN_EXECUTION_TURN_INPUT, PrincipalContext,
        PrincipalRole, RuntimeTurnExecution, SecretRedactor, ToolBoundaryPolicy,
        ToolBoundaryRuntime, is_plan_execution_turn, plan_execution_turn_request,
        submit_confirmed_plan_execution,
    };
    use tokio::{
        sync::Notify,
        time::{Duration, timeout},
    };

    let starts = Arc::new(tokio::sync::Mutex::new(Vec::new()));
    let first_started = Arc::new(Notify::new());
    let release_first = Arc::new(Notify::new());
    let executor = Arc::new(DeferredPlanExecutionExecutor::new());

    let contributor_boundary = ToolBoundaryRuntime::new(
        Uuid::new_v4(),
        PrincipalContext {
            principal_id: "plan-execution-contributor".to_string(),
            role: PrincipalRole::Contributor,
        },
        ToolBoundaryPolicy::default_runtime(),
        SecretRedactor::default_runtime(),
        Arc::new(InMemoryAuditSink::default()),
    );
    let (handle, worker) = AgentRuntimeWorker::spawn(AgentRuntimeWorkerConfig::new(
        executor.clone(),
        contributor_boundary,
    ));

    let starts_for_first = starts.clone();
    let first_started_notify = first_started.clone();
    let release_first_for_runner = release_first.clone();
    submit_confirmed_plan_execution(
        &handle,
        executor.as_ref(),
        "session",
        "plan-exec-1",
        Box::new(move |_context| {
            Box::pin(async move {
                starts_for_first
                    .lock()
                    .await
                    .push("plan-exec-1".to_string());
                first_started_notify.notify_one();
                release_first_for_runner.notified().await;
                Ok(RuntimeTurnExecution::Completed {
                    summary: "first plan executed".to_string(),
                })
            })
        }),
    )
    .await
    .expect("first plan execution admitted");

    timeout(Duration::from_secs(2), first_started.notified())
        .await
        .expect("first plan execution started on the worker");
    assert_eq!(
        starts.lock().await.as_slice(),
        ["plan-exec-1"],
        "only the dequeued plan-execution turn may start"
    );
    let snapshot = handle.snapshot("session").await.expect("snapshot");
    assert_eq!(snapshot.active_turn_id.as_deref(), Some("plan-exec-1"));
    assert_eq!(snapshot.queued_turns, 0);
    assert!(is_plan_execution_turn(&plan_execution_turn_request(
        "session",
        "plan-exec-1"
    )));
    assert_eq!(
        plan_execution_turn_request("session", "x").input,
        PLAN_EXECUTION_TURN_INPUT
    );

    // While the first plan turn is active, stage+submit a second plan turn —
    // it must remain queued until cancelled or the first completes.
    let starts_for_second = starts.clone();
    submit_confirmed_plan_execution(
        &handle,
        executor.as_ref(),
        "session",
        "plan-exec-2",
        Box::new(move |_context| {
            Box::pin(async move {
                starts_for_second
                    .lock()
                    .await
                    .push("plan-exec-2".to_string());
                Ok(RuntimeTurnExecution::Completed {
                    summary: "second plan executed".to_string(),
                })
            })
        }),
    )
    .await
    .expect("second plan execution queued");

    let snapshot = handle
        .snapshot("session")
        .await
        .expect("snapshot while held");
    assert_eq!(snapshot.active_turn_id.as_deref(), Some("plan-exec-1"));
    assert_eq!(snapshot.queued_turns, 1);
    assert_eq!(
        starts.lock().await.as_slice(),
        ["plan-exec-1"],
        "queued plan must not start while the prior plan-execution turn is active"
    );

    // Cancel the queued second plan before it dequeues — on_admission_discarded
    // must release the staged runner so a later plan can stage again.
    handle
        .cancel("session", "plan-exec-2")
        .await
        .expect("cancel queued plan-execution turn");
    let snapshot = handle
        .snapshot("session")
        .await
        .expect("snapshot after queued cancel");
    assert_eq!(snapshot.active_turn_id.as_deref(), Some("plan-exec-1"));
    assert_eq!(snapshot.queued_turns, 0);
    assert_eq!(
        starts.lock().await.as_slice(),
        ["plan-exec-1"],
        "cancelled queued plan must never execute"
    );

    let third_started = Arc::new(Notify::new());
    let starts_for_third = starts.clone();
    let third_started_notify = third_started.clone();
    submit_confirmed_plan_execution(
        &handle,
        executor.as_ref(),
        "session",
        "plan-exec-3",
        Box::new(move |_context| {
            Box::pin(async move {
                starts_for_third
                    .lock()
                    .await
                    .push("plan-exec-3".to_string());
                third_started_notify.notify_one();
                Ok(RuntimeTurnExecution::Completed {
                    summary: "third plan executed".to_string(),
                })
            })
        }),
    )
    .await
    .expect("third plan stages after queued cancel discarded the second runner");

    release_first.notify_one();
    timeout(Duration::from_secs(2), third_started.notified())
        .await
        .expect("third plan starts after the first completes");
    assert_eq!(
        starts.lock().await.as_slice(),
        ["plan-exec-1", "plan-exec-3"],
        "cancelled queued plan must stay out of the execution order"
    );
    timeout(Duration::from_secs(2), async {
        loop {
            let snapshot = handle.snapshot("session").await.expect("snapshot");
            if snapshot.active_turn_id.is_none()
                && matches!(snapshot.interaction, InteractionState::Completed)
            {
                return;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("third plan reaches a terminal idle snapshot");

    // Shutdown must also discard any remaining queued staged runner.
    let shutdown_started = Arc::new(Notify::new());
    let shutdown_release = Arc::new(Notify::new());
    let shutdown_body = Arc::new(AtomicUsize::new(0));
    let shutdown_started_notify = shutdown_started.clone();
    let shutdown_release_wait = shutdown_release.clone();
    submit_confirmed_plan_execution(
        &handle,
        executor.as_ref(),
        "session",
        "plan-exec-shutdown-active",
        Box::new(move |_context| {
            Box::pin(async move {
                shutdown_started_notify.notify_one();
                shutdown_release_wait.notified().await;
                Ok(RuntimeTurnExecution::Completed {
                    summary: "shutdown active".to_string(),
                })
            })
        }),
    )
    .await
    .expect("shutdown-active plan admitted");
    timeout(Duration::from_secs(2), shutdown_started.notified())
        .await
        .expect("shutdown-active plan started");
    let shutdown_body_queued = shutdown_body.clone();
    submit_confirmed_plan_execution(
        &handle,
        executor.as_ref(),
        "session",
        "plan-exec-shutdown-queued",
        Box::new(move |_context| {
            Box::pin(async move {
                shutdown_body_queued.fetch_add(1, Ordering::SeqCst);
                Ok(RuntimeTurnExecution::Completed {
                    summary: "should be discarded on shutdown".to_string(),
                })
            })
        }),
    )
    .await
    .expect("shutdown-queued plan staged");
    let shutdown = handle.shutdown();
    // Release the active turn so shutdown can finish cooperatively.
    shutdown_release.notify_one();
    timeout(Duration::from_secs(5), shutdown)
        .await
        .expect("shutdown timeout")
        .expect("worker shutdown discards queued plan admissions");
    assert_eq!(
        shutdown_body.load(Ordering::SeqCst),
        0,
        "shutdown must discard queued plan runners without executing them"
    );
    worker.abort();

    // Observer principal must deny confirmed plan execution before the body runs.
    let observer_executor = Arc::new(DeferredPlanExecutionExecutor::new());
    let observer_boundary = ToolBoundaryRuntime::new(
        Uuid::new_v4(),
        PrincipalContext {
            principal_id: "plan-execution-observer".to_string(),
            role: PrincipalRole::Observer,
        },
        ToolBoundaryPolicy::default_runtime(),
        SecretRedactor::default_runtime(),
        Arc::new(InMemoryAuditSink::default()),
    );
    let (observer_handle, observer_worker) = AgentRuntimeWorker::spawn(
        AgentRuntimeWorkerConfig::new(observer_executor.clone(), observer_boundary),
    );
    let body_ran = Arc::new(AtomicUsize::new(0));
    let body_ran_runner = body_ran.clone();
    submit_confirmed_plan_execution(
        &observer_handle,
        observer_executor.as_ref(),
        "session",
        "denied-plan",
        Box::new(move |_context| {
            Box::pin(async move {
                body_ran_runner.fetch_add(1, Ordering::SeqCst);
                Ok(RuntimeTurnExecution::Completed {
                    summary: "should not run".to_string(),
                })
            })
        }),
    )
    .await
    .expect("denied plan still admits onto the queue");

    timeout(Duration::from_secs(2), async {
        loop {
            let snapshot = observer_handle.snapshot("session").await.expect("snapshot");
            if matches!(
                snapshot.interaction,
                InteractionState::Failed { .. } | InteractionState::Completed
            ) && snapshot.active_turn_id.is_none()
            {
                return;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("observer-denied plan reaches a terminal state");
    assert_eq!(
        body_ran.load(Ordering::SeqCst),
        0,
        "mutating plan body must not run when the tool boundary denies apply_patch"
    );
    let denied = observer_handle
        .snapshot("session")
        .await
        .expect("final snapshot");
    assert!(
        matches!(denied.interaction, InteractionState::Failed { .. }),
        "observer deny must fail the plan-execution turn: {denied:?}"
    );
    observer_worker.abort();
}

/// W2-05: a tool loop with sequential user-input deliveries must persist
/// every `InteractionResolved` atomically with terminal success — not only
/// the last response stored on `ActiveTurn`.
#[tokio::test]
async fn sequential_user_input_resolutions_all_persist_on_terminal_success() {
    use std::sync::Arc;

    use libra::internal::ai::{
        runtime::{
            AgentRuntimeWorker, AgentRuntimeWorkerConfig, InMemoryAuditSink, InteractionResponse,
            InteractionState, PrincipalContext, PrincipalRole, RuntimeCommandDurability,
            RuntimeExecutionContext, RuntimeInteractionDelivery, RuntimeTurnExecution,
            RuntimeTurnExecutor, RuntimeWorkerError, SecretRedactor, ToolBoundaryPolicy,
            ToolBoundaryRuntime, TurnRequest,
        },
        session::{CodeWorkflowEventKind, SessionJsonlStore},
    };
    use tokio::{
        sync::Notify,
        time::{Duration, timeout},
    };

    struct LiveToolLoopExecutor {
        started: Arc<Notify>,
        allow_complete: Arc<Notify>,
    }

    #[async_trait]
    impl RuntimeTurnExecutor for LiveToolLoopExecutor {
        async fn execute(
            &self,
            _request: TurnRequest,
            _context: RuntimeExecutionContext,
        ) -> Result<RuntimeTurnExecution, RuntimeWorkerError> {
            self.started.notify_one();
            self.allow_complete.notified().await;
            Ok(RuntimeTurnExecution::Completed {
                summary: "both inputs answered".to_string(),
            })
        }
    }

    struct PersistUserInputDelivery;

    #[async_trait]
    impl RuntimeInteractionDelivery for PersistUserInputDelivery {
        fn validate(&self, _interaction: &InteractionResponse) -> Result<(), RuntimeWorkerError> {
            Ok(())
        }

        fn persist_interaction_resolved_after_terminal(&self) -> bool {
            true
        }

        fn interaction_resolution(&self, interaction: &InteractionResponse) -> String {
            interaction.response.clone()
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

    let temp = tempfile::tempdir().expect("temp dir");
    let session_root = temp.path().join("session");
    let store = SessionJsonlStore::new(session_root.clone());
    let durability = RuntimeCommandDurability::new(SessionJsonlStore::new(session_root.clone()));
    let started = Arc::new(Notify::new());
    let allow_complete = Arc::new(Notify::new());
    let boundary = ToolBoundaryRuntime::new(
        Uuid::new_v4(),
        PrincipalContext {
            principal_id: "multi-resolution-persist".to_string(),
            role: PrincipalRole::Contributor,
        },
        ToolBoundaryPolicy::default_runtime(),
        SecretRedactor::default_runtime(),
        Arc::new(InMemoryAuditSink::default()),
    );
    let (handle, worker) = AgentRuntimeWorker::spawn(
        AgentRuntimeWorkerConfig::new(
            Arc::new(LiveToolLoopExecutor {
                started: Arc::clone(&started),
                allow_complete: Arc::clone(&allow_complete),
            }),
            boundary,
        )
        .with_durability(durability, "repo", "principal")
        .with_durability_command_kind("tui_local_turn"),
    );

    handle
        .submit(TurnRequest::new(
            "session",
            "multi-input-turn",
            "request input",
            false,
        ))
        .await
        .expect("tool-loop turn accepted");
    timeout(Duration::from_secs(2), started.notified())
        .await
        .expect("live executor started");

    handle
        .register_interaction_with_delivery(
            "session",
            "multi-input-turn",
            InteractionState::AwaitingUserInput {
                interaction_id: "input-first".to_string(),
            },
            Box::new(PersistUserInputDelivery),
        )
        .await
        .expect("first user-input delivery registered");
    handle
        .respond(
            "session",
            "multi-input-turn",
            InteractionResponse::new("input-first", "answered-first"),
        )
        .await
        .expect("first user-input settles without completing the turn");

    handle
        .register_interaction_with_delivery(
            "session",
            "multi-input-turn",
            InteractionState::AwaitingUserInput {
                interaction_id: "input-second".to_string(),
            },
            Box::new(PersistUserInputDelivery),
        )
        .await
        .expect("second user-input delivery registered");
    handle
        .respond(
            "session",
            "multi-input-turn",
            InteractionResponse::new("input-second", "answered-second"),
        )
        .await
        .expect("second user-input settles");

    allow_complete.notify_one();
    timeout(Duration::from_secs(2), async {
        loop {
            let snapshot = handle.snapshot("session").await.expect("snapshot");
            if !matches!(
                snapshot.interaction,
                InteractionState::Running
                    | InteractionState::AwaitingUserInput { .. }
                    | InteractionState::Cancelling
            ) {
                break;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("turn leaves Running after both resolutions");

    let events: Vec<_> = store
        .load_code_workflow_replay()
        .expect("replay")
        .events
        .into_iter()
        .map(|event| event.event)
        .collect();
    assert!(
        events.iter().any(|event| matches!(
            event,
            CodeWorkflowEventKind::CommandTerminalSuccessWithInteractionResolved {
                interaction_id,
                resolution,
                prior_interaction_resolutions,
                ..
            } if interaction_id == "input-second"
                && resolution == "answered-second"
                && prior_interaction_resolutions == &vec![(
                    "input-first".to_string(),
                    "answered-first".to_string(),
                )]
        )),
        "all sequential resolutions must share one crash-atomic terminal row: {events:?}"
    );
    assert!(
        !events.iter().any(|event| matches!(
            event,
            CodeWorkflowEventKind::InteractionResolved { interaction_id, .. }
                if interaction_id == "input-first"
        )),
        "the earlier resolution must not be exposed as a torn-write prefix: {events:?}"
    );
    worker.abort();
}

/// W2-05: multi-question request input remains pending until every answer is
/// present, and runtime cancellation drops the continuation fail-closed.
async fn exercise_request_user_input_multi_question_and_cancel_fail_closed() {
    use std::{
        collections::HashMap,
        sync::{Arc, atomic::AtomicBool},
    };

    use libra::internal::ai::runtime::{
        AgentRuntimeWorker, AgentRuntimeWorkerConfig, ExternalTurnTrackingExecutor,
        InMemoryAuditSink, InteractionResponse, InteractionState, RuntimeExecutionContext,
        RuntimeInteractionDelivery, RuntimeTurnExecution, RuntimeWorkerError, ToolBoundaryRuntime,
        TurnRequest,
    };
    use tokio::sync::oneshot;
    use tokio_util::sync::CancellationToken;

    struct MultiQuestionDelivery {
        question_ids: Vec<String>,
        sender: oneshot::Sender<HashMap<String, Vec<String>>>,
    }

    #[async_trait]
    impl RuntimeInteractionDelivery for MultiQuestionDelivery {
        fn validate(&self, interaction: &InteractionResponse) -> Result<(), RuntimeWorkerError> {
            let answers =
                serde_json::from_str::<HashMap<String, Vec<String>>>(&interaction.response)
                    .map_err(|error| RuntimeWorkerError::ExecutionFailed(error.to_string()))?;
            for question_id in &self.question_ids {
                let values = answers.get(question_id).ok_or_else(|| {
                    RuntimeWorkerError::ExecutionFailed(format!(
                        "missing answer for question '{question_id}'"
                    ))
                })?;
                if values.is_empty() || values.iter().any(|value| value.trim().is_empty()) {
                    return Err(RuntimeWorkerError::ExecutionFailed(format!(
                        "empty answer for question '{question_id}'"
                    )));
                }
            }
            if answers.len() != self.question_ids.len() {
                return Err(RuntimeWorkerError::ExecutionFailed(
                    "response contains an unknown question id".to_string(),
                ));
            }
            Ok(())
        }

        async fn deliver(
            self: Box<Self>,
            _request: TurnRequest,
            interaction: InteractionResponse,
            _context: RuntimeExecutionContext,
        ) -> Result<RuntimeTurnExecution, RuntimeWorkerError> {
            self.validate(&interaction)?;
            let answers = serde_json::from_str(&interaction.response)
                .map_err(|error| RuntimeWorkerError::ExecutionFailed(error.to_string()))?;
            self.sender.send(answers).map_err(|_| {
                RuntimeWorkerError::ExecutionFailed(
                    "request_user_input receiver closed".to_string(),
                )
            })?;
            Ok(RuntimeTurnExecution::InteractionResponseDelivered)
        }
    }

    let (handle, worker) = AgentRuntimeWorker::spawn(AgentRuntimeWorkerConfig::new(
        Arc::new(ExternalTurnTrackingExecutor),
        ToolBoundaryRuntime::system(Uuid::new_v4(), Arc::new(InMemoryAuditSink::default())),
    ));

    let (answer_tx, answer_rx) = oneshot::channel();
    handle
        .track_external_turn(
            TurnRequest::new("session", "answer-turn", "request input", false),
            CancellationToken::new(),
            Arc::new(AtomicBool::new(false)),
        )
        .await
        .expect("runtime tracks request_user_input turn");
    handle
        .register_interaction_with_delivery(
            "session",
            "answer-turn",
            InteractionState::AwaitingUserInput {
                interaction_id: "input-1".to_string(),
            },
            Box::new(MultiQuestionDelivery {
                question_ids: vec!["language".to_string(), "edition".to_string()],
                sender: answer_tx,
            }),
        )
        .await
        .expect("runtime owns multi-question continuation");

    assert!(
        handle
            .respond(
                "session",
                "answer-turn",
                InteractionResponse::new("input-1", r#"{"language":["Rust"]}"#),
            )
            .await
            .is_err()
    );
    assert!(matches!(
        handle
            .snapshot("session")
            .await
            .expect("snapshot")
            .interaction,
        InteractionState::AwaitingUserInput { .. }
    ));
    handle
        .respond(
            "session",
            "answer-turn",
            InteractionResponse::new("input-1", r#"{"language":["Rust"],"edition":["2024"]}"#),
        )
        .await
        .expect("complete multi-question response is delivered once");
    assert_eq!(
        answer_rx
            .await
            .expect("tool continuation receives answers")
            .len(),
        2
    );
    handle
        .finish_external_turn(
            "session",
            "answer-turn",
            Ok(RuntimeTurnExecution::Completed {
                summary: "input answered".to_string(),
            }),
        )
        .await
        .expect("answered turn finalizes");

    let (cancel_tx, cancel_rx) = oneshot::channel();
    handle
        .track_external_turn(
            TurnRequest::new("session", "cancel-turn", "request input", false),
            CancellationToken::new(),
            Arc::new(AtomicBool::new(false)),
        )
        .await
        .expect("runtime tracks cancellation turn");
    handle
        .register_interaction_with_delivery(
            "session",
            "cancel-turn",
            InteractionState::AwaitingUserInput {
                interaction_id: "input-2".to_string(),
            },
            Box::new(MultiQuestionDelivery {
                question_ids: vec!["confirm".to_string()],
                sender: cancel_tx,
            }),
        )
        .await
        .expect("runtime owns cancellation continuation");
    handle
        .cancel("session", "cancel-turn")
        .await
        .expect("runtime cancellation accepted");
    assert!(
        cancel_rx.await.is_err(),
        "cancellation drops delivery sender fail-closed"
    );
    worker.abort();
}

/// W2-05: runtime state, not an adapter-local map, remains the source of
/// truth while malformed input is retried.
#[tokio::test]
async fn request_user_input_multi_question_and_cancel_fail_closed() {
    exercise_request_user_input_multi_question_and_cancel_fail_closed().await;
}

/// W2-05: adapters must not keep private pending maps; runtime registration is
/// the sole owner of approval/user-input continuations.
#[tokio::test]
async fn interaction_pending_owner_is_runtime_only() {
    // Behavioral owner: malformed answers stay on InteractionState until a
    // complete respond succeeds (same exercise as multi-question AC).
    exercise_request_user_input_multi_question_and_cancel_fail_closed().await;

    // Source pin: headless/Codex no longer retain HashMap-backed pending
    // continuations outside AgentRuntimeWorker.
    let headless = include_str!("../src/internal/ai/web/headless.rs");
    assert!(
        !headless.contains("pending_user_inputs") && !headless.contains("pending_exec_approvals"),
        "headless must not own private pending_user_inputs/pending_exec_approvals maps"
    );
    let codex = include_str!("../src/internal/ai/codex/mod.rs");
    assert!(
        !codex.contains("pending_approvals"),
        "managed Codex adapter must not own a private pending_approvals map"
    );
}

// ---------------------------------------------------------------------------
// CEX-00.5: top-level Event / Snapshot trait contract
// ---------------------------------------------------------------------------

mod cex_00_5 {
    use chrono::Utc;
    use libra::internal::ai::{
        hooks::lifecycle::{LifecycleEvent, LifecycleEventKind},
        runtime::{
            Event, Snapshot, audit_action_for,
            contracts::{MaterializedProjection, ProjectionFreshness, ProjectionVersions},
        },
    };
    use uuid::Uuid;

    fn lifecycle(kind: LifecycleEventKind) -> LifecycleEvent {
        LifecycleEvent {
            kind,
            session_id: "test-session".to_string(),
            session_ref: None,
            prompt: None,
            model: None,
            source: None,
            tool_name: None,
            tool_input: None,
            tool_response: None,
            assistant_message: None,
            timestamp: Utc::now(),
        }
    }

    fn projection(thread_id: Uuid) -> MaterializedProjection {
        MaterializedProjection {
            thread_id,
            versions: ProjectionVersions::default(),
            freshness: ProjectionFreshness::Fresh,
            summary: serde_json::Value::Null,
        }
    }

    #[test]
    fn lifecycle_event_kinds_match_display_strings() {
        // The Event::event_kind impl must mirror the existing Display impl
        // verbatim; drift between them would fork audit / wire / log readers.
        let cases = [
            (LifecycleEventKind::SessionStart, "session_start"),
            (LifecycleEventKind::TurnStart, "turn_start"),
            (LifecycleEventKind::ToolUse, "tool_use"),
            (LifecycleEventKind::ModelUpdate, "model_update"),
            (LifecycleEventKind::Compaction, "compaction"),
            (LifecycleEventKind::TurnEnd, "turn_end"),
            (LifecycleEventKind::SessionEnd, "session_end"),
        ];
        for (kind, expected) in cases {
            let event = lifecycle(kind);
            assert_eq!(event.event_kind(), expected);
            assert_eq!(format!("{}", event.kind), expected);
        }
    }

    /// CEX-00.5 P2 fix (round 3 — byte-for-byte golden): pin the actual
    /// `Uuid::new_v5` output for a fixed `(session_id, timestamp_nanos,
    /// kind)` tuple so that **any** change to either the namespace UUID
    /// (`LIFECYCLE_EVENT_NAMESPACE` in `lifecycle.rs`) or the name-bytes
    /// layout will fail this test. Audit logs may persist `event_id`, so a
    /// silent change to the derivation would break dedupe / correlation
    /// across upgrades.
    ///
    /// To regenerate the golden value (only on a deliberate, versioned
    /// migration to a new namespace / layout):
    /// ```text
    /// $ cargo test --test ai_runtime_contract_test \
    ///     lifecycle_event_id_v5_golden -- --nocapture
    /// ```
    /// then copy the printed value into `EXPECTED_GOLDEN` and document
    /// the migration in the audit closure.
    #[test]
    fn lifecycle_event_id_v5_golden_value_is_stable() {
        const EXPECTED_GOLDEN: &str = "69eaa838-b433-55f6-8068-d943a56cfcb8";

        let event = LifecycleEvent {
            kind: LifecycleEventKind::TurnStart,
            session_id: "golden".to_string(),
            session_ref: None,
            prompt: None,
            model: None,
            source: None,
            tool_name: None,
            tool_input: None,
            tool_response: None,
            assistant_message: None,
            timestamp: chrono::DateTime::<Utc>::from_timestamp_nanos(1_700_000_000_000_000_000),
        };

        let id = event.event_id();
        let expected = Uuid::parse_str(EXPECTED_GOLDEN).expect("parseable golden UUID");
        assert_eq!(
            id, expected,
            "lifecycle event_id derivation drifted — see test docs for migration steps"
        );

        // Structural sanity (catches the case where someone updates both
        // the golden and the derivation but breaks the version/variant).
        assert_eq!(id.get_version_num(), 5, "must be UUIDv5 (SHA-1 namespaced)");
        assert_eq!(
            id.get_variant(),
            uuid::Variant::RFC4122,
            "must be RFC 4122 variant"
        );
    }

    /// CEX-00.5 P1 (R-A3) test coverage: the `Event` trait does not
    /// enforce envelope-with-typed-payload at compile time, but the
    /// canonical pattern (`tag = "kind", content = "payload"` plus an
    /// `untagged` Known/Unknown wrapper) MUST stay reachable so concrete
    /// implementors keep using it. This test exercises the pattern with a
    /// tiny in-test event hierarchy and proves an unknown future variant
    /// falls through to `Unknown(Value)` instead of erroring.
    #[test]
    fn r_a3_envelope_pattern_round_trips_and_survives_unknown_kinds() {
        use serde::{Deserialize, Serialize};
        use serde_json::json;

        #[derive(Debug, Serialize, Deserialize, PartialEq, Eq)]
        #[serde(tag = "kind", content = "payload", rename_all = "snake_case")]
        enum DemoEvent {
            Started { id: u64 },
            Stopped { id: u64, reason: String },
        }

        #[derive(Debug, Serialize, Deserialize)]
        #[serde(untagged)]
        enum DemoEventEnvelope {
            Known(Box<DemoEvent>),
            Unknown(serde_json::Value),
        }

        // 1. A known variant round-trips through the envelope.
        let started = DemoEvent::Started { id: 7 };
        let wire = serde_json::to_value(&started).unwrap();
        assert_eq!(wire["kind"], "started");
        let envelope: DemoEventEnvelope = serde_json::from_value(wire).unwrap();
        match envelope {
            DemoEventEnvelope::Known(event) => assert_eq!(*event, started),
            DemoEventEnvelope::Unknown(_) => panic!("known variant must not fall through"),
        }

        // 2. An unknown future variant falls through to Unknown(Value)
        // and the raw payload is preserved verbatim.
        let future = json!({
            "kind": "future_variant",
            "payload": { "anything": [1, 2, 3] }
        });
        let envelope: DemoEventEnvelope = serde_json::from_value(future.clone())
            .expect("unknown kind must not error — R-A3 / S2-INV-10");
        match envelope {
            DemoEventEnvelope::Known(_) => panic!("unknown kind must not parse as Known"),
            DemoEventEnvelope::Unknown(raw) => assert_eq!(raw, future),
        }
    }

    #[test]
    fn lifecycle_event_id_is_deterministic_and_collision_safe() {
        // CEX-00.5 P2 fix: derive `event_id()` deterministically from
        // (session_id, timestamp_nanos, kind) so the id is stable for an
        // occurrence and distinct events do not silently collide.
        let mut a = lifecycle(LifecycleEventKind::TurnStart);
        a.session_id = "alpha".to_string();
        a.timestamp = chrono::DateTime::<Utc>::from_timestamp_nanos(1_700_000_000_000_000_000);
        let mut b = a.clone();

        // Same input -> same id.
        assert_eq!(a.event_id(), b.event_id());
        // Stable across clones / impl Trait coercion.
        let dyn_ref: &dyn Event = &a;
        assert_eq!(dyn_ref.event_id(), a.event_id());

        // Different session -> different id.
        b.session_id = "beta".to_string();
        assert_ne!(a.event_id(), b.event_id());

        // Different timestamp -> different id.
        let mut c = a.clone();
        c.timestamp = a.timestamp + chrono::Duration::nanoseconds(1);
        assert_ne!(a.event_id(), c.event_id());

        // Different kind -> different id.
        let mut d = a.clone();
        d.kind = LifecycleEventKind::ToolUse;
        assert_ne!(a.event_id(), d.event_id());

        // Never the nil UUID for a real event.
        assert_ne!(a.event_id(), Uuid::nil());
    }

    #[test]
    fn lifecycle_event_summary_includes_kind_and_session() {
        let event = lifecycle(LifecycleEventKind::ToolUse);
        let summary = event.event_summary();
        assert!(summary.contains("kind=tool_use"));
        assert!(summary.contains("session=test-session"));
    }

    #[test]
    fn lifecycle_event_summary_carries_tool_when_present() {
        let mut event = lifecycle(LifecycleEventKind::ToolUse);
        event.tool_name = Some("apply_patch".to_string());
        let summary = event.event_summary();
        assert!(summary.contains("tool=apply_patch"));
    }

    #[test]
    fn audit_action_for_lifecycle_event_produces_event_prefixed_kind() {
        let event = lifecycle(LifecycleEventKind::SessionEnd);
        let dyn_ref: &dyn Event = &event;
        assert_eq!(audit_action_for(dyn_ref), "event/session_end");
    }

    #[test]
    fn event_trait_is_dyn_compatible() {
        // CEX-00.5 contract: Event must be dyn-compatible so callers can
        // pass `&dyn Event` (e.g. into `AuditSink::record_event`).
        let event = lifecycle(LifecycleEventKind::Compaction);
        let dyn_ref: &dyn Event = &event;
        let _kind = dyn_ref.event_kind();
    }

    #[test]
    fn lifecycle_event_kind_strings_are_stable_snake_case() {
        for kind in [
            LifecycleEventKind::SessionStart,
            LifecycleEventKind::TurnStart,
            LifecycleEventKind::ToolUse,
            LifecycleEventKind::ModelUpdate,
            LifecycleEventKind::Compaction,
            LifecycleEventKind::TurnEnd,
            LifecycleEventKind::SessionEnd,
        ] {
            let event = lifecycle(kind);
            let kind_str = event.event_kind();
            assert!(
                kind_str.chars().all(|c| c.is_ascii_lowercase() || c == '_'),
                "event_kind '{kind_str}' must be snake_case ascii"
            );
            assert!(!kind_str.is_empty());
        }
    }

    #[test]
    fn materialized_projection_snapshot_id_is_thread_id() {
        let id = Uuid::new_v4();
        let snap = projection(id);
        assert_eq!(snap.snapshot_kind(), "materialized_projection");
        assert_eq!(snap.snapshot_id(), id);
    }

    #[test]
    fn snapshot_trait_is_dyn_compatible() {
        let snap = projection(Uuid::nil());
        let dyn_ref: &dyn Snapshot = &snap;
        assert_eq!(dyn_ref.snapshot_kind(), "materialized_projection");
    }

    #[test]
    fn snapshot_id_is_stable_under_clone() {
        let id = Uuid::new_v4();
        let snap = projection(id);
        let cloned = snap.clone();
        assert_eq!(snap.snapshot_id(), cloned.snapshot_id());
        assert_eq!(snap.snapshot_kind(), cloned.snapshot_kind());
    }
}
