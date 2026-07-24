//! Persistence layer for orchestrator plans, tasks, runs, evidence, decisions, and
//! projection records.
//!
//! Boundary: persistence writes immutable AI objects plus index rows; it must preserve
//! idempotency for retries and produce rebuildable projection state. Storage-flow,
//! schema-migration, and scheduler tests cover replay, duplicate writes, and missing
//! preview artifacts.

use std::{
    collections::{BTreeMap, BTreeSet, HashMap},
    fs,
    path::{Path, PathBuf},
    process::Command,
    sync::Arc,
};

use chrono::Utc;
use git_internal::{
    hash::ObjectHash,
    internal::object::{
        context_frame::{ContextFrame, FrameKind},
        patchset::PatchSet as GitPatchSet,
        plan::Plan as GitPlan,
        provenance::Provenance as GitProvenance,
        run::Run as GitRun,
        run_event::{RunEvent, RunEventKind},
        task_event::{TaskEvent, TaskEventKind},
        types::{ActorRef, ObjectType},
    },
};
use rmcp::model::CallToolResult;
use serde_json::json;
use tokio::sync::{Mutex, mpsc, oneshot};
use uuid::Uuid;

use super::{
    checkpoint_policy::{checkpoint_before_replan, checkpoint_on_replan},
    run_state::RunStateSnapshot,
    types::{
        DecisionOutcome, ExecutionPlanSpec, GateReport, GateStage, OrchestratorError,
        PersistedCheckpoint, PersistedDerivedRecords, PersistedExecution,
        PersistedPlanReviewBundle, PersistedTaskArtifacts, SystemReport, TaskKind, TaskResult,
        ToolCallRecord,
    },
};
use crate::{
    internal::ai::{
        codex::{
            model::{
                EvidenceEvent, IntentSnapshot, PatchSetSnapshot, PlanSnapshot, PlanStepSnapshot,
                ProvenanceSnapshot, RunSnapshot, TaskSnapshot, ToolInvocationEvent,
            },
            types::{FileChange, PatchStatus},
        },
        completion::CompletionUsageSummary,
        intentspec::{persistence::persist_intentspec, types::IntentSpec},
        mcp::{
            resource::{
                AgentInstanceParams, ArtifactParams, ContextItemParams,
                CreateContextSnapshotParams, CreateDecisionParams, CreateEvidenceParams,
                CreatePatchSetParams, CreatePlanParams, CreatePlanStepEventParams,
                CreateProvenanceParams, CreateRunParams, CreateRunUsageParams, CreateTaskParams,
                CreateToolInvocationParams, IoFootprintParams, PlanStepParams, TouchedFileParams,
                UpdateIntentParams,
            },
            server::LibraMcpServer,
        },
        projection::ProjectionRebuilder,
        runtime::{
            DecisionPolicy, DecisionProposal, DecisionProposalRoute, DecisionProposalStore,
            FinalDecision, FinalDecisionStore, ValidationOutcome, ValidationReportStore,
            ValidationStage, ValidationStageResult, ValidatorEngine, aggregate_risk_score,
            build_decision_proposal,
            contracts::{EvidenceKind, FinalDecisionVerdict, TaskExecutionStatus},
            phase3::{ArtifactLedger, TaskArtifactRefs},
        },
        session::{SessionStore, jsonl::SessionJsonlStore},
        tools::ToolOutput,
        workflow_objects::{build_git_intent, build_git_plan, parse_object_id},
    },
    utils::{storage_ext::StorageExt, util::try_get_storage_path},
};

const ZERO_COMMIT_SHA: &str = "0000000000000000000000000000000000000000";

pub struct ExecutionPersistenceRequest<'a> {
    pub mcp_server: &'a Arc<LibraMcpServer>,
    pub spec: &'a IntentSpec,
    pub execution_plan_spec: &'a ExecutionPlanSpec,
    pub plan_revision_specs: &'a [ExecutionPlanSpec],
    pub run_state: &'a RunStateSnapshot,
    pub system_report: &'a SystemReport,
    pub decision: &'a DecisionOutcome,
    pub working_dir: &'a Path,
    pub base_commit: Option<&'a str>,
    pub model_name: &'a str,
}

pub struct ExecutionFinalizeRequest<'a> {
    pub spec: &'a IntentSpec,
    pub execution_plan_spec: &'a ExecutionPlanSpec,
    pub plan_revision_specs: &'a [ExecutionPlanSpec],
    pub run_state: &'a RunStateSnapshot,
    pub system_report: &'a SystemReport,
    pub decision: &'a DecisionOutcome,
    pub working_dir: &'a Path,
    pub model_name: &'a str,
}

struct PatchSetRequest<'a> {
    mcp_server: &'a Arc<LibraMcpServer>,
    run_id: &'a str,
    base_commit_sha: &'a str,
    generation: u32,
    task_title: &'a str,
    task_objective: &'a str,
    tool_calls: &'a [ToolCallRecord],
}

struct EvidenceRequest<'a> {
    mcp_server: &'a Arc<LibraMcpServer>,
    run_id: &'a str,
    patchset_id: Option<&'a str>,
    kind: &'a str,
    tool: &'a str,
    command: Option<String>,
    exit_code: Option<i32>,
    summary: Option<String>,
}

struct FinalDecisionRequest<'a> {
    mcp_server: &'a Arc<LibraMcpServer>,
    run_id: &'a str,
    chosen_patchset_id: Option<&'a str>,
    checkpoint_id: Option<&'a str>,
    execution_plan: &'a ExecutionPlanSpec,
    task_results: &'a [TaskResult],
    system_report: &'a SystemReport,
    decision: &'a DecisionOutcome,
}

struct RunRequest<'a> {
    mcp_server: &'a Arc<LibraMcpServer>,
    task_id: &'a str,
    base_commit_sha: &'a str,
    plan_id: Option<&'a str>,
    context_snapshot_id: Option<&'a str>,
    task_results: &'a [TaskResult],
    decision: &'a DecisionOutcome,
    model_name: &'a str,
}

struct PersistedTaskRequest<'a> {
    mcp_server: &'a Arc<LibraMcpServer>,
    intent_id: &'a str,
    parent_task_id: Option<&'a str>,
    task: &'a super::types::TaskSpec,
    dependency_task_ids: Vec<String>,
    persisted_step_id: Option<Uuid>,
    status: &'a str,
}

struct PersistedPlanRevision {
    plan_id: String,
    step_id_map: HashMap<Uuid, Uuid>,
}

struct PersistedPlanSet {
    execution: PersistedPlanRevision,
    test: PersistedPlanRevision,
    step_id_map: HashMap<Uuid, Uuid>,
    plan_id_by_task_id: HashMap<Uuid, String>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum PersistedPlanRole {
    Execution,
    Test,
}

impl PersistedPlanRole {
    fn label(self) -> &'static str {
        match self {
            Self::Execution => "execution",
            Self::Test => "test",
        }
    }

    fn synthetic_step_description(self) -> &'static str {
        match self {
            Self::Execution => "No implementation or analysis tasks required",
            Self::Test => "No test gates required",
        }
    }
}

struct PlanSnapshotFamilyRequest<'a> {
    thread_id: &'a str,
    intent_id: &'a str,
    root_task_id: &'a str,
    plan_id: &'a str,
    parent_plan_id: Option<&'a str>,
    plan: &'a ExecutionPlanSpec,
    persisted_step_ids: &'a HashMap<Uuid, Uuid>,
    persisted_task_ids: &'a HashMap<Uuid, String>,
}

struct RunEventRequest<'a> {
    run_id: &'a str,
    kind: RunEventKind,
    reason: Option<String>,
    error: Option<String>,
    metrics: Option<serde_json::Value>,
    patchset_id: Option<&'a str>,
}

struct PlanStepEventsRequest<'a> {
    mcp_server: &'a Arc<LibraMcpServer>,
    plan_id: &'a str,
    fallback_run_id: &'a str,
    plan: &'a ExecutionPlanSpec,
    run_state: &'a RunStateSnapshot,
    persisted_step_ids: &'a HashMap<Uuid, Uuid>,
    persisted_task_ids: &'a HashMap<Uuid, String>,
    persisted_task_run_ids: &'a HashMap<Uuid, String>,
}

struct RuntimeAuditState {
    thread_id: String,
    intent_id: String,
    root_task_id: String,
    run_id: String,
    base_commit_sha: String,
    initial_snapshot_id: Option<String>,
    plan_ids: Vec<String>,
    latest_plan_id: Option<String>,
    latest_execution_plan_id: Option<String>,
    latest_test_plan_id: Option<String>,
    latest_plan_revision: Option<u32>,
    // Both maps are derived from the same persisted plan revision:
    // step_id -> persisted_step_id for runtime event lookup by task.step_id(),
    // task_id -> persisted_step_id for final result lookup by task result id.
    persisted_step_ids: HashMap<Uuid, Uuid>,
    persisted_step_ids_by_task_id: HashMap<Uuid, Uuid>,
    persisted_task_ids: HashMap<Uuid, String>,
    persisted_plan_ids_by_task_id: HashMap<Uuid, String>,
    persisted_task_run_ids: HashMap<Uuid, String>,
    latest_task_event_kind: HashMap<Uuid, TaskEventKind>,
    latest_plan_step_status: HashMap<Uuid, &'static str>,
    latest_run_event_kind: Option<RunEventKind>,
    latest_task_run_event_kind: HashMap<Uuid, RunEventKind>,
    preview_plan_id: Option<String>,
    preview_test_plan_id: Option<String>,
}

enum RuntimeAuditCommand {
    TaskRuntime {
        task: Box<super::types::TaskSpec>,
        event: Box<super::types::TaskRuntimeEvent>,
    },
    Flush {
        ack: oneshot::Sender<()>,
    },
    Shutdown,
}

pub struct ExecutionAuditSession {
    mcp_server: Arc<LibraMcpServer>,
    actor: ActorRef,
    state: Arc<Mutex<RuntimeAuditState>>,
    tx: mpsc::UnboundedSender<RuntimeAuditCommand>,
    worker: tokio::task::JoinHandle<()>,
    runtime_observer: Arc<dyn super::types::OrchestratorObserver>,
}

struct RuntimeAuditObserver {
    tx: mpsc::UnboundedSender<RuntimeAuditCommand>,
}

impl super::types::OrchestratorObserver for RuntimeAuditObserver {
    fn on_task_runtime_event(
        &self,
        task: &super::types::TaskSpec,
        event: super::types::TaskRuntimeEvent,
    ) {
        // Streaming reasoning deltas are UI-only; persisting each token-sized
        // fragment can create a large audit backlog before finalization.
        if matches!(event, super::types::TaskRuntimeEvent::ThinkingDelta(_)) {
            return;
        }

        let _ = self.tx.send(RuntimeAuditCommand::TaskRuntime {
            task: Box::new(task.clone()),
            event: Box::new(event),
        });
    }
}

impl ExecutionAuditSession {
    pub async fn start(
        mcp_server: Arc<LibraMcpServer>,
        spec: &IntentSpec,
        working_dir: &Path,
        persisted_intent_id: Option<&str>,
        persisted_plan_bundle: Option<PersistedPlanReviewBundle>,
        persisted_plan_id: Option<&str>,
    ) -> Result<Self, OrchestratorError> {
        let actor = resolve_actor(&mcp_server, Some("system"), Some("libra-orchestrator"))?;
        let base_commit_sha = resolve_base_commit(None, working_dir);
        let intent_id = match persisted_intent_id {
            Some(intent_id) => intent_id.to_string(),
            None => persist_intentspec(spec, &mcp_server).await.map_err(|e| {
                OrchestratorError::ConfigError(format!("MCP create_intent failed: {e}"))
            })?,
        };
        persist_intent_snapshot(&mcp_server, spec, &intent_id).await?;
        let initial_snapshot_id = if snapshot_on_run_start(spec) {
            Some(
                create_context_snapshot(
                    &mcp_server,
                    build_snapshot_summary(spec, None, "Run start context snapshot"),
                    collect_snapshot_items(spec, None, working_dir, &[]),
                )
                .await?,
            )
        } else {
            None
        };
        let root_task_id = create_execution_task(
            &mcp_server,
            &intent_id,
            &format!("spec execution for {}", spec.intent.summary),
            spec.intent.problem_statement.as_str(),
            "running",
            "orchestrator execution root task",
        )
        .await?;
        let run_id = create_initial_run(
            &mcp_server,
            &root_task_id,
            &base_commit_sha,
            initial_snapshot_id.as_deref(),
        )
        .await?;
        let preview_plan_id = persisted_plan_bundle
            .as_ref()
            .map(|bundle| bundle.plan_id.clone())
            .or_else(|| persisted_plan_id.map(ToString::to_string));
        let preview_test_plan_id = persisted_plan_bundle
            .as_ref()
            .map(|bundle| bundle.test_plan_id.clone());
        let preview_step_ids = persisted_plan_bundle
            .as_ref()
            .map(|bundle| bundle.step_ids.clone())
            .unwrap_or_default();
        let preview_task_ids = persisted_plan_bundle
            .as_ref()
            .map(|bundle| bundle.task_ids.clone())
            .unwrap_or_default();
        let state = Arc::new(Mutex::new(RuntimeAuditState {
            thread_id: intent_id.clone(),
            intent_id,
            root_task_id,
            run_id,
            base_commit_sha,
            initial_snapshot_id,
            plan_ids: Vec::new(),
            latest_plan_id: None,
            latest_execution_plan_id: None,
            latest_test_plan_id: None,
            latest_plan_revision: None,
            persisted_step_ids: preview_step_ids,
            persisted_step_ids_by_task_id: HashMap::new(),
            persisted_task_ids: preview_task_ids,
            persisted_plan_ids_by_task_id: HashMap::new(),
            persisted_task_run_ids: HashMap::new(),
            latest_task_event_kind: HashMap::new(),
            latest_plan_step_status: HashMap::new(),
            latest_run_event_kind: Some(RunEventKind::Created),
            latest_task_run_event_kind: HashMap::new(),
            preview_plan_id,
            preview_test_plan_id,
        }));
        let (tx, rx) = mpsc::unbounded_channel();
        let observer: Arc<dyn super::types::OrchestratorObserver> =
            Arc::new(RuntimeAuditObserver { tx: tx.clone() });
        let worker = tokio::spawn(runtime_audit_worker(
            Arc::clone(&mcp_server),
            actor.clone(),
            Arc::clone(&state),
            rx,
        ));
        Ok(Self {
            mcp_server,
            actor,
            state,
            tx,
            worker,
            runtime_observer: observer,
        })
    }

    pub fn observer(&self) -> Arc<dyn super::types::OrchestratorObserver> {
        Arc::clone(&self.runtime_observer)
    }

    pub async fn record_plan_compiled(
        &self,
        plan: &ExecutionPlanSpec,
    ) -> Result<(), OrchestratorError> {
        let (intent_id, root_task_id, parent_execution_plan_id, parent_test_plan_id, thread_id) = {
            let state = self.state.lock().await;
            (
                state.intent_id.clone(),
                state.root_task_id.clone(),
                state.latest_execution_plan_id.clone(),
                state.latest_test_plan_id.clone(),
                state.thread_id.clone(),
            )
        };
        let preview_plan_id = {
            let state = self.state.lock().await;
            (plan.revision == 1 && state.plan_ids.is_empty())
                .then(|| state.preview_plan_id.clone())
                .flatten()
        };
        let preview_test_plan_id = {
            let state = self.state.lock().await;
            (plan.revision == 1 && state.plan_ids.is_empty())
                .then(|| state.preview_test_plan_id.clone())
                .flatten()
        };
        let (persisted_plan_set, can_reuse_preview_tasks) = if let Some(plan_id) = preview_plan_id {
            if let Some(test_plan_id) = preview_test_plan_id {
                match bind_existing_plan_set(&self.mcp_server, &plan_id, &test_plan_id, plan).await
                {
                    Ok(plan_set) => (plan_set, true),
                    Err(error) if is_missing_persisted_plan_error(&error) => {
                        tracing::warn!(
                            plan_id = %plan_id,
                            test_plan_id = %test_plan_id,
                            "preview plan set was not found during execution; creating a new plan revision"
                        );
                        (
                            create_plan_set_revision(
                                &self.mcp_server,
                                &intent_id,
                                parent_execution_plan_id.as_deref(),
                                parent_test_plan_id.as_deref(),
                                plan,
                            )
                            .await?,
                            false,
                        )
                    }
                    Err(error) => return Err(error),
                }
            } else {
                match bind_existing_plan_revision(&self.mcp_server, &plan_id, plan).await {
                    Ok(execution_plan) => {
                        let test_plan = create_plan_revision_for_role(
                            &self.mcp_server,
                            &intent_id,
                            parent_test_plan_id.as_deref(),
                            plan,
                            PersistedPlanRole::Test,
                        )
                        .await?;
                        (build_plan_set(plan, execution_plan, test_plan)?, true)
                    }
                    Err(error) if is_missing_persisted_plan_error(&error) => {
                        tracing::warn!(
                            plan_id = %plan_id,
                            "preview plan was not found during execution; creating a new plan revision"
                        );
                        (
                            create_plan_set_revision(
                                &self.mcp_server,
                                &intent_id,
                                parent_execution_plan_id.as_deref(),
                                parent_test_plan_id.as_deref(),
                                plan,
                            )
                            .await?,
                            false,
                        )
                    }
                    Err(error) => return Err(error),
                }
            }
        } else {
            (
                create_plan_set_revision(
                    &self.mcp_server,
                    &intent_id,
                    parent_execution_plan_id.as_deref(),
                    parent_test_plan_id.as_deref(),
                    plan,
                )
                .await?,
                false,
            )
        };
        let preview_task_ids = if can_reuse_preview_tasks {
            let state = self.state.lock().await;
            state.persisted_task_ids.clone()
        } else {
            HashMap::new()
        };
        let persisted_task_ids = if preview_task_ids.is_empty() {
            create_compiled_tasks_initial(
                &self.mcp_server,
                &intent_id,
                Some(&root_task_id),
                plan,
                &persisted_plan_set.step_id_map,
            )
            .await?
        } else {
            persisted_task_ids_for_plan(plan, &preview_task_ids)?
        };
        let run_id = {
            let state = self.state.lock().await;
            state.run_id.clone()
        };
        create_pending_plan_step_events(
            &self.mcp_server,
            &persisted_plan_set.execution.plan_id,
            &run_id,
            plan,
            &persisted_plan_set.execution.step_id_map,
            &persisted_task_ids,
        )
        .await?;
        create_pending_plan_step_events(
            &self.mcp_server,
            &persisted_plan_set.test.plan_id,
            &run_id,
            plan,
            &persisted_plan_set.test.step_id_map,
            &persisted_task_ids,
        )
        .await?;
        let persisted_step_ids_by_task_id =
            persisted_step_ids_by_task_for_plan(plan, &persisted_plan_set.step_id_map)?;
        persist_plan_snapshot_family(
            &self.mcp_server,
            PlanSnapshotFamilyRequest {
                thread_id: &thread_id,
                intent_id: &intent_id,
                root_task_id: &root_task_id,
                plan_id: &persisted_plan_set.execution.plan_id,
                parent_plan_id: parent_execution_plan_id.as_deref(),
                plan,
                persisted_step_ids: &persisted_plan_set.execution.step_id_map,
                persisted_task_ids: &persisted_task_ids,
            },
        )
        .await?;
        persist_plan_snapshot_family(
            &self.mcp_server,
            PlanSnapshotFamilyRequest {
                thread_id: &thread_id,
                intent_id: &intent_id,
                root_task_id: &root_task_id,
                plan_id: &persisted_plan_set.test.plan_id,
                parent_plan_id: parent_test_plan_id.as_deref(),
                plan,
                persisted_step_ids: &persisted_plan_set.test.step_id_map,
                persisted_task_ids: &persisted_task_ids,
            },
        )
        .await?;
        let mut state = self.state.lock().await;
        let execution_plan_id = persisted_plan_set.execution.plan_id.clone();
        let test_plan_id = persisted_plan_set.test.plan_id.clone();
        state.plan_ids.push(execution_plan_id.clone());
        state.plan_ids.push(test_plan_id.clone());
        state.latest_plan_id = Some(execution_plan_id.clone());
        state.latest_execution_plan_id = Some(execution_plan_id);
        state.latest_test_plan_id = Some(test_plan_id);
        state.latest_plan_revision = Some(plan.revision);
        state.preview_plan_id = None;
        state.persisted_step_ids = persisted_plan_set.step_id_map;
        state.persisted_step_ids_by_task_id = persisted_step_ids_by_task_id;
        state.persisted_plan_ids_by_task_id = persisted_plan_set.plan_id_by_task_id;
        for (task_id, persisted_task_id) in persisted_task_ids {
            state.persisted_task_ids.insert(task_id, persisted_task_id);
            state
                .latest_task_event_kind
                .entry(task_id)
                .or_insert(TaskEventKind::Created);
            state
                .latest_plan_step_status
                .entry(task_id)
                .or_insert("pending");
        }
        Ok(())
    }

    pub(crate) async fn record_attempt_start(
        &self,
        task: &super::types::TaskSpec,
        model_name: &str,
        summary: Option<String>,
    ) -> Result<crate::internal::ai::runtime::phase2::AttemptWriteOutcome, OrchestratorError> {
        let logical_task_id = task.id();
        let (
            persisted_task_id,
            existing_run_id,
            plan_id,
            step_id,
            base_commit_sha,
            initial_snapshot_id,
            latest_task_event_kind,
            latest_plan_step_status,
            latest_task_run_event_kind,
        ) = {
            let state = self.state.lock().await;
            (
                state
                    .persisted_task_ids
                    .get(&logical_task_id)
                    .cloned()
                    .ok_or_else(|| {
                        OrchestratorError::PersistenceError(format!(
                            "cannot start Phase 2 attempt for task {logical_task_id}: \
                             no persisted task exists; call record_plan_compiled before \
                             write_attempt_start_with_session"
                        ))
                    })?,
                state.persisted_task_run_ids.get(&logical_task_id).cloned(),
                state
                    .persisted_plan_ids_by_task_id
                    .get(&logical_task_id)
                    .cloned()
                    .or_else(|| state.latest_plan_id.clone()),
                state
                    .persisted_step_ids_by_task_id
                    .get(&logical_task_id)
                    .copied(),
                state.base_commit_sha.clone(),
                state.initial_snapshot_id.clone(),
                state.latest_task_event_kind.get(&logical_task_id).cloned(),
                state.latest_plan_step_status.get(&logical_task_id).copied(),
                state
                    .latest_task_run_event_kind
                    .get(&logical_task_id)
                    .cloned(),
            )
        };

        if matches!(
            latest_task_run_event_kind,
            Some(RunEventKind::Completed | RunEventKind::Failed)
        ) {
            return Err(OrchestratorError::PersistenceError(format!(
                "cannot start Phase 2 attempt for task {logical_task_id}: \
                 its task run is already terminal"
            )));
        }

        let start_kind = start_run_event_kind_for_task(task);
        let run_id = if let Some(run_id) = existing_run_id {
            run_id
        } else {
            let run_id = create_task_attempt_run(
                &self.mcp_server,
                task,
                &persisted_task_id,
                plan_id.as_deref(),
                &base_commit_sha,
                initial_snapshot_id.as_deref(),
                model_name,
                start_kind.clone(),
                summary.clone(),
            )
            .await?;
            let mut state = self.state.lock().await;
            state
                .persisted_task_run_ids
                .insert(logical_task_id, run_id.clone());
            state
                .latest_task_run_event_kind
                .insert(logical_task_id, start_kind.clone());
            run_id
        };

        if latest_task_event_kind.as_ref() != Some(&TaskEventKind::Running) {
            append_task_event(
                &self.mcp_server,
                &self.actor,
                &persisted_task_id,
                Some(run_id.as_str()),
                TaskEventKind::Running,
                summary.clone().or_else(|| Some("task started".to_string())),
            )
            .await?;
            let mut state = self.state.lock().await;
            state
                .latest_task_event_kind
                .insert(logical_task_id, TaskEventKind::Running);
        }

        if latest_plan_step_status != Some("progressing")
            && let (Some(plan_id), Some(step_id)) = (plan_id.as_deref(), step_id)
        {
            create_plan_step_event(
                &self.mcp_server,
                plan_id,
                &step_id.to_string(),
                &run_id,
                "progressing",
                &persisted_task_id,
                summary.clone().or_else(|| Some("task started".to_string())),
            )
            .await?;
            let mut state = self.state.lock().await;
            state
                .latest_plan_step_status
                .insert(logical_task_id, "progressing");
        }

        let run_uuid = parse_object_id(&run_id).map_err(|e| {
            OrchestratorError::PersistenceError(format!("invalid persisted attempt run id: {e}"))
        })?;
        Ok(crate::internal::ai::runtime::phase2::write_attempt_start(
            crate::internal::ai::runtime::phase2::AttemptStartParams {
                task_id: logical_task_id,
                run_id: run_uuid,
                summary,
            },
        ))
    }

    pub(crate) async fn record_attempt_finish(
        &self,
        task: &super::types::TaskSpec,
        status: crate::internal::ai::runtime::contracts::TaskExecutionStatus,
        summary: Option<String>,
    ) -> Result<crate::internal::ai::runtime::phase2::AttemptWriteOutcome, OrchestratorError> {
        let logical_task_id = task.id();
        let (
            persisted_task_id,
            run_id,
            plan_id,
            step_id,
            latest_task_event_kind,
            latest_plan_step_status,
            latest_task_run_event_kind,
        ) = {
            let state = self.state.lock().await;
            (
                state
                    .persisted_task_ids
                    .get(&logical_task_id)
                    .cloned()
                    .ok_or_else(|| {
                        OrchestratorError::PersistenceError(format!(
                            "cannot finish Phase 2 attempt for task {logical_task_id}: \
                             no persisted task exists; call record_plan_compiled before \
                             write_attempt_finish_with_session"
                        ))
                    })?,
                state
                    .persisted_task_run_ids
                    .get(&logical_task_id)
                    .cloned()
                    .ok_or_else(|| {
                        OrchestratorError::PersistenceError(format!(
                            "cannot finish Phase 2 attempt for task {logical_task_id}: \
                             no task run was started; call \
                             write_attempt_start_with_session first"
                        ))
                    })?,
                state
                    .persisted_plan_ids_by_task_id
                    .get(&logical_task_id)
                    .cloned()
                    .or_else(|| state.latest_plan_id.clone()),
                state
                    .persisted_step_ids_by_task_id
                    .get(&logical_task_id)
                    .copied(),
                state.latest_task_event_kind.get(&logical_task_id).cloned(),
                state.latest_plan_step_status.get(&logical_task_id).copied(),
                state
                    .latest_task_run_event_kind
                    .get(&logical_task_id)
                    .cloned(),
            )
        };
        let task_event_kind = task_event_kind_for_attempt_status(&status);
        let plan_status = plan_step_status_for_attempt_status(&status);
        let run_event_kind = run_event_kind_for_attempt_status(&status);

        if latest_task_event_kind.as_ref() != Some(&task_event_kind) {
            append_task_event(
                &self.mcp_server,
                &self.actor,
                &persisted_task_id,
                Some(run_id.as_str()),
                task_event_kind.clone(),
                summary.clone(),
            )
            .await?;
            let mut state = self.state.lock().await;
            state
                .latest_task_event_kind
                .insert(logical_task_id, task_event_kind);
        }

        if latest_plan_step_status != Some(plan_status)
            && let (Some(plan_id), Some(step_id)) = (plan_id.as_deref(), step_id)
        {
            create_plan_step_event(
                &self.mcp_server,
                plan_id,
                &step_id.to_string(),
                &run_id,
                plan_status,
                &persisted_task_id,
                summary.clone(),
            )
            .await?;
            let mut state = self.state.lock().await;
            state
                .latest_plan_step_status
                .insert(logical_task_id, plan_status);
        }

        if latest_task_run_event_kind.as_ref() != Some(&run_event_kind) {
            append_run_event(
                &self.mcp_server,
                &self.actor,
                RunEventRequest {
                    run_id: &run_id,
                    kind: run_event_kind.clone(),
                    reason: summary.clone(),
                    error: (run_event_kind == RunEventKind::Failed).then(|| {
                        summary
                            .clone()
                            .unwrap_or_else(|| "task execution failed".to_string())
                    }),
                    metrics: None,
                    patchset_id: None,
                },
            )
            .await?;
            let mut state = self.state.lock().await;
            state
                .latest_task_run_event_kind
                .insert(logical_task_id, run_event_kind);
        }

        let run_uuid = parse_object_id(&run_id).map_err(|e| {
            OrchestratorError::PersistenceError(format!("invalid persisted attempt run id: {e}"))
        })?;
        Ok(crate::internal::ai::runtime::phase2::write_attempt_finish(
            logical_task_id,
            run_uuid,
            status,
            summary,
        ))
    }

    pub async fn finalize(
        self,
        request: ExecutionFinalizeRequest<'_>,
    ) -> Result<PersistedExecution, OrchestratorError> {
        self.flush_runtime_events().await?;
        let task_results = request.run_state.ordered_task_results();
        self.create_task_runs(
            request.execution_plan_spec,
            task_results,
            request.decision,
            request.model_name,
        )
        .await?;
        self.finalize_terminal_events(request.run_state, request.decision, request.model_name)
            .await?;

        let state = self.state.lock().await;
        let thread_id = state.thread_id.clone();
        let root_task_id = state.root_task_id.clone();
        let run_id = state.run_id.clone();
        let base_commit_sha = state.base_commit_sha.clone();
        let plan_ids = state.plan_ids.clone();
        let latest_execution_plan_id = state.latest_execution_plan_id.clone();
        let initial_snapshot_id = state.initial_snapshot_id.clone();
        let persisted_task_ids = state.persisted_task_ids.clone();
        let persisted_plan_ids_by_task_id = state.persisted_plan_ids_by_task_id.clone();
        let persisted_task_run_ids = state.persisted_task_run_ids.clone();
        drop(state);

        let provenance_id = Some(
            create_provenance(
                &self.mcp_server,
                &run_id,
                request.execution_plan_spec,
                task_results,
                request.system_report,
                request.decision,
                request.model_name,
            )
            .await?,
        );
        let run_usage_id = create_run_usage_from_task_results(
            &self.mcp_server,
            &self.actor,
            &run_id,
            task_results,
        )
        .await?;
        let mut checkpoints = create_replan_checkpoints(
            &self.mcp_server,
            request.spec,
            &run_id,
            request.plan_revision_specs,
            request.working_dir,
            task_results,
        )
        .await?;

        let task_index: HashMap<Uuid, _> = request
            .execution_plan_spec
            .tasks
            .iter()
            .map(|task| (task.id(), task))
            .collect();
        let mut persisted_tasks = Vec::with_capacity(task_results.len());
        let mut generation: u32 = 1;

        for result in task_results {
            let task = task_index.get(&result.task_id).ok_or_else(|| {
                OrchestratorError::PlanningFailed(format!(
                    "missing compiled task for result {} during persistence",
                    result.task_id
                ))
            })?;
            let mut persisted = PersistedTaskArtifacts {
                task_id: result.task_id,
                persisted_task_id: persisted_task_ids.get(&result.task_id).cloned(),
                ..PersistedTaskArtifacts::default()
            };
            let task_run_id = persisted_task_run_ids
                .get(&result.task_id)
                .map(String::as_str)
                .unwrap_or(run_id.as_str());

            for call in &result.tool_calls {
                let tool_invocation_id =
                    create_tool_invocation(&self.mcp_server, task_run_id, task.title(), call)
                        .await?;
                persisted.tool_invocation_ids.push(tool_invocation_id);
            }

            if task.kind == TaskKind::Implementation
                && let Some(patchset_id) = create_patchset(PatchSetRequest {
                    mcp_server: &self.mcp_server,
                    run_id: &run_id,
                    base_commit_sha: &base_commit_sha,
                    generation,
                    task_title: task.title(),
                    task_objective: task.objective.as_str(),
                    tool_calls: &result.tool_calls,
                })
                .await?
            {
                generation += 1;
                persist_patchset_snapshot(
                    &self.mcp_server,
                    &thread_id,
                    &run_id,
                    &patchset_id,
                    &result.tool_calls,
                )
                .await?;
                persisted.patchset_id = Some(patchset_id);
            }

            if let Some(report) = &result.gate_report {
                for gate in &report.results {
                    let summary = format!(
                        "{} [{}] {}",
                        gate.check_id,
                        gate.kind,
                        if gate.passed { "passed" } else { "failed" }
                    );
                    let patchset_id = persisted.patchset_id.clone();
                    append_evidence_id(
                        &mut persisted,
                        EvidenceRequest {
                            mcp_server: &self.mcp_server,
                            run_id: if patchset_id.is_some() {
                                run_id.as_str()
                            } else {
                                task_run_id
                            },
                            patchset_id: patchset_id.as_deref(),
                            kind: normalize_evidence_kind(&gate.kind),
                            tool: task_gate_tool_name(task.gate_stage.as_ref()),
                            command: Some(gate.check_id.clone()),
                            exit_code: Some(gate.exit_code),
                            summary: Some(summary),
                        },
                    )
                    .await?;
                }
            }

            if !result.policy_violations.is_empty() {
                let summary = result
                    .policy_violations
                    .iter()
                    .map(|violation| format!("{}: {}", violation.code, violation.message))
                    .collect::<Vec<_>>()
                    .join("; ");
                let patchset_id = persisted.patchset_id.clone();
                append_evidence_id(
                    &mut persisted,
                    EvidenceRequest {
                        mcp_server: &self.mcp_server,
                        run_id: if patchset_id.is_some() {
                            run_id.as_str()
                        } else {
                            task_run_id
                        },
                        patchset_id: patchset_id.as_deref(),
                        kind: "policy",
                        tool: "policy-engine",
                        command: None,
                        exit_code: None,
                        summary: Some(summary),
                    },
                )
                .await?;
            }

            if let Some(review) = &result.review {
                let summary = if review.issues.is_empty() {
                    review.summary.clone()
                } else {
                    format!("{} [{}]", review.summary, review.issues.join("; "))
                };
                let patchset_id = persisted.patchset_id.clone();
                append_evidence_id(
                    &mut persisted,
                    EvidenceRequest {
                        mcp_server: &self.mcp_server,
                        run_id: if patchset_id.is_some() {
                            run_id.as_str()
                        } else {
                            task_run_id
                        },
                        patchset_id: patchset_id.as_deref(),
                        kind: "review",
                        tool: "reviewer",
                        command: None,
                        exit_code: None,
                        summary: Some(summary),
                    },
                )
                .await?;
            }

            persisted_tasks.push(persisted);
        }

        let chosen_patchset_id = if *request.decision == DecisionOutcome::Commit {
            persisted_tasks
                .iter()
                .rev()
                .find_map(|task| task.patchset_id.clone())
        } else {
            None
        };
        let final_checkpoint_id = if *request.decision == DecisionOutcome::HumanReviewRequired {
            Some(
                create_context_snapshot(
                    &self.mcp_server,
                    build_snapshot_summary(
                        request.spec,
                        Some(request.execution_plan_spec),
                        "Human review checkpoint",
                    ),
                    collect_snapshot_items(
                        request.spec,
                        Some(request.execution_plan_spec),
                        request.working_dir,
                        task_results,
                    ),
                )
                .await?,
            )
        } else {
            None
        };
        let decision_id = Some(
            create_decision(FinalDecisionRequest {
                mcp_server: &self.mcp_server,
                run_id: &run_id,
                chosen_patchset_id: chosen_patchset_id.as_deref(),
                checkpoint_id: final_checkpoint_id.as_deref(),
                execution_plan: request.execution_plan_spec,
                task_results,
                system_report: request.system_report,
                decision: request.decision,
            })
            .await?,
        );
        record_terminal_intent_event(&self.mcp_server, &thread_id, request.decision).await?;
        let artifact_ledger = build_artifact_ledger(&thread_id, &persisted_tasks)?;
        let release_candidate_patchset_id = parse_optional_persisted_object_id(
            "release candidate patchset",
            chosen_patchset_id.as_deref(),
        )?;
        let derived_records = Some(
            persist_validation_decision_derivatives(
                &self.mcp_server,
                &thread_id,
                &run_id,
                &artifact_ledger,
                release_candidate_patchset_id,
                request.system_report,
                request.decision,
            )
            .await?,
        );
        if let Some(snapshot_id) = final_checkpoint_id {
            checkpoints.push(PersistedCheckpoint {
                revision: request.execution_plan_spec.revision,
                reason: "human review required".to_string(),
                snapshot_id: Some(snapshot_id),
                decision_id: decision_id.clone(),
                dagrs_checkpoint_id: request
                    .run_state
                    .dagrs_runtime
                    .checkpoints
                    .last()
                    .map(|checkpoint| checkpoint.checkpoint_id.clone()),
            });
        }
        checkpoints.extend(
            request
                .run_state
                .dagrs_runtime
                .checkpoints
                .iter()
                .map(|checkpoint| PersistedCheckpoint {
                    revision: request.execution_plan_spec.revision,
                    reason: format!(
                        "dagrs runtime checkpoint at pc {} after {} completed nodes",
                        checkpoint.pc, checkpoint.completed_nodes
                    ),
                    snapshot_id: None,
                    decision_id: None,
                    dagrs_checkpoint_id: Some(checkpoint.checkpoint_id.clone()),
                }),
        );

        persist_run_snapshot_family(
            &self.mcp_server,
            &thread_id,
            &run_id,
            latest_execution_plan_id.as_deref(),
            &root_task_id,
            &provenance_id,
        )
        .await?;
        for (task_id, task_run_id) in &persisted_task_run_ids {
            let Some(task_object_id) = persisted_task_ids.get(task_id) else {
                continue;
            };
            persist_run_snapshot_family(
                &self.mcp_server,
                &thread_id,
                task_run_id,
                persisted_plan_ids_by_task_id
                    .get(task_id)
                    .map(String::as_str),
                task_object_id,
                &None,
            )
            .await?;
        }

        let projection_rebuild = rebuild_thread_projection(&self.mcp_server, &thread_id).await;

        let _ = self.tx.send(RuntimeAuditCommand::Shutdown);
        let _ = self.worker.await;
        projection_rebuild?;

        Ok(PersistedExecution {
            thread_id: Some(thread_id),
            run_id,
            initial_snapshot_id,
            provenance_id,
            run_usage_id,
            decision_id,
            plan_ids,
            checkpoints,
            tasks: persisted_tasks,
            derived_records,
        })
    }

    async fn create_task_runs(
        &self,
        plan: &ExecutionPlanSpec,
        task_results: &[TaskResult],
        decision: &DecisionOutcome,
        model_name: &str,
    ) -> Result<(), OrchestratorError> {
        let task_index = plan
            .tasks
            .iter()
            .map(|task| (task.id(), task))
            .collect::<HashMap<_, _>>();

        for result in task_results {
            let already_persisted = {
                let state = self.state.lock().await;
                state.persisted_task_run_ids.contains_key(&result.task_id)
            };
            if already_persisted {
                continue;
            }
            let Some(task) = task_index.get(&result.task_id) else {
                continue;
            };
            let (persisted_task_id, plan_id, base_commit_sha, initial_snapshot_id) = {
                let state = self.state.lock().await;
                (
                    state.persisted_task_ids.get(&result.task_id).cloned(),
                    state
                        .persisted_plan_ids_by_task_id
                        .get(&result.task_id)
                        .cloned(),
                    state.base_commit_sha.clone(),
                    state.initial_snapshot_id.clone(),
                )
            };
            let Some(persisted_task_id) = persisted_task_id else {
                continue;
            };
            let run_id = create_task_run(
                &self.mcp_server,
                task,
                result,
                &persisted_task_id,
                plan_id.as_deref(),
                &base_commit_sha,
                initial_snapshot_id.as_deref(),
                decision,
                model_name,
            )
            .await?;
            let mut state = self.state.lock().await;
            state.persisted_task_run_ids.insert(result.task_id, run_id);
        }

        Ok(())
    }

    async fn flush_runtime_events(&self) -> Result<(), OrchestratorError> {
        let (tx, rx) = oneshot::channel();
        self.tx
            .send(RuntimeAuditCommand::Flush { ack: tx })
            .map_err(|_| {
                OrchestratorError::ConfigError(
                    "runtime audit worker stopped before flush".to_string(),
                )
            })?;
        rx.await.map_err(|_| {
            OrchestratorError::ConfigError("runtime audit flush acknowledgement failed".to_string())
        })?;
        Ok(())
    }

    async fn finalize_terminal_events(
        &self,
        run_state: &RunStateSnapshot,
        decision: &DecisionOutcome,
        model_name: &str,
    ) -> Result<(), OrchestratorError> {
        let task_results = run_state.ordered_task_results();
        for result in task_results {
            let snapshot = {
                let state = self.state.lock().await;
                (
                    state.persisted_task_ids.get(&result.task_id).cloned(),
                    state
                        .persisted_task_run_ids
                        .get(&result.task_id)
                        .cloned()
                        .unwrap_or_else(|| state.run_id.clone()),
                    state
                        .persisted_plan_ids_by_task_id
                        .get(&result.task_id)
                        .cloned()
                        .or_else(|| state.latest_plan_id.clone()),
                    state
                        .persisted_step_ids_by_task_id
                        .get(&result.task_id)
                        .copied(),
                    state.latest_task_event_kind.get(&result.task_id).cloned(),
                    state.latest_plan_step_status.get(&result.task_id).copied(),
                )
            };
            let (
                Some(persisted_task_id),
                run_id,
                latest_plan_id,
                persisted_step_id,
                latest_task_event_kind,
                latest_plan_step_status,
            ) = snapshot
            else {
                continue;
            };
            let task_kind = match result.status {
                super::types::TaskNodeStatus::Completed => TaskEventKind::Done,
                super::types::TaskNodeStatus::Failed => TaskEventKind::Failed,
                super::types::TaskNodeStatus::Skipped => TaskEventKind::Cancelled,
                super::types::TaskNodeStatus::Pending => TaskEventKind::Created,
                super::types::TaskNodeStatus::Running => TaskEventKind::Running,
            };
            if latest_task_event_kind.as_ref() != Some(&task_kind) {
                append_task_event(
                    &self.mcp_server,
                    &self.actor,
                    &persisted_task_id,
                    Some(run_id.as_str()),
                    task_kind.clone(),
                    result.agent_output.clone(),
                )
                .await?;
                let mut state = self.state.lock().await;
                state
                    .latest_task_event_kind
                    .insert(result.task_id, task_kind);
            }
            let plan_status = match result.status {
                super::types::TaskNodeStatus::Completed => "completed",
                super::types::TaskNodeStatus::Failed => "failed",
                super::types::TaskNodeStatus::Skipped => "skipped",
                super::types::TaskNodeStatus::Pending => "pending",
                super::types::TaskNodeStatus::Running => "progressing",
            };
            if latest_plan_step_status != Some(plan_status)
                && let (Some(plan_id), Some(step_id)) =
                    (latest_plan_id.as_deref(), persisted_step_id)
            {
                create_plan_step_event(
                    &self.mcp_server,
                    plan_id,
                    &step_id.to_string(),
                    &run_id,
                    plan_status,
                    persisted_task_id.as_str(),
                    result.agent_output.clone(),
                )
                .await?;
                let mut state = self.state.lock().await;
                state
                    .latest_plan_step_status
                    .insert(result.task_id, plan_status);
            }
        }
        let (root_task_id, run_id, latest_run_event_kind) = {
            let state = self.state.lock().await;
            (
                state.root_task_id.clone(),
                state.run_id.clone(),
                state.latest_run_event_kind.clone(),
            )
        };
        let root_kind = match decision {
            DecisionOutcome::Abandon => TaskEventKind::Failed,
            DecisionOutcome::Commit | DecisionOutcome::HumanReviewRequired => TaskEventKind::Done,
        };
        append_task_event(
            &self.mcp_server,
            &self.actor,
            &root_task_id,
            Some(run_id.as_str()),
            root_kind,
            Some(format!(
                "spec execution finished with decision {:?}",
                decision
            )),
        )
        .await?;
        let final_run_kind = match decision {
            DecisionOutcome::Abandon => RunEventKind::Failed,
            DecisionOutcome::Commit | DecisionOutcome::HumanReviewRequired => {
                RunEventKind::Completed
            }
        };
        if latest_run_event_kind.as_ref() != Some(&final_run_kind) {
            append_run_event(
                &self.mcp_server,
                &self.actor,
                RunEventRequest {
                    run_id: &run_id,
                    kind: final_run_kind.clone(),
                    reason: Some(format!("orchestrator finished with decision {:?}", decision)),
                    error: task_results.iter().find_map(|result| {
                        (result.status == super::types::TaskNodeStatus::Failed).then(|| {
                            result
                                .agent_output
                                .clone()
                                .unwrap_or_else(|| "task execution failed".to_string())
                        })
                    }),
                    metrics: Some(
                        json!({
                            "taskCount": task_results.len(),
                            "completedTasks": task_results.iter().filter(|result| result.status == super::types::TaskNodeStatus::Completed).count(),
                            "failedTasks": task_results.iter().filter(|result| result.status == super::types::TaskNodeStatus::Failed).count(),
                            "toolCalls": task_results.iter().map(|result| result.tool_calls.len()).sum::<usize>(),
                            "policyViolations": task_results.iter().map(|result| result.policy_violations.len()).sum::<usize>(),
                            "model": model_name,
                        }),
                    ),
                    patchset_id: None,
                },
            )
            .await?;
            let mut state = self.state.lock().await;
            state.latest_run_event_kind = Some(final_run_kind);
        }
        Ok(())
    }
}

async fn runtime_audit_worker(
    mcp_server: Arc<LibraMcpServer>,
    actor: ActorRef,
    state: Arc<Mutex<RuntimeAuditState>>,
    mut rx: mpsc::UnboundedReceiver<RuntimeAuditCommand>,
) {
    while let Some(command) = rx.recv().await {
        match command {
            RuntimeAuditCommand::TaskRuntime { task, event } => {
                if let Err(error) =
                    persist_runtime_event(&mcp_server, &actor, &state, task.as_ref(), *event).await
                {
                    tracing::warn!(task_id = %task.id(), "failed to persist runtime audit event: {error}");
                }
            }
            RuntimeAuditCommand::Flush { ack } => {
                let _ = ack.send(());
            }
            RuntimeAuditCommand::Shutdown => break,
        }
    }
}

async fn persist_intent_snapshot(
    mcp_server: &Arc<LibraMcpServer>,
    spec: &IntentSpec,
    intent_id: &str,
) -> Result<(), OrchestratorError> {
    let intent = build_git_intent(spec)
        .map_err(|e| OrchestratorError::ConfigError(format!("failed to build git intent: {e}")))?;
    let snapshot = IntentSnapshot {
        id: intent_id.to_string(),
        content: intent.prompt().to_string(),
        thread_id: intent_id.to_string(),
        parents: intent.parents().iter().map(ToString::to_string).collect(),
        analysis_context_frames: intent
            .analysis_context_frames()
            .iter()
            .map(ToString::to_string)
            .collect(),
        created_at: Utc::now(),
    };
    put_history_json(mcp_server, "intent_snapshot", intent_id, &snapshot).await
}

async fn create_initial_run(
    mcp_server: &Arc<LibraMcpServer>,
    task_id: &str,
    base_commit_sha: &str,
    context_snapshot_id: Option<&str>,
) -> Result<String, OrchestratorError> {
    let params = CreateRunParams {
        task_id: task_id.to_string(),
        base_commit_sha: base_commit_sha.to_string(),
        plan_id: None,
        status: Some("created".to_string()),
        context_snapshot_id: context_snapshot_id.map(ToString::to_string),
        error: None,
        agent_instances: Some(vec![AgentInstanceParams {
            role: "orchestrator".to_string(),
            provider_route: Some("libra-intentspec".to_string()),
        }]),
        metrics_json: None,
        reason: Some("orchestrator execution started".to_string()),
        orchestrator_version: Some("libra-intentspec".to_string()),
        tags: None,
        external_ids: None,
        actor_kind: Some("system".to_string()),
        actor_id: Some("libra-orchestrator".to_string()),
    };
    let actor = resolve_actor(
        mcp_server,
        params.actor_kind.as_deref(),
        params.actor_id.as_deref(),
    )?;
    let result = mcp_server
        .create_run_impl(params, actor)
        .await
        .map_err(|e| OrchestratorError::ConfigError(format!("MCP create_run failed: {e:?}")))?;
    parse_created_id("run", &result)
}

async fn create_compiled_tasks_initial(
    mcp_server: &Arc<LibraMcpServer>,
    intent_id: &str,
    parent_task_id: Option<&str>,
    plan: &ExecutionPlanSpec,
    persisted_step_ids: &HashMap<Uuid, Uuid>,
) -> Result<HashMap<Uuid, String>, OrchestratorError> {
    let mut persisted_ids = HashMap::new();
    let mut remaining = plan.tasks.iter().collect::<Vec<_>>();

    while !remaining.is_empty() {
        let mut progressed = false;
        let mut next_remaining = Vec::new();

        for task in remaining {
            if !task
                .dependencies()
                .iter()
                .all(|dep| persisted_ids.contains_key(dep))
            {
                next_remaining.push(task);
                continue;
            }

            let dependency_task_ids = task
                .dependencies()
                .iter()
                .map(|dep| {
                    persisted_ids.get(dep).cloned().ok_or_else(|| {
                        OrchestratorError::PlanningFailed(format!(
                            "missing persisted dependency task for compiled task {}",
                            task.id()
                        ))
                    })
                })
                .collect::<Result<Vec<_>, _>>()?;

            let persisted_id = create_compiled_task(PersistedTaskRequest {
                mcp_server,
                intent_id,
                parent_task_id,
                task,
                dependency_task_ids,
                persisted_step_id: persisted_step_ids.get(&task.step_id()).copied(),
                status: "draft",
            })
            .await?;
            persisted_ids.insert(task.id(), persisted_id);
            progressed = true;
        }

        if !progressed {
            return Err(OrchestratorError::PlanningFailed(
                "unable to persist compiled tasks due to unresolved task dependencies".to_string(),
            ));
        }

        remaining = next_remaining;
    }

    Ok(persisted_ids)
}

pub async fn persist_plan_review_bundle(
    mcp_server: &Arc<LibraMcpServer>,
    intent_id: &str,
    plan: &ExecutionPlanSpec,
) -> Result<PersistedPlanReviewBundle, OrchestratorError> {
    let persisted_plan_set =
        create_plan_set_revision(mcp_server, intent_id, None, None, plan).await?;
    let task_ids = create_compiled_tasks_initial(
        mcp_server,
        intent_id,
        None,
        plan,
        &persisted_plan_set.step_id_map,
    )
    .await?;

    Ok(PersistedPlanReviewBundle {
        plan_id: persisted_plan_set.execution.plan_id,
        test_plan_id: persisted_plan_set.test.plan_id,
        step_ids: persisted_plan_set.step_id_map,
        task_ids,
        plan_id_by_task_id: persisted_plan_set.plan_id_by_task_id,
    })
}

fn persisted_task_ids_for_plan(
    plan: &ExecutionPlanSpec,
    persisted_task_ids: &HashMap<Uuid, String>,
) -> Result<HashMap<Uuid, String>, OrchestratorError> {
    plan.tasks
        .iter()
        .map(|task| {
            persisted_task_ids
                .get(&task.id())
                .cloned()
                .map(|persisted_id| (task.id(), persisted_id))
                .ok_or_else(|| {
                    OrchestratorError::PersistenceError(format!(
                        "persisted review bundle is missing task snapshot for compiled task {}",
                        task.id()
                    ))
                })
        })
        .collect()
}

fn persisted_step_ids_by_task_for_plan(
    plan: &ExecutionPlanSpec,
    persisted_step_ids: &HashMap<Uuid, Uuid>,
) -> Result<HashMap<Uuid, Uuid>, OrchestratorError> {
    plan.tasks
        .iter()
        .map(|task| {
            persisted_step_ids
                .get(&task.step_id())
                .copied()
                .map(|persisted_step_id| (task.id(), persisted_step_id))
                .ok_or_else(|| {
                    OrchestratorError::PersistenceError(format!(
                        "persisted plan is missing step snapshot for compiled task {}",
                        task.id()
                    ))
                })
        })
        .collect()
}

async fn bind_existing_plan_revision(
    mcp_server: &Arc<LibraMcpServer>,
    plan_id: &str,
    plan: &ExecutionPlanSpec,
) -> Result<PersistedPlanRevision, OrchestratorError> {
    let persisted_plan = load_persisted_plan(mcp_server, plan_id).await?;
    if persisted_plan.steps().len() != plan.tasks.len() {
        return Err(OrchestratorError::PersistenceError(format!(
            "persisted preview plan step count mismatch: expected {}, got {}",
            plan.tasks.len(),
            persisted_plan.steps().len()
        )));
    }
    for (task, step) in plan.tasks.iter().zip(persisted_plan.steps().iter()) {
        if step.description() != task.title() {
            return Err(OrchestratorError::PersistenceError(format!(
                "persisted preview plan does not match compiled execution plan at step '{}'",
                task.title()
            )));
        }
    }
    let step_id_map = plan
        .tasks
        .iter()
        .zip(persisted_plan.steps().iter())
        .map(|(task, step)| (task.step_id(), step.step_id()))
        .collect();
    Ok(PersistedPlanRevision {
        plan_id: plan_id.to_string(),
        step_id_map,
    })
}

async fn bind_existing_plan_set(
    mcp_server: &Arc<LibraMcpServer>,
    execution_plan_id: &str,
    test_plan_id: &str,
    plan: &ExecutionPlanSpec,
) -> Result<PersistedPlanSet, OrchestratorError> {
    let execution = bind_existing_plan_revision_for_role(
        mcp_server,
        execution_plan_id,
        plan,
        PersistedPlanRole::Execution,
    )
    .await?;
    let test = bind_existing_plan_revision_for_role(
        mcp_server,
        test_plan_id,
        plan,
        PersistedPlanRole::Test,
    )
    .await?;
    build_plan_set(plan, execution, test)
}

async fn bind_existing_plan_revision_for_role(
    mcp_server: &Arc<LibraMcpServer>,
    plan_id: &str,
    plan: &ExecutionPlanSpec,
    role: PersistedPlanRole,
) -> Result<PersistedPlanRevision, OrchestratorError> {
    let persisted_plan = load_persisted_plan(mcp_server, plan_id).await?;
    let role_tasks = plan
        .tasks
        .iter()
        .filter(|task| plan_role_for_task(task) == role)
        .collect::<Vec<_>>();
    let expected_step_count = role_tasks.len().max(1);
    if persisted_plan.steps().len() != expected_step_count {
        return Err(OrchestratorError::PersistenceError(format!(
            "persisted preview {} plan step count mismatch: expected {}, got {}",
            role.label(),
            expected_step_count,
            persisted_plan.steps().len()
        )));
    }
    for (task, step) in role_tasks.iter().zip(persisted_plan.steps().iter()) {
        if step.description() != task.title() {
            return Err(OrchestratorError::PersistenceError(format!(
                "persisted preview {} plan does not match compiled execution plan at step '{}'",
                role.label(),
                task.title()
            )));
        }
    }
    let step_id_map = role_tasks
        .into_iter()
        .zip(persisted_plan.steps().iter())
        .map(|(task, step)| (task.step_id(), step.step_id()))
        .collect();
    Ok(PersistedPlanRevision {
        plan_id: plan_id.to_string(),
        step_id_map,
    })
}

async fn create_pending_plan_step_events(
    mcp_server: &Arc<LibraMcpServer>,
    plan_id: &str,
    run_id: &str,
    plan: &ExecutionPlanSpec,
    persisted_step_ids: &HashMap<Uuid, Uuid>,
    persisted_task_ids: &HashMap<Uuid, String>,
) -> Result<(), OrchestratorError> {
    for task in &plan.tasks {
        let Some(step_id) = persisted_step_ids.get(&task.step_id()) else {
            continue;
        };
        let Some(task_id) = persisted_task_ids.get(&task.id()) else {
            continue;
        };
        create_plan_step_event(
            mcp_server,
            plan_id,
            &step_id.to_string(),
            run_id,
            "pending",
            task_id,
            Some("compiled execution task".to_string()),
        )
        .await?;
    }
    Ok(())
}

async fn persist_plan_snapshot_family(
    mcp_server: &Arc<LibraMcpServer>,
    request: PlanSnapshotFamilyRequest<'_>,
) -> Result<(), OrchestratorError> {
    let persisted_tasks = request
        .plan
        .tasks
        .iter()
        .filter(|task| request.persisted_step_ids.contains_key(&task.step_id()))
        .collect::<Vec<_>>();
    let plan_snapshot = PlanSnapshot {
        id: request.plan_id.to_string(),
        thread_id: request.thread_id.to_string(),
        intent_id: Some(request.intent_id.to_string()),
        turn_id: Some(request.thread_id.to_string()),
        step_text: persisted_tasks
            .iter()
            .map(|task| task.title().to_string())
            .collect::<Vec<_>>()
            .join("\n"),
        parents: request
            .parent_plan_id
            .map(|parent| vec![parent.to_string()])
            .unwrap_or_default(),
        context_frames: Vec::new(),
        created_at: Utc::now(),
    };
    put_history_json(mcp_server, "plan_snapshot", request.plan_id, &plan_snapshot).await?;
    for (ordinal, task) in persisted_tasks.into_iter().enumerate() {
        let Some(step_id) = request.persisted_step_ids.get(&task.step_id()) else {
            continue;
        };
        let step_id = step_id.to_string();
        let step_snapshot = PlanStepSnapshot {
            id: step_id.clone(),
            plan_id: request.plan_id.to_string(),
            text: task.title().to_string(),
            ordinal: ordinal as i64,
            created_at: Utc::now(),
        };
        put_history_json(mcp_server, "plan_step_snapshot", &step_id, &step_snapshot).await?;
        let task_snapshot = TaskSnapshot {
            id: request
                .persisted_task_ids
                .get(&task.id())
                .cloned()
                .unwrap_or_else(|| format!("task_{}", task.id())),
            thread_id: request.thread_id.to_string(),
            plan_id: Some(request.plan_id.to_string()),
            intent_id: Some(request.intent_id.to_string()),
            turn_id: Some(request.thread_id.to_string()),
            title: Some(task.title().to_string()),
            parent_task_id: Some(request.root_task_id.to_string()),
            origin_step_id: Some(step_id),
            dependencies: task
                .dependencies()
                .iter()
                .filter_map(|dep| request.persisted_task_ids.get(dep).cloned())
                .collect(),
            created_at: Utc::now(),
        };
        put_history_json(
            mcp_server,
            "task_snapshot",
            &task_snapshot.id,
            &task_snapshot,
        )
        .await?;
    }
    Ok(())
}

async fn persist_runtime_event(
    mcp_server: &Arc<LibraMcpServer>,
    actor: &ActorRef,
    state: &Arc<Mutex<RuntimeAuditState>>,
    task: &super::types::TaskSpec,
    event: super::types::TaskRuntimeEvent,
) -> Result<(), OrchestratorError> {
    let context = {
        let state = state.lock().await;
        RuntimeEventContext {
            thread_id: state.thread_id.clone(),
            intent_id: state.intent_id.clone(),
            run_id: state.run_id.clone(),
            plan_id: state
                .persisted_plan_ids_by_task_id
                .get(&task.id())
                .cloned()
                .or_else(|| state.latest_plan_id.clone()),
            step_id: state
                .persisted_step_ids
                .get(&task.step_id())
                .map(ToString::to_string),
            persisted_task_id: state.persisted_task_ids.get(&task.id()).cloned(),
        }
    };

    match event {
        super::types::TaskRuntimeEvent::Phase(phase) => match phase {
            super::types::TaskRuntimePhase::Starting => {
                let persisted_task_id = context.persisted_task_id.as_deref();
                if let Some(task_id) = persisted_task_id {
                    let should_write = {
                        let state = state.lock().await;
                        state.latest_task_event_kind.get(&task.id())
                            != Some(&TaskEventKind::Running)
                    };
                    if should_write {
                        append_task_event(
                            mcp_server,
                            actor,
                            task_id,
                            Some(context.run_id.as_str()),
                            TaskEventKind::Running,
                            Some("task started".to_string()),
                        )
                        .await?;
                        let mut state = state.lock().await;
                        state
                            .latest_task_event_kind
                            .insert(task.id(), TaskEventKind::Running);
                    }
                    if let (Some(plan_id), Some(step_id)) =
                        (context.plan_id.as_deref(), context.step_id.as_deref())
                    {
                        let should_write = {
                            let state = state.lock().await;
                            state.latest_plan_step_status.get(&task.id()) != Some(&"progressing")
                        };
                        if should_write {
                            create_plan_step_event(
                                mcp_server,
                                plan_id,
                                step_id,
                                &context.run_id,
                                "progressing",
                                task_id,
                                Some("task started".to_string()),
                            )
                            .await?;
                            let mut state = state.lock().await;
                            state
                                .latest_plan_step_status
                                .insert(task.id(), "progressing");
                        }
                    }
                }
                let run_kind = if task.kind == TaskKind::Gate {
                    RunEventKind::Validating
                } else {
                    RunEventKind::Patching
                };
                let should_write = {
                    let state = state.lock().await;
                    state.latest_run_event_kind.as_ref() != Some(&run_kind)
                };
                if should_write {
                    append_run_event(
                        mcp_server,
                        actor,
                        RunEventRequest {
                            run_id: &context.run_id,
                            kind: run_kind.clone(),
                            reason: Some(format!("{} started", task.title())),
                            error: None,
                            metrics: None,
                            patchset_id: None,
                        },
                    )
                    .await?;
                    let mut state = state.lock().await;
                    state.latest_run_event_kind = Some(run_kind);
                }
            }
            super::types::TaskRuntimePhase::AwaitingModel { turn } => {
                persist_context_frame(
                    mcp_server,
                    actor,
                    &context,
                    FrameKind::SystemState,
                    format!("awaiting model turn {}", turn),
                    json!({
                        "event": "awaiting_model",
                        "turn": turn,
                        "taskId": task.id().to_string(),
                        "taskTitle": task.title(),
                    }),
                )
                .await?;
            }
            super::types::TaskRuntimePhase::ExecutingTool { tool_name } => {
                persist_context_frame(
                    mcp_server,
                    actor,
                    &context,
                    FrameKind::ToolCall,
                    format!("executing tool {}", tool_name),
                    json!({
                        "event": "executing_tool",
                        "toolName": tool_name,
                        "taskId": task.id().to_string(),
                        "taskTitle": task.title(),
                    }),
                )
                .await?;
            }
            super::types::TaskRuntimePhase::Reviewing => {
                persist_context_frame(
                    mcp_server,
                    actor,
                    &context,
                    FrameKind::StepSummary,
                    "reviewing task output".to_string(),
                    json!({
                        "event": "reviewing",
                        "taskId": task.id().to_string(),
                        "taskTitle": task.title(),
                    }),
                )
                .await?;
            }
            super::types::TaskRuntimePhase::Completed
            | super::types::TaskRuntimePhase::Failed
            | super::types::TaskRuntimePhase::Pending => {}
        },
        super::types::TaskRuntimeEvent::WorkspaceReady {
            working_dir,
            isolated,
            backend,
            main_working_dir,
        } => {
            persist_context_frame(
                mcp_server,
                actor,
                &context,
                FrameKind::SystemState,
                if isolated {
                    "workspace ready (isolated)".to_string()
                } else {
                    "workspace ready (shared)".to_string()
                },
                json!({
                    "event": "workspace_ready",
                    "isolated": isolated,
                    "backend": backend,
                    "workingDir": working_dir,
                    "mainWorkingDir": main_working_dir,
                    "taskId": task.id().to_string(),
                    "taskTitle": task.title(),
                }),
            )
            .await?;
        }
        super::types::TaskRuntimeEvent::Note { level, text } => {
            let kind = match level {
                super::types::TaskRuntimeNoteLevel::Info => FrameKind::SystemState,
                super::types::TaskRuntimeNoteLevel::Error => FrameKind::ErrorRecovery,
            };
            persist_context_frame(
                mcp_server,
                actor,
                &context,
                kind,
                summarize_runtime_text(&text, 96),
                json!({
                    "event": "note",
                    "level": match level {
                        super::types::TaskRuntimeNoteLevel::Info => "info",
                        super::types::TaskRuntimeNoteLevel::Error => "error",
                    },
                    "text": text,
                    "taskId": task.id().to_string(),
                    "taskTitle": task.title(),
                }),
            )
            .await?;
        }
        super::types::TaskRuntimeEvent::AssistantMessage(text) => {
            let summary = summarize_runtime_text(&text, 240);
            let content_chars = text.chars().count();
            persist_context_frame(
                mcp_server,
                actor,
                &context,
                FrameKind::StepSummary,
                summarize_runtime_text(&text, 96),
                json!({
                    "event": "assistant_message",
                    "summary": summary,
                    "contentChars": content_chars,
                    "fullTextStored": false,
                    "taskId": task.id().to_string(),
                    "taskTitle": task.title(),
                }),
            )
            .await?;
        }
        super::types::TaskRuntimeEvent::ThinkingDelta(text) => {
            let summary = summarize_runtime_text(&text, 240);
            let content_chars = text.chars().count();
            persist_context_frame(
                mcp_server,
                actor,
                &context,
                FrameKind::Other("reasoning".to_string()),
                summarize_runtime_text(&text, 96),
                json!({
                    "event": "thinking_delta",
                    "summary": summary,
                    "contentChars": content_chars,
                    "fullTextStored": false,
                    "taskId": task.id().to_string(),
                    "taskTitle": task.title(),
                }),
            )
            .await?;
        }
        super::types::TaskRuntimeEvent::ToolCallBegin {
            call_id,
            tool_name,
            arguments,
        } => {
            persist_tool_invocation_event(
                mcp_server,
                &context,
                task,
                &call_id,
                &tool_name,
                "in_progress",
                json!({
                    "invocation_id": build_runtime_invocation_key(task, &call_id),
                    "call_id": call_id,
                    "arguments": arguments,
                }),
            )
            .await?;
        }
        super::types::TaskRuntimeEvent::ToolCallEnd {
            call_id,
            tool_name,
            result,
        } => {
            let payload = match &result {
                Ok(output) => json!({
                    "invocation_id": build_runtime_invocation_key(task, &call_id),
                    "call_id": call_id,
                    "result": tool_output_to_json(output),
                    "error": serde_json::Value::Null,
                }),
                Err(error) => json!({
                    "invocation_id": build_runtime_invocation_key(task, &call_id),
                    "call_id": call_id,
                    "result": serde_json::Value::Null,
                    "error": error,
                }),
            };
            let status = match &result {
                Ok(output) if output.is_success() => "completed",
                Ok(_) | Err(_) => "failed",
            };
            persist_tool_invocation_event(
                mcp_server, &context, task, &call_id, &tool_name, status, payload,
            )
            .await?;
            if let Ok(output) = &result {
                persist_sandbox_evidence_events(
                    mcp_server, &context, task, &call_id, &tool_name, output,
                )
                .await?;
            }
        }
        super::types::TaskRuntimeEvent::UsageUpdated { .. } => {}
    }

    Ok(())
}

struct RuntimeEventContext {
    thread_id: String,
    intent_id: String,
    run_id: String,
    plan_id: Option<String>,
    step_id: Option<String>,
    persisted_task_id: Option<String>,
}

async fn persist_context_frame(
    mcp_server: &Arc<LibraMcpServer>,
    actor: &ActorRef,
    context: &RuntimeEventContext,
    kind: FrameKind,
    summary: String,
    data: serde_json::Value,
) -> Result<(), OrchestratorError> {
    let mut frame = ContextFrame::new(actor.clone(), kind, summary.clone()).map_err(|e| {
        OrchestratorError::ConfigError(format!("failed to create context frame: {e}"))
    })?;
    frame.set_intent_id(Some(parse_object_id(&context.intent_id).map_err(|e| {
        OrchestratorError::ConfigError(format!("invalid context intent id: {e}"))
    })?));
    frame.set_run_id(Some(parse_object_id(&context.run_id).map_err(|e| {
        OrchestratorError::ConfigError(format!("invalid context run id: {e}"))
    })?));
    frame.set_plan_id(
        context
            .plan_id
            .as_deref()
            .map(parse_object_id)
            .transpose()
            .map_err(|e| OrchestratorError::ConfigError(format!("invalid context plan id: {e}")))?,
    );
    frame.set_step_id(
        context
            .step_id
            .as_deref()
            .map(parse_object_id)
            .transpose()
            .map_err(|e| OrchestratorError::ConfigError(format!("invalid context step id: {e}")))?,
    );
    frame.set_data(Some(data));
    frame.set_token_estimate(Some(token_estimate_for_summary(&summary)));
    put_history_json(
        mcp_server,
        "context_frame",
        &frame.header().object_id().to_string(),
        &frame,
    )
    .await
}

async fn persist_tool_invocation_event(
    mcp_server: &Arc<LibraMcpServer>,
    context: &RuntimeEventContext,
    task: &super::types::TaskSpec,
    call_id: &str,
    tool_name: &str,
    status: &str,
    payload: serde_json::Value,
) -> Result<(), OrchestratorError> {
    let object_id = stable_history_object_id(
        "orchestrator_tool_invocation_event",
        &json!({
            "run_id": context.run_id,
            "task_id": task.id().to_string(),
            "call_id": call_id,
            "status": status,
            "payload": payload,
        }),
    )?;
    let event = ToolInvocationEvent {
        id: object_id.clone(),
        run_id: context.run_id.clone(),
        thread_id: context.thread_id.clone(),
        tool: tool_name.to_string(),
        server: None,
        status: status.to_string(),
        at: Utc::now(),
        payload,
    };
    put_history_json(mcp_server, "tool_invocation_event", &object_id, &event).await
}

async fn persist_sandbox_evidence_events(
    mcp_server: &Arc<LibraMcpServer>,
    context: &RuntimeEventContext,
    task: &super::types::TaskSpec,
    call_id: &str,
    tool_name: &str,
    output: &ToolOutput,
) -> Result<(), OrchestratorError> {
    let Some(events) = sandbox_evidence_events_from_output(output) else {
        return Ok(());
    };

    for (index, event_data) in events.iter().enumerate() {
        let data = json!({
            "task_id": task.id().to_string(),
            "tool": tool_name,
            "call_id": call_id,
            "event": event_data,
        });
        let object_id = stable_history_object_id(
            "orchestrator_sandbox_evidence_event",
            &json!({
                "run_id": context.run_id,
                "task_id": task.id().to_string(),
                "call_id": call_id,
                "index": index,
                "event": event_data,
            }),
        )?;
        let evidence = EvidenceEvent {
            id: object_id.clone(),
            run_id: context.run_id.clone(),
            patchset_id: None,
            at: Utc::now(),
            kind: "sandbox".to_string(),
            data,
        };
        put_history_json(mcp_server, "evidence", &object_id, &evidence).await?;
    }

    Ok(())
}

async fn append_task_event(
    mcp_server: &Arc<LibraMcpServer>,
    actor: &ActorRef,
    task_id: &str,
    run_id: Option<&str>,
    kind: TaskEventKind,
    reason: Option<String>,
) -> Result<(), OrchestratorError> {
    let mut event = TaskEvent::new(
        actor.clone(),
        parse_object_id(task_id)
            .map_err(|e| OrchestratorError::ConfigError(format!("invalid task id: {e}")))?,
        kind,
    )
    .map_err(|e| OrchestratorError::ConfigError(format!("failed to create task event: {e}")))?;
    event.set_reason(reason);
    event.set_run_id(
        run_id
            .map(parse_object_id)
            .transpose()
            .map_err(|e| OrchestratorError::ConfigError(format!("invalid run id: {e}")))?,
    );
    put_history_json(
        mcp_server,
        "task_event",
        &event.header().object_id().to_string(),
        &event,
    )
    .await
}

async fn append_run_event(
    mcp_server: &Arc<LibraMcpServer>,
    actor: &ActorRef,
    request: RunEventRequest<'_>,
) -> Result<(), OrchestratorError> {
    let mut event = RunEvent::new(
        actor.clone(),
        parse_object_id(request.run_id)
            .map_err(|e| OrchestratorError::ConfigError(format!("invalid run id: {e}")))?,
        request.kind,
    )
    .map_err(|e| OrchestratorError::ConfigError(format!("failed to create run event: {e}")))?;
    event.set_reason(request.reason);
    event.set_error(request.error);
    event.set_metrics(request.metrics);
    event.set_patchset_id(
        request
            .patchset_id
            .map(parse_object_id)
            .transpose()
            .map_err(|e| OrchestratorError::ConfigError(format!("invalid patchset id: {e}")))?,
    );
    put_history_json(
        mcp_server,
        "run_event",
        &event.header().object_id().to_string(),
        &event,
    )
    .await
}

async fn create_plan_step_event(
    mcp_server: &Arc<LibraMcpServer>,
    plan_id: &str,
    step_id: &str,
    run_id: &str,
    status: &str,
    spawned_task_id: &str,
    reason: Option<String>,
) -> Result<(), OrchestratorError> {
    let params = CreatePlanStepEventParams {
        plan_id: plan_id.to_string(),
        step_id: step_id.to_string(),
        run_id: run_id.to_string(),
        status: status.to_string(),
        reason,
        consumed_frames: None,
        produced_frames: None,
        spawned_task_id: Some(spawned_task_id.to_string()),
        outputs: None,
        actor_kind: Some("system".to_string()),
        actor_id: Some("libra-orchestrator".to_string()),
    };
    let actor = resolve_actor(
        mcp_server,
        params.actor_kind.as_deref(),
        params.actor_id.as_deref(),
    )?;
    mcp_server
        .create_plan_step_event_impl(params, actor)
        .await
        .map_err(|e| {
            OrchestratorError::ConfigError(format!("MCP create_plan_step_event failed: {e:?}"))
        })?;
    Ok(())
}

async fn persist_run_snapshot_family(
    mcp_server: &Arc<LibraMcpServer>,
    thread_id: &str,
    run_id: &str,
    latest_plan_id: Option<&str>,
    root_task_id: &str,
    provenance_id: &Option<String>,
) -> Result<(), OrchestratorError> {
    let run: GitRun = read_tracked_json(mcp_server, "run", run_id).await?;
    let run_snapshot = RunSnapshot {
        id: run_id.to_string(),
        thread_id: thread_id.to_string(),
        plan_id: latest_plan_id.map(ToString::to_string),
        task_id: Some(root_task_id.to_string()),
        started_at: run.header().created_at(),
    };
    put_history_json(mcp_server, "run_snapshot", run_id, &run_snapshot).await?;
    if let Some(provenance_id) = provenance_id {
        let provenance: GitProvenance =
            read_tracked_json(mcp_server, "provenance", provenance_id).await?;
        let snapshot = ProvenanceSnapshot {
            id: provenance_id.clone(),
            run_id: run_id.to_string(),
            model: Some(provenance.model().to_string()),
            provider: Some(provenance.provider().to_string()),
            parameters: provenance
                .parameters()
                .cloned()
                .unwrap_or_else(|| json!({})),
            created_at: provenance.header().created_at(),
        };
        put_history_json(mcp_server, "provenance_snapshot", provenance_id, &snapshot).await?;
    }
    Ok(())
}

async fn create_run_usage_from_task_results(
    mcp_server: &Arc<LibraMcpServer>,
    actor: &ActorRef,
    run_id: &str,
    task_results: &[TaskResult],
) -> Result<Option<String>, OrchestratorError> {
    let mut usage = CompletionUsageSummary::default();
    for result in task_results {
        if let Some(task_usage) = result.model_usage.as_ref() {
            usage.merge(task_usage);
        }
    }
    if usage.is_zero() {
        return Ok(None);
    }
    let result = mcp_server
        .create_run_usage_impl(
            CreateRunUsageParams {
                run_id: run_id.to_string(),
                input_tokens: usage.input_tokens,
                output_tokens: usage.output_tokens,
                cost_usd: usage.cost_usd,
                actor_kind: Some(actor.kind().to_string()),
                actor_id: Some(actor.id().to_string()),
            },
            actor.clone(),
        )
        .await
        .map_err(|error| {
            OrchestratorError::ConfigError(format!("MCP create_run_usage failed: {error:?}"))
        })?;
    parse_created_id("run usage", &result).map(Some)
}

async fn persist_patchset_snapshot(
    mcp_server: &Arc<LibraMcpServer>,
    thread_id: &str,
    run_id: &str,
    patchset_id: &str,
    tool_calls: &[ToolCallRecord],
) -> Result<(), OrchestratorError> {
    let patchset: GitPatchSet = read_tracked_json(mcp_server, "patchset", patchset_id).await?;
    let snapshot = PatchSetSnapshot {
        id: patchset_id.to_string(),
        run_id: run_id.to_string(),
        thread_id: thread_id.to_string(),
        created_at: patchset.header().created_at(),
        status: PatchStatus::Completed,
        changes: build_patchset_snapshot_changes(tool_calls),
    };
    put_history_json(mcp_server, "patchset_snapshot", patchset_id, &snapshot).await
}

fn build_patchset_snapshot_changes(tool_calls: &[ToolCallRecord]) -> Vec<FileChange> {
    let mut changes = Vec::new();
    for call in tool_calls {
        for diff in &call.diffs {
            let Some(path) = normalize_patch_path(&diff.path) else {
                continue;
            };
            changes.push(FileChange {
                path,
                diff: normalize_diff_text(&diff.diff).unwrap_or_else(|| diff.diff.clone()),
                change_type: normalize_change_type(&diff.change_type).to_string(),
            });
        }
    }
    changes
}

fn tool_output_to_json(output: &ToolOutput) -> serde_json::Value {
    match output {
        ToolOutput::Function {
            content,
            success,
            metadata,
        } => json!({
            "kind": "function",
            "content": content,
            "success": success,
            "metadata": metadata,
        }),
        ToolOutput::Mcp { result } => json!({
            "kind": "mcp",
            "result": result,
        }),
    }
}

fn sandbox_evidence_events_from_output(output: &ToolOutput) -> Option<&Vec<serde_json::Value>> {
    output
        .metadata()
        .and_then(|metadata| metadata.get("sandbox_evidence"))
        .and_then(|value| value.as_array())
        .filter(|events| !events.is_empty())
}

fn build_runtime_invocation_key(task: &super::types::TaskSpec, call_id: &str) -> String {
    format!("{}:{call_id}", task.id())
}

fn summarize_runtime_text(text: &str, max_chars: usize) -> String {
    let trimmed = text.trim();
    if trimmed.is_empty() {
        return "runtime event".to_string();
    }
    let mut summary = trimmed.lines().next().unwrap_or(trimmed).trim().to_string();
    if summary.chars().count() > max_chars {
        summary = summary.chars().take(max_chars).collect::<String>() + "...";
    }
    summary
}

fn token_estimate_for_summary(summary: &str) -> u64 {
    ((summary.chars().count().max(1) as u64) / 4).max(1)
}

fn stable_history_object_id(
    prefix: &str,
    seed: &serde_json::Value,
) -> Result<String, OrchestratorError> {
    let seed_bytes = serde_json::to_vec(seed).map_err(|e| {
        OrchestratorError::ConfigError(format!("failed to serialize derived object seed: {e}"))
    })?;
    let hash = ObjectHash::from_type_and_data(ObjectType::Blob, &seed_bytes);
    Ok(format!("{prefix}__{hash}"))
}

async fn put_history_json<T: serde::Serialize + Send + Sync>(
    mcp_server: &Arc<LibraMcpServer>,
    object_type: &str,
    object_id: &str,
    value: &T,
) -> Result<(), OrchestratorError> {
    let storage = mcp_server
        .storage
        .as_ref()
        .ok_or_else(|| OrchestratorError::ConfigError("MCP storage not available".to_string()))?;
    let object_hash = storage.put_json(value).await.map_err(|e| {
        OrchestratorError::ConfigError(format!(
            "failed to persist {object_type} '{object_id}' JSON blob: {e}"
        ))
    })?;
    if let Some(history) = &mcp_server.intent_history_manager {
        let existing_hash = history
            .get_object_hash(object_type, object_id)
            .await
            .map_err(|e| {
                OrchestratorError::ConfigError(format!(
                    "failed to inspect {object_type} history for '{object_id}': {e}"
                ))
            })?;
        if existing_hash != Some(object_hash) {
            history
                .append(object_type, object_id, object_hash)
                .await
                .map_err(|e| {
                    OrchestratorError::ConfigError(format!(
                        "failed to append {object_type} '{object_id}' to history: {e}"
                    ))
                })?;
        }
    }
    Ok(())
}

async fn read_tracked_json<T: serde::de::DeserializeOwned + Send + Sync>(
    mcp_server: &Arc<LibraMcpServer>,
    object_type: &str,
    object_id: &str,
) -> Result<T, OrchestratorError> {
    let history = mcp_server
        .intent_history_manager
        .as_ref()
        .ok_or_else(|| OrchestratorError::ConfigError("MCP history not available".to_string()))?;
    let storage = mcp_server
        .storage
        .as_ref()
        .ok_or_else(|| OrchestratorError::ConfigError("MCP storage not available".to_string()))?;
    let hash = history
        .get_object_hash(object_type, object_id)
        .await
        .map_err(|e| {
            OrchestratorError::ConfigError(format!(
                "failed to resolve {object_type} '{object_id}' hash: {e}"
            ))
        })?
        .ok_or_else(|| {
            OrchestratorError::ConfigError(format!(
                "persisted {object_type} not found: {object_id}"
            ))
        })?;
    storage.get_json::<T>(&hash).await.map_err(|e| {
        OrchestratorError::ConfigError(format!(
            "failed to read {object_type} '{object_id}' JSON: {e}"
        ))
    })
}

pub async fn persist_execution(
    request: ExecutionPersistenceRequest<'_>,
) -> Result<PersistedExecution, OrchestratorError> {
    let task_results = request.run_state.ordered_task_results();
    let base_commit_sha = resolve_base_commit(request.base_commit, request.working_dir);
    let intent_id = persist_intentspec(request.spec, request.mcp_server)
        .await
        .map_err(|e| OrchestratorError::ConfigError(format!("MCP create_intent failed: {e}")))?;
    let initial_snapshot_id = if snapshot_on_run_start(request.spec) {
        Some(
            create_context_snapshot(
                request.mcp_server,
                build_snapshot_summary(
                    request.spec,
                    request.plan_revision_specs.first(),
                    "Run start context snapshot",
                ),
                collect_snapshot_items(
                    request.spec,
                    request.plan_revision_specs.first(),
                    request.working_dir,
                    task_results,
                ),
            )
            .await?,
        )
    } else {
        None
    };
    let mut plan_ids = Vec::with_capacity(request.plan_revision_specs.len().saturating_mul(2));
    let mut parent_execution_plan_id = None;
    let mut parent_test_plan_id = None;
    let mut persisted_step_ids = HashMap::new();
    let mut persisted_plan_ids_by_task_id = HashMap::new();
    let mut latest_execution_step_ids = HashMap::new();
    let mut latest_test_step_ids = HashMap::new();
    let mut latest_execution_plan_id = None;
    let mut latest_test_plan_id = None;
    for plan_spec in request.plan_revision_specs {
        let persisted_plan_set = create_plan_set_revision(
            request.mcp_server,
            &intent_id,
            parent_execution_plan_id.as_deref(),
            parent_test_plan_id.as_deref(),
            plan_spec,
        )
        .await?;
        persisted_step_ids = persisted_plan_set.step_id_map;
        persisted_plan_ids_by_task_id = persisted_plan_set.plan_id_by_task_id;
        latest_execution_step_ids = persisted_plan_set.execution.step_id_map.clone();
        latest_test_step_ids = persisted_plan_set.test.step_id_map.clone();
        parent_execution_plan_id = Some(persisted_plan_set.execution.plan_id.clone());
        parent_test_plan_id = Some(persisted_plan_set.test.plan_id.clone());
        latest_execution_plan_id = parent_execution_plan_id.clone();
        latest_test_plan_id = parent_test_plan_id.clone();
        plan_ids.push(persisted_plan_set.execution.plan_id);
        plan_ids.push(persisted_plan_set.test.plan_id);
    }
    let execution_summary = request.execution_plan_spec.summary_line();
    let root_task_id = create_execution_task(
        request.mcp_server,
        &intent_id,
        execution_summary.as_str(),
        request.spec.intent.problem_statement.as_str(),
        "running",
        "orchestrator execution root task",
    )
    .await?;
    let persisted_task_ids = create_compiled_tasks(
        request.mcp_server,
        &intent_id,
        &root_task_id,
        request.execution_plan_spec,
        request.run_state,
        &persisted_step_ids,
    )
    .await?;
    let run_id = create_run(RunRequest {
        mcp_server: request.mcp_server,
        task_id: &root_task_id,
        base_commit_sha: &base_commit_sha,
        plan_id: latest_execution_plan_id.as_deref(),
        context_snapshot_id: initial_snapshot_id.as_deref(),
        task_results,
        decision: request.decision,
        model_name: request.model_name,
    })
    .await?;
    let persisted_task_run_ids = create_task_runs_for_results(
        request.mcp_server,
        request.execution_plan_spec,
        task_results,
        &persisted_task_ids,
        &persisted_plan_ids_by_task_id,
        &base_commit_sha,
        initial_snapshot_id.as_deref(),
        request.decision,
        request.model_name,
    )
    .await?;
    if let Some(plan_id) = latest_execution_plan_id.as_deref() {
        create_plan_step_events(PlanStepEventsRequest {
            mcp_server: request.mcp_server,
            plan_id,
            fallback_run_id: &run_id,
            plan: request.execution_plan_spec,
            run_state: request.run_state,
            persisted_step_ids: &latest_execution_step_ids,
            persisted_task_ids: &persisted_task_ids,
            persisted_task_run_ids: &persisted_task_run_ids,
        })
        .await?;
    }
    if let Some(plan_id) = latest_test_plan_id.as_deref() {
        create_plan_step_events(PlanStepEventsRequest {
            mcp_server: request.mcp_server,
            plan_id,
            fallback_run_id: &run_id,
            plan: request.execution_plan_spec,
            run_state: request.run_state,
            persisted_step_ids: &latest_test_step_ids,
            persisted_task_ids: &persisted_task_ids,
            persisted_task_run_ids: &persisted_task_run_ids,
        })
        .await?;
    }

    let provenance_id = Some(
        create_provenance(
            request.mcp_server,
            &run_id,
            request.execution_plan_spec,
            task_results,
            request.system_report,
            request.decision,
            request.model_name,
        )
        .await?,
    );
    let actor = resolve_actor(
        request.mcp_server,
        Some("system"),
        Some("libra-orchestrator"),
    )?;
    let run_usage_id =
        create_run_usage_from_task_results(request.mcp_server, &actor, &run_id, task_results)
            .await?;
    let mut checkpoints = create_replan_checkpoints(
        request.mcp_server,
        request.spec,
        &run_id,
        request.plan_revision_specs,
        request.working_dir,
        task_results,
    )
    .await?;

    let task_index: HashMap<Uuid, _> = request
        .execution_plan_spec
        .tasks
        .iter()
        .map(|task| (task.id(), task))
        .collect();

    let mut persisted_tasks = Vec::with_capacity(task_results.len());
    let mut generation: u32 = 1;

    for result in task_results {
        let task = task_index.get(&result.task_id).ok_or_else(|| {
            OrchestratorError::PlanningFailed(format!(
                "missing compiled task for result {} during persistence",
                result.task_id
            ))
        })?;

        let mut persisted = PersistedTaskArtifacts {
            task_id: result.task_id,
            persisted_task_id: persisted_task_ids.get(&result.task_id).cloned(),
            ..PersistedTaskArtifacts::default()
        };
        let task_run_id = persisted_task_run_ids
            .get(&result.task_id)
            .map(String::as_str)
            .unwrap_or(run_id.as_str());

        for call in &result.tool_calls {
            let tool_invocation_id =
                create_tool_invocation(request.mcp_server, task_run_id, task.title(), call).await?;
            persisted.tool_invocation_ids.push(tool_invocation_id);
        }

        if task.kind == TaskKind::Implementation
            && let Some(patchset_id) = create_patchset(PatchSetRequest {
                mcp_server: request.mcp_server,
                run_id: &run_id,
                base_commit_sha: &base_commit_sha,
                generation,
                task_title: task.title(),
                task_objective: task.objective.as_str(),
                tool_calls: &result.tool_calls,
            })
            .await?
        {
            generation += 1;
            persisted.patchset_id = Some(patchset_id);
        }

        if let Some(report) = &result.gate_report {
            for gate in &report.results {
                let summary = format!(
                    "{} [{}] {}",
                    gate.check_id,
                    gate.kind,
                    if gate.passed { "passed" } else { "failed" }
                );
                let patchset_id = persisted.patchset_id.clone();
                append_evidence_id(
                    &mut persisted,
                    EvidenceRequest {
                        mcp_server: request.mcp_server,
                        run_id: if patchset_id.is_some() {
                            run_id.as_str()
                        } else {
                            task_run_id
                        },
                        patchset_id: patchset_id.as_deref(),
                        kind: normalize_evidence_kind(&gate.kind),
                        tool: task_gate_tool_name(task.gate_stage.as_ref()),
                        command: Some(gate.check_id.clone()),
                        exit_code: Some(gate.exit_code),
                        summary: Some(summary),
                    },
                )
                .await?;
            }
        }

        if !result.policy_violations.is_empty() {
            let summary = result
                .policy_violations
                .iter()
                .map(|violation| format!("{}: {}", violation.code, violation.message))
                .collect::<Vec<_>>()
                .join("; ");
            let patchset_id = persisted.patchset_id.clone();
            append_evidence_id(
                &mut persisted,
                EvidenceRequest {
                    mcp_server: request.mcp_server,
                    run_id: if patchset_id.is_some() {
                        run_id.as_str()
                    } else {
                        task_run_id
                    },
                    patchset_id: patchset_id.as_deref(),
                    kind: "policy",
                    tool: "policy-engine",
                    command: None,
                    exit_code: None,
                    summary: Some(summary),
                },
            )
            .await?;
        }

        if let Some(review) = &result.review {
            let summary = if review.issues.is_empty() {
                review.summary.clone()
            } else {
                format!("{} [{}]", review.summary, review.issues.join("; "))
            };
            let patchset_id = persisted.patchset_id.clone();
            append_evidence_id(
                &mut persisted,
                EvidenceRequest {
                    mcp_server: request.mcp_server,
                    run_id: if patchset_id.is_some() {
                        run_id.as_str()
                    } else {
                        task_run_id
                    },
                    patchset_id: patchset_id.as_deref(),
                    kind: "review",
                    tool: "reviewer",
                    command: None,
                    exit_code: None,
                    summary: Some(summary),
                },
            )
            .await?;
        }

        persisted_tasks.push(persisted);
    }

    let chosen_patchset_id = if *request.decision == DecisionOutcome::Commit {
        persisted_tasks
            .iter()
            .rev()
            .find_map(|task| task.patchset_id.clone())
    } else {
        None
    };
    let final_checkpoint_id = if *request.decision == DecisionOutcome::HumanReviewRequired {
        Some(
            create_context_snapshot(
                request.mcp_server,
                build_snapshot_summary(
                    request.spec,
                    Some(request.execution_plan_spec),
                    "Human review checkpoint",
                ),
                collect_snapshot_items(
                    request.spec,
                    Some(request.execution_plan_spec),
                    request.working_dir,
                    task_results,
                ),
            )
            .await?,
        )
    } else {
        None
    };

    let decision_id = Some(
        create_decision(FinalDecisionRequest {
            mcp_server: request.mcp_server,
            run_id: &run_id,
            chosen_patchset_id: chosen_patchset_id.as_deref(),
            checkpoint_id: final_checkpoint_id.as_deref(),
            execution_plan: request.execution_plan_spec,
            task_results,
            system_report: request.system_report,
            decision: request.decision,
        })
        .await?,
    );
    record_terminal_intent_event(request.mcp_server, &intent_id, request.decision).await?;
    let artifact_ledger = build_artifact_ledger(&intent_id, &persisted_tasks)?;
    let release_candidate_patchset_id = parse_optional_persisted_object_id(
        "release candidate patchset",
        chosen_patchset_id.as_deref(),
    )?;
    let derived_records = Some(
        persist_validation_decision_derivatives(
            request.mcp_server,
            &intent_id,
            &run_id,
            &artifact_ledger,
            release_candidate_patchset_id,
            request.system_report,
            request.decision,
        )
        .await?,
    );
    if let Some(snapshot_id) = final_checkpoint_id {
        checkpoints.push(PersistedCheckpoint {
            revision: request.execution_plan_spec.revision,
            reason: "human review required".to_string(),
            snapshot_id: Some(snapshot_id),
            decision_id: decision_id.clone(),
            dagrs_checkpoint_id: request
                .run_state
                .dagrs_runtime
                .checkpoints
                .last()
                .map(|checkpoint| checkpoint.checkpoint_id.clone()),
        });
    }

    checkpoints.extend(
        request
            .run_state
            .dagrs_runtime
            .checkpoints
            .iter()
            .map(|checkpoint| PersistedCheckpoint {
                revision: request.execution_plan_spec.revision,
                reason: format!(
                    "dagrs runtime checkpoint at pc {} after {} completed nodes",
                    checkpoint.pc, checkpoint.completed_nodes
                ),
                snapshot_id: None,
                decision_id: None,
                dagrs_checkpoint_id: Some(checkpoint.checkpoint_id.clone()),
            }),
    );

    rebuild_thread_projection(request.mcp_server, &intent_id).await?;

    Ok(PersistedExecution {
        thread_id: Some(intent_id),
        run_id,
        initial_snapshot_id,
        provenance_id,
        run_usage_id,
        decision_id,
        plan_ids,
        checkpoints,
        tasks: persisted_tasks,
        derived_records,
    })
}

async fn create_run(request: RunRequest<'_>) -> Result<String, OrchestratorError> {
    let status = match request.decision {
        DecisionOutcome::Abandon => "failed",
        DecisionOutcome::Commit | DecisionOutcome::HumanReviewRequired => "completed",
    };
    let metrics_json = json!({
        "taskCount": request.task_results.len(),
        "completedTasks": request.task_results.iter().filter(|result| result.status == super::types::TaskNodeStatus::Completed).count(),
        "failedTasks": request.task_results.iter().filter(|result| result.status == super::types::TaskNodeStatus::Failed).count(),
        "toolCalls": request.task_results.iter().map(|result| result.tool_calls.len()).sum::<usize>(),
        "policyViolations": request.task_results.iter().map(|result| result.policy_violations.len()).sum::<usize>(),
        "model": request.model_name,
    })
    .to_string();

    let params = CreateRunParams {
        task_id: request.task_id.to_string(),
        base_commit_sha: request.base_commit_sha.to_string(),
        plan_id: request.plan_id.map(ToString::to_string),
        status: Some(status.to_string()),
        context_snapshot_id: request.context_snapshot_id.map(ToString::to_string),
        error: request.task_results.iter().find_map(|result| {
            (result.status == super::types::TaskNodeStatus::Failed).then(|| {
                result
                    .agent_output
                    .clone()
                    .unwrap_or_else(|| "task execution failed".to_string())
            })
        }),
        agent_instances: Some(vec![AgentInstanceParams {
            role: "orchestrator".to_string(),
            provider_route: Some(request.model_name.to_string()),
        }]),
        metrics_json: Some(metrics_json),
        reason: Some(format!(
            "orchestrator finished with decision {:?}",
            request.decision
        )),
        orchestrator_version: Some("libra-intentspec".to_string()),
        tags: None,
        external_ids: None,
        actor_kind: Some("system".to_string()),
        actor_id: Some("libra-orchestrator".to_string()),
    };

    let actor = resolve_actor(
        request.mcp_server,
        params.actor_kind.as_deref(),
        params.actor_id.as_deref(),
    )?;
    let result = request
        .mcp_server
        .create_run_impl(params, actor)
        .await
        .map_err(|e| OrchestratorError::ConfigError(format!("MCP create_run failed: {e:?}")))?;
    parse_created_id("run", &result)
}

#[allow(clippy::too_many_arguments)]
async fn create_task_run(
    mcp_server: &Arc<LibraMcpServer>,
    task: &super::types::TaskSpec,
    result: &TaskResult,
    persisted_task_id: &str,
    plan_id: Option<&str>,
    base_commit_sha: &str,
    context_snapshot_id: Option<&str>,
    decision: &DecisionOutcome,
    model_name: &str,
) -> Result<String, OrchestratorError> {
    let status = match result.status {
        super::types::TaskNodeStatus::Completed => "completed",
        super::types::TaskNodeStatus::Failed | super::types::TaskNodeStatus::Skipped => "failed",
        super::types::TaskNodeStatus::Running => {
            if task.kind == TaskKind::Gate {
                "validating"
            } else {
                "patching"
            }
        }
        super::types::TaskNodeStatus::Pending => "created",
    };
    let metrics_json = json!({
        "taskId": result.task_id,
        "taskTitle": task.title(),
        "taskKind": format!("{:?}", task.kind).to_lowercase(),
        "retryCount": result.retry_count,
        "toolCalls": result.tool_calls.len(),
        "policyViolations": result.policy_violations.len(),
        "model": model_name,
        "decision": format!("{:?}", decision).to_lowercase(),
    })
    .to_string();
    let params = CreateRunParams {
        task_id: persisted_task_id.to_string(),
        base_commit_sha: base_commit_sha.to_string(),
        plan_id: plan_id.map(ToString::to_string),
        status: Some(status.to_string()),
        context_snapshot_id: context_snapshot_id.map(ToString::to_string),
        error: matches!(
            result.status,
            super::types::TaskNodeStatus::Failed | super::types::TaskNodeStatus::Skipped
        )
        .then(|| {
            result
                .agent_output
                .clone()
                .unwrap_or_else(|| "task execution failed".to_string())
        }),
        agent_instances: Some(vec![AgentInstanceParams {
            role: if task.kind == TaskKind::Gate {
                "verifier".to_string()
            } else {
                "executor".to_string()
            },
            provider_route: Some(model_name.to_string()),
        }]),
        metrics_json: Some(metrics_json),
        reason: Some(format!("{} task run for {}", status, task.title())),
        orchestrator_version: Some("libra-intentspec".to_string()),
        tags: None,
        external_ids: None,
        actor_kind: Some("agent".to_string()),
        actor_id: Some(if task.kind == TaskKind::Gate {
            "libra-verifier".to_string()
        } else {
            "libra-coder".to_string()
        }),
    };
    let actor = resolve_actor(
        mcp_server,
        params.actor_kind.as_deref(),
        params.actor_id.as_deref(),
    )?;
    let result = mcp_server
        .create_run_impl(params, actor)
        .await
        .map_err(|e| OrchestratorError::ConfigError(format!("MCP create_run failed: {e:?}")))?;
    parse_created_id("run", &result)
}

#[allow(clippy::too_many_arguments)]
async fn create_task_attempt_run(
    mcp_server: &Arc<LibraMcpServer>,
    task: &super::types::TaskSpec,
    persisted_task_id: &str,
    plan_id: Option<&str>,
    base_commit_sha: &str,
    context_snapshot_id: Option<&str>,
    model_name: &str,
    start_kind: RunEventKind,
    summary: Option<String>,
) -> Result<String, OrchestratorError> {
    let status = run_event_status_label(&start_kind);
    let metrics_json = json!({
        "taskId": task.id(),
        "taskTitle": task.title(),
        "taskKind": format!("{:?}", task.kind).to_lowercase(),
        "model": model_name,
        "phase": "runtime_phase2_attempt_start",
    })
    .to_string();
    let params = CreateRunParams {
        task_id: persisted_task_id.to_string(),
        base_commit_sha: base_commit_sha.to_string(),
        plan_id: plan_id.map(ToString::to_string),
        status: Some(status.to_string()),
        context_snapshot_id: context_snapshot_id.map(ToString::to_string),
        error: None,
        agent_instances: Some(vec![AgentInstanceParams {
            role: if task.kind == TaskKind::Gate {
                "verifier".to_string()
            } else {
                "executor".to_string()
            },
            provider_route: Some(model_name.to_string()),
        }]),
        metrics_json: Some(metrics_json),
        reason: summary.or_else(|| Some(format!("{} started", task.title()))),
        orchestrator_version: Some("libra-runtime-phase2".to_string()),
        tags: None,
        external_ids: None,
        actor_kind: Some("agent".to_string()),
        actor_id: Some(if task.kind == TaskKind::Gate {
            "libra-verifier".to_string()
        } else {
            "libra-coder".to_string()
        }),
    };
    let actor = resolve_actor(
        mcp_server,
        params.actor_kind.as_deref(),
        params.actor_id.as_deref(),
    )?;
    let result = mcp_server
        .create_run_impl(params, actor)
        .await
        .map_err(|e| OrchestratorError::ConfigError(format!("MCP create_run failed: {e:?}")))?;
    parse_created_id("run", &result)
}

fn start_run_event_kind_for_task(task: &super::types::TaskSpec) -> RunEventKind {
    if task.kind == TaskKind::Gate {
        RunEventKind::Validating
    } else {
        RunEventKind::Patching
    }
}

fn task_event_kind_for_attempt_status(status: &TaskExecutionStatus) -> TaskEventKind {
    match status {
        TaskExecutionStatus::Completed => TaskEventKind::Done,
        TaskExecutionStatus::Cancelled => TaskEventKind::Cancelled,
        TaskExecutionStatus::Failed
        | TaskExecutionStatus::TimedOut
        | TaskExecutionStatus::Interrupted => TaskEventKind::Failed,
    }
}

fn plan_step_status_for_attempt_status(status: &TaskExecutionStatus) -> &'static str {
    match status {
        TaskExecutionStatus::Completed => "completed",
        TaskExecutionStatus::Cancelled => "skipped",
        TaskExecutionStatus::Failed
        | TaskExecutionStatus::TimedOut
        | TaskExecutionStatus::Interrupted => "failed",
    }
}

fn run_event_kind_for_attempt_status(status: &TaskExecutionStatus) -> RunEventKind {
    match status {
        TaskExecutionStatus::Completed => RunEventKind::Completed,
        TaskExecutionStatus::Failed
        | TaskExecutionStatus::Cancelled
        | TaskExecutionStatus::TimedOut
        | TaskExecutionStatus::Interrupted => RunEventKind::Failed,
    }
}

fn run_event_status_label(kind: &RunEventKind) -> &'static str {
    match kind {
        RunEventKind::Created => "created",
        RunEventKind::Patching => "patching",
        RunEventKind::Validating => "validating",
        RunEventKind::Completed => "completed",
        RunEventKind::Failed => "failed",
        RunEventKind::Checkpointed => "checkpointed",
    }
}

#[allow(clippy::too_many_arguments)]
async fn create_task_runs_for_results(
    mcp_server: &Arc<LibraMcpServer>,
    plan: &ExecutionPlanSpec,
    task_results: &[TaskResult],
    persisted_task_ids: &HashMap<Uuid, String>,
    persisted_plan_ids_by_task_id: &HashMap<Uuid, String>,
    base_commit_sha: &str,
    context_snapshot_id: Option<&str>,
    decision: &DecisionOutcome,
    model_name: &str,
) -> Result<HashMap<Uuid, String>, OrchestratorError> {
    let task_index = plan
        .tasks
        .iter()
        .map(|task| (task.id(), task))
        .collect::<HashMap<_, _>>();
    let mut run_ids = HashMap::new();
    for result in task_results {
        let task = task_index.get(&result.task_id).ok_or_else(|| {
            OrchestratorError::PlanningFailed(format!(
                "missing compiled task for result {} during task-run persistence",
                result.task_id
            ))
        })?;
        let Some(persisted_task_id) = persisted_task_ids.get(&result.task_id) else {
            continue;
        };
        let run_id = create_task_run(
            mcp_server,
            task,
            result,
            persisted_task_id,
            persisted_plan_ids_by_task_id
                .get(&result.task_id)
                .map(String::as_str),
            base_commit_sha,
            context_snapshot_id,
            decision,
            model_name,
        )
        .await?;
        run_ids.insert(result.task_id, run_id);
    }
    Ok(run_ids)
}

async fn create_execution_task(
    mcp_server: &Arc<LibraMcpServer>,
    intent_id: &str,
    title: &str,
    description: &str,
    status: &str,
    reason: &str,
) -> Result<String, OrchestratorError> {
    let params = CreateTaskParams {
        title: title.to_string(),
        description: Some(description.to_string()),
        goal_type: None,
        constraints: None,
        acceptance_criteria: None,
        requested_by_kind: None,
        requested_by_id: None,
        dependencies: None,
        intent_id: Some(intent_id.to_string()),
        parent_task_id: None,
        origin_step_id: None,
        status: Some(status.to_string()),
        reason: Some(reason.to_string()),
        tags: None,
        external_ids: None,
        actor_kind: Some("system".to_string()),
        actor_id: Some("libra-executor".to_string()),
    };

    let actor = resolve_actor(
        mcp_server,
        params.actor_kind.as_deref(),
        params.actor_id.as_deref(),
    )?;
    let result = mcp_server
        .create_task_impl(params, actor)
        .await
        .map_err(|e| OrchestratorError::ConfigError(format!("MCP create_task failed: {e:?}")))?;
    parse_created_id("task", &result)
}

async fn create_compiled_tasks(
    mcp_server: &Arc<LibraMcpServer>,
    intent_id: &str,
    parent_task_id: &str,
    plan: &ExecutionPlanSpec,
    run_state: &RunStateSnapshot,
    persisted_step_ids: &HashMap<Uuid, Uuid>,
) -> Result<HashMap<Uuid, String>, OrchestratorError> {
    let mut persisted_ids = HashMap::new();
    let mut remaining = plan.tasks.iter().collect::<Vec<_>>();

    while !remaining.is_empty() {
        let mut progressed = false;
        let mut next_remaining = Vec::new();

        for task in remaining {
            if !task
                .dependencies()
                .iter()
                .all(|dep| persisted_ids.contains_key(dep))
            {
                next_remaining.push(task);
                continue;
            }

            let dependency_task_ids = task
                .dependencies()
                .iter()
                .map(|dep| {
                    persisted_ids.get(dep).cloned().ok_or_else(|| {
                        OrchestratorError::PlanningFailed(format!(
                            "missing persisted dependency task for compiled task {}",
                            task.id()
                        ))
                    })
                })
                .collect::<Result<Vec<_>, _>>()?;
            let status = match run_state.status_for(task.id()) {
                super::types::TaskNodeStatus::Completed => "done",
                super::types::TaskNodeStatus::Failed => "failed",
                super::types::TaskNodeStatus::Running => "running",
                super::types::TaskNodeStatus::Pending | super::types::TaskNodeStatus::Skipped => {
                    "draft"
                }
            };

            let persisted_id = create_compiled_task(PersistedTaskRequest {
                mcp_server,
                intent_id,
                parent_task_id: Some(parent_task_id),
                task,
                dependency_task_ids,
                persisted_step_id: persisted_step_ids.get(&task.step_id()).copied(),
                status,
            })
            .await?;
            persisted_ids.insert(task.id(), persisted_id);
            progressed = true;
        }

        if !progressed {
            return Err(OrchestratorError::PlanningFailed(
                "unable to persist compiled tasks due to unresolved task dependencies".to_string(),
            ));
        }

        remaining = next_remaining;
    }

    Ok(persisted_ids)
}

async fn create_compiled_task(
    request: PersistedTaskRequest<'_>,
) -> Result<String, OrchestratorError> {
    let goal_type = request
        .task
        .task
        .goal()
        .map(|goal| goal.as_str().to_string())
        .or_else(|| {
            Some(match request.task.kind {
                TaskKind::Implementation => "implementation".to_string(),
                TaskKind::Analysis => "analysis".to_string(),
                TaskKind::Gate => "test".to_string(),
            })
        });
    let description = request
        .task
        .description()
        .map(ToString::to_string)
        .or_else(|| Some(request.task.objective.clone()));
    let params = CreateTaskParams {
        title: request.task.title().to_string(),
        description,
        goal_type,
        constraints: (!request.task.constraints().is_empty())
            .then(|| request.task.constraints().to_vec()),
        acceptance_criteria: (!request.task.acceptance_criteria().is_empty())
            .then(|| request.task.acceptance_criteria().to_vec()),
        requested_by_kind: None,
        requested_by_id: None,
        dependencies: (!request.dependency_task_ids.is_empty())
            .then_some(request.dependency_task_ids),
        intent_id: Some(request.intent_id.to_string()),
        parent_task_id: request.parent_task_id.map(ToString::to_string),
        origin_step_id: request.persisted_step_id.map(|step_id| step_id.to_string()),
        status: Some(request.status.to_string()),
        reason: Some(
            if request.parent_task_id.is_some() {
                "compiled execution task"
            } else {
                "compiled plan review task"
            }
            .to_string(),
        ),
        tags: None,
        external_ids: None,
        actor_kind: Some("agent".to_string()),
        actor_id: Some("libra-executor".to_string()),
    };

    let actor = resolve_actor(
        request.mcp_server,
        params.actor_kind.as_deref(),
        params.actor_id.as_deref(),
    )?;
    let result = request
        .mcp_server
        .create_task_impl(params, actor)
        .await
        .map_err(|e| OrchestratorError::ConfigError(format!("MCP create_task failed: {e:?}")))?;
    parse_created_id("task", &result)
}

#[cfg(test)]
async fn create_plan_revision(
    mcp_server: &Arc<LibraMcpServer>,
    intent_id: &str,
    parent_plan_id: Option<&str>,
    plan: &ExecutionPlanSpec,
) -> Result<PersistedPlanRevision, OrchestratorError> {
    let git_plan = build_git_plan(
        parse_object_id(intent_id)
            .map_err(|e| OrchestratorError::ConfigError(format!("invalid intent id: {e}")))?,
        plan,
    )
    .map_err(|e| OrchestratorError::ConfigError(format!("failed to build git plan: {e}")))?;
    let steps = git_plan
        .steps()
        .iter()
        .map(|step| PlanStepParams {
            description: step.description().to_string(),
            inputs: step.inputs().cloned(),
            checks: step.checks().cloned(),
        })
        .collect::<Vec<_>>();

    let params = CreatePlanParams {
        intent_id: intent_id.to_string(),
        parent_plan_ids: parent_plan_id.map(|id| vec![id.to_string()]),
        context_frame_ids: None,
        steps: Some(steps),
        tags: Some(HashMap::from([
            ("role".to_string(), "execution".to_string()),
            (
                "path".to_string(),
                "transitional-single-execution-plan".to_string(),
            ),
        ])),
        external_ids: None,
        actor_kind: Some("system".to_string()),
        actor_id: Some("libra-plan".to_string()),
    };

    let actor = resolve_actor(
        mcp_server,
        params.actor_kind.as_deref(),
        params.actor_id.as_deref(),
    )?;
    let result = mcp_server
        .create_plan_impl(params, actor)
        .await
        .map_err(|e| OrchestratorError::ConfigError(format!("MCP create_plan failed: {e:?}")))?;
    let plan_id = parse_created_id("plan", &result)?;
    let persisted_plan = load_persisted_plan(mcp_server, &plan_id).await?;
    if persisted_plan.steps().len() != plan.tasks.len() {
        return Err(OrchestratorError::PersistenceError(format!(
            "persisted plan step count mismatch: expected {}, got {}",
            plan.tasks.len(),
            persisted_plan.steps().len()
        )));
    }

    let step_id_map = plan
        .tasks
        .iter()
        .zip(persisted_plan.steps().iter())
        .map(|(task, step)| (task.step_id(), step.step_id()))
        .collect();

    Ok(PersistedPlanRevision {
        plan_id,
        step_id_map,
    })
}

async fn create_plan_set_revision(
    mcp_server: &Arc<LibraMcpServer>,
    intent_id: &str,
    parent_execution_plan_id: Option<&str>,
    parent_test_plan_id: Option<&str>,
    plan: &ExecutionPlanSpec,
) -> Result<PersistedPlanSet, OrchestratorError> {
    let execution = create_plan_revision_for_role(
        mcp_server,
        intent_id,
        parent_execution_plan_id,
        plan,
        PersistedPlanRole::Execution,
    )
    .await?;
    let test = create_plan_revision_for_role(
        mcp_server,
        intent_id,
        parent_test_plan_id,
        plan,
        PersistedPlanRole::Test,
    )
    .await?;
    build_plan_set(plan, execution, test)
}

/// Wave 1B bridge: persist a new plan set and return the result as
/// [`crate::internal::ai::runtime::phase1::PlanWriteOutcome`] so the
/// Runtime's [`phase1::write_plan_set`](crate::internal::ai::runtime::phase1::write_plan_set)
/// entry point can delegate here without exposing
/// [`PersistedPlanSet`]'s private fields.
///
/// This is the transitional landing pattern documented at
/// `runtime/phase1.rs`: the Runtime owns the public contract surface
/// (signature + outcome type); the orchestrator owns the
/// `PersistedPlanRevision` / `step_id_map` plumbing. Once the orchestrator's
/// persistence layer is folded into `runtime/phase1.rs`, this bridge
/// disappears.
pub(crate) async fn write_plan_set_with_outcome(
    mcp_server: &Arc<LibraMcpServer>,
    intent_id: &str,
    parent_execution_plan_id: Option<&str>,
    parent_test_plan_id: Option<&str>,
    plan: &ExecutionPlanSpec,
) -> Result<crate::internal::ai::runtime::phase1::PlanWriteOutcome, OrchestratorError> {
    let plan_set = create_plan_set_revision(
        mcp_server,
        intent_id,
        parent_execution_plan_id,
        parent_test_plan_id,
        plan,
    )
    .await?;
    Ok(crate::internal::ai::runtime::phase1::PlanWriteOutcome {
        execution_plan_id: plan_set.execution.plan_id,
        test_plan_id: plan_set.test.plan_id,
        plan_id_by_task_id: plan_set.plan_id_by_task_id,
    })
}

async fn create_plan_revision_for_role(
    mcp_server: &Arc<LibraMcpServer>,
    intent_id: &str,
    parent_plan_id: Option<&str>,
    plan: &ExecutionPlanSpec,
    role: PersistedPlanRole,
) -> Result<PersistedPlanRevision, OrchestratorError> {
    let intent_uuid = parse_object_id(intent_id)
        .map_err(|e| OrchestratorError::ConfigError(format!("invalid intent id: {e}")))?;
    let role_tasks = plan
        .tasks
        .iter()
        .filter(|task| plan_role_for_task(task) == role)
        .collect::<Vec<_>>();
    let steps = plan_step_params_for_role(intent_uuid, plan, role)?;
    let params = CreatePlanParams {
        intent_id: intent_id.to_string(),
        parent_plan_ids: parent_plan_id.map(|id| vec![id.to_string()]),
        context_frame_ids: None,
        steps: Some(steps),
        tags: Some(HashMap::from([
            ("role".to_string(), role.label().to_string()),
            ("path".to_string(), "execution-test-plan-set".to_string()),
        ])),
        external_ids: None,
        actor_kind: Some("system".to_string()),
        actor_id: Some("libra-plan".to_string()),
    };

    let actor = resolve_actor(
        mcp_server,
        params.actor_kind.as_deref(),
        params.actor_id.as_deref(),
    )?;
    let result = mcp_server
        .create_plan_impl(params, actor)
        .await
        .map_err(|e| OrchestratorError::ConfigError(format!("MCP create_plan failed: {e:?}")))?;
    let plan_id = parse_created_id("plan", &result)?;
    let persisted_plan = load_persisted_plan(mcp_server, &plan_id).await?;
    let expected_step_count = role_tasks.len().max(1);
    if persisted_plan.steps().len() != expected_step_count {
        return Err(OrchestratorError::PersistenceError(format!(
            "persisted {role:?} plan step count mismatch: expected {}, got {}",
            expected_step_count,
            persisted_plan.steps().len()
        )));
    }

    let step_id_map = role_tasks
        .into_iter()
        .zip(persisted_plan.steps().iter())
        .map(|(task, step)| (task.step_id(), step.step_id()))
        .collect();

    Ok(PersistedPlanRevision {
        plan_id,
        step_id_map,
    })
}

fn plan_step_params_for_role(
    intent_id: Uuid,
    plan: &ExecutionPlanSpec,
    role: PersistedPlanRole,
) -> Result<Vec<PlanStepParams>, OrchestratorError> {
    let git_plan = build_git_plan(intent_id, plan)
        .map_err(|e| OrchestratorError::ConfigError(format!("failed to build git plan: {e}")))?;
    let mut steps = plan
        .tasks
        .iter()
        .zip(git_plan.steps().iter())
        .filter(|(task, _)| plan_role_for_task(task) == role)
        .map(|(_, step)| PlanStepParams {
            description: step.description().to_string(),
            inputs: Some(inputs_with_plan_role(step.inputs().cloned(), role)),
            checks: step.checks().cloned(),
        })
        .collect::<Vec<_>>();

    if steps.is_empty() {
        steps.push(PlanStepParams {
            description: role.synthetic_step_description().to_string(),
            inputs: Some(json!({
                "planRole": role.label(),
                "synthetic": true,
                "revision": plan.revision,
            })),
            checks: None,
        });
    }

    Ok(steps)
}

fn inputs_with_plan_role(
    inputs: Option<serde_json::Value>,
    role: PersistedPlanRole,
) -> serde_json::Value {
    let mut object = match inputs {
        Some(serde_json::Value::Object(object)) => object,
        Some(other) => {
            let mut object = serde_json::Map::new();
            object.insert("payload".to_string(), other);
            object
        }
        None => serde_json::Map::new(),
    };
    object.insert("planRole".to_string(), json!(role.label()));
    serde_json::Value::Object(object)
}

fn plan_role_for_task(task: &super::types::TaskSpec) -> PersistedPlanRole {
    if task.kind == TaskKind::Gate {
        PersistedPlanRole::Test
    } else {
        PersistedPlanRole::Execution
    }
}

fn build_plan_set(
    plan: &ExecutionPlanSpec,
    execution: PersistedPlanRevision,
    test: PersistedPlanRevision,
) -> Result<PersistedPlanSet, OrchestratorError> {
    let mut step_id_map = HashMap::new();
    let mut plan_id_by_task_id = HashMap::new();
    for task in &plan.tasks {
        let (role_plan, role_name) = if plan_role_for_task(task) == PersistedPlanRole::Test {
            (&test, PersistedPlanRole::Test.label())
        } else {
            (&execution, PersistedPlanRole::Execution.label())
        };
        let persisted_step_id = role_plan
            .step_id_map
            .get(&task.step_id())
            .copied()
            .ok_or_else(|| {
                OrchestratorError::PersistenceError(format!(
                    "persisted {role_name} plan is missing step for task {}",
                    task.id()
                ))
            })?;
        step_id_map.insert(task.step_id(), persisted_step_id);
        plan_id_by_task_id.insert(task.id(), role_plan.plan_id.clone());
    }

    Ok(PersistedPlanSet {
        execution,
        test,
        step_id_map,
        plan_id_by_task_id,
    })
}

fn is_missing_persisted_plan_error(error: &OrchestratorError) -> bool {
    matches!(
        error,
        OrchestratorError::PersistenceError(message)
            if message.starts_with("persisted plan not found:")
    )
}

async fn load_persisted_plan(
    mcp_server: &Arc<LibraMcpServer>,
    plan_id: &str,
) -> Result<GitPlan, OrchestratorError> {
    let history = mcp_server
        .intent_history_manager
        .as_ref()
        .ok_or_else(|| OrchestratorError::ConfigError("MCP history not available".to_string()))?;
    let storage = mcp_server
        .storage
        .as_ref()
        .ok_or_else(|| OrchestratorError::ConfigError("MCP storage not available".to_string()))?;
    let plan_uuid = parse_object_id(plan_id)
        .map_err(|e| OrchestratorError::ConfigError(format!("invalid plan id: {e}")))?;
    let hash = history
        .get_object_hash("plan", &plan_uuid.to_string())
        .await
        .map_err(|e| {
            OrchestratorError::PersistenceError(format!("failed to resolve plan hash: {e}"))
        })?
        .ok_or_else(|| {
            OrchestratorError::PersistenceError(format!("persisted plan not found: {plan_id}"))
        })?;

    storage.get_json::<GitPlan>(&hash).await.map_err(|e| {
        OrchestratorError::PersistenceError(format!("failed to load persisted plan: {e}"))
    })
}

async fn create_provenance(
    mcp_server: &Arc<LibraMcpServer>,
    run_id: &str,
    execution_plan: &ExecutionPlanSpec,
    task_results: &[TaskResult],
    system_report: &SystemReport,
    decision: &DecisionOutcome,
    model_name: &str,
) -> Result<String, OrchestratorError> {
    let parameters_json = json!({
        "intentSpecId": execution_plan.intent_spec_id,
        "planSummary": execution_plan.summary_line(),
        "parallelGroups": execution_plan.parallel_groups().len(),
        "checkpoints": execution_plan.checkpoints.iter().map(|checkpoint| checkpoint.label.clone()).collect::<Vec<_>>(),
        "decision": format!("{decision:?}"),
        "systemReport": {
            "overallPassed": system_report.overall_passed,
            "integrationPassed": system_report.integration.all_required_passed,
            "securityPassed": system_report.security.all_required_passed,
            "releasePassed": system_report.release.all_required_passed,
        },
        "taskRetries": task_results.iter().map(|result| json!({
            "taskId": result.task_id,
            "retryCount": result.retry_count,
        })).collect::<Vec<_>>(),
    })
    .to_string();

    let params = CreateProvenanceParams {
        run_id: run_id.to_string(),
        provider: "internal".to_string(),
        model: model_name.to_string(),
        parameters_json: Some(parameters_json),
        temperature: None,
        max_tokens: None,
        tags: None,
        external_ids: None,
        actor_kind: Some("agent".to_string()),
        actor_id: Some("libra-coder".to_string()),
    };

    let actor = resolve_actor(
        mcp_server,
        params.actor_kind.as_deref(),
        params.actor_id.as_deref(),
    )?;
    let result = mcp_server
        .create_provenance_impl(params, actor)
        .await
        .map_err(|e| {
            OrchestratorError::ConfigError(format!("MCP create_provenance failed: {e:?}"))
        })?;
    parse_created_id("provenance", &result)
}

async fn create_plan_step_events(
    request: PlanStepEventsRequest<'_>,
) -> Result<(), OrchestratorError> {
    for task in &request.plan.tasks {
        let Some(step_id) = request.persisted_step_ids.get(&task.step_id()) else {
            continue;
        };
        let Some(result) = request.run_state.result_for(task.id()) else {
            continue;
        };

        let params = CreatePlanStepEventParams {
            plan_id: request.plan_id.to_string(),
            step_id: step_id.to_string(),
            run_id: request
                .persisted_task_run_ids
                .get(&task.id())
                .map(String::as_str)
                .unwrap_or(request.fallback_run_id)
                .to_string(),
            status: plan_step_event_status(&result.status).to_string(),
            reason: match result.status {
                super::types::TaskNodeStatus::Failed => result.agent_output.clone(),
                _ => None,
            },
            consumed_frames: None,
            produced_frames: None,
            spawned_task_id: request.persisted_task_ids.get(&task.id()).cloned(),
            outputs: Some(json!({
                "taskTitle": task.title(),
                "taskKind": format!("{:?}", task.kind).to_lowercase(),
                "retryCount": result.retry_count,
                "toolCalls": result.tool_calls.len(),
                "policyViolations": result.policy_violations.len(),
            })),
            actor_kind: Some("system".to_string()),
            actor_id: Some("libra-orchestrator".to_string()),
        };

        let actor = resolve_actor(
            request.mcp_server,
            params.actor_kind.as_deref(),
            params.actor_id.as_deref(),
        )?;
        request
            .mcp_server
            .create_plan_step_event_impl(params, actor)
            .await
            .map_err(|e| {
                OrchestratorError::ConfigError(format!("MCP create_plan_step_event failed: {e:?}"))
            })?;
    }

    Ok(())
}

fn plan_step_event_status(status: &super::types::TaskNodeStatus) -> &'static str {
    match status {
        super::types::TaskNodeStatus::Pending => "pending",
        super::types::TaskNodeStatus::Running => "progressing",
        super::types::TaskNodeStatus::Completed => "completed",
        super::types::TaskNodeStatus::Failed => "failed",
        super::types::TaskNodeStatus::Skipped => "skipped",
    }
}

async fn create_tool_invocation(
    mcp_server: &Arc<LibraMcpServer>,
    run_id: &str,
    task_title: &str,
    call: &ToolCallRecord,
) -> Result<String, OrchestratorError> {
    let result_summary = call
        .summary
        .as_ref()
        .map(|summary| format!("{task_title}: {summary}"));
    let params = CreateToolInvocationParams {
        run_id: run_id.to_string(),
        tool_name: call.tool_name.clone(),
        status: Some(if call.success { "ok" } else { "error" }.to_string()),
        args_json: call
            .arguments_json
            .as_ref()
            .map(serde_json::to_string)
            .transpose()
            .map_err(|e| {
                OrchestratorError::ConfigError(format!("failed to encode tool args for MCP: {e}"))
            })?,
        io_footprint: Some(IoFootprintParams {
            paths_read: (!call.paths_read.is_empty()).then(|| call.paths_read.clone()),
            paths_written: (!call.paths_written.is_empty()).then(|| call.paths_written.clone()),
        }),
        result_summary,
        artifacts: None,
        tags: None,
        external_ids: None,
        actor_kind: Some("agent".to_string()),
        actor_id: Some("libra-coder".to_string()),
    };

    let actor = resolve_actor(
        mcp_server,
        params.actor_kind.as_deref(),
        params.actor_id.as_deref(),
    )?;
    let result = mcp_server
        .create_tool_invocation_impl(params, actor)
        .await
        .map_err(|e| {
            OrchestratorError::ConfigError(format!("MCP create_tool_invocation failed: {e:?}"))
        })?;
    parse_created_id("tool invocation", &result)
}

async fn create_patchset(
    request: PatchSetRequest<'_>,
) -> Result<Option<String>, OrchestratorError> {
    let (touched_files, diff_text) = build_patchset_payload(request.tool_calls);
    if touched_files.is_empty() {
        return Ok(None);
    }

    let diff_artifact = if let Some(diff_text) = diff_text.as_ref() {
        let storage = request.mcp_server.storage.as_ref().ok_or_else(|| {
            OrchestratorError::ConfigError("MCP storage not available".to_string())
        })?;
        Some(
            storage
                .put_artifact(diff_text.as_bytes())
                .await
                .map_err(|e| {
                    OrchestratorError::ConfigError(format!(
                        "failed to persist patchset diff artifact: {e}"
                    ))
                })?,
        )
    } else {
        None
    };

    let params = CreatePatchSetParams {
        run_id: request.run_id.to_string(),
        generation: request.generation,
        sequence: Some(request.generation),
        base_commit_sha: request.base_commit_sha.to_string(),
        touched_files: Some(touched_files),
        rationale: Some(format!(
            "{}: {}",
            request.task_title, request.task_objective
        )),
        diff_format: diff_text.as_ref().map(|_| "unified_diff".to_string()),
        diff_artifact: diff_artifact.map(|artifact| ArtifactParams {
            store: artifact.store().to_string(),
            key: artifact.key().to_string(),
            content_type: Some("text/x-diff".to_string()),
            size_bytes: None,
            hash: Some(artifact.key().to_string()),
        }),
        tags: None,
        external_ids: None,
        actor_kind: Some("agent".to_string()),
        actor_id: Some("libra-coder".to_string()),
    };

    let actor = resolve_actor(
        request.mcp_server,
        params.actor_kind.as_deref(),
        params.actor_id.as_deref(),
    )?;
    let result = request
        .mcp_server
        .create_patchset_impl(params, actor)
        .await
        .map_err(|e| {
            OrchestratorError::ConfigError(format!("MCP create_patchset failed: {e:?}"))
        })?;
    parse_created_id("patchset", &result).map(Some)
}

async fn create_evidence(request: EvidenceRequest<'_>) -> Result<String, OrchestratorError> {
    let params = CreateEvidenceParams {
        run_id: request.run_id.to_string(),
        patchset_id: request.patchset_id.map(ToString::to_string),
        kind: request.kind.to_string(),
        tool: request.tool.to_string(),
        command: request.command,
        exit_code: request.exit_code,
        summary: request.summary,
        report_artifacts: None,
        tags: None,
        external_ids: None,
        actor_kind: Some("system".to_string()),
        actor_id: Some("libra-verifier".to_string()),
    };

    let actor = resolve_actor(
        request.mcp_server,
        params.actor_kind.as_deref(),
        params.actor_id.as_deref(),
    )?;
    let result = request
        .mcp_server
        .create_evidence_impl(params, actor)
        .await
        .map_err(|e| {
            OrchestratorError::ConfigError(format!("MCP create_evidence failed: {e:?}"))
        })?;
    parse_created_id("evidence", &result)
}

async fn append_evidence_id(
    persisted: &mut PersistedTaskArtifacts,
    request: EvidenceRequest<'_>,
) -> Result<(), OrchestratorError> {
    let evidence_id = create_evidence(request).await?;
    persisted.evidence_ids.push(evidence_id);
    Ok(())
}

async fn create_decision(request: FinalDecisionRequest<'_>) -> Result<String, OrchestratorError> {
    let decision_type = match request.decision {
        DecisionOutcome::Commit => "commit",
        DecisionOutcome::HumanReviewRequired => "checkpoint",
        DecisionOutcome::Abandon => "abandon",
    };
    let rationale = Some(format!(
        "{}; overall_passed={}; failed_tasks={}; checkpoints={}",
        request.execution_plan.summary_line(),
        request.system_report.overall_passed,
        request
            .task_results
            .iter()
            .filter(|result| result.status == super::types::TaskNodeStatus::Failed)
            .count(),
        request.execution_plan.checkpoints.len()
    ));

    let params = CreateDecisionParams {
        run_id: request.run_id.to_string(),
        decision_type: decision_type.to_string(),
        chosen_patchset_id: request.chosen_patchset_id.map(ToString::to_string),
        result_commit_sha: None,
        checkpoint_id: request.checkpoint_id.map(ToString::to_string),
        rationale,
        tags: None,
        external_ids: None,
        actor_kind: Some("system".to_string()),
        actor_id: Some("libra-orchestrator".to_string()),
    };

    let actor = resolve_actor(
        request.mcp_server,
        params.actor_kind.as_deref(),
        params.actor_id.as_deref(),
    )?;
    let result = request
        .mcp_server
        .create_decision_impl(params, actor)
        .await
        .map_err(|e| {
            OrchestratorError::ConfigError(format!("MCP create_decision failed: {e:?}"))
        })?;
    parse_created_id("decision", &result)
}

async fn record_terminal_intent_event(
    mcp_server: &Arc<LibraMcpServer>,
    intent_id: &str,
    decision: &DecisionOutcome,
) -> Result<(), OrchestratorError> {
    let (status, reason) = match decision {
        DecisionOutcome::Commit => ("completed", "orchestrator committed execution result"),
        DecisionOutcome::HumanReviewRequired => {
            ("completed", "orchestrator checkpointed for human review")
        }
        DecisionOutcome::Abandon => ("cancelled", "orchestrator abandoned execution result"),
    };
    mcp_server
        .update_intent_impl(UpdateIntentParams {
            intent_id: intent_id.to_string(),
            status: Some(status.to_string()),
            commit_sha: None,
            reason: Some(reason.to_string()),
            next_intent_id: None,
        })
        .await
        .map_err(|error| {
            OrchestratorError::ConfigError(format!("MCP update_intent failed: {error:?}"))
        })?;
    Ok(())
}

fn build_artifact_ledger(
    thread_id: &str,
    tasks: &[PersistedTaskArtifacts],
) -> Result<ArtifactLedger, OrchestratorError> {
    let thread_id = parse_persisted_object_id("artifact ledger thread", thread_id)?;
    let mut ledger = ArtifactLedger::new(thread_id);
    for task in tasks {
        let mut refs = TaskArtifactRefs::new(task.task_id);
        if let Some(patchset_id) = task.patchset_id.as_deref() {
            refs.patchset_ids.push(parse_persisted_object_id(
                "artifact ledger patchset",
                patchset_id,
            )?);
        }
        ledger.push_task(refs);
    }
    Ok(ledger)
}

fn parse_optional_persisted_object_id(
    label: &str,
    value: Option<&str>,
) -> Result<Option<Uuid>, OrchestratorError> {
    value
        .map(|value| parse_persisted_object_id(label, value))
        .transpose()
}

fn parse_persisted_object_id(label: &str, value: &str) -> Result<Uuid, OrchestratorError> {
    parse_object_id(value).map_err(|error| {
        OrchestratorError::ConfigError(format!("invalid {label} id '{value}': {error:#}"))
    })
}

async fn persist_validation_decision_derivatives(
    mcp_server: &Arc<LibraMcpServer>,
    thread_id: &str,
    run_id: &str,
    artifact_ledger: &ArtifactLedger,
    release_candidate_patchset_id: Option<Uuid>,
    system_report: &SystemReport,
    decision: &DecisionOutcome,
) -> Result<PersistedDerivedRecords, OrchestratorError> {
    let history = mcp_server.intent_history_manager.as_ref().ok_or_else(|| {
        OrchestratorError::ConfigError(
            "cannot persist validation decision records without AI history manager".to_string(),
        )
    })?;
    let thread_id = Uuid::parse_str(thread_id).map_err(|error| {
        OrchestratorError::ConfigError(format!(
            "cannot persist validation decision records because thread id '{thread_id}' is not a UUID: {error}"
        ))
    })?;
    let run_id = Uuid::parse_str(run_id).map_err(|error| {
        OrchestratorError::ConfigError(format!(
            "cannot persist validation decision records because run id '{run_id}' is not a UUID: {error}"
        ))
    })?;

    let validator = ValidatorEngine::default_policy();
    let report = validator.build_report(
        thread_id,
        Some(run_id),
        validation_stages_from_system_report_with_artifacts(
            system_report,
            artifact_ledger,
            release_candidate_patchset_id,
            decision,
        ),
    );
    let policy = DecisionPolicy::default();
    let risk = aggregate_risk_score(&report, &policy);
    let mut proposal = build_decision_proposal(&report, &risk, &policy);
    align_decision_proposal_with_outcome(&mut proposal, decision);

    let db = history.database_connection();
    let session_mirror = session_jsonl_store_for_thread(mcp_server, thread_id);
    let validation_store = ValidationReportStore::new(db.clone());
    if let Some(session_mirror) = session_mirror.as_ref() {
        validation_store
            .write_latest_with_session_mirror(&report, session_mirror)
            .await
    } else {
        validation_store.write_latest(&report).await
    }
    .map_err(|error| {
        OrchestratorError::ConfigError(format!(
            "failed to persist validation report {} for thread {}: {error}",
            report.report_id, report.thread_id
        ))
    })?;

    let decision_store = DecisionProposalStore::new(db.clone());
    if let Some(session_mirror) = session_mirror.as_ref() {
        decision_store
            .write_latest_with_session_mirror(&risk, &proposal, session_mirror)
            .await
    } else {
        decision_store.write_latest(&risk, &proposal).await
    }
    .map_err(|error| {
        OrchestratorError::ConfigError(format!(
            "failed to persist decision proposal {} for thread {}: {error}",
            proposal.proposal_id, proposal.thread_id
        ))
    })?;

    // Phase 4 completion: finalise an AutoAccept proposal into the formal
    // final `Decision` artifact, closing the
    // ValidationReport -> RiskScoreBreakdown -> DecisionProposal -> Decision
    // chain. Human-gated routes (HumanReview / RequestChanges) are NOT
    // finalised here — they resolve through the CEX-S2-13 human-gated merge
    // flow that owns the approval interaction.
    if let Some(final_decision) = FinalDecision::finalize_auto_accept(&proposal, Utc::now()) {
        let final_store = FinalDecisionStore::new(db);
        if let Some(session_mirror) = session_mirror.as_ref() {
            final_store
                .write_latest_with_session_mirror(&final_decision, session_mirror)
                .await
        } else {
            final_store.write_latest(&final_decision).await
        }
        .map_err(|error| {
            OrchestratorError::ConfigError(format!(
                "failed to persist final decision {} for thread {}: {error}",
                final_decision.decision_id, final_decision.thread_id
            ))
        })?;
    }

    Ok(PersistedDerivedRecords {
        validation_report_id: report.report_id,
        risk_score_breakdown_id: risk.breakdown_id,
        decision_proposal_id: proposal.proposal_id,
    })
}

fn align_decision_proposal_with_outcome(
    proposal: &mut DecisionProposal,
    decision: &DecisionOutcome,
) {
    match decision {
        DecisionOutcome::Commit => {}
        DecisionOutcome::HumanReviewRequired => {
            proposal.summary.route = DecisionProposalRoute::HumanReview;
            proposal.summary.proposed_verdict = FinalDecisionVerdict::Accepted;
            proposal.summary.requires_human_review = true;
            push_unique_rationale(
                &mut proposal.summary.rationale,
                "orchestrator decision requires human review",
            );
        }
        DecisionOutcome::Abandon => {
            proposal.summary.route = DecisionProposalRoute::Abandon;
            proposal.summary.proposed_verdict = FinalDecisionVerdict::Abandon;
            proposal.summary.requires_human_review = false;
            push_unique_rationale(
                &mut proposal.summary.rationale,
                "orchestrator decision abandoned execution",
            );
        }
    }
}

fn session_jsonl_store_for_thread(
    mcp_server: &Arc<LibraMcpServer>,
    thread_id: Uuid,
) -> Option<SessionJsonlStore> {
    let working_dir = mcp_server.working_dir.as_ref()?;
    // FAIL-CLOSED (Part C §C.4.1): never mint a phantom `<working_dir>/.libra`
    // when storage resolution fails — a session store rooted at a library-less
    // gitdir would read/write the wrong (or an empty) transcript tree. A
    // resolution failure here simply means "no session store", so return None.
    let storage_root = match try_get_storage_path(Some(working_dir.clone())) {
        Ok(root) => root,
        Err(error) => {
            tracing::warn!(
                working_dir = %working_dir.display(),
                %error,
                "cannot resolve storage root for the session store; skipping (run `libra worktree repair <worktree-path>` if this is a linked worktree)"
            );
            return None;
        }
    };
    let session_store = SessionStore::from_storage_path(&storage_root);
    let working_dir_str = working_dir.to_string_lossy().to_string();

    match session_store.load_for_thread_id(&thread_id.to_string(), &working_dir_str) {
        Ok(Some(session)) => Some(SessionJsonlStore::new(
            session_store.session_root(&session.id),
        )),
        Ok(None) => {
            tracing::debug!(
                %thread_id,
                working_dir = %working_dir.display(),
                "skipping Phase 3/4 session artifact mirror because no matching Code session was found"
            );
            None
        }
        Err(error) => {
            tracing::warn!(
                %thread_id,
                working_dir = %working_dir.display(),
                error = %error,
                "skipping Phase 3/4 session artifact mirror because the Code session could not be loaded"
            );
            None
        }
    }
}

fn push_unique_rationale(rationale: &mut Vec<String>, reason: &str) {
    if !rationale.iter().any(|entry| entry == reason) {
        rationale.push(reason.to_string());
    }
}

async fn rebuild_thread_projection(
    mcp_server: &Arc<LibraMcpServer>,
    thread_id: &str,
) -> Result<(), OrchestratorError> {
    let history = mcp_server.intent_history_manager.as_ref().ok_or_else(|| {
        OrchestratorError::ProjectionError(
            "cannot rebuild workflow projection without AI history manager".to_string(),
        )
    })?;
    let storage = mcp_server.storage.as_ref().ok_or_else(|| {
        OrchestratorError::ProjectionError(
            "cannot rebuild workflow projection without MCP storage".to_string(),
        )
    })?;
    let thread_id = Uuid::parse_str(thread_id).map_err(|error| {
        OrchestratorError::ProjectionError(format!(
            "cannot rebuild workflow projection because thread id '{thread_id}' is not a UUID: {error}"
        ))
    })?;
    let db = history.database_connection();
    let rebuilder = ProjectionRebuilder::new(storage.as_ref(), history.as_ref());
    let rebuild = rebuilder
        .materialize_thread(&db, thread_id)
        .await
        .map_err(|error| {
            OrchestratorError::ProjectionError(format!(
                "failed to rebuild workflow projection for thread {thread_id}: {error:#}"
            ))
        })?;
    if rebuild.is_none() {
        return Err(OrchestratorError::ProjectionError(format!(
            "failed to rebuild workflow projection for thread {thread_id}: no projection was produced"
        )));
    }

    Ok(())
}

#[cfg(test)]
fn validation_stages_from_system_report(
    system_report: &SystemReport,
) -> Vec<ValidationStageResult> {
    validation_stages_from_system_report_with_artifacts(
        system_report,
        &ArtifactLedger::new(Uuid::nil()),
        None,
        &DecisionOutcome::HumanReviewRequired,
    )
}

fn validation_stages_from_system_report_with_artifacts(
    system_report: &SystemReport,
    artifact_ledger: &ArtifactLedger,
    release_candidate_patchset_id: Option<Uuid>,
    decision: &DecisionOutcome,
) -> Vec<ValidationStageResult> {
    let mut release_blockers = release_stage_blockers(system_report);
    release_blockers.extend(release_candidate_blockers(
        artifact_ledger,
        release_candidate_patchset_id,
        decision,
    ));
    let release_passed = system_report.release.all_required_passed
        && system_report.review_passed
        && system_report.artifacts_complete
        && release_blockers.is_empty();
    vec![
        validation_stage_from_gate_report(
            ValidationStage::Integration,
            &system_report.integration,
            system_report.integration.all_required_passed,
            Vec::new(),
        ),
        validation_stage_from_gate_report(
            ValidationStage::Security,
            &system_report.security,
            system_report.security.all_required_passed,
            Vec::new(),
        ),
        validation_stage_from_gate_report(
            ValidationStage::Release,
            &system_report.release,
            release_passed,
            release_blockers,
        ),
    ]
}

fn release_candidate_blockers(
    artifact_ledger: &ArtifactLedger,
    release_candidate_patchset_id: Option<Uuid>,
    decision: &DecisionOutcome,
) -> Vec<String> {
    if *decision != DecisionOutcome::Commit {
        return Vec::new();
    }

    match release_candidate_patchset_id {
        Some(patchset_id) if artifact_ledger.has_patchset(patchset_id) => Vec::new(),
        Some(patchset_id) => vec![format!(
            "release candidate patchset {patchset_id} is not present in the artifact ledger"
        )],
        None => vec!["release candidate patchset is missing for commit decision".to_string()],
    }
}

fn validation_stage_from_gate_report(
    stage: ValidationStage,
    report: &GateReport,
    passed: bool,
    extra_blockers: Vec<String>,
) -> ValidationStageResult {
    let evidence = validation_stage_evidence(report, &extra_blockers);
    let outcome = if report.results.iter().any(|result| result.timed_out) {
        ValidationOutcome::InfrastructureFailed
    } else if passed {
        ValidationOutcome::Passed
    } else {
        ValidationOutcome::BlockingFailed
    };
    let summary = Some(validation_stage_summary(report, &extra_blockers));

    ValidationStageResult {
        stage,
        outcome,
        evidence,
        summary,
    }
}

fn validation_stage_evidence(report: &GateReport, extra_blockers: &[String]) -> Vec<EvidenceKind> {
    let mut evidence = report
        .results
        .iter()
        .map(|result| evidence_kind_from_gate_result(&result.kind, result.timed_out))
        .collect::<Vec<_>>();
    evidence.extend(
        extra_blockers
            .iter()
            .map(|_| EvidenceKind::ValidationBlockingFailed),
    );
    evidence
}

fn evidence_kind_from_gate_result(kind: &str, timed_out: bool) -> EvidenceKind {
    if timed_out {
        return EvidenceKind::Timeout;
    }

    match kind {
        "test" => EvidenceKind::Test,
        "lint" => EvidenceKind::Lint,
        "build" => EvidenceKind::Build,
        "security" => EvidenceKind::Security,
        "performance" => EvidenceKind::Performance,
        other => EvidenceKind::Other(other.to_string()),
    }
}

fn release_stage_blockers(system_report: &SystemReport) -> Vec<String> {
    let mut blockers = Vec::new();
    if !system_report.review_passed {
        if system_report.review_findings.is_empty() {
            blockers.push("review did not pass".to_string());
        } else {
            blockers.extend(system_report.review_findings.iter().cloned());
        }
    }
    if !system_report.artifacts_complete {
        if system_report.missing_artifacts.is_empty() {
            blockers.push("required artifacts are incomplete".to_string());
        } else {
            blockers.push(format!(
                "missing artifacts: {}",
                system_report.missing_artifacts.join(", ")
            ));
        }
    }
    let all_validation_conditions_passed = system_report.integration.all_required_passed
        && system_report.security.all_required_passed
        && system_report.release.all_required_passed
        && system_report.review_passed
        && system_report.artifacts_complete;
    if blockers.is_empty() && all_validation_conditions_passed && !system_report.overall_passed {
        blockers.push("execution did not complete all required planned tasks".to_string());
    }
    blockers
}

fn validation_stage_summary(report: &GateReport, extra_blockers: &[String]) -> String {
    let passed = report.results.iter().filter(|result| result.passed).count();
    let failed = report.results.len().saturating_sub(passed);
    let mut parts = vec![format!(
        "{passed}/{} gate checks passed; {failed} failed",
        report.results.len()
    )];
    if !extra_blockers.is_empty() {
        parts.push(extra_blockers.join("; "));
    }
    parts.join("; ")
}

fn build_patchset_payload(
    tool_calls: &[ToolCallRecord],
) -> (Vec<TouchedFileParams>, Option<String>) {
    let mut touched: BTreeMap<String, TouchedFileParams> = BTreeMap::new();
    let mut diffs = BTreeMap::<String, String>::new();

    for call in tool_calls {
        if !call.diffs.is_empty() {
            for diff in &call.diffs {
                let Some(path) = normalize_patch_path(&diff.path) else {
                    continue;
                };
                let normalized_diff =
                    normalize_diff_text(&diff.diff).unwrap_or_else(|| diff.diff.clone());
                let (lines_added, lines_deleted) = count_diff_lines(&normalized_diff);
                touched.insert(
                    path.clone(),
                    TouchedFileParams {
                        path: path.clone(),
                        change_type: normalize_change_type(&diff.change_type).to_string(),
                        lines_added,
                        lines_deleted,
                    },
                );
                diffs.insert(path, normalized_diff);
            }
            continue;
        }

        for path in &call.paths_written {
            let Some(path) = normalize_patch_path(path) else {
                continue;
            };
            touched.entry(path.clone()).or_insert(TouchedFileParams {
                path,
                change_type: "modify".to_string(),
                lines_added: 0,
                lines_deleted: 0,
            });
        }
    }

    let diff_text = (!diffs.is_empty()).then(|| diffs.into_values().collect::<Vec<_>>().join("\n"));
    (touched.into_values().collect(), diff_text)
}

fn normalize_patch_path(path: &str) -> Option<String> {
    let path = path.trim().replace('\\', "/");
    if path.is_empty() || path == "/dev/null" {
        return None;
    }
    let path = path
        .strip_prefix("a/")
        .or_else(|| path.strip_prefix("b/"))
        .unwrap_or(&path);
    let path = if let Some((_, relative)) = path.split_once("/workspace/") {
        relative
    } else if let Some(relative) = path.strip_prefix("workspace/") {
        relative
    } else if Path::new(path).is_absolute() {
        return None;
    } else {
        path
    };
    let path = path.trim_start_matches("./").trim_start_matches('/');
    if path.is_empty() || path.starts_with(".libra/") {
        None
    } else {
        Some(path.to_string())
    }
}

fn normalize_diff_text(diff: &str) -> Option<String> {
    let mut changed = false;
    let mut lines = Vec::new();
    for line in diff.lines() {
        if let Some(rest) = line.strip_prefix("diff --git ") {
            let mut parts = rest.split_whitespace();
            if let (Some(left), Some(right)) = (parts.next(), parts.next())
                && let (Some(left), Some(right)) =
                    (normalize_patch_path(left), normalize_patch_path(right))
            {
                lines.push(format!("diff --git a/{left} b/{right}"));
                changed = true;
                continue;
            }
        }
        if let Some(rewritten) = normalize_diff_file_header(line, "--- ", "a") {
            lines.push(rewritten);
            changed = true;
            continue;
        }
        if let Some(rewritten) = normalize_diff_file_header(line, "+++ ", "b") {
            lines.push(rewritten);
            changed = true;
            continue;
        }
        lines.push(line.to_string());
    }

    changed.then(|| lines.join("\n"))
}

fn normalize_diff_file_header(line: &str, prefix: &str, side: &str) -> Option<String> {
    let rest = line.strip_prefix(prefix)?;
    let (path, suffix) = split_diff_header_path(rest);
    if path == "/dev/null" {
        return None;
    }
    normalize_patch_path(path).map(|path| format!("{prefix}{side}/{path}{suffix}"))
}

fn split_diff_header_path(rest: &str) -> (&str, &str) {
    match rest.find(char::is_whitespace) {
        Some(index) => (&rest[..index], &rest[index..]),
        None => (rest, ""),
    }
}

fn count_diff_lines(diff: &str) -> (u32, u32) {
    let mut added = 0_u32;
    let mut deleted = 0_u32;
    for line in diff.lines() {
        if line.starts_with("+++") || line.starts_with("---") {
            continue;
        }
        if line.starts_with('+') {
            added += 1;
        } else if line.starts_with('-') {
            deleted += 1;
        }
    }
    (added, deleted)
}

fn normalize_change_type(change_type: &str) -> &str {
    match change_type {
        "add" | "modify" | "delete" | "rename" | "copy" => change_type,
        "update" => "modify",
        _ => "modify",
    }
}

fn normalize_evidence_kind(kind: &str) -> &str {
    match kind {
        "test" | "lint" | "build" => kind,
        _ => "other",
    }
}

fn task_gate_tool_name(stage: Option<&GateStage>) -> &'static str {
    match stage {
        Some(GateStage::Fast) => "gate-fast",
        Some(GateStage::Integration) => "gate-integration",
        Some(GateStage::Security) => "gate-security",
        Some(GateStage::Release) => "gate-release",
        None => "gate",
    }
}

fn resolve_actor(
    mcp_server: &Arc<LibraMcpServer>,
    actor_kind: Option<&str>,
    actor_id: Option<&str>,
) -> Result<ActorRef, OrchestratorError> {
    mcp_server
        .resolve_actor_from_params(actor_kind, actor_id)
        .map_err(|e| OrchestratorError::ConfigError(format!("failed to resolve MCP actor: {e:?}")))
}

fn parse_created_id(kind: &str, result: &CallToolResult) -> Result<String, OrchestratorError> {
    if result.is_error.unwrap_or(false) {
        return Err(OrchestratorError::ConfigError(format!(
            "MCP create_{kind} returned an error result"
        )));
    }

    for content in &result.content {
        if let Some(text) = content.as_text().map(|value| value.text.as_str())
            && let Some(id) = text.split("ID:").nth(1)
        {
            let id = id
                .trim()
                .split(|c: char| c.is_ascii_whitespace() || c == '|')
                .next()
                .unwrap_or("");
            if !id.is_empty() {
                return parse_object_id(id)
                    .map(|id| id.to_string())
                    .map_err(|error| {
                        OrchestratorError::ConfigError(format!(
                            "failed to parse {kind} id from MCP response: {error}"
                        ))
                    });
            }
        }
    }

    Err(OrchestratorError::ConfigError(format!(
        "failed to parse {kind} id from MCP response"
    )))
}

fn resolve_base_commit(base_commit: Option<&str>, working_dir: &Path) -> String {
    if let Some(commit) = base_commit {
        let trimmed = commit.trim();
        if !trimmed.is_empty() {
            return trimmed.to_string();
        }
    }

    let output = Command::new("git")
        .arg("rev-parse")
        .arg("HEAD")
        .current_dir(working_dir)
        .output();

    match output {
        Ok(output) if output.status.success() => {
            let text = String::from_utf8_lossy(&output.stdout).trim().to_string();
            if text.is_empty() {
                ZERO_COMMIT_SHA.to_string()
            } else {
                text
            }
        }
        _ => ZERO_COMMIT_SHA.to_string(),
    }
}

async fn create_replan_checkpoints(
    mcp_server: &Arc<LibraMcpServer>,
    spec: &IntentSpec,
    run_id: &str,
    plan_revisions: &[ExecutionPlanSpec],
    working_dir: &Path,
    task_results: &[TaskResult],
) -> Result<Vec<PersistedCheckpoint>, OrchestratorError> {
    if !checkpoint_on_replan(spec) && !checkpoint_before_replan(spec) {
        return Ok(Vec::new());
    }

    let mut persisted = Vec::new();
    for (index, entry) in spec.lifecycle.change_log.iter().enumerate() {
        let Some(plan) = plan_revisions.get(index) else {
            break;
        };

        let snapshot_id = if checkpoint_on_replan(spec) || checkpoint_before_replan(spec) {
            Some(
                create_context_snapshot(
                    mcp_server,
                    build_checkpoint_summary(plan, entry.reason.as_str()),
                    collect_snapshot_items(spec, Some(plan), working_dir, task_results),
                )
                .await?,
            )
        } else {
            None
        };
        let decision_id = if checkpoint_before_replan(spec) {
            Some(
                create_checkpoint_decision(
                    mcp_server,
                    run_id,
                    snapshot_id.as_deref(),
                    plan,
                    entry.reason.as_str(),
                )
                .await?,
            )
        } else {
            None
        };
        persisted.push(PersistedCheckpoint {
            revision: plan.revision,
            reason: entry.reason.clone(),
            snapshot_id,
            decision_id,
            dagrs_checkpoint_id: None,
        });
    }

    Ok(persisted)
}

async fn create_context_snapshot(
    mcp_server: &Arc<LibraMcpServer>,
    summary: String,
    items: Vec<ContextItemParams>,
) -> Result<String, OrchestratorError> {
    let params = CreateContextSnapshotParams {
        selection_strategy: if items.is_empty() {
            "heuristic".to_string()
        } else {
            "explicit".to_string()
        },
        items: (!items.is_empty()).then_some(items),
        summary: Some(summary),
        tags: None,
        external_ids: None,
        actor_kind: Some("system".to_string()),
        actor_id: Some("libra-orchestrator".to_string()),
    };

    let actor = resolve_actor(
        mcp_server,
        params.actor_kind.as_deref(),
        params.actor_id.as_deref(),
    )?;
    let result = mcp_server
        .create_context_snapshot_impl(params, actor)
        .await
        .map_err(|e| {
            OrchestratorError::ConfigError(format!("MCP create_context_snapshot failed: {e:?}"))
        })?;
    parse_created_id("context snapshot", &result)
}

async fn create_checkpoint_decision(
    mcp_server: &Arc<LibraMcpServer>,
    run_id: &str,
    checkpoint_id: Option<&str>,
    plan: &ExecutionPlanSpec,
    reason: &str,
) -> Result<String, OrchestratorError> {
    let params = CreateDecisionParams {
        run_id: run_id.to_string(),
        decision_type: "checkpoint".to_string(),
        chosen_patchset_id: None,
        result_commit_sha: None,
        checkpoint_id: checkpoint_id.map(ToString::to_string),
        rationale: Some(format!(
            "checkpoint before replanning plan revision {}: {}",
            plan.revision, reason
        )),
        tags: None,
        external_ids: None,
        actor_kind: Some("system".to_string()),
        actor_id: Some("libra-orchestrator".to_string()),
    };

    let actor = resolve_actor(
        mcp_server,
        params.actor_kind.as_deref(),
        params.actor_id.as_deref(),
    )?;
    let result = mcp_server
        .create_decision_impl(params, actor)
        .await
        .map_err(|e| {
            OrchestratorError::ConfigError(format!("MCP create_checkpoint_decision failed: {e:?}"))
        })?;
    parse_created_id("decision", &result)
}

fn collect_snapshot_items(
    spec: &IntentSpec,
    plan: Option<&ExecutionPlanSpec>,
    working_dir: &Path,
    task_results: &[TaskResult],
) -> Vec<ContextItemParams> {
    let mut candidates = BTreeSet::new();
    if let Some(touch_hints) = &spec.intent.touch_hints {
        candidates.extend(touch_hints.files.iter().cloned());
    }
    if let Some(plan) = plan {
        for task in &plan.tasks {
            candidates.extend(task.contract.touch_files.iter().cloned());
        }
    }
    for result in task_results {
        for call in &result.tool_calls {
            candidates.extend(call.paths_written.iter().cloned());
            candidates.extend(call.paths_read.iter().cloned());
        }
    }

    candidates
        .into_iter()
        .filter_map(|path| build_context_item(working_dir, path))
        .collect()
}

fn build_context_item(working_dir: &Path, path: String) -> Option<ContextItemParams> {
    if !is_literal_file_path(&path) {
        return None;
    }

    let resolved = resolve_workspace_file(working_dir, &path)?;
    let content_hash = hash_file_blob(working_dir, &resolved)?;
    Some(ContextItemParams {
        kind: Some("file".to_string()),
        path,
        preview: None,
        content_hash: Some(content_hash.clone()),
        blob_hash: Some(content_hash),
    })
}

fn resolve_workspace_file(working_dir: &Path, path: &str) -> Option<PathBuf> {
    let workspace_root = fs::canonicalize(working_dir).ok()?;
    let candidate = if Path::new(path).is_absolute() {
        PathBuf::from(path)
    } else {
        workspace_root.join(path)
    };
    let canonical = fs::canonicalize(candidate).ok()?;
    (canonical.is_file() && canonical.starts_with(&workspace_root)).then_some(canonical)
}

fn is_literal_file_path(path: &str) -> bool {
    !path.is_empty()
        && !path.ends_with('/')
        && !path.contains('*')
        && !path.contains('?')
        && !path.contains('[')
        && !path.contains('{')
}

fn hash_file_blob(working_dir: &Path, path: &Path) -> Option<String> {
    let output = Command::new("git")
        .arg("hash-object")
        .arg(path)
        .current_dir(working_dir)
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    let hash = String::from_utf8(output.stdout).ok()?;
    let trimmed = hash.trim();
    (!trimmed.is_empty()).then(|| trimmed.to_string())
}

fn build_snapshot_summary(
    spec: &IntentSpec,
    plan: Option<&ExecutionPlanSpec>,
    prefix: &str,
) -> String {
    match plan {
        Some(plan) => format!(
            "{prefix}: {} (intent {}, plan revision {})",
            spec.intent.summary, spec.metadata.id, plan.revision
        ),
        None => format!(
            "{prefix}: {} (intent {})",
            spec.intent.summary, spec.metadata.id
        ),
    }
}

fn build_checkpoint_summary(plan: &ExecutionPlanSpec, reason: &str) -> String {
    format!(
        "Checkpoint before replan after revision {}: {}",
        plan.revision, reason
    )
}

fn snapshot_on_run_start(spec: &IntentSpec) -> bool {
    spec.libra
        .as_ref()
        .and_then(|libra| libra.run_policy.as_ref())
        .is_none_or(|policy| policy.snapshot_on_run_start)
}

#[cfg(test)]
mod tests {
    use std::{collections::BTreeMap, path::Path, sync::Arc};

    use git_internal::internal::object::{
        intent_event::{IntentEvent, IntentEventKind},
        plan::Plan as GitPlan,
        plan_step_event::{PlanStepEvent, PlanStepStatus},
        task::Task as GitTask,
        types::ActorRef,
    };
    use sea_orm::EntityTrait;
    use tempfile::tempdir;

    use super::*;
    use crate::{
        internal::{
            ai::{
                history::HistoryManager,
                intentspec::types::*,
                orchestrator::{
                    run_state::{RunStateSnapshot, TaskStatusSnapshot},
                    types::{
                        ExecutionCheckpoint, ExecutionPlanSpec, GateReport, GateResult, GateStage,
                        TaskContract, TaskKind, TaskNodeStatus, TaskRuntimeEvent, TaskRuntimePhase,
                        TaskSpec, ToolDiffRecord,
                    },
                },
                session::SessionState,
            },
            db,
            model::{
                ai_decision_proposal, ai_index_intent_plan, ai_index_plan_step_task,
                ai_index_run_event, ai_index_run_patchset, ai_index_task_run,
                ai_risk_score_breakdown, ai_scheduler_plan_head, ai_scheduler_selected_plan,
                ai_validation_report,
            },
        },
        utils::{storage::local::LocalStorage, storage_ext::StorageExt},
    };

    async fn setup_server() -> Arc<LibraMcpServer> {
        let temp_dir = tempdir().unwrap();
        let temp_path = temp_dir.keep();
        let db_path = temp_path.join("libra.db");
        let db = db::create_database(db_path.to_str().unwrap())
            .await
            .unwrap();
        let storage = Arc::new(LocalStorage::new(temp_path.join("objects")));
        let history_manager = Arc::new(HistoryManager::new(
            storage.clone(),
            temp_path,
            Arc::new(db),
        ));
        Arc::new(LibraMcpServer::new(Some(history_manager), Some(storage)))
    }

    fn test_spec(change_log: Vec<ChangeLogEntry>) -> IntentSpec {
        IntentSpec {
            api_version: "intentspec.io/v1alpha1".into(),
            kind: "IntentSpec".into(),
            metadata: Metadata {
                id: "intent-1".into(),
                created_at: "2025-01-01T00:00:00Z".into(),
                created_by: CreatedBy {
                    creator_type: CreatorType::User,
                    id: "tester".into(),
                    display_name: None,
                },
                target: Target {
                    repo: RepoTarget {
                        repo_type: RepoType::Local,
                        locator: ".".into(),
                    },
                    base_ref: "HEAD".into(),
                    workspace_id: None,
                    labels: BTreeMap::new(),
                },
            },
            intent: Intent {
                summary: "Implement feature and verify it".into(),
                problem_statement: "problem".into(),
                change_type: ChangeType::Feature,
                objectives: vec![Objective {
                    title: "Update src/lib.rs".into(),
                    kind: ObjectiveKind::Implementation,
                }],
                in_scope: vec!["src/".into()],
                out_of_scope: vec![],
                touch_hints: Some(TouchHints {
                    files: vec!["src/lib.rs".into()],
                    symbols: vec![],
                    apis: vec![],
                }),
            },
            acceptance: Acceptance {
                success_criteria: vec!["tests pass".into()],
                verification_plan: VerificationPlan {
                    fast_checks: vec![],
                    integration_checks: vec![],
                    security_checks: vec![],
                    release_checks: vec![],
                },
                quality_gates: None,
            },
            constraints: Constraints {
                security: ConstraintSecurity {
                    network_policy: NetworkPolicy::Deny,
                    dependency_policy: DependencyPolicy::NoNew,
                    crypto_policy: String::new(),
                },
                privacy: ConstraintPrivacy {
                    data_classes_allowed: vec![DataClass::Public],
                    redaction_required: false,
                    retention_days: 1,
                },
                licensing: ConstraintLicensing {
                    allowed_spdx: vec![],
                    forbid_new_licenses: false,
                },
                platform: ConstraintPlatform {
                    language_runtime: "rust".into(),
                    supported_os: vec![],
                },
                resources: ConstraintResources {
                    max_wall_clock_seconds: 30,
                    max_cost_units: 0,
                },
            },
            risk: Risk {
                level: RiskLevel::Low,
                rationale: String::new(),
                factors: vec![],
                human_in_loop: HumanInLoop {
                    required: false,
                    min_approvers: 0,
                },
            },
            evidence: EvidencePolicy {
                strategy: EvidenceStrategy::RepoFirst,
                trust_tiers: vec![TrustTier::Repo],
                domain_allowlist_mode: DomainAllowlistMode::Disabled,
                allowed_domains: vec![],
                blocked_domains: vec![],
                min_citations_per_decision: 1,
            },
            security: SecurityPolicy {
                tool_acl: ToolAcl {
                    allow: vec![],
                    deny: vec![],
                },
                secrets: SecretPolicy {
                    policy: SecretAccessPolicy::DenyAll,
                    allowed_scopes: vec![],
                },
                prompt_injection: PromptInjectionPolicy {
                    treat_retrieved_content_as_untrusted: true,
                    enforce_output_schema: true,
                    disallow_instruction_from_evidence: true,
                },
                output_handling: OutputHandlingPolicy {
                    encoding_policy: EncodingPolicy::StrictJson,
                    no_direct_eval: true,
                },
            },
            execution: ExecutionPolicy {
                concurrency: ConcurrencyPolicy {
                    max_parallel_tasks: 1,
                },
                retry: RetryPolicy {
                    max_retries: 1,
                    backoff_seconds: 0,
                },
                replan: ReplanPolicy {
                    triggers: vec![ReplanTrigger::SecurityGateFail],
                },
            },
            provenance: ProvenancePolicy {
                require_slsa_provenance: true,
                require_sbom: false,
                transparency_log: TransparencyLogPolicy {
                    mode: TransparencyMode::None,
                },
                bindings: ProvenanceBindings {
                    embed_intent_spec_digest: true,
                    embed_evidence_digests: true,
                },
            },
            lifecycle: Lifecycle {
                schema_version: "1".into(),
                status: LifecycleStatus::Active,
                change_log,
            },
            libra: Some(LibraBinding {
                object_store: None,
                context_pipeline: None,
                plan_generation: None,
                run_policy: None,
                actor_mapping: None,
                decision_policy: None,
            }),
            artifacts: Artifacts {
                required: vec![],
                retention: ArtifactRetention::default(),
            },
            extensions: BTreeMap::new(),
        }
    }

    #[test]
    fn incomplete_execution_marks_validation_report_blocking_failed() {
        let system_report = SystemReport {
            integration: GateReport::empty(),
            security: GateReport::empty(),
            release: GateReport::empty(),
            review_passed: true,
            review_findings: vec![],
            artifacts_complete: true,
            missing_artifacts: vec![],
            overall_passed: false,
        };

        let stages = validation_stages_from_system_report(&system_report);
        let release = stages
            .iter()
            .find(|stage| stage.stage == ValidationStage::Release)
            .expect("release stage");

        assert_eq!(release.outcome, ValidationOutcome::BlockingFailed);
        assert!(
            release
                .summary
                .as_deref()
                .is_some_and(|summary| summary.contains("execution did not complete"))
        );
    }

    #[test]
    fn commit_validation_requires_release_candidate_patchset_in_artifact_ledger() {
        let system_report = SystemReport {
            integration: GateReport::empty(),
            security: GateReport::empty(),
            release: GateReport::empty(),
            review_passed: true,
            review_findings: vec![],
            artifacts_complete: true,
            missing_artifacts: vec![],
            overall_passed: true,
        };
        let thread_id = Uuid::new_v4();
        let missing_patchset_id = Uuid::new_v4();
        let empty_ledger = ArtifactLedger::new(thread_id);

        let stages = validation_stages_from_system_report_with_artifacts(
            &system_report,
            &empty_ledger,
            Some(missing_patchset_id),
            &DecisionOutcome::Commit,
        );
        let release = stages
            .iter()
            .find(|stage| stage.stage == ValidationStage::Release)
            .expect("release stage");

        assert_eq!(release.outcome, ValidationOutcome::BlockingFailed);
        assert!(
            release
                .summary
                .as_deref()
                .is_some_and(|summary| summary.contains("is not present in the artifact ledger"))
        );
    }

    #[test]
    fn commit_validation_accepts_release_candidate_patchset_from_artifact_ledger() {
        let system_report = SystemReport {
            integration: GateReport::empty(),
            security: GateReport::empty(),
            release: GateReport::empty(),
            review_passed: true,
            review_findings: vec![],
            artifacts_complete: true,
            missing_artifacts: vec![],
            overall_passed: true,
        };
        let thread_id = Uuid::new_v4();
        let patchset_id = Uuid::new_v4();
        let mut ledger = ArtifactLedger::new(thread_id);
        let mut task_refs = TaskArtifactRefs::new(Uuid::new_v4());
        task_refs.patchset_ids.push(patchset_id);
        ledger.push_task(task_refs);

        let stages = validation_stages_from_system_report_with_artifacts(
            &system_report,
            &ledger,
            Some(patchset_id),
            &DecisionOutcome::Commit,
        );
        let release = stages
            .iter()
            .find(|stage| stage.stage == ValidationStage::Release)
            .expect("release stage");

        assert_eq!(release.outcome, ValidationOutcome::Passed);
    }

    #[test]
    fn abandon_decision_overrides_auto_accept_decision_proposal() {
        let thread_id = Uuid::new_v4();
        let run_id = Uuid::new_v4();
        let validator = ValidatorEngine::default_policy();
        let report = validator.build_report(
            thread_id,
            Some(run_id),
            vec![ValidationStageResult {
                stage: ValidationStage::Integration,
                outcome: ValidationOutcome::Passed,
                evidence: vec![],
                summary: Some("passed".to_string()),
            }],
        );
        let policy = DecisionPolicy::default();
        let risk = aggregate_risk_score(&report, &policy);
        let mut proposal = build_decision_proposal(&report, &risk, &policy);

        align_decision_proposal_with_outcome(&mut proposal, &DecisionOutcome::Abandon);

        assert_eq!(proposal.summary.route, DecisionProposalRoute::Abandon);
        assert_eq!(
            proposal.summary.proposed_verdict,
            FinalDecisionVerdict::Abandon
        );
        assert!(!proposal.summary.requires_human_review);
        assert!(
            proposal
                .summary
                .rationale
                .iter()
                .any(|reason| reason.contains("abandoned execution"))
        );
    }

    #[test]
    fn session_jsonl_store_for_thread_loads_matching_code_session() {
        let temp_dir = tempdir().unwrap();
        let working_dir = temp_dir.path().to_path_buf();
        let storage_root = working_dir.join(".libra");
        fs::create_dir_all(storage_root.join("objects")).unwrap();
        fs::create_dir_all(storage_root.join("hooks")).unwrap();
        let session_store = SessionStore::from_storage_path(&storage_root);
        let thread_id = Uuid::new_v4();
        let mut session = SessionState::new(&working_dir.to_string_lossy());
        session.id = "session-jsonl-mirror-test".to_string();
        session
            .metadata
            .insert("thread_id".to_string(), json!(thread_id.to_string()));
        session_store.save(&session).unwrap();

        let server = Arc::new(LibraMcpServer::new_with_working_dir(
            None,
            None,
            working_dir,
        ));

        let mirror = session_jsonl_store_for_thread(&server, thread_id)
            .expect("matching session mirror store");
        assert_eq!(
            mirror.events_path(),
            session_store.session_root(&session.id).join("events.jsonl")
        );
    }

    #[tokio::test]
    async fn test_persist_execution_creates_object_chain() {
        let server = setup_server().await;
        let spec = test_spec(vec![ChangeLogEntry {
            at: "2025-01-01T00:01:00Z".into(),
            by: "libra-orchestrator".into(),
            reason: "security gate failed".into(),
            diff_summary: "revision 2: replan in serial mode".into(),
        }]);
        let impl_task = {
            let actor = ActorRef::agent("test-persistence").unwrap();
            GitTask::new(actor, "Edit source", None).unwrap()
        };
        let impl_task_id = impl_task.header().object_id();
        let gate_task = {
            let actor = ActorRef::agent("test-persistence").unwrap();
            let mut task = GitTask::new(actor, "Run fast checks", None).unwrap();
            task.add_dependency(impl_task_id);
            task
        };
        let gate_task_id = gate_task.header().object_id();
        let plan_spec = ExecutionPlanSpec {
            intent_spec_id: "intent-1".to_string(),
            revision: 1,
            parent_revision: None,
            replan_reason: None,
            tasks: vec![
                TaskSpec {
                    step: git_internal::internal::object::plan::PlanStep::new("Edit source"),
                    task: impl_task,
                    objective: "Update src/lib.rs".to_string(),
                    kind: TaskKind::Implementation,
                    gate_stage: None,
                    owner_role: Some("coder".to_string()),
                    scope_in: vec!["src/".to_string()],
                    scope_out: vec![],
                    checks: vec![],
                    contract: TaskContract::default(),
                },
                TaskSpec {
                    step: git_internal::internal::object::plan::PlanStep::new("Run fast checks"),
                    task: gate_task,
                    objective: "Verify".to_string(),
                    kind: TaskKind::Gate,
                    gate_stage: Some(GateStage::Fast),
                    owner_role: Some("verifier".to_string()),
                    scope_in: vec![],
                    scope_out: vec![],
                    checks: vec![],
                    contract: TaskContract::default(),
                },
            ],
            max_parallel: 1,
            checkpoints: vec![ExecutionCheckpoint {
                label: "after-fast".to_string(),
                after_tasks: vec![gate_task_id],
                reason: "verify".to_string(),
            }],
        };
        let results = vec![
            TaskResult {
                task_id: impl_task_id,
                status: TaskNodeStatus::Completed,
                gate_report: None,
                agent_output: Some("done".to_string()),
                retry_count: 0,
                tool_calls: vec![ToolCallRecord {
                    tool_name: "apply_patch".to_string(),
                    action: "write".to_string(),
                    arguments_json: Some(json!({"input": "*** Begin Patch"})),
                    paths_read: vec![],
                    paths_written: vec!["src/lib.rs".to_string()],
                    success: true,
                    summary: Some("updated src/lib.rs".to_string()),
                    diffs: vec![ToolDiffRecord {
                        path: "src/lib.rs".to_string(),
                        change_type: "modify".to_string(),
                        diff: "--- a/src/lib.rs\n+++ b/src/lib.rs\n+fn added() {}\n".to_string(),
                    }],
                }],
                policy_violations: vec![],
                model_usage: None,
                review: None,
                thinking: None,
            },
            TaskResult {
                task_id: gate_task_id,
                status: TaskNodeStatus::Completed,
                gate_report: Some(GateReport {
                    results: vec![GateResult {
                        check_id: "cargo-test".to_string(),
                        kind: "test".to_string(),
                        passed: true,
                        exit_code: 0,
                        stdout: String::new(),
                        stderr: String::new(),
                        duration_ms: 10,
                        timed_out: false,
                    }],
                    all_required_passed: true,
                }),
                agent_output: None,
                retry_count: 0,
                tool_calls: vec![],
                policy_violations: vec![],
                model_usage: None,
                review: None,
                thinking: None,
            },
        ];
        let system_report = SystemReport {
            integration: GateReport::empty(),
            security: GateReport::empty(),
            release: GateReport::empty(),
            review_passed: true,
            review_findings: vec![],
            artifacts_complete: true,
            missing_artifacts: vec![],
            overall_passed: true,
        };
        let run_state = RunStateSnapshot {
            intent_spec_id: plan_spec.intent_spec_id.clone(),
            revision: plan_spec.revision,
            task_statuses: results
                .iter()
                .map(|result| TaskStatusSnapshot {
                    task_id: result.task_id,
                    status: result.status.clone(),
                })
                .collect(),
            task_results: results.clone(),
            dagrs_runtime: Default::default(),
        };

        let persisted = persist_execution(ExecutionPersistenceRequest {
            mcp_server: &server,
            spec: &spec,
            execution_plan_spec: &plan_spec,
            plan_revision_specs: std::slice::from_ref(&plan_spec),
            run_state: &run_state,
            system_report: &system_report,
            decision: &DecisionOutcome::Commit,
            working_dir: Path::new("."),
            base_commit: Some(ZERO_COMMIT_SHA),
            model_name: "test-model",
        })
        .await
        .unwrap();

        assert!(!persisted.run_id.is_empty());
        assert!(persisted.initial_snapshot_id.is_some());
        assert!(persisted.provenance_id.is_some());
        assert!(persisted.decision_id.is_some());
        assert_eq!(persisted.plan_ids.len(), 2);
        assert_eq!(persisted.checkpoints.len(), 1);
        assert_eq!(persisted.tasks.len(), 2);
        assert!(
            persisted
                .tasks
                .iter()
                .all(|task| task.persisted_task_id.is_some())
        );
        assert_eq!(persisted.tasks[0].tool_invocation_ids.len(), 1);
        assert!(persisted.tasks[0].patchset_id.is_some());
        assert_eq!(persisted.tasks[1].evidence_ids.len(), 1);
        let derived = persisted
            .derived_records
            .as_ref()
            .expect("validation decision derived records");

        let history = server.intent_history_manager.as_ref().unwrap();
        let db = history.database_connection();
        assert!(
            ai_validation_report::Entity::find_by_id(derived.validation_report_id.to_string())
                .one(&db)
                .await
                .unwrap()
                .is_some()
        );
        let latest_report = ValidationReportStore::new(db.clone())
            .load_latest(parse_object_id(persisted.thread_id.as_deref().unwrap()).unwrap())
            .await
            .unwrap()
            .expect("latest validation report");
        let release_stage = latest_report
            .summary
            .stages
            .iter()
            .find(|stage| stage.stage == ValidationStage::Release)
            .expect("release validation stage");
        assert_eq!(release_stage.outcome, ValidationOutcome::Passed);
        assert!(
            ai_risk_score_breakdown::Entity::find_by_id(
                derived.risk_score_breakdown_id.to_string()
            )
            .one(&db)
            .await
            .unwrap()
            .is_some()
        );
        assert!(
            ai_decision_proposal::Entity::find_by_id(derived.decision_proposal_id.to_string())
                .one(&db)
                .await
                .unwrap()
                .is_some()
        );
        assert_eq!(history.list_objects("task").await.unwrap().len(), 3);
        assert_eq!(history.list_objects("run").await.unwrap().len(), 3);
        assert_eq!(history.list_objects("plan").await.unwrap().len(), 2);
        assert_eq!(history.list_objects("patchset").await.unwrap().len(), 1);
        assert_eq!(history.list_objects("evidence").await.unwrap().len(), 1);
        assert_eq!(history.list_objects("decision").await.unwrap().len(), 2);
        assert_eq!(history.list_objects("provenance").await.unwrap().len(), 1);
        assert_eq!(history.list_objects("invocation").await.unwrap().len(), 1);
        assert_eq!(
            history.list_objects("plan_step_event").await.unwrap().len(),
            2
        );
        assert_eq!(history.list_objects("snapshot").await.unwrap().len(), 2);

        let storage = server.storage.as_ref().unwrap();
        let intent_id =
            parse_object_id(persisted.thread_id.as_deref().expect("persisted thread id")).unwrap();
        let mut saw_terminal_intent_event = false;
        for (_, hash) in history.list_objects("intent_event").await.unwrap() {
            let event = storage.get_json::<IntentEvent>(&hash).await.unwrap();
            if event.intent_id() == intent_id && event.kind() == &IntentEventKind::Completed {
                saw_terminal_intent_event = true;
                break;
            }
        }
        assert!(saw_terminal_intent_event);

        let mut persisted_step_ids = std::collections::BTreeSet::new();
        for plan_id in &persisted.plan_ids {
            let plan_hash = history
                .get_object_hash("plan", &parse_object_id(plan_id).unwrap().to_string())
                .await
                .unwrap()
                .unwrap();
            let persisted_plan = storage.get_json::<GitPlan>(&plan_hash).await.unwrap();
            persisted_step_ids.extend(persisted_plan.steps().iter().map(|step| step.step_id()));
        }

        for task_artifacts in &persisted.tasks {
            let persisted_task_id = task_artifacts.persisted_task_id.as_ref().unwrap();
            let task_hash = history
                .get_object_hash(
                    "task",
                    &parse_object_id(persisted_task_id).unwrap().to_string(),
                )
                .await
                .unwrap()
                .unwrap();
            let persisted_task = storage.get_json::<GitTask>(&task_hash).await.unwrap();
            assert!(persisted_step_ids.contains(&persisted_task.origin_step_id().unwrap()));
        }
    }

    #[tokio::test]
    async fn execution_audit_session_persists_runtime_side_objects() {
        let server = setup_server().await;
        let spec = test_spec(vec![]);
        let impl_task = {
            let actor = ActorRef::agent("test-audit").unwrap();
            GitTask::new(actor, "Edit source", None).unwrap()
        };
        let impl_task_id = impl_task.header().object_id();
        let plan_spec = ExecutionPlanSpec {
            intent_spec_id: "intent-1".to_string(),
            revision: 1,
            parent_revision: None,
            replan_reason: None,
            tasks: vec![TaskSpec {
                step: git_internal::internal::object::plan::PlanStep::new("Edit source"),
                task: impl_task,
                objective: "Update src/lib.rs".to_string(),
                kind: TaskKind::Implementation,
                gate_stage: None,
                owner_role: Some("coder".to_string()),
                scope_in: vec!["src/".to_string()],
                scope_out: vec![],
                checks: vec![],
                contract: TaskContract::default(),
            }],
            max_parallel: 1,
            checkpoints: vec![],
        };
        let session =
            ExecutionAuditSession::start(server.clone(), &spec, Path::new("."), None, None, None)
                .await
                .unwrap();
        session.record_plan_compiled(&plan_spec).await.unwrap();
        let observer = session.observer();
        observer.on_task_runtime_event(
            &plan_spec.tasks[0],
            TaskRuntimeEvent::Phase(TaskRuntimePhase::Starting),
        );
        let raw_tail = "UNSTORED_RAW_ASSISTANT_TAIL";
        let long_assistant_message = format!(
            "Editing src/lib.rs to fix the issue. {} {raw_tail}",
            "x".repeat(320)
        );
        observer.on_task_runtime_event(
            &plan_spec.tasks[0],
            TaskRuntimeEvent::AssistantMessage(long_assistant_message),
        );
        for index in 0..25 {
            observer.on_task_runtime_event(
                &plan_spec.tasks[0],
                TaskRuntimeEvent::ThinkingDelta(format!("reasoning delta {index}")),
            );
        }
        observer.on_task_runtime_event(
            &plan_spec.tasks[0],
            TaskRuntimeEvent::ToolCallBegin {
                call_id: "call-1".to_string(),
                tool_name: "apply_patch".to_string(),
                arguments: json!({"path":"src/lib.rs"}),
            },
        );
        observer.on_task_runtime_event(
            &plan_spec.tasks[0],
            TaskRuntimeEvent::ToolCallEnd {
                call_id: "call-1".to_string(),
                tool_name: "apply_patch".to_string(),
                result: Ok(crate::internal::ai::tools::ToolOutput::success("patched")),
            },
        );
        observer.on_task_runtime_event(
            &plan_spec.tasks[0],
            TaskRuntimeEvent::ToolCallBegin {
                call_id: "call-2".to_string(),
                tool_name: "shell".to_string(),
                arguments: json!({"command":"true"}),
            },
        );
        observer.on_task_runtime_event(
            &plan_spec.tasks[0],
            TaskRuntimeEvent::ToolCallEnd {
                call_id: "call-2".to_string(),
                tool_name: "shell".to_string(),
                result: Ok(crate::internal::ai::tools::ToolOutput::failure(
                    "sandbox rejected command",
                )
                .with_metadata(json!({
                    "sandbox_evidence": [{
                        "kind": "writable_root_rejected",
                        "root": "/",
                        "reason": "dangerous writable root",
                    }]
                }))),
            },
        );
        let results = vec![TaskResult {
            task_id: impl_task_id,
            status: TaskNodeStatus::Completed,
            gate_report: None,
            agent_output: Some("done".to_string()),
            retry_count: 0,
            tool_calls: vec![ToolCallRecord {
                tool_name: "apply_patch".to_string(),
                action: "write".to_string(),
                arguments_json: Some(json!({"input":"*** Begin Patch"})),
                paths_read: vec![],
                paths_written: vec!["src/lib.rs".to_string()],
                success: true,
                summary: Some("updated src/lib.rs".to_string()),
                diffs: vec![ToolDiffRecord {
                    path: "src/lib.rs".to_string(),
                    change_type: "modify".to_string(),
                    diff: "--- a/src/lib.rs\n+++ b/src/lib.rs\n+fn added() {}\n".to_string(),
                }],
            }],
            policy_violations: vec![],
            model_usage: Some(crate::internal::ai::completion::CompletionUsageSummary {
                input_tokens: 10,
                output_tokens: 5,
                cached_tokens: None,
                reasoning_tokens: None,
                total_tokens: Some(15),
                cost_usd: None,
            }),
            review: None,
            thinking: None,
        }];
        let system_report = SystemReport {
            integration: GateReport::empty(),
            security: GateReport::empty(),
            release: GateReport::empty(),
            review_passed: true,
            review_findings: vec![],
            artifacts_complete: true,
            missing_artifacts: vec![],
            overall_passed: true,
        };
        let run_state = RunStateSnapshot {
            intent_spec_id: plan_spec.intent_spec_id.clone(),
            revision: plan_spec.revision,
            task_statuses: vec![TaskStatusSnapshot {
                task_id: impl_task_id,
                status: TaskNodeStatus::Completed,
            }],
            task_results: results.clone(),
            dagrs_runtime: Default::default(),
        };
        let persisted = session
            .finalize(ExecutionFinalizeRequest {
                spec: &spec,
                execution_plan_spec: &plan_spec,
                plan_revision_specs: std::slice::from_ref(&plan_spec),
                run_state: &run_state,
                system_report: &system_report,
                decision: &DecisionOutcome::Commit,
                working_dir: Path::new("."),
                model_name: "test-model",
            })
            .await
            .unwrap();
        assert!(!persisted.run_id.is_empty());
        assert!(persisted.provenance_id.is_some());
        assert!(persisted.run_usage_id.is_some());
        assert!(persisted.decision_id.is_some());
        assert!(persisted.derived_records.is_some());
        assert_eq!(persisted.tasks.len(), 1);
        let task_artifacts = &persisted.tasks[0];
        assert_eq!(task_artifacts.task_id, impl_task_id);
        assert!(task_artifacts.persisted_task_id.is_some());
        assert_eq!(task_artifacts.tool_invocation_ids.len(), 1);
        assert!(task_artifacts.patchset_id.is_some());

        let history = server.intent_history_manager.as_ref().unwrap();
        let storage = server.storage.as_ref().unwrap();
        for (object_type, object_id) in [
            ("run", persisted.run_id.as_str()),
            ("provenance", persisted.provenance_id.as_deref().unwrap()),
            ("run_usage", persisted.run_usage_id.as_deref().unwrap()),
            ("decision", persisted.decision_id.as_deref().unwrap()),
            ("patchset", task_artifacts.patchset_id.as_deref().unwrap()),
            ("invocation", task_artifacts.tool_invocation_ids[0].as_str()),
        ] {
            assert!(
                history
                    .get_object_hash(
                        object_type,
                        &parse_object_id(object_id).unwrap().to_string()
                    )
                    .await
                    .unwrap()
                    .is_some(),
                "expected persisted {object_type} id {object_id} to resolve in history",
            );
        }
        let db = history.database_connection();
        let intent_id =
            parse_object_id(persisted.thread_id.as_deref().expect("persisted thread id")).unwrap();
        let derived = persisted
            .derived_records
            .as_ref()
            .expect("validation decision derived records");
        let latest_proposal = DecisionProposalStore::new(db.clone())
            .load_latest_proposal(intent_id)
            .await
            .unwrap()
            .expect("latest decision proposal");
        assert_eq!(latest_proposal.proposal_id, derived.decision_proposal_id);
        assert_eq!(
            latest_proposal.summary.route,
            DecisionProposalRoute::AutoAccept
        );
        assert_eq!(
            latest_proposal.summary.proposed_verdict,
            FinalDecisionVerdict::Accepted
        );
        assert!(!latest_proposal.summary.requires_human_review);
        assert!(
            !ai_scheduler_selected_plan::Entity::find()
                .all(&db)
                .await
                .unwrap()
                .is_empty()
        );
        assert!(
            !ai_scheduler_plan_head::Entity::find()
                .all(&db)
                .await
                .unwrap()
                .is_empty()
        );
        assert!(
            !ai_index_intent_plan::Entity::find()
                .all(&db)
                .await
                .unwrap()
                .is_empty()
        );
        assert!(
            !ai_index_plan_step_task::Entity::find()
                .all(&db)
                .await
                .unwrap()
                .is_empty()
        );
        assert!(
            !ai_index_task_run::Entity::find()
                .all(&db)
                .await
                .unwrap()
                .is_empty()
        );
        assert!(
            !ai_index_run_event::Entity::find()
                .all(&db)
                .await
                .unwrap()
                .is_empty()
        );
        assert!(
            !ai_index_run_patchset::Entity::find()
                .all(&db)
                .await
                .unwrap()
                .is_empty()
        );
        assert_eq!(
            history.list_objects("plan_snapshot").await.unwrap().len(),
            2
        );
        assert_eq!(
            history.list_objects("task_snapshot").await.unwrap().len(),
            1
        );
        assert_eq!(history.list_objects("run_snapshot").await.unwrap().len(), 2);
        assert_eq!(
            history
                .list_objects("patchset_snapshot")
                .await
                .unwrap()
                .len(),
            1
        );
        assert_eq!(
            history
                .list_objects("provenance_snapshot")
                .await
                .unwrap()
                .len(),
            1
        );
        assert_eq!(history.list_objects("run_usage").await.unwrap().len(), 1);
        let mut saw_terminal_intent_event = false;
        for (_, hash) in history.list_objects("intent_event").await.unwrap() {
            let event = storage.get_json::<IntentEvent>(&hash).await.unwrap();
            if event.intent_id() == intent_id && event.kind() == &IntentEventKind::Completed {
                saw_terminal_intent_event = true;
                break;
            }
        }
        assert!(saw_terminal_intent_event);
        assert!(
            !history
                .list_objects("context_frame")
                .await
                .unwrap()
                .is_empty()
        );
        let context_frames = history.list_objects("context_frame").await.unwrap();
        let persisted_plan = load_persisted_plan(&server, &persisted.plan_ids[0])
            .await
            .unwrap();
        let expected_step_id = persisted_plan.steps()[0].step_id();
        let expected_task_id = parse_object_id(
            persisted.tasks[0]
                .persisted_task_id
                .as_deref()
                .expect("persisted task id"),
        )
        .unwrap();
        let mut saw_terminal_step_event = false;
        for (_, hash) in history.list_objects("plan_step_event").await.unwrap() {
            let event = storage.get_json::<PlanStepEvent>(&hash).await.unwrap();
            if event.step_id() == expected_step_id
                && event.spawned_task_id() == Some(expected_task_id)
                && event.status() == &PlanStepStatus::Completed
            {
                saw_terminal_step_event = true;
                break;
            }
        }
        assert!(saw_terminal_step_event);
        let mut assistant_context_frame = None;
        let mut thinking_context_frame_count = 0;
        for (_, hash) in context_frames {
            let value = storage.get_json::<serde_json::Value>(&hash).await.unwrap();
            let serialized = serde_json::to_string(&value).unwrap();
            if serialized.contains("\"thinking_delta\"") {
                thinking_context_frame_count += 1;
            }
            if serialized.contains("\"assistant_message\"") {
                assistant_context_frame = Some(serialized);
            }
        }
        assert_eq!(thinking_context_frame_count, 0);
        let assistant_context_frame =
            assistant_context_frame.expect("expected assistant message context frame");
        assert!(assistant_context_frame.contains("\"fullTextStored\":false"));
        assert!(assistant_context_frame.contains("\"contentChars\""));
        assert!(!assistant_context_frame.contains(raw_tail));
        let tool_invocation_events = history.list_objects("tool_invocation_event").await.unwrap();
        assert!(tool_invocation_events.len() >= 2);
        let mut saw_failed_shell_invocation = false;
        for (_, hash) in &tool_invocation_events {
            let value = storage.get_json::<serde_json::Value>(hash).await.unwrap();
            if value["payload"]["call_id"] == "call-2" && value["status"] == "failed" {
                assert_eq!(value["tool"], "shell");
                saw_failed_shell_invocation = true;
            }
        }
        assert!(saw_failed_shell_invocation);
        let mut saw_sandbox_evidence = false;
        for (_, hash) in history.list_objects("evidence").await.unwrap() {
            let value = storage.get_json::<serde_json::Value>(&hash).await.unwrap();
            if value.get("kind").and_then(|kind| kind.as_str()) == Some("sandbox") {
                assert_eq!(value["data"]["tool"], "shell");
                assert_eq!(value["data"]["call_id"], "call-2");
                assert_eq!(value["data"]["event"]["kind"], "writable_root_rejected");
                saw_sandbox_evidence = true;
            }
        }
        assert!(saw_sandbox_evidence);
        assert!(history.list_objects("run_event").await.unwrap().len() >= 2);
        assert!(history.list_objects("task_event").await.unwrap().len() >= 4);
    }

    #[tokio::test]
    async fn phase2_session_bridge_persists_attempt_lifecycle_events() {
        use crate::internal::ai::runtime::{
            contracts::TaskExecutionStatus,
            phase2::{write_attempt_finish_with_session, write_attempt_start_with_session},
        };

        let server = setup_server().await;
        let spec = test_spec(vec![]);
        let impl_task = {
            let actor = ActorRef::agent("test-phase2-bridge").unwrap();
            GitTask::new(actor, "Bridge runtime attempt", None).unwrap()
        };
        let logical_task_id = impl_task.header().object_id();
        let plan_spec = ExecutionPlanSpec {
            intent_spec_id: "intent-1".to_string(),
            revision: 1,
            parent_revision: None,
            replan_reason: None,
            tasks: vec![TaskSpec {
                step: git_internal::internal::object::plan::PlanStep::new("Bridge runtime attempt"),
                task: impl_task,
                objective: "Persist a stateful attempt lifecycle".to_string(),
                kind: TaskKind::Implementation,
                gate_stage: None,
                owner_role: Some("coder".to_string()),
                scope_in: vec!["src/".to_string()],
                scope_out: vec![],
                checks: vec![],
                contract: TaskContract::default(),
            }],
            max_parallel: 1,
            checkpoints: vec![],
        };
        let task = &plan_spec.tasks[0];
        let session =
            ExecutionAuditSession::start(server.clone(), &spec, Path::new("."), None, None, None)
                .await
                .unwrap();
        session.record_plan_compiled(&plan_spec).await.unwrap();

        let start = write_attempt_start_with_session(
            &session,
            task,
            "test-model",
            Some("first attempt".to_string()),
        )
        .await
        .unwrap();
        assert_eq!(start.task_id, logical_task_id);
        assert_eq!(start.status, TaskExecutionStatus::Interrupted);
        assert!(start.is_failure());
        assert!(!start.is_terminal());

        let finish = write_attempt_finish_with_session(
            &session,
            task,
            TaskExecutionStatus::Completed,
            Some("attempt completed".to_string()),
        )
        .await
        .unwrap();
        assert_eq!(finish.task_id, logical_task_id);
        assert_eq!(finish.run_id, start.run_id);
        assert_eq!(finish.status, TaskExecutionStatus::Completed);
        assert!(!finish.is_failure());
        assert!(finish.is_terminal());

        let history = server.intent_history_manager.as_ref().unwrap();
        let storage = server.storage.as_ref().unwrap();

        let mut run_event_kinds = Vec::new();
        for (_, hash) in history.list_objects("run_event").await.unwrap() {
            let event = storage.get_json::<RunEvent>(&hash).await.unwrap();
            if event.run_id() == start.run_id {
                run_event_kinds.push(event.kind().clone());
            }
        }
        assert!(run_event_kinds.contains(&RunEventKind::Patching));
        assert!(run_event_kinds.contains(&RunEventKind::Completed));

        let mut saw_running_task_event = false;
        let mut saw_done_task_event = false;
        for (_, hash) in history.list_objects("task_event").await.unwrap() {
            let event = storage.get_json::<TaskEvent>(&hash).await.unwrap();
            if event.run_id() != Some(start.run_id) {
                continue;
            }
            saw_running_task_event |= event.kind() == &TaskEventKind::Running;
            saw_done_task_event |= event.kind() == &TaskEventKind::Done;
        }
        assert!(saw_running_task_event);
        assert!(saw_done_task_event);

        let mut saw_progressing_step_event = false;
        let mut saw_completed_step_event = false;
        for (_, hash) in history.list_objects("plan_step_event").await.unwrap() {
            let event = storage.get_json::<PlanStepEvent>(&hash).await.unwrap();
            if event.run_id() != start.run_id {
                continue;
            }
            saw_progressing_step_event |= event.status() == &PlanStepStatus::Progressing;
            saw_completed_step_event |= event.status() == &PlanStepStatus::Completed;
        }
        assert!(saw_progressing_step_event);
        assert!(saw_completed_step_event);
    }

    #[test]
    fn phase2_attempt_status_mappings_cover_terminal_variants() {
        use crate::internal::ai::runtime::contracts::TaskExecutionStatus;

        let cases = [
            (
                TaskExecutionStatus::Completed,
                TaskEventKind::Done,
                "completed",
                RunEventKind::Completed,
            ),
            (
                TaskExecutionStatus::Failed,
                TaskEventKind::Failed,
                "failed",
                RunEventKind::Failed,
            ),
            (
                TaskExecutionStatus::Cancelled,
                TaskEventKind::Cancelled,
                "skipped",
                RunEventKind::Failed,
            ),
            (
                TaskExecutionStatus::TimedOut,
                TaskEventKind::Failed,
                "failed",
                RunEventKind::Failed,
            ),
            (
                TaskExecutionStatus::Interrupted,
                TaskEventKind::Failed,
                "failed",
                RunEventKind::Failed,
            ),
        ];

        for (status, task_kind, plan_status, run_kind) in cases {
            assert_eq!(task_event_kind_for_attempt_status(&status), task_kind);
            assert_eq!(plan_step_status_for_attempt_status(&status), plan_status);
            assert_eq!(run_event_kind_for_attempt_status(&status), run_kind);
        }
    }

    #[tokio::test]
    async fn execution_audit_session_reuses_preview_intent_and_plan_chain() {
        let server = setup_server().await;
        let spec = test_spec(vec![]);
        let analysis_task = {
            let actor = ActorRef::agent("test-preview-reuse").unwrap();
            GitTask::new(actor, "Analyze repository", None).unwrap()
        };
        let analysis_task_id = analysis_task.header().object_id();
        let plan_spec = ExecutionPlanSpec {
            intent_spec_id: "intent-preview".to_string(),
            revision: 1,
            parent_revision: None,
            replan_reason: None,
            tasks: vec![TaskSpec {
                step: git_internal::internal::object::plan::PlanStep::new("Analyze repository"),
                task: analysis_task,
                objective: "Summarize repository state".to_string(),
                kind: TaskKind::Analysis,
                gate_stage: None,
                owner_role: Some("analyst".to_string()),
                scope_in: vec!["src/".to_string()],
                scope_out: vec![],
                checks: vec![],
                contract: TaskContract::default(),
            }],
            max_parallel: 1,
            checkpoints: vec![],
        };
        let intent_id = persist_intentspec(&spec, &server).await.unwrap();
        let preview_plan = create_plan_revision(&server, &intent_id, None, &plan_spec)
            .await
            .unwrap();
        let session = ExecutionAuditSession::start(
            server.clone(),
            &spec,
            Path::new("."),
            Some(&intent_id),
            None,
            Some(&preview_plan.plan_id),
        )
        .await
        .unwrap();
        session.record_plan_compiled(&plan_spec).await.unwrap();
        let results = vec![TaskResult {
            task_id: analysis_task_id,
            status: TaskNodeStatus::Completed,
            gate_report: None,
            agent_output: Some("analysis complete".to_string()),
            retry_count: 0,
            tool_calls: vec![],
            policy_violations: vec![],
            model_usage: None,
            review: None,
            thinking: None,
        }];
        let run_state = RunStateSnapshot {
            intent_spec_id: plan_spec.intent_spec_id.clone(),
            revision: plan_spec.revision,
            task_statuses: vec![TaskStatusSnapshot {
                task_id: analysis_task_id,
                status: TaskNodeStatus::Completed,
            }],
            task_results: results.clone(),
            dagrs_runtime: Default::default(),
        };
        let system_report = SystemReport {
            integration: GateReport::empty(),
            security: GateReport::empty(),
            release: GateReport::empty(),
            review_passed: true,
            review_findings: vec![],
            artifacts_complete: true,
            missing_artifacts: vec![],
            overall_passed: true,
        };

        let persisted = session
            .finalize(ExecutionFinalizeRequest {
                spec: &spec,
                execution_plan_spec: &plan_spec,
                plan_revision_specs: std::slice::from_ref(&plan_spec),
                run_state: &run_state,
                system_report: &system_report,
                decision: &DecisionOutcome::Commit,
                working_dir: Path::new("."),
                model_name: "test-model",
            })
            .await
            .unwrap();

        assert_eq!(persisted.plan_ids.len(), 2);
        assert_eq!(persisted.plan_ids[0], preview_plan.plan_id);
        assert_eq!(persisted.run_usage_id, None);

        let history = server.intent_history_manager.as_ref().unwrap();
        assert_eq!(history.list_objects("intent").await.unwrap().len(), 1);
        assert_eq!(history.list_objects("plan").await.unwrap().len(), 2);
    }

    #[tokio::test]
    async fn review_bundle_persists_plan_tasks_and_execution_reuses_tasks() {
        let server = setup_server().await;
        let spec = test_spec(vec![]);
        let first_task = {
            let actor = ActorRef::agent("test-review-bundle").unwrap();
            GitTask::new(actor, "Inspect implementation", None).unwrap()
        };
        let second_task = {
            let actor = ActorRef::agent("test-review-bundle").unwrap();
            GitTask::new(actor, "Run regression checks", None).unwrap()
        };
        let plan_spec = ExecutionPlanSpec {
            intent_spec_id: "intent-review-bundle".to_string(),
            revision: 1,
            parent_revision: None,
            replan_reason: None,
            tasks: vec![
                TaskSpec {
                    step: git_internal::internal::object::plan::PlanStep::new(
                        "Inspect implementation",
                    ),
                    task: first_task,
                    objective: "Inspect implementation".to_string(),
                    kind: TaskKind::Analysis,
                    gate_stage: None,
                    owner_role: Some("analyst".to_string()),
                    scope_in: vec!["src/".to_string()],
                    scope_out: vec![],
                    checks: vec![],
                    contract: TaskContract::default(),
                },
                TaskSpec {
                    step: git_internal::internal::object::plan::PlanStep::new(
                        "Run regression checks",
                    ),
                    task: second_task,
                    objective: "Run regression checks".to_string(),
                    kind: TaskKind::Gate,
                    gate_stage: Some(GateStage::Integration),
                    owner_role: Some("tester".to_string()),
                    scope_in: vec!["tests/".to_string()],
                    scope_out: vec![],
                    checks: vec![],
                    contract: TaskContract::default(),
                },
            ],
            max_parallel: 1,
            checkpoints: vec![],
        };
        let intent_id = persist_intentspec(&spec, &server).await.unwrap();

        let bundle = persist_plan_review_bundle(&server, &intent_id, &plan_spec)
            .await
            .unwrap();

        assert_eq!(bundle.step_ids.len(), plan_spec.tasks.len());
        assert_eq!(bundle.task_ids.len(), plan_spec.tasks.len());
        assert_ne!(
            bundle.plan_id, bundle.test_plan_id,
            "Phase 1 review must persist distinct execution/test plans"
        );
        assert_eq!(
            bundle
                .plan_id_by_task_id
                .get(&plan_spec.tasks[0].id())
                .map(String::as_str),
            Some(bundle.plan_id.as_str())
        );
        assert_eq!(
            bundle
                .plan_id_by_task_id
                .get(&plan_spec.tasks[1].id())
                .map(String::as_str),
            Some(bundle.test_plan_id.as_str())
        );
        let history = server.intent_history_manager.as_ref().unwrap();
        assert_eq!(history.list_objects("plan").await.unwrap().len(), 2);
        assert_eq!(
            history.list_objects("task").await.unwrap().len(),
            plan_spec.tasks.len()
        );

        let storage = server.storage.as_ref().unwrap();
        for task in &plan_spec.tasks {
            let persisted_task_id = bundle.task_ids.get(&task.id()).unwrap();
            let task_hash = history
                .get_object_hash(
                    "task",
                    &parse_object_id(persisted_task_id).unwrap().to_string(),
                )
                .await
                .unwrap()
                .unwrap();
            let persisted_task = storage.get_json::<GitTask>(&task_hash).await.unwrap();
            assert_eq!(
                persisted_task.origin_step_id(),
                bundle.step_ids.get(&task.step_id()).copied()
            );
        }

        let session = ExecutionAuditSession::start(
            server.clone(),
            &spec,
            Path::new("."),
            Some(&intent_id),
            Some(bundle),
            None,
        )
        .await
        .unwrap();
        session.record_plan_compiled(&plan_spec).await.unwrap();

        assert_eq!(history.list_objects("plan").await.unwrap().len(), 2);
        assert_eq!(
            history.list_objects("task").await.unwrap().len(),
            plan_spec.tasks.len() + 1
        );
    }

    #[tokio::test]
    async fn execution_audit_session_creates_new_plan_when_preview_plan_is_missing() {
        let server = setup_server().await;
        let spec = test_spec(vec![]);
        let analysis_task = {
            let actor = ActorRef::agent("test-missing-preview").unwrap();
            GitTask::new(actor, "Analyze repository", None).unwrap()
        };
        let plan_spec = ExecutionPlanSpec {
            intent_spec_id: "intent-preview".to_string(),
            revision: 1,
            parent_revision: None,
            replan_reason: None,
            tasks: vec![TaskSpec {
                step: git_internal::internal::object::plan::PlanStep::new("Analyze repository"),
                task: analysis_task,
                objective: "Summarize repository state".to_string(),
                kind: TaskKind::Analysis,
                gate_stage: None,
                owner_role: Some("analyst".to_string()),
                scope_in: vec!["src/".to_string()],
                scope_out: vec![],
                checks: vec![],
                contract: TaskContract::default(),
            }],
            max_parallel: 1,
            checkpoints: vec![],
        };
        let intent_id = persist_intentspec(&spec, &server).await.unwrap();
        let missing_plan_id = Uuid::new_v4().to_string();
        let session = ExecutionAuditSession::start(
            server.clone(),
            &spec,
            Path::new("."),
            Some(&intent_id),
            None,
            Some(&missing_plan_id),
        )
        .await
        .unwrap();

        session.record_plan_compiled(&plan_spec).await.unwrap();

        let history = server.intent_history_manager.as_ref().unwrap();
        assert_eq!(history.list_objects("plan").await.unwrap().len(), 2);
        assert!(
            history
                .get_object_hash("plan", &missing_plan_id)
                .await
                .unwrap()
                .is_none()
        );
    }
}
