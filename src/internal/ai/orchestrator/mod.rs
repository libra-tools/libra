//! AI orchestrator subsystem.
//!
//! Boundary: this layer coordinates planning, workspace preparation, execution,
//! verification, decisions, checkpointing, and persistence for formal AI runs. Provider
//! adapters and raw completion APIs stay outside this module. End-to-end coverage lives
//! in the AI runtime, scheduler, storage-flow, and validation-decision tests.

pub mod acl;
mod checkpoint_policy;
pub mod decider;
pub mod executor;
pub mod gate;
pub mod persistence;
pub mod planner;
pub mod policy;
pub mod replan;
pub mod run_state;
pub mod types;
pub mod verifier;
pub(crate) mod workspace;

// SAFETY: The unwrap() and expect() calls in this module are documented with safety
// justifications where used. Test code uses unwrap for test assertions. Production code
// uses unwrap/expect only when invariants are guaranteed by the code structure.

use std::sync::Arc;

use types::{
    OrchestratorConfig, OrchestratorError, OrchestratorResult, PhaseConfirmationDecision,
    PhaseConfirmationPrompt,
};
use uuid::Uuid;

use crate::internal::ai::{
    completion::{CompletionModel, CompletionUsage, ThrottledCompletionModel},
    intentspec::{repair_intentspec, types::IntentSpec, validate_intentspec},
    tools::registry::ToolRegistry,
};

/// The main orchestrator that drives IntentSpec execution through all phases.
///
/// Phases:
/// 0. Validate + repair IntentSpec
/// 1. Compile an execution plan
/// 2. Execute plan tasks with retry and policy enforcement
/// 3. Build system verification report from gate tasks
/// 4. Make decision
pub struct Orchestrator<M: CompletionModel> {
    model: M,
    registry: Arc<ToolRegistry>,
    config: OrchestratorConfig,
    /// Process-scoped FUSE provisioning gate. Persisted across `run()` calls so
    /// that once a task fails to mount FUSE, every subsequent orchestrator
    /// invocation in the same process skips FUSE without re-paying the
    /// mount-and-fail cost or emitting another fallback warning.
    fuse_state: workspace::FuseProvisionState,
}

struct FanoutObserver {
    observers: Vec<Arc<dyn types::OrchestratorObserver>>,
}

impl types::OrchestratorObserver for FanoutObserver {
    fn on_plan_compiled(&self, plan: &types::ExecutionPlanSpec) {
        for observer in &self.observers {
            observer.on_plan_compiled(plan);
        }
    }

    fn on_task_runtime_event(&self, task: &types::TaskSpec, event: types::TaskRuntimeEvent) {
        for observer in &self.observers {
            observer.on_task_runtime_event(task, event.clone());
        }
    }

    fn on_graph_progress(&self, completed: usize, total: usize) {
        for observer in &self.observers {
            observer.on_graph_progress(completed, total);
        }
    }

    fn on_graph_checkpoint_saved(&self, checkpoint_id: &str, pc: usize, completed_nodes: usize) {
        for observer in &self.observers {
            observer.on_graph_checkpoint_saved(checkpoint_id, pc, completed_nodes);
        }
    }

    fn on_graph_checkpoint_restored(&self, checkpoint_id: &str, pc: usize) {
        for observer in &self.observers {
            observer.on_graph_checkpoint_restored(checkpoint_id, pc);
        }
    }

    fn on_system_verification(
        &self,
        plan: &types::ExecutionPlanSpec,
        report: &types::SystemReport,
    ) {
        for observer in &self.observers {
            observer.on_system_verification(plan, report);
        }
    }

    fn on_decision(&self, plan: &types::ExecutionPlanSpec, decision: &types::DecisionOutcome) {
        for observer in &self.observers {
            observer.on_decision(plan, decision);
        }
    }

    fn on_replan(
        &self,
        current_revision: u32,
        next_revision: u32,
        reason: &str,
        diff_summary: &str,
    ) {
        for observer in &self.observers {
            observer.on_replan(current_revision, next_revision, reason, diff_summary);
        }
    }
}

async fn confirm_phase(
    confirmer: Option<&dyn types::OrchestratorPhaseConfirmer>,
    prompt: PhaseConfirmationPrompt,
) -> Result<(), OrchestratorError> {
    let phase_number = prompt.phase.number();
    let phase_label = prompt.phase.label();
    let title = prompt.title.clone();
    let summary = prompt.summary.clone();
    let detail_count = prompt.details.len();

    let Some(confirmer) = confirmer else {
        tracing::info!(
            phase = phase_number,
            phase_label = %phase_label,
            title = %title,
            summary = %summary,
            detail_count,
            "orchestrator phase confirmation bypassed"
        );
        return Ok(());
    };

    tracing::info!(
        phase = phase_number,
        phase_label = %phase_label,
        title = %title,
        summary = %summary,
        detail_count,
        "orchestrator phase confirmation requested"
    );
    let decision = confirmer.confirm(prompt).await;
    tracing::info!(
        phase = phase_number,
        phase_label = %phase_label,
        decision = ?decision,
        "orchestrator phase confirmation resolved"
    );

    match decision {
        PhaseConfirmationDecision::Continue => Ok(()),
        PhaseConfirmationDecision::Reject => Err(OrchestratorError::PolicyViolation(format!(
            "{phase_label} was rejected by the user"
        ))),
        PhaseConfirmationDecision::Abort => Err(OrchestratorError::PolicyViolation(format!(
            "{phase_label} was aborted by the user"
        ))),
    }
}

fn phase3_confirmation_prompt(
    plan: &types::ExecutionPlanSpec,
    run_state: &run_state::RunStateSnapshot,
) -> PhaseConfirmationPrompt {
    let completed = run_state
        .task_results
        .iter()
        .filter(|result| result.status == types::TaskNodeStatus::Completed)
        .count();
    let failed = run_state
        .task_results
        .iter()
        .filter(|result| result.status == types::TaskNodeStatus::Failed)
        .count();
    let details = plan
        .tasks
        .iter()
        .map(|task| {
            let status = run_state.status_for(task.id());
            format!(
                "{:02} {:?} · {:?} · {}",
                task_position(plan, task.id()),
                task.kind,
                status,
                task.title()
            )
        })
        .collect::<Vec<_>>();

    PhaseConfirmationPrompt {
        phase: types::OrchestratorPhase::SystemVerification,
        title: "Confirm Phase 3".to_string(),
        summary: format!(
            "Plan r{} execution finished: {completed}/{} completed, {failed} failed. Continue to system validation and audit?",
            plan.revision,
            plan.tasks.len()
        ),
        details,
    }
}

fn phase4_confirmation_prompt(
    plan: &types::ExecutionPlanSpec,
    report: &types::SystemReport,
) -> PhaseConfirmationPrompt {
    let mut details = vec![
        format!(
            "integration: {} ({})",
            report.integration.all_required_passed,
            report.integration.results.len()
        ),
        format!(
            "security: {} ({})",
            report.security.all_required_passed,
            report.security.results.len()
        ),
        format!(
            "release: {} ({})",
            report.release.all_required_passed,
            report.release.results.len()
        ),
        format!("review passed: {}", report.review_passed),
        format!("artifacts complete: {}", report.artifacts_complete),
    ];
    details.extend(
        report
            .review_findings
            .iter()
            .map(|finding| format!("review finding: {finding}")),
    );
    details.extend(
        report
            .missing_artifacts
            .iter()
            .map(|artifact| format!("missing artifact: {artifact}")),
    );

    PhaseConfirmationPrompt {
        phase: types::OrchestratorPhase::Decision,
        title: "Confirm Phase 4".to_string(),
        summary: format!(
            "Plan r{} validation overall_passed={}. Continue to final decision?",
            plan.revision, report.overall_passed
        ),
        details,
    }
}

fn task_position(plan: &types::ExecutionPlanSpec, task_id: uuid::Uuid) -> usize {
    plan.tasks
        .iter()
        .position(|task| task.id() == task_id)
        .map(|index| index + 1)
        .unwrap_or_default()
}

fn execution_complete_for_phase3(
    plan: &types::ExecutionPlanSpec,
    run_state: &run_state::RunStateSnapshot,
) -> bool {
    plan.tasks
        .iter()
        .all(|task| run_state.status_for(task.id()) == types::TaskNodeStatus::Completed)
}

fn task_status_count(
    run_state: &run_state::RunStateSnapshot,
    status: types::TaskNodeStatus,
) -> usize {
    run_state
        .task_results
        .iter()
        .filter(|result| result.status == status)
        .count()
}

fn log_phase3_entry(plan: &types::ExecutionPlanSpec, run_state: &run_state::RunStateSnapshot) {
    tracing::info!(
        phase = 3_u8,
        revision = plan.revision,
        completed = task_status_count(run_state, types::TaskNodeStatus::Completed),
        failed = task_status_count(run_state, types::TaskNodeStatus::Failed),
        total = plan.tasks.len(),
        dagrs_completed = run_state.dagrs_runtime.completed_nodes,
        dagrs_total = run_state.dagrs_runtime.total_nodes,
        "orchestrator entering phase 3 system verification"
    );
}

fn log_phase3_report(plan: &types::ExecutionPlanSpec, report: &types::SystemReport) {
    tracing::info!(
        phase = 3_u8,
        revision = plan.revision,
        overall_passed = report.overall_passed,
        integration_passed = report.integration.all_required_passed,
        integration_checks = report.integration.results.len(),
        security_passed = report.security.all_required_passed,
        security_checks = report.security.results.len(),
        release_passed = report.release.all_required_passed,
        release_checks = report.release.results.len(),
        review_passed = report.review_passed,
        review_findings = report.review_findings.len(),
        artifacts_complete = report.artifacts_complete,
        missing_artifacts = report.missing_artifacts.len(),
        "orchestrator phase 3 system verification completed"
    );
}

impl<M: CompletionModel + 'static> Orchestrator<M> {
    pub fn new(model: M, registry: Arc<ToolRegistry>, config: OrchestratorConfig) -> Self {
        Self {
            model,
            registry,
            config,
            fuse_state: workspace::FuseProvisionState::default(),
        }
    }

    /// Run the full orchestration pipeline.
    pub async fn run(&self, mut spec: IntentSpec) -> Result<OrchestratorResult, OrchestratorError>
    where
        M::Response: CompletionUsage,
    {
        // Phase 0: Validate and repair
        let issues = validate_intentspec(&spec);
        if !issues.is_empty() {
            repair_intentspec(&mut spec, &issues);
            let remaining = validate_intentspec(&spec);
            if !remaining.is_empty() {
                return Err(OrchestratorError::ValidationFailed(
                    remaining
                        .iter()
                        .map(|e| e.message.clone())
                        .collect::<Vec<_>>()
                        .join("; "),
                ));
            }
        }

        let mut tool_loop_config = self.config.tool_loop_config.clone();
        tool_loop_config.intent_id = self
            .config
            .persisted_intent_id
            .clone()
            .or_else(|| Some(spec.metadata.id.clone()));
        tool_loop_config.run_id = Some(Uuid::now_v7().to_string());
        if self.config.coder_preamble.is_some() {
            tool_loop_config.preamble = self.config.coder_preamble.clone();
        }
        let max_replans = replan::max_replans(&spec);
        let mut replan_count = 0_u32;
        let mut plan_revision_specs = Vec::new();
        // FUSE provisioning gate. Reused across orchestrator runs in the same
        // process so the first failure disables FUSE for every subsequent task,
        // not just for the current run's replans.
        let fuse_state = self.fuse_state.clone();
        let downstream_observer = self.config.observer.clone();
        let persistence_session = if let Some(ref mcp_server) = self.config.mcp_server {
            Some(
                persistence::ExecutionAuditSession::start(
                    Arc::clone(mcp_server),
                    &spec,
                    &self.config.working_dir,
                    self.config.persisted_intent_id.as_deref(),
                    self.config.persisted_plan_bundle.clone(),
                    self.config.persisted_plan_id.as_deref(),
                )
                .await?,
            )
        } else {
            None
        };
        let observer = match (
            downstream_observer.clone(),
            persistence_session
                .as_ref()
                .map(|session| session.observer()),
        ) {
            (Some(left), Some(right)) => Some(Arc::new(FanoutObserver {
                observers: vec![left, right],
            })
                as Arc<dyn types::OrchestratorObserver>),
            (Some(left), None) => Some(left),
            (None, Some(right)) => Some(right),
            (None, None) => None,
        };
        let (execution_plan_spec, run_state, system_report, decision) = loop {
            // Phase 1: Compile execution plan
            let mut plan_spec = if replan_count == 0 {
                self.config
                    .initial_plan
                    .clone()
                    .map(Ok)
                    .unwrap_or_else(|| planner::compile_execution_plan_spec(&spec))?
            } else {
                planner::compile_execution_plan_spec(&spec)?
            };
            plan_spec.revision = replan_count + 1;
            plan_spec.parent_revision = (replan_count > 0).then_some(replan_count);
            plan_spec.replan_reason = spec
                .lifecycle
                .change_log
                .last()
                .map(|entry| entry.reason.clone());
            if let Some(session) = persistence_session.as_ref() {
                session.record_plan_compiled(&plan_spec).await?;
            }
            if let Some(observer) = &observer {
                observer.on_plan_compiled(&plan_spec);
            }

            // Phase 2: Execute tasks
            let executor_config = executor::ExecutorConfig {
                tool_loop_config: tool_loop_config.clone(),
                max_retries: spec.execution.retry.max_retries,
                backoff_seconds: spec.execution.retry.backoff_seconds,
                working_dir: self.config.working_dir.clone(),
                spec: Arc::new(spec.clone()),
                reviewer_preamble: self.config.reviewer_preamble.clone(),
                dagrs_resume_checkpoint_id: self.config.dagrs_resume_checkpoint_id.clone(),
                observer: observer.clone(),
                workspace_baseline: None,
                fuse_state: fuse_state.clone(),
            };

            let provider_parallel_limit =
                usize::from(spec.execution.concurrency.max_parallel_tasks.max(1));
            let throttled_model =
                ThrottledCompletionModel::new(self.model.clone(), provider_parallel_limit);
            let run_state = executor::execute_dag(
                &plan_spec,
                &throttled_model,
                &self.registry,
                &executor_config,
            )
            .await?;

            let system_report = verifier::build_system_report(&spec, &plan_spec, &run_state);

            if replan_count < max_replans
                && let Some(directive) =
                    replan::detect_replan(&spec, &plan_spec, &run_state, &system_report)
            {
                plan_revision_specs.push(plan_spec.clone());
                replan_count += 1;
                if let Some(observer) = &observer {
                    observer.on_replan(
                        plan_spec.revision,
                        replan_count + 1,
                        &directive.reason,
                        &directive.diff_summary,
                    );
                }
                replan::apply_replan(&mut spec, replan_count + 1, &directive);
                continue;
            }

            let execution_complete = execution_complete_for_phase3(&plan_spec, &run_state);
            if execution_complete {
                // Phase 3: System verification
                log_phase3_entry(&plan_spec, &run_state);
                confirm_phase(
                    self.config.phase_confirmer.as_deref(),
                    phase3_confirmation_prompt(&plan_spec, &run_state),
                )
                .await?;
                if let Some(observer) = &observer {
                    observer.on_system_verification(&plan_spec, &system_report);
                }
                log_phase3_report(&plan_spec, &system_report);

                // Phase 4: Decision
                tracing::info!(
                    phase = 4_u8,
                    revision = plan_spec.revision,
                    overall_passed = system_report.overall_passed,
                    "orchestrator entering phase 4 decision"
                );
                confirm_phase(
                    self.config.phase_confirmer.as_deref(),
                    phase4_confirmation_prompt(&plan_spec, &system_report),
                )
                .await?;
            } else {
                tracing::warn!(
                    revision = plan_spec.revision,
                    completed = run_state
                        .task_results
                        .iter()
                        .filter(|result| result.status == types::TaskNodeStatus::Completed)
                        .count(),
                    total = plan_spec.tasks.len(),
                    "execution did not complete all required tasks; skipping phase 3 and phase 4 confirmations"
                );
            }

            let decision = decider::make_decision(
                &run_state,
                &system_report,
                &spec.risk.level,
                spec.risk.human_in_loop.required,
            );
            if let Some(observer) = &observer {
                observer.on_decision(&plan_spec, &decision);
            }
            tracing::info!(
                phase = 4_u8,
                revision = plan_spec.revision,
                decision = ?decision,
                risk_level = ?spec.risk.level,
                human_in_loop_required = spec.risk.human_in_loop.required,
                "orchestrator phase 4 decision made"
            );
            plan_revision_specs.push(plan_spec.clone());
            break (plan_spec, run_state, system_report, decision);
        };
        let task_results = run_state.task_results.clone();

        let persistence = if let Some(session) = persistence_session {
            let persisted = session
                .finalize(persistence::ExecutionFinalizeRequest {
                    spec: &spec,
                    execution_plan_spec: &execution_plan_spec,
                    plan_revision_specs: &plan_revision_specs,
                    run_state: &run_state,
                    system_report: &system_report,
                    decision: &decision,
                    working_dir: &self.config.working_dir,
                    model_name: std::any::type_name::<M>(),
                })
                .await?;
            if let Some(observer) = &downstream_observer {
                observer.on_persistence_complete(&persisted);
            }
            Some(persisted)
        } else {
            None
        };

        Ok(OrchestratorResult {
            decision,
            execution_plan_spec,
            plan_revision_specs,
            run_state,
            task_results,
            system_report,
            intent_spec_id: spec.metadata.id.clone(),
            lifecycle_change_log: spec.lifecycle.change_log.clone(),
            replan_count,
            persistence,
        })
    }
}

#[cfg(test)]
mod tests {
    use std::{
        collections::BTreeMap,
        path::Path,
        sync::{Arc, Mutex},
    };

    use super::*;
    use crate::internal::ai::{
        agent::ToolLoopConfig,
        completion::{
            CompletionError, CompletionRequest, CompletionResponse, CompletionThinking,
            message::{AssistantContent, Function, Message, Text, ToolCall, UserContent},
        },
        intentspec::types::*,
        tools::handlers::ApplyPatchHandler,
    };

    #[derive(Clone)]
    struct MockOrchestratorModel;

    #[derive(Clone)]
    struct ThinkingRecordingModel {
        seen: Arc<Mutex<Vec<Option<CompletionThinking>>>>,
    }

    struct RecordingObserver {
        events: Arc<Mutex<Vec<String>>>,
    }

    struct RecordingPhaseConfirmer {
        phases: Arc<Mutex<Vec<u8>>>,
    }

    #[async_trait::async_trait]
    impl types::OrchestratorPhaseConfirmer for RecordingPhaseConfirmer {
        async fn confirm(
            &self,
            prompt: types::PhaseConfirmationPrompt,
        ) -> types::PhaseConfirmationDecision {
            self.phases.lock().unwrap().push(prompt.phase.number());
            types::PhaseConfirmationDecision::Continue
        }
    }

    impl types::OrchestratorObserver for RecordingObserver {
        fn on_plan_compiled(&self, plan: &types::ExecutionPlanSpec) {
            self.events
                .lock()
                .unwrap()
                .push(format!("plan:{}", plan.revision));
        }

        fn on_task_runtime_event(&self, task: &types::TaskSpec, event: types::TaskRuntimeEvent) {
            let mut events = self.events.lock().unwrap();
            match event {
                types::TaskRuntimeEvent::Phase(types::TaskRuntimePhase::Starting) => {
                    events.push(format!("start:{}", task.title()));
                }
                types::TaskRuntimeEvent::Phase(types::TaskRuntimePhase::Completed) => {
                    events.push(format!("done:{}", task.title()));
                }
                _ => {}
            }
        }

        fn on_graph_progress(&self, completed: usize, total: usize) {
            self.events
                .lock()
                .unwrap()
                .push(format!("graph:{completed}/{total}"));
        }

        fn on_system_verification(
            &self,
            plan: &types::ExecutionPlanSpec,
            report: &types::SystemReport,
        ) {
            self.events.lock().unwrap().push(format!(
                "verify:{}:{}",
                plan.revision, report.overall_passed
            ));
        }

        fn on_decision(&self, plan: &types::ExecutionPlanSpec, decision: &types::DecisionOutcome) {
            self.events
                .lock()
                .unwrap()
                .push(format!("decision:{}:{decision:?}", plan.revision));
        }
    }

    impl CompletionModel for MockOrchestratorModel {
        type Response = ();

        async fn completion(
            &self,
            request: CompletionRequest,
        ) -> Result<CompletionResponse<Self::Response>, CompletionError> {
            let prompt = latest_user_text(&request);
            if !has_tool_result(&request) && prompt.contains("apply_patch") {
                return Ok(CompletionResponse {
                    content: vec![AssistantContent::ToolCall(add_file_patch_call(
                        &prompt,
                        "orchestrator",
                    ))],
                    reasoning_content: None,
                    raw_response: (),
                });
            }

            Ok(CompletionResponse {
                content: vec![AssistantContent::Text(Text {
                    text: "task complete".into(),
                })],
                reasoning_content: None,
                raw_response: (),
            })
        }
    }

    impl CompletionModel for ThinkingRecordingModel {
        type Response = ();

        async fn completion(
            &self,
            request: CompletionRequest,
        ) -> Result<CompletionResponse<Self::Response>, CompletionError> {
            self.seen.lock().unwrap().push(request.thinking);
            let prompt = latest_user_text(&request);
            if !has_tool_result(&request) && prompt.contains("apply_patch") {
                return Ok(CompletionResponse {
                    content: vec![AssistantContent::ToolCall(add_file_patch_call(
                        &prompt, "thinking",
                    ))],
                    reasoning_content: None,
                    raw_response: (),
                });
            }

            Ok(CompletionResponse {
                content: vec![AssistantContent::Text(Text {
                    text: "task complete".into(),
                })],
                reasoning_content: None,
                raw_response: (),
            })
        }
    }

    fn has_tool_result(request: &CompletionRequest) -> bool {
        request.chat_history.iter().any(|message| match message {
            Message::User { content } => content
                .iter()
                .any(|item| matches!(item, UserContent::ToolResult(_))),
            _ => false,
        })
    }

    fn latest_user_text(request: &CompletionRequest) -> String {
        request
            .chat_history
            .iter()
            .rev()
            .find_map(|message| match message {
                Message::User { content } => content.iter().find_map(|item| match item {
                    UserContent::Text(text) => Some(text.text.clone()),
                    _ => None,
                }),
                _ => None,
            })
            .unwrap_or_default()
    }

    fn add_file_patch_call(prompt: &str, prefix: &str) -> ToolCall {
        let slug = prompt
            .split("## Task\n")
            .nth(1)
            .and_then(|tail| tail.lines().next())
            .map(slugify_task_title)
            .filter(|slug| !slug.is_empty())
            .unwrap_or_else(|| "task".to_string());
        let path = format!("src/{prefix}_{slug}.txt");
        let patch = format!("*** Begin Patch\n*** Add File: {path}\n+done\n*** End Patch");
        ToolCall {
            id: format!("call_{prefix}_{slug}"),
            name: "apply_patch".to_string(),
            function: Function {
                name: "apply_patch".to_string(),
                arguments: serde_json::json!({ "input": patch }),
            },
        }
    }

    fn slugify_task_title(title: &str) -> String {
        title
            .chars()
            .map(|ch| {
                if ch.is_ascii_alphanumeric() {
                    ch.to_ascii_lowercase()
                } else {
                    '_'
                }
            })
            .collect::<String>()
            .trim_matches('_')
            .to_string()
    }

    fn test_registry(working_dir: &Path) -> Arc<ToolRegistry> {
        std::fs::create_dir_all(working_dir.join("src")).unwrap();
        let mut registry = ToolRegistry::with_working_dir(working_dir.to_path_buf());
        registry.register("apply_patch", Arc::new(ApplyPatchHandler));
        Arc::new(registry)
    }

    fn test_spec() -> IntentSpec {
        IntentSpec {
            api_version: "intentspec.io/v1alpha1".into(),
            kind: "IntentSpec".into(),
            metadata: Metadata {
                id: "orch-test".into(),
                created_at: "2025-01-01T00:00:00Z".into(),
                created_by: CreatedBy {
                    creator_type: CreatorType::User,
                    id: "tester".into(),
                    display_name: None,
                },
                target: Target {
                    repo: RepoTarget {
                        repo_type: RepoType::Local,
                        locator: "/tmp/test".into(),
                    },
                    base_ref: "HEAD".into(),
                    workspace_id: None,
                    labels: BTreeMap::new(),
                },
            },
            intent: Intent {
                summary: "test orchestration".into(),
                problem_statement: "test".into(),
                change_type: ChangeType::Feature,
                objectives: vec![Objective {
                    title: "implement feature".into(),
                    kind: ObjectiveKind::Implementation,
                }],
                in_scope: vec!["src/".into()],
                out_of_scope: vec![],
                touch_hints: None,
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
                    retention_days: 90,
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
                    max_wall_clock_seconds: 3600,
                    max_cost_units: 100,
                },
            },
            risk: Risk {
                level: RiskLevel::Low,
                rationale: "test".into(),
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
                    allow: vec![ToolRule {
                        tool: "workspace.fs".into(),
                        actions: vec!["read".into(), "write".into()],
                        constraints: BTreeMap::new(),
                    }],
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
                    encoding_policy: EncodingPolicy::ContextualEscape,
                    no_direct_eval: true,
                },
            },
            execution: ExecutionPolicy {
                retry: RetryPolicy {
                    max_retries: 2,
                    backoff_seconds: 0,
                },
                replan: ReplanPolicy { triggers: vec![] },
                concurrency: ConcurrencyPolicy {
                    max_parallel_tasks: 1,
                },
            },
            artifacts: Artifacts {
                required: vec![],
                retention: ArtifactRetention { days: 90 },
            },
            provenance: ProvenancePolicy {
                require_slsa_provenance: false,
                require_sbom: false,
                transparency_log: TransparencyLogPolicy {
                    mode: TransparencyMode::None,
                },
                bindings: ProvenanceBindings {
                    embed_intent_spec_digest: false,
                    embed_evidence_digests: false,
                },
            },
            lifecycle: Lifecycle {
                schema_version: "1.0.0".into(),
                status: LifecycleStatus::Active,
                change_log: vec![],
            },
            libra: None,
            extensions: BTreeMap::new(),
        }
    }

    #[tokio::test]
    async fn test_orchestrator_full_pipeline() {
        let dir = tempfile::tempdir().unwrap();
        let model = MockOrchestratorModel;
        let registry = test_registry(dir.path());
        let config = OrchestratorConfig {
            working_dir: dir.path().to_path_buf(),
            base_commit: None,
            persisted_intent_id: None,
            persisted_plan_bundle: None,
            persisted_plan_id: None,
            initial_plan: None,
            dagrs_resume_checkpoint_id: None,
            tool_loop_config: ToolLoopConfig::default(),
            coder_preamble: None,
            reviewer_preamble: None,
            mcp_server: None,
            observer: None,
            phase_confirmer: None,
        };
        let orchestrator = Orchestrator::new(model, registry, config);
        let spec = test_spec();
        let result = orchestrator.run(spec).await.unwrap();
        assert_eq!(result.decision, types::DecisionOutcome::Commit);
        assert_eq!(result.task_results.len(), 4);
        assert_eq!(result.execution_plan_spec.tasks.len(), 4);
        assert!(result.system_report.overall_passed);
    }

    #[tokio::test]
    async fn test_orchestrator_uses_approved_initial_plan() {
        let dir = tempfile::tempdir().unwrap();
        let model = MockOrchestratorModel;
        let registry = test_registry(dir.path());
        let spec = test_spec();
        let mut planned_spec = spec.clone();
        planned_spec.intent.objectives = vec![
            Objective {
                title: "LLM step one".into(),
                kind: ObjectiveKind::Implementation,
            },
            Objective {
                title: "LLM step two".into(),
                kind: ObjectiveKind::Implementation,
            },
        ];
        let initial_plan = planner::compile_execution_plan_spec(&planned_spec).unwrap();
        let config = OrchestratorConfig {
            working_dir: dir.path().to_path_buf(),
            base_commit: None,
            persisted_intent_id: None,
            persisted_plan_bundle: None,
            persisted_plan_id: None,
            initial_plan: Some(initial_plan),
            dagrs_resume_checkpoint_id: None,
            tool_loop_config: ToolLoopConfig::default(),
            coder_preamble: None,
            reviewer_preamble: None,
            mcp_server: None,
            observer: None,
            phase_confirmer: None,
        };

        let orchestrator = Orchestrator::new(model, registry, config);
        let result = orchestrator.run(spec).await.unwrap();
        let task_titles = result
            .execution_plan_spec
            .tasks
            .iter()
            .map(|task| task.title().to_string())
            .collect::<Vec<_>>();

        assert!(task_titles.contains(&"LLM step one".to_string()));
        assert!(task_titles.contains(&"LLM step two".to_string()));
    }

    #[tokio::test]
    async fn test_orchestrator_preserves_tool_loop_thinking_for_execution() {
        let dir = tempfile::tempdir().unwrap();
        let seen = Arc::new(Mutex::new(Vec::new()));
        let model = ThinkingRecordingModel {
            seen: Arc::clone(&seen),
        };
        let registry = test_registry(dir.path());
        let config = OrchestratorConfig {
            working_dir: dir.path().to_path_buf(),
            base_commit: None,
            persisted_intent_id: None,
            persisted_plan_bundle: None,
            persisted_plan_id: None,
            initial_plan: None,
            dagrs_resume_checkpoint_id: None,
            tool_loop_config: ToolLoopConfig {
                thinking: Some(CompletionThinking::High),
                ..Default::default()
            },
            coder_preamble: None,
            reviewer_preamble: None,
            mcp_server: None,
            observer: None,
            phase_confirmer: None,
        };
        let orchestrator = Orchestrator::new(model, registry, config);

        let result = orchestrator.run(test_spec()).await.unwrap();

        assert_eq!(result.decision, types::DecisionOutcome::Commit);
        let seen = seen.lock().unwrap().clone();
        assert!(!seen.is_empty());
        assert!(
            seen.iter()
                .all(|thinking| *thinking == Some(CompletionThinking::High)),
            "{seen:?}"
        );
    }

    #[tokio::test]
    async fn test_orchestrator_emits_progress_events() {
        let dir = tempfile::tempdir().unwrap();
        let model = MockOrchestratorModel;
        let registry = test_registry(dir.path());
        let events = Arc::new(Mutex::new(Vec::new()));
        let observer: Arc<dyn types::OrchestratorObserver> = Arc::new(RecordingObserver {
            events: Arc::clone(&events),
        });
        let config = OrchestratorConfig {
            working_dir: dir.path().to_path_buf(),
            base_commit: None,
            persisted_intent_id: None,
            persisted_plan_bundle: None,
            persisted_plan_id: None,
            initial_plan: None,
            dagrs_resume_checkpoint_id: None,
            tool_loop_config: ToolLoopConfig::default(),
            coder_preamble: None,
            reviewer_preamble: None,
            mcp_server: None,
            observer: Some(observer),
            phase_confirmer: None,
        };
        let orchestrator = Orchestrator::new(model, registry, config);
        let result = orchestrator.run(test_spec()).await.unwrap();
        let events = events.lock().unwrap().clone();
        assert!(!events.is_empty());
        assert!(
            events.iter().any(|event| event.starts_with("plan:")),
            "{events:?}"
        );
        assert!(
            events.iter().any(|event| event.starts_with("start:")),
            "{events:?}"
        );
        assert!(
            events.iter().any(|event| event.starts_with("graph:")),
            "{events:?}"
        );
        assert!(
            events.iter().any(|event| event.starts_with("verify:")),
            "{events:?}"
        );
        assert!(
            events.iter().any(|event| event.starts_with("decision:")),
            "{events:?}"
        );
        let expected_decision = format!(
            "decision:{}:{:?}",
            result.execution_plan_spec.revision, result.decision
        );
        assert!(events.contains(&expected_decision), "{events:?}");
    }

    #[tokio::test]
    async fn test_orchestrator_confirms_phase_three_and_four() {
        let dir = tempfile::tempdir().unwrap();
        let model = MockOrchestratorModel;
        let registry = test_registry(dir.path());
        let phases = Arc::new(Mutex::new(Vec::new()));
        let confirmer: Arc<dyn types::OrchestratorPhaseConfirmer> =
            Arc::new(RecordingPhaseConfirmer {
                phases: Arc::clone(&phases),
            });
        let config = OrchestratorConfig {
            working_dir: dir.path().to_path_buf(),
            base_commit: None,
            persisted_intent_id: None,
            persisted_plan_bundle: None,
            persisted_plan_id: None,
            initial_plan: None,
            dagrs_resume_checkpoint_id: None,
            tool_loop_config: ToolLoopConfig::default(),
            coder_preamble: None,
            reviewer_preamble: None,
            mcp_server: None,
            observer: None,
            phase_confirmer: Some(confirmer),
        };
        let orchestrator = Orchestrator::new(model, registry, config);
        let result = orchestrator.run(test_spec()).await.unwrap();

        assert_eq!(result.decision, types::DecisionOutcome::Commit);
        assert_eq!(*phases.lock().unwrap(), vec![3, 4]);
    }

    #[tokio::test]
    async fn test_orchestrator_skips_phase_confirmation_when_required_gate_fails() {
        let dir = tempfile::tempdir().unwrap();
        let model = MockOrchestratorModel;
        let registry = test_registry(dir.path());
        let phases = Arc::new(Mutex::new(Vec::new()));
        let confirmer: Arc<dyn types::OrchestratorPhaseConfirmer> =
            Arc::new(RecordingPhaseConfirmer {
                phases: Arc::clone(&phases),
            });
        let config = OrchestratorConfig {
            working_dir: dir.path().to_path_buf(),
            base_commit: None,
            persisted_intent_id: None,
            persisted_plan_bundle: None,
            persisted_plan_id: None,
            initial_plan: None,
            dagrs_resume_checkpoint_id: None,
            tool_loop_config: ToolLoopConfig::default(),
            coder_preamble: None,
            reviewer_preamble: None,
            mcp_server: None,
            observer: None,
            phase_confirmer: Some(confirmer),
        };
        let orchestrator = Orchestrator::new(model, registry, config);
        let mut spec = test_spec();
        spec.acceptance.verification_plan.integration_checks = vec![Check {
            id: "integration-fail".into(),
            kind: CheckKind::Command,
            command: Some("false".into()),
            timeout_seconds: Some(1),
            expected_exit_code: None,
            required: true,
            artifacts_produced: vec![],
        }];

        let result = orchestrator.run(spec).await.unwrap();

        assert_eq!(result.decision, types::DecisionOutcome::Abandon);
        assert!(
            result
                .task_results
                .iter()
                .any(|result| result.status == types::TaskNodeStatus::Failed)
        );
        assert_eq!(*phases.lock().unwrap(), Vec::<u8>::new());
    }

    #[tokio::test]
    async fn test_orchestrator_high_risk_human_review() {
        let dir = tempfile::tempdir().unwrap();
        let model = MockOrchestratorModel;
        let registry = test_registry(dir.path());
        let config = OrchestratorConfig {
            working_dir: dir.path().to_path_buf(),
            base_commit: None,
            persisted_intent_id: None,
            persisted_plan_bundle: None,
            persisted_plan_id: None,
            initial_plan: None,
            dagrs_resume_checkpoint_id: None,
            tool_loop_config: ToolLoopConfig::default(),
            coder_preamble: None,
            reviewer_preamble: None,
            mcp_server: None,
            observer: None,
            phase_confirmer: None,
        };
        let orchestrator = Orchestrator::new(model, registry, config);
        let mut spec = test_spec();
        spec.risk.level = RiskLevel::High;
        spec.risk.human_in_loop.required = true;
        spec.risk.human_in_loop.min_approvers = 2;
        let result = orchestrator.run(spec).await.unwrap();
        assert_eq!(result.decision, types::DecisionOutcome::HumanReviewRequired);
    }

    #[tokio::test]
    async fn test_orchestrator_analysis_only_pipeline_commits_without_patchset_or_gates() {
        let dir = tempfile::tempdir().unwrap();
        let model = MockOrchestratorModel;
        let registry = test_registry(dir.path());
        let config = OrchestratorConfig {
            working_dir: dir.path().to_path_buf(),
            base_commit: None,
            persisted_intent_id: None,
            persisted_plan_bundle: None,
            persisted_plan_id: None,
            initial_plan: None,
            dagrs_resume_checkpoint_id: None,
            tool_loop_config: ToolLoopConfig::default(),
            coder_preamble: None,
            reviewer_preamble: None,
            mcp_server: None,
            observer: None,
            phase_confirmer: None,
        };
        let orchestrator = Orchestrator::new(model, registry, config);
        let mut spec = test_spec();
        spec.intent.change_type = ChangeType::Unknown;
        spec.intent.objectives = vec![
            Objective {
                title: "Analyze repository structure".into(),
                kind: ObjectiveKind::Analysis,
            },
            Objective {
                title: "Summarize technical debt hotspots".into(),
                kind: ObjectiveKind::Analysis,
            },
        ];
        spec.artifacts.required.clear();
        spec.acceptance.verification_plan.fast_checks.clear();
        spec.acceptance.verification_plan.integration_checks.clear();
        spec.acceptance.verification_plan.security_checks.clear();
        spec.acceptance.verification_plan.release_checks.clear();

        let result = orchestrator.run(spec).await.unwrap();
        assert_eq!(result.decision, types::DecisionOutcome::Commit);
        assert!(
            result
                .execution_plan_spec
                .tasks
                .iter()
                .all(|task| task.kind == types::TaskKind::Analysis)
        );
        assert!(result.system_report.overall_passed);
        assert!(
            result
                .task_results
                .iter()
                .all(|task| task.status == types::TaskNodeStatus::Completed)
        );
    }

    #[tokio::test]
    async fn test_orchestrator_validation_failure() {
        let dir = tempfile::tempdir().unwrap();
        let model = MockOrchestratorModel;
        let registry = test_registry(dir.path());
        let config = OrchestratorConfig {
            working_dir: dir.path().to_path_buf(),
            base_commit: None,
            persisted_intent_id: None,
            persisted_plan_bundle: None,
            persisted_plan_id: None,
            initial_plan: None,
            dagrs_resume_checkpoint_id: None,
            tool_loop_config: ToolLoopConfig::default(),
            coder_preamble: None,
            reviewer_preamble: None,
            mcp_server: None,
            observer: None,
            phase_confirmer: None,
        };
        let orchestrator = Orchestrator::new(model, registry, config);
        let mut spec = test_spec();
        // Empty required fields that repair cannot fix
        spec.metadata.id = String::new();
        spec.intent.summary = String::new();
        let err = orchestrator.run(spec).await.unwrap_err();
        assert!(matches!(err, OrchestratorError::ValidationFailed(_)));
    }

    #[tokio::test]
    async fn test_orchestrator_replans_after_security_gate_failure() {
        let dir = tempfile::tempdir().unwrap();
        let model = MockOrchestratorModel;
        let registry = test_registry(dir.path());
        let config = OrchestratorConfig {
            working_dir: dir.path().to_path_buf(),
            base_commit: None,
            persisted_intent_id: None,
            persisted_plan_bundle: None,
            persisted_plan_id: None,
            initial_plan: None,
            dagrs_resume_checkpoint_id: None,
            tool_loop_config: ToolLoopConfig::default(),
            coder_preamble: None,
            reviewer_preamble: None,
            mcp_server: None,
            observer: None,
            phase_confirmer: None,
        };
        let orchestrator = Orchestrator::new(model, registry, config);
        let mut spec = test_spec();
        spec.execution.replan.triggers = vec![ReplanTrigger::SecurityGateFail];
        spec.acceptance.verification_plan.security_checks = vec![Check {
            id: "security-fail".into(),
            kind: CheckKind::Command,
            command: Some("false".into()),
            timeout_seconds: Some(1),
            expected_exit_code: None,
            required: true,
            artifacts_produced: vec![],
        }];

        let result = orchestrator.run(spec).await.unwrap();
        assert_eq!(result.replan_count, 1);
        assert_eq!(result.lifecycle_change_log.len(), 1);
        assert_eq!(result.plan_revision_specs.len(), 2);
        assert_eq!(result.execution_plan_spec.revision, 2);
        assert_eq!(result.decision, types::DecisionOutcome::Abandon);
    }
}
