//! Code UI bridge for the UI-neutral [`AgentRuntimeHandle`].
//!
//! The adapter deliberately owns no terminal-UI state. Its session is the event
//! projection cache used by the HTTP/SSE surface; commands are admitted,
//! responded to, and cancelled by the serialized runtime worker.
//!
//! For default Web non-Codex launches, optional [`WebCodeUiAdmission`] supplies
//! persist-before-gate transcript semantics and plan-vs-explicit routing while
//! this adapter remains the mounted [`CodeUiCommandAdapter`] write-path owner.

use std::sync::{Arc, Weak};

use anyhow::{Result, anyhow};
use async_trait::async_trait;
use futures_util::future::BoxFuture;
use tokio::sync::Mutex;
use uuid::Uuid;

use super::{
    code_ui::{
        CodeUiApiError, CodeUiCapabilities, CodeUiCommandAdapter, CodeUiInteractionResponse,
        CodeUiReadModel, CodeUiSession,
    },
    web_admission::WebCodeUiAdmission,
};
use crate::internal::ai::{
    agent::runtime::{RuntimeUsageService, RuntimeUsageTotals},
    observed_agents::{IndexedSkillEvent, SkillEventProjection},
    permission::revoke_session_approval_memos,
    runtime::{
        AgentEventKind, AgentRuntimeHandle, CodeSkillActivation, CodeSkillSearch, EventCursor,
        ExecutionControlService, InteractionResponse, InteractionState, RuntimeCommandDurability,
        RuntimeWorkerError, TurnRequest, runtime_worker_adapter_message,
    },
    sandbox::ApprovalStore,
    usage::UsageQueryFilter,
};

#[derive(Clone, Debug)]
struct ActiveTurnSlot {
    turn_id: String,
    text: String,
}

/// Process-level shutdown hook for web-owned adapter mounts.
pub trait CodeUiLifecycleShutdown: Send + Sync {
    fn shutdown(&self) -> BoxFuture<'_, Result<()>>;

    /// Durable workflow fan-out for SSE wire v2 when this host owns a session
    /// JSONL store (W3-06). Default is unavailable.
    fn workflow_hub(&self) -> Option<std::sync::Arc<super::sse_wire::CodeUiWorkflowHub>> {
        None
    }
}

/// Production Code UI command bridge backed by the serialized Agent runtime.
///
/// `runtime_session_id` is intentionally separate from the browser-visible
/// session id: it is the worker/durability namespace used for turn admission.
#[derive(Clone)]
pub struct AgentRuntimeCodeUiAdapter {
    session: Arc<CodeUiSession>,
    capabilities: CodeUiCapabilities,
    runtime: AgentRuntimeHandle,
    runtime_session_id: String,
    execution_control: Arc<ExecutionControlService>,
    usage: Option<RuntimeUsageService>,
    durability: Option<RuntimeCommandDurability>,
    active_turn: Arc<Mutex<Option<ActiveTurnSlot>>>,
    /// When set, browser submit/cancel/respond use persist-before-gate web
    /// admission (W3-03) instead of the lightweight managed-session path.
    web_admission: Option<Arc<WebCodeUiAdmission>>,
    /// Optional lifecycle shutdown for web-only mounts (worker join, fence).
    /// Held as [`Weak`] so the adapter does not form a retain cycle with the
    /// headless lifecycle host (`Headless` → adapter → host).
    lifecycle_shutdown: Arc<Mutex<Option<Weak<dyn CodeUiLifecycleShutdown>>>>,
    /// In-memory session/TTL approval cache to drop on lease takeover (W4-13).
    approval_store: Arc<Mutex<Option<Arc<Mutex<ApprovalStore>>>>>,
    /// DF-07: A0-07 skill activations pending consumption by the next plain
    /// turn (session-scoped, ids only — never skill contents/credentials).
    pending_skills: Arc<Mutex<PendingSkillContext>>,
}

/// DF-07: session-scoped pending skill activations. Consumption happens
/// only at the definitive provider-turn seam (right before
/// `runtime.submit`, after every admission/routing guard); the composed
/// block travels as [`TurnRequest::provider_context`] — execution-only —
/// so raw text remains the sole durable/retry identity everywhere (terra
/// R4: no payload cache is needed for idempotency, and a retry with the
/// same raw text is idempotent no matter what context either attempt
/// carried). Failed pre-provider exits restore the consumed set.
#[derive(Default)]
pub(crate) struct PendingSkillContext {
    /// `(provider, name)` in activation order, deduplicated.
    active: Vec<(String, String)>,
}

/// One provider-turn context composition: the execution-only context block
/// plus what [`PendingSkillContext::restore`] gives back when the
/// submission fails before the provider runs.
pub(crate) struct ComposedSkillContext {
    pub(crate) context: String,
    consumed: Vec<(String, String)>,
}

impl PendingSkillContext {
    /// Compose the execution-only activation context for an admitted plain
    /// turn, consuming the active set. Returns `None` — and consumes
    /// nothing — when there is nothing pending or the text is slash/empty
    /// (callers already route those away from this seam; this is defense
    /// in depth).
    pub(crate) fn compose_context(&mut self, text: &str) -> Option<ComposedSkillContext> {
        let trimmed = text.trim();
        if self.active.is_empty() || trimmed.is_empty() || trimmed.starts_with('/') {
            return None;
        }
        let mut lines = vec![
            "[skill activation] The operator activated these provider skills for this \
             session; consume them on this turn when relevant. Tool permissions are \
             unchanged by activation:"
                .to_string(),
        ];
        for (provider, name) in &self.active {
            lines.push(format!("- '{name}' ({provider})"));
        }
        let consumed = std::mem::take(&mut self.active);
        Some(ComposedSkillContext {
            context: lines.join("\n"),
            consumed,
        })
    }

    /// Give a failed submission's consumed activations back (in front, so
    /// activation order is preserved). Deduplicated against anything
    /// re-activated while the failed turn was in flight — the pending set
    /// holds at most one slot per `(provider, name)` (terra R5).
    pub(crate) fn restore(&mut self, outcome: ComposedSkillContext) {
        if outcome.consumed.is_empty() {
            return;
        }
        let mut restored = outcome.consumed;
        for entry in std::mem::take(&mut self.active) {
            if !restored.contains(&entry) {
                restored.push(entry);
            }
        }
        restored.dedup();
        self.active = restored;
    }

    /// Record one validated activation; duplicates keep their original slot.
    /// Returns the pending count.
    fn record(&mut self, provider: &str, name: &str) -> usize {
        if !self.active.iter().any(|(p, n)| p == provider && n == name) {
            self.active.push((provider.to_string(), name.to_string()));
        }
        self.active.len()
    }
}

impl super::web_admission::WebCodeUiAdmission {
    /// DF-07: bind the adapter's pending-skill state so the admission path
    /// composes the provider input at its own submit seam.
    pub(crate) fn bind_pending_skills(&self, pending: Arc<Mutex<PendingSkillContext>>) {
        // Poison recovery is safe: the slot only holds a binding pointer, so
        // adopting the inner value can never observe a torn state. Never
        // panic on a production path (GC-11).
        *self
            .pending_skills
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = Some(pending);
    }
}

impl AgentRuntimeCodeUiAdapter {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        session: Arc<CodeUiSession>,
        capabilities: CodeUiCapabilities,
        runtime: AgentRuntimeHandle,
        runtime_session_id: impl Into<String>,
        execution_control: Arc<ExecutionControlService>,
        usage: Option<RuntimeUsageService>,
        durability: Option<RuntimeCommandDurability>,
    ) -> Arc<Self> {
        Self::new_with_web_admission(
            session,
            capabilities,
            runtime,
            runtime_session_id,
            execution_control,
            usage,
            durability,
            None,
        )
    }

    /// Construct the production adapter with optional web admit semantics.
    #[allow(clippy::too_many_arguments)]
    pub fn new_with_web_admission(
        session: Arc<CodeUiSession>,
        capabilities: CodeUiCapabilities,
        runtime: AgentRuntimeHandle,
        runtime_session_id: impl Into<String>,
        execution_control: Arc<ExecutionControlService>,
        usage: Option<RuntimeUsageService>,
        durability: Option<RuntimeCommandDurability>,
        web_admission: Option<Arc<WebCodeUiAdmission>>,
    ) -> Arc<Self> {
        let pending_skills = Arc::new(Mutex::new(PendingSkillContext::default()));
        if let Some(admission) = web_admission.as_ref() {
            admission.bind_pending_skills(pending_skills.clone());
        }
        Arc::new(Self {
            session,
            capabilities,
            runtime,
            runtime_session_id: runtime_session_id.into(),
            execution_control,
            usage,
            durability,
            active_turn: Arc::new(Mutex::new(None)),
            web_admission,
            lifecycle_shutdown: Arc::new(Mutex::new(None)),
            approval_store: Arc::new(Mutex::new(None)),
            pending_skills,
        })
    }

    /// Bind the runtime ApprovalStore so a controller lease takeover can drop
    /// session/TTL memos (W4-13). Always rows stay in `approved_permission`.
    pub async fn set_approval_store(&self, store: Arc<Mutex<ApprovalStore>>) {
        *self.approval_store.lock().await = Some(store);
    }

    /// Attach process shutdown for a web-only mount after the lifecycle host
    /// `Arc` exists. Uses [`Weak`] so dropping the externally retained host
    /// (or [`super::code_ui::CodeUiRuntimeHandle`]) can tear down the worker
    /// without an adapter↔host retain cycle.
    pub async fn attach_lifecycle_shutdown(&self, shutdown: Arc<dyn CodeUiLifecycleShutdown>) {
        *self.lifecycle_shutdown.lock().await = Some(Arc::downgrade(&shutdown));
    }

    fn turn_id(&self, command_id: Option<String>) -> Result<String> {
        match command_id {
            Some(_command_id) if self.durability.is_none() && self.web_admission.is_none() => {
                Err(anyhow!(
                    "commandId requires durable AgentRuntime command storage; omit commandId or resume this Code session"
                ))
            }
            Some(command_id) if command_id.trim().is_empty() => Err(anyhow!(
                "commandId must be a non-empty string when provided"
            )),
            Some(command_id) => Ok(command_id),
            None => Ok(format!("code-ui-{}", Uuid::new_v4())),
        }
    }

    fn map_runtime_error(error: RuntimeWorkerError) -> anyhow::Error {
        anyhow!(
            "AgentRuntime rejected the Code UI command: {}",
            runtime_worker_adapter_message(error)
        )
    }

    fn spawn_release_watcher(
        &self,
        mut stream: crate::internal::ai::runtime::AgentEventStream,
        turn_id: String,
    ) {
        let active_turn = self.active_turn.clone();
        let session_id = self.runtime_session_id.clone();
        tokio::spawn(async move {
            loop {
                match stream.recv().await {
                    Ok(event)
                        if event.session_id == session_id
                            && event.turn_id.as_deref() == Some(turn_id.as_str()) =>
                    {
                        match event.kind {
                            AgentEventKind::TurnCompleted { .. }
                            | AgentEventKind::TurnCancelled
                            | AgentEventKind::TurnFailed { .. }
                            | AgentEventKind::TurnIndeterminateSideEffect { .. } => {
                                let mut slot = active_turn.lock().await;
                                if slot
                                    .as_ref()
                                    .is_some_and(|active| active.turn_id == turn_id)
                                {
                                    *slot = None;
                                }
                                return;
                            }
                            _ => {}
                        }
                    }
                    Ok(_) => {}
                    Err(_) => return,
                }
            }
        });
    }

    async fn rollback_active_turn(&self, turn_id: &str) {
        let mut slot = self.active_turn.lock().await;
        if slot
            .as_ref()
            .is_some_and(|active| active.turn_id == turn_id)
        {
            *slot = None;
        }
    }

    /// Query usage only when the runtime was constructed with a durable usage
    /// recorder. Returning an error (rather than zero totals) preserves the
    /// unknown/partial distinction on the HTTP surface.
    pub async fn usage_cumulative(&self, filter: UsageQueryFilter) -> Result<RuntimeUsageTotals> {
        let usage = self
            .usage
            .as_ref()
            .ok_or_else(|| anyhow!("usage is unavailable for this Code runtime"))?;
        usage.cumulative(filter).await.map_err(Into::into)
    }

    /// A0-07 search is read-only and remains projection-backed.
    pub fn skill_search<'a>(
        &self,
        projection: &'a SkillEventProjection,
        search: &CodeSkillSearch,
    ) -> Vec<&'a IndexedSkillEvent> {
        self.execution_control.skill_search(projection, search)
    }

    /// Validate an A0-07 activation before a provider consumes it.
    pub fn skill_activate(&self, activation: &CodeSkillActivation) -> Result<()> {
        self.execution_control.skill_activate(activation)
    }

    /// Test-provider-only view of the still-pending activations, so race
    /// regressions can assert restore semantics without a provider turn.
    #[cfg(feature = "test-provider")]
    pub async fn pending_skill_activations_for_test(&self) -> Vec<(String, String)> {
        self.pending_skills.lock().await.active.clone()
    }
}

#[async_trait]
impl CodeUiReadModel for AgentRuntimeCodeUiAdapter {
    fn session(&self) -> Arc<CodeUiSession> {
        self.session.clone()
    }
}

#[async_trait]
impl CodeUiCommandAdapter for AgentRuntimeCodeUiAdapter {
    fn capabilities(&self) -> CodeUiCapabilities {
        self.capabilities.clone()
    }

    /// DF-07: validate against the A0-07 curated registry (unknown provider
    /// or undiscoverable name fail closed there) and queue the activation
    /// for the next plain turn. No new registry, no widened tools.
    async fn activate_skill(
        &self,
        provider: &str,
        name: &str,
    ) -> Result<super::code_ui::CodeUiSkillActivationAck> {
        self.execution_control
            .skill_activate(&CodeSkillActivation {
                provider: provider.to_string(),
                name: name.to_string(),
            })?;
        let pending = self.pending_skills.lock().await.record(provider, name);
        Ok(super::code_ui::CodeUiSkillActivationAck {
            provider: provider.to_string(),
            name: name.to_string(),
            pending,
        })
    }

    async fn submit_message(&self, text: String) -> Result<()> {
        self.submit_message_with_command_id(text, None).await
    }

    async fn submit_message_with_command_id(
        &self,
        text: String,
        command_id: Option<String>,
    ) -> Result<()> {
        if let Some(admission) = self.web_admission.as_ref() {
            return admission
                .submit_message_with_command_id(&self.runtime, &self.session, text, command_id)
                .await;
        }
        if text.trim().is_empty() {
            return Err(anyhow!("Empty messages are not accepted by libra code"));
        }
        let turn_id = self.turn_id(command_id)?;
        {
            let mut active_turn = self.active_turn.lock().await;
            if let Some(existing) = active_turn.as_ref() {
                if existing.turn_id == turn_id {
                    if existing.text == text {
                        // Idempotent retry with the same payload.
                        return Ok(());
                    }
                    return Err(RuntimeWorkerError::CommandPayloadConflict {
                        session_id: self.runtime_session_id.clone(),
                        turn_id,
                    }
                    .into());
                }
                return Err(anyhow!(
                    "A Code UI turn is already active; cancel it or wait for it to finish"
                ));
            }
            // Reserve before awaiting observe/submit so a concurrent caller
            // cannot also admit a second tool-capable turn into the worker.
            *active_turn = Some(ActiveTurnSlot {
                turn_id: turn_id.clone(),
                text: text.clone(),
            });
        }
        // Subscribe before admission so a fast terminal broadcast cannot be
        // missed between submit and the release watcher.
        let stream = match self
            .runtime
            .observe(EventCursor::new(self.runtime_session_id.clone(), 0))
            .await
        {
            Ok(stream) => stream,
            Err(error) => {
                self.rollback_active_turn(&turn_id).await;
                return Err(Self::map_runtime_error(error));
            }
        };
        // DF-07: definitive provider-turn seam for the direct path — every
        // guard passed and the slot is reserved. The raw text is the turn's
        // durable identity; the activation block rides as execution-only
        // provider context, and a rejected submission restores it.
        let composed = self.pending_skills.lock().await.compose_context(&text);
        let mut turn_request = TurnRequest::new(
            self.runtime_session_id.clone(),
            turn_id.clone(),
            text.clone(),
            true,
        );
        if let Some(composed) = composed.as_ref() {
            turn_request = turn_request.with_provider_context(composed.context.clone());
        }
        if let Err(error) = self.runtime.submit(turn_request).await {
            if let Some(composed) = composed {
                self.pending_skills.lock().await.restore(composed);
            }
            self.rollback_active_turn(&turn_id).await;
            // A terminal idempotent retry of a succeeded command is an
            // acknowledgement, not a failure (terra R2); the consumed
            // activations were already restored above.
            if let RuntimeWorkerError::IdempotentCommand { ack_ok: true, .. } = &error {
                return Ok(());
            }
            return Err(Self::map_runtime_error(error));
        }
        self.spawn_release_watcher(stream, turn_id);
        Ok(())
    }

    async fn respond_interaction(
        &self,
        interaction_id: &str,
        response: CodeUiInteractionResponse,
    ) -> Result<()> {
        if let Some(admission) = self.web_admission.as_ref() {
            return admission
                .respond_interaction(&self.runtime, &self.session, interaction_id, response)
                .await;
        }
        let turn_id = self
            .active_turn
            .lock()
            .await
            .as_ref()
            .map(|slot| slot.turn_id.clone())
            .ok_or_else(|| {
                anyhow!(CodeUiApiError::conflict(
                    "INTERACTION_NOT_ACTIVE",
                    format!(
                        "interaction '{interaction_id}' has no active AgentRuntime turn to receive a response"
                    )
                ))
            })?;
        let response = serde_json::to_string(&response)
            .map_err(|error| anyhow!("failed to encode interaction response: {error}"))?;
        self.runtime
            .respond(
                self.runtime_session_id.clone(),
                turn_id,
                InteractionResponse::new(interaction_id, response),
            )
            .await
            .map_err(Self::map_runtime_error)
    }

    async fn cancel_turn(&self) -> Result<()> {
        if let Some(admission) = self.web_admission.as_ref() {
            return admission
                .cancel_turn(&self.runtime, &self.session, |state| match state {
                    InteractionState::AwaitingIntentReview { interaction_id }
                    | InteractionState::AwaitingPlanReview { interaction_id }
                    | InteractionState::AwaitingPlanRepair { interaction_id }
                    | InteractionState::AwaitingNetworkPolicy { interaction_id }
                    | InteractionState::AwaitingUserInput { interaction_id }
                    | InteractionState::AwaitingToolApproval { interaction_id, .. } => {
                        Some(interaction_id.as_str())
                    }
                    InteractionState::Idle
                    | InteractionState::Queued
                    | InteractionState::Running
                    | InteractionState::Cancelling
                    | InteractionState::Completed
                    | InteractionState::Failed { .. }
                    | InteractionState::Cancelled
                    | InteractionState::IndeterminateSideEffect { .. } => None,
                })
                .await;
        }
        let turn_id = self
            .active_turn
            .lock()
            .await
            .as_ref()
            .map(|slot| slot.turn_id.clone());
        let Some(turn_id) = turn_id else {
            return Ok(());
        };
        self.runtime
            .cancel(self.runtime_session_id.clone(), turn_id)
            .await
            .map_err(Self::map_runtime_error)
        // Do not clear here: CancelRequested is not terminal. The observe task
        // releases the slot on TurnCancelled / IndeterminateSideEffect / Failed.
    }

    async fn task_dispatch(&self, agent: String, prompt: String) -> Result<String> {
        if let Some(admission) = self.web_admission.as_ref() {
            admission.ensure_not_shutting_down()?;
            admission
                .ensure_session_is_recoverable(&self.session)
                .await?;
        }
        self.execution_control.task_dispatch(agent, prompt).await
    }

    async fn goal_start(&self, objective: String) -> Result<String> {
        if let Some(admission) = self.web_admission.as_ref() {
            admission.ensure_not_shutting_down()?;
        }
        self.execution_control
            .goal_start(objective)
            .await
            .map_err(Into::into)
    }

    async fn goal_status(&self) -> Result<String> {
        self.execution_control
            .goal_status()
            .await
            .map_err(Into::into)
    }

    async fn goal_cancel(&self, reason: String) -> Result<String> {
        if let Some(admission) = self.web_admission.as_ref() {
            admission.ensure_not_shutting_down()?;
        }
        self.execution_control
            .goal_cancel(reason)
            .await
            .map_err(Into::into)
    }

    async fn shutdown(&self) -> Result<()> {
        let hook = self
            .lifecycle_shutdown
            .lock()
            .await
            .as_ref()
            .and_then(Weak::upgrade);
        if let Some(hook) = hook {
            return hook.shutdown().await;
        }
        Ok(())
    }

    async fn on_controller_lease_takeover(&self) -> Result<()> {
        if let Some(store) = self.approval_store.lock().await.clone() {
            revoke_session_approval_memos(&store).await;
        }
        self.runtime
            .drop_pending_after_lease_takeover(&self.runtime_session_id)
            .await
            .map_err(Self::map_runtime_error)?;
        if let Some(admission) = self.web_admission.as_ref() {
            admission
                .clear_pending_tool_interactions(&self.session)
                .await?;
        } else {
            self.session.clear_pending_tool_interactions().await;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::PendingSkillContext;

    /// DF-07: plain turns compose+consume the context block at the submit
    /// seam, slash/empty text passes through (defense in depth — routing
    /// already keeps those away), and duplicates hold one slot. The block
    /// never contains the user text: it rides `TurnRequest::provider_context`
    /// while the raw text stays the durable identity.
    #[test]
    fn pending_skill_context_composes_only_plain_text() {
        let mut ctx = PendingSkillContext::default();
        assert_eq!(ctx.record("claude-code", "/review"), 1);
        assert_eq!(ctx.record("claude-code", "/review"), 1, "dedup");
        assert_eq!(ctx.record("codex", "/plan"), 2);

        assert!(ctx.compose_context("/help").is_none());
        assert!(ctx.compose_context("   ").is_none());
        assert_eq!(ctx.active.len(), 2, "still pending after control text");

        let composed = ctx
            .compose_context("review my diff")
            .expect("plain text composes");
        assert!(
            composed.context.contains("[skill activation]")
                && composed.context.contains("- '/review' (claude-code)")
                && composed.context.contains("- '/plan' (codex)"),
            "activation block must list the consumed set: {}",
            composed.context
        );
        assert!(
            !composed.context.contains("review my diff"),
            "the block is context-only; raw text stays the identity"
        );
        assert!(ctx.active.is_empty(), "consumed at the seam");
        assert!(
            ctx.compose_context("next message").is_none(),
            "a later turn must not re-inject"
        );
    }

    /// DF-07 (terra R1/R3): a failed submission restores the consumed set
    /// in order, and the next successful plain turn still carries it.
    #[test]
    fn pending_skill_context_restores_on_failed_submission() {
        let mut ctx = PendingSkillContext::default();
        ctx.record("claude-code", "/review");
        ctx.record("codex", "/plan");
        let composed = ctx.compose_context("do it").expect("composes");
        assert!(ctx.active.is_empty());

        ctx.restore(composed);
        assert_eq!(
            ctx.active,
            vec![
                ("claude-code".to_string(), "/review".to_string()),
                ("codex".to_string(), "/plan".to_string())
            ],
            "restore must reinstate the consumed set in order"
        );
        let retried = ctx.compose_context("do it").expect("recomposes");
        assert!(
            retried.context.contains("'/review'") && retried.context.contains("'/plan'"),
            "the next successful attempt still carries the activations"
        );
    }

    /// DF-07 (terra R5): a skill re-activated while its consumed turn was
    /// still in flight must hold ONE pending slot after the failed turn
    /// restores — never a duplicate context line.
    #[test]
    fn pending_skill_context_restore_dedupes_against_reactivation() {
        let mut ctx = PendingSkillContext::default();
        ctx.record("claude-code", "/review");
        let composed = ctx.compose_context("do it").expect("composes");
        // Re-activate the same skill while the turn is in flight…
        assert_eq!(ctx.record("claude-code", "/review"), 1);
        ctx.record("codex", "/review");
        // …then the turn fails before the provider starts.
        ctx.restore(composed);
        assert_eq!(
            ctx.active,
            vec![
                ("claude-code".to_string(), "/review".to_string()),
                ("codex".to_string(), "/review".to_string())
            ],
            "restore must keep one ordered slot per (provider, name)"
        );
        let recomposed = ctx.compose_context("retry").expect("recomposes");
        assert_eq!(
            recomposed
                .context
                .matches("- '/review' (claude-code)")
                .count(),
            1,
            "no duplicate context lines: {}",
            recomposed.context
        );
    }
}
