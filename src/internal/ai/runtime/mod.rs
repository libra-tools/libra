//! Shared runtime contracts for the `libra code` workflow.
//!
//! Phase 0 keeps this module contract-only so existing provider paths can adapt
//! to one stable surface before scheduler and provider cutover starts.

pub mod contracts;
pub mod controller;
mod derived_records;
pub mod durability;
pub mod environment;
pub mod event;
pub mod execution_control;
pub mod fix_bridge;
mod fix_control;
pub mod fix_execution;
mod fix_protocol;
mod fix_response;
pub mod hardening;
pub mod lifecycle;
pub mod phase0;
pub mod phase1;
pub mod phase2;
pub mod phase3;
pub mod phase4;
pub mod plan_execution;
pub mod plan_execution_repair;
pub mod prompt_builders;
pub mod revision;
pub mod services;
pub mod snapshot;
pub mod task_executors;
pub mod worker;

pub use contracts::{PromptPackage, WorkflowPhase};
pub use controller::{
    ControllerInitial, ControllerKind, ControllerLease, ControllerService, ControllerServiceError,
    ControllerServiceOptions, ControllerSnapshot, ControllerWritePermit,
    DEFAULT_CONTROLLER_LEASE_SECS,
};
pub use durability::{
    DurableCommandCrashPoint, RuntimeCommandDurability, RuntimeCommandDurabilityError,
};
pub use event::{Event, audit_action_for};
pub use execution_control::{
    CodeSkillActivation, CodeSkillSearch, ExecutionControlService, GoalControlError,
};
pub use fix_bridge::{
    INVESTIGATE_FIX_ADMISSION_MESSAGE, REVIEW_FIX_ADMISSION_MESSAGE, ReviewFixBridgeError,
    ReviewFixInput,
};
pub use fix_execution::{
    ReviewFixExecutionOutcome, ReviewFixInteractionResponder, execute_review_fix,
};
pub use fix_protocol::{
    ReviewFixInteraction, ReviewFixInteractionKind, ReviewFixInteractionResponse, ReviewFixQuestion,
};
pub use hardening::{
    AuditEvent, AuditSink, BoundaryDecision, InMemoryAuditSink, PrincipalContext, PrincipalRole,
    SecretRedactor, ToolBoundaryPolicy, ToolBoundaryRuntime, ToolOperation, ToolOperationDetails,
    TracingAuditSink, project_json_for_wire, redact_json_value,
};
pub use lifecycle::{
    LifecycleShutdownError, LifecycleShutdownOwner, LifecycleShutdownStep, LifecycleStepError,
    resource as lifecycle_resource,
};
pub use phase3::{
    ArtifactLedger, ValidationOutcome, ValidationReport, ValidationReportStore, ValidationStage,
    ValidationStageResult, ValidatorEngine,
};
pub use phase4::{
    DecisionPolicy, DecisionProposal, DecisionProposalRoute, DecisionProposalStore, FinalDecision,
    FinalDecisionStore, FinalDecisionSummary, RiskScoreBreakdown, aggregate_risk_score,
    build_decision_proposal,
};
pub use plan_execution::{
    DeferredPlanExecutionExecutor, PLAN_EXECUTION_TURN_INPUT, PlanExecutionRunner,
    ensure_plan_execution_mutating_gate, is_plan_execution_turn, plan_execution_turn_request,
    submit_confirmed_plan_execution, submit_repaired_plan_execution,
};
pub use plan_execution_repair::{
    DEFAULT_AUTOMATIC_PLAN_REPAIR_ATTEMPTS, ExecutionFailureEvidence, ExecutionFailureRevision,
    MAX_AUTOMATIC_PLAN_REPAIR_ATTEMPTS, PlanExecutionRepairAckDelivery, PlanExecutionRepairService,
    PlanExecutionRepairState, open_plan_execution_repair_from_workflow,
    park_plan_execution_repair_gate, persist_and_park_plan_execution_repair_gate,
    persist_plan_execution_repair_gate, persist_plan_execution_repair_gate_superseding,
    persist_plan_execution_repair_gate_with_predecessor, redacted_failure_summary,
    speculative_plan_execution_repair_continuations_from_workflow,
};
pub use prompt_builders::{IntentPromptBuilder, PlanningPromptBuilder, TaskPromptBuilder};
pub use services::{
    CodeAgentApprovalConfig, CodeAgentLaunchProfile, CodeAgentSandboxProfile, CodeAgentServices,
    CodeAgentServicesBuilder, tool_runtime_context,
};
pub use snapshot::Snapshot;
pub use worker::{
    AgentEvent, AgentEventKind, AgentEventStream, AgentRuntimeHandle, AgentRuntimeWorker,
    AgentRuntimeWorkerConfig, AgentSnapshot, EventCursor, ExternalTurnTrackingExecutor,
    InteractionResponse, InteractionState, RuntimeCommand, RuntimeExecutionContext,
    RuntimeInteractionDelivery, RuntimeObserveError, RuntimeShutdownError, RuntimeTurnExecution,
    RuntimeTurnExecutor, RuntimeWorkerError, TurnReceipt, TurnRequest, TurnStateMachine,
    runtime_worker_adapter_message,
};

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RuntimeConfig {
    pub principal: String,
}

impl Default for RuntimeConfig {
    fn default() -> Self {
        Self {
            principal: "libra-runtime".to_string(),
        }
    }
}

#[derive(Clone, Debug)]
pub struct Runtime {
    config: RuntimeConfig,
}

impl Runtime {
    pub fn new(config: RuntimeConfig) -> Self {
        Self { config }
    }

    pub fn principal(&self) -> &str {
        &self.config.principal
    }

    pub fn intent_prompt_builder(
        &self,
        provider: impl Into<String>,
        model: impl Into<String>,
    ) -> IntentPromptBuilder {
        IntentPromptBuilder::new(provider, model).principal(self.principal())
    }

    pub fn planning_prompt_builder(
        &self,
        provider: impl Into<String>,
        model: impl Into<String>,
    ) -> PlanningPromptBuilder {
        PlanningPromptBuilder::new(provider, model).principal(self.principal())
    }

    pub fn task_prompt_builder(
        &self,
        provider: impl Into<String>,
        model: impl Into<String>,
    ) -> TaskPromptBuilder {
        TaskPromptBuilder::new(provider, model).principal(self.principal())
    }
}

impl Default for Runtime {
    fn default() -> Self {
        Self::new(RuntimeConfig::default())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn runtime_exposes_narrow_prompt_builder_entrypoints() {
        let runtime = Runtime::new(RuntimeConfig {
            principal: "tester".into(),
        });
        let package = runtime
            .intent_prompt_builder("mock", "model")
            .request("make tests pass")
            .build();

        assert_eq!(runtime.principal(), "tester");
        assert_eq!(package.phase, WorkflowPhase::Intent);
        assert_eq!(package.provider, "mock");
        assert!(package.preamble.contains("IntentSpec"));
    }
}
