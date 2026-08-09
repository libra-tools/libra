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
pub mod hardening;
pub mod phase0;
pub mod phase1;
pub mod phase2;
pub mod phase3;
pub mod phase4;
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
pub use hardening::{
    AuditEvent, AuditSink, BoundaryDecision, InMemoryAuditSink, PrincipalContext, PrincipalRole,
    SecretRedactor, ToolBoundaryPolicy, ToolBoundaryRuntime, ToolOperation, ToolOperationDetails,
    TracingAuditSink,
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
