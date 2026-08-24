//! Sub-agent dispatcher contract — types and trait definitions only.
//!
//! This module started as the OC-Phase 3 P3.2 vocabulary from
//! `docs/development/commands/_general.md` and now also carries the shared
//! runtime collaborators used by the shipped dispatcher tail:
//! [`SubAgentDispatcher`], [`DispatchContext`], the task
//! invocation/result/failure shapes, permission asking, context-frame
//! loading, provider-option resolution, and the default child runner.
//!
//! What this module is:
//! - Forward-stable shapes and small services the dispatcher and tool
//!   loop share. Any field rename here has to update the contract
//!   section of `opencode.md` first.
//! - The object-safe seams that let tests substitute permission
//!   askers, provider-option resolvers, and child runners without
//!   constructing a full terminal-UI session.
//!
//! What this module is **not**:
//! - It does not register the `task` tool — that is OC-Phase 3 P3.1's
//!   `ToolSpec::task()` constructor, gated separately.
//! - It does not implement the 14-step dispatcher main flow from the
//!   doc — that lands in P3.3 / P3.4 / P3.7.
//! - It does not own the runtime services it references
//!   ([`PermissionService`], [`ContextFrameLoader`]). Their method
//!   signatures are the public contract; their bodies are intentionally
//!   small and test-pinned.

use std::sync::{
    Arc, Mutex,
    atomic::{AtomicBool, Ordering},
};

use futures::future::BoxFuture;
use serde::{Deserialize, Serialize};

use crate::internal::ai::{
    agent::profile::{AgentExecutionSpec, ModelBinding},
    agent_run::AgentRunId,
    completion::{CompletionError, CompletionUsageSummary},
    context_budget::frame::ContextFrameEvent,
    permission::PermissionRuleset,
    providers::{ProviderBuildOptions, ProviderFactory},
    sandbox::ToolRuntimeContext,
    session::{
        SessionId, SessionMessage, SessionState,
        jsonl::{SessionEvent, SessionJsonlStore, SessionToolCallEvent, SessionToolResultEvent},
    },
    tools::{ToolOutput, ToolRegistry},
    usage::UsageRecorder,
};

/// Opaque alias for a runtime message identifier. Today's runtime stores
/// these as strings (the JSONL row id of the assistant message that
/// triggered the dispatch); a later PR may promote it to a newtype with
/// stricter parsing.
pub type MessageId = String;

/// Inputs the dispatcher needs to materialize an isolated per-run
/// workspace for a sub-agent (CEX-S2-12 / S2-INV-03).
///
/// Attached to `DefaultSubAgentDispatcher` via
/// `with_workspace_isolation` (set by `libra code`'s
/// `build_subagent_runtime_for_session`). When present, the dispatcher
/// materializes a workspace through
/// [`crate::internal::ai::orchestrator::workspace::materialize_sub_agent_workspace`],
/// re-roots the child tool registry onto it, and rebases the inherited
/// sandbox `writable_roots` to it before running the child — so a
/// sub-agent's writes land in the workspace, not the main worktree.
/// `None` (every existing test and any flag-off path) means no
/// isolation: behaviour is byte-for-byte identical to before this slice.
#[derive(Clone)]
pub struct WorkspaceIsolationConfig {
    /// Per-session FUSE-provisioning state (degrades to copy backend
    /// when FUSE is unavailable). `Arc`-backed, cheap to clone.
    pub fuse_state: crate::internal::ai::orchestrator::workspace::FuseProvisionState,
    /// `.libra/sessions` root the per-run `AgentRunEventStore` writes the
    /// `WorkspaceMaterialized` event under.
    pub sessions_root: std::path::PathBuf,
    /// Whether an expensive full-copy fallback is permitted when the
    /// preferred (size-selected) strategy cannot be materialized
    /// (`code.multi_agent.allow_full_copy`).
    pub allow_full_copy: bool,
}

impl std::fmt::Debug for WorkspaceIsolationConfig {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("WorkspaceIsolationConfig")
            .field("sessions_root", &self.sessions_root)
            .field("allow_full_copy", &self.allow_full_copy)
            .finish_non_exhaustive()
    }
}

// ─── Cancellation primitive ─────────────────────────────────────────────

/// Cooperative cancellation token threaded through a sub-agent dispatch.
///
/// The token is intentionally small and dependency-free, but it provides
/// the key P3.7 invariant the dispatcher needs: cancelling a parent
/// token propagates to every child token created from it, and waiters can
/// asynchronously observe the cancellation.
#[derive(Clone, Default)]
pub struct AbortToken {
    inner: Arc<AbortInner>,
}

#[derive(Default)]
struct AbortInner {
    cancelled: AtomicBool,
    notify: tokio::sync::Notify,
    children: Mutex<Vec<AbortToken>>,
}

impl AbortToken {
    /// Create a fresh, un-cancelled root token.
    pub fn new() -> Self {
        Self::default()
    }

    /// Trigger cancellation. After this returns, `is_cancelled()` is true.
    pub fn cancel(&self) {
        if self.inner.cancelled.swap(true, Ordering::AcqRel) {
            return;
        }

        let children = self
            .inner
            .children
            .lock()
            .map(|children| children.clone())
            .unwrap_or_default();
        for child in children {
            child.cancel();
        }
        self.inner.notify.notify_waiters();
    }

    /// Returns `true` once cancellation has been requested on this token.
    pub fn is_cancelled(&self) -> bool {
        self.inner.cancelled.load(Ordering::Acquire)
    }

    /// Wait until cancellation is requested.
    pub async fn cancelled(&self) {
        loop {
            if self.is_cancelled() {
                return;
            }
            let notified = self.inner.notify.notified();
            if self.is_cancelled() {
                return;
            }
            notified.await;
        }
    }

    /// Spawn a child token whose cancellation follows this parent.
    pub fn child(&self) -> Self {
        let child = Self::default();
        if self.is_cancelled() {
            child.cancel();
            return child;
        }

        match self.inner.children.lock() {
            Ok(mut children) => children.push(child.clone()),
            Err(_) => {
                child.cancel();
                return child;
            }
        }

        if self.is_cancelled() {
            child.cancel();
        }
        child
    }
}

impl std::fmt::Debug for AbortToken {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AbortToken")
            .field("cancelled", &self.is_cancelled())
            .finish_non_exhaustive()
    }
}

// ─── Permission service: real trait-shaped contract (OC-Phase 3 P3.4) ──

/// User reply to a `permission.ask(...)` prompt. Mirrors the doc's
/// three-state reply (`Once` / `Always` / `Reject`) so a future opencode
/// interchange is a structural copy.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum PermissionReply {
    /// Allow this single call. Do not persist.
    Once,
    /// Allow this call and persist `(permission, pattern)` rows for the
    /// listed patterns into the project's `approved_permission` table
    /// (OC-Phase 2 P2.5). The patterns are typically the request's
    /// declared patterns, but the user-facing surface may narrow them
    /// (e.g. only the path the assistant is acting on).
    Always { patterns: Vec<String> },
    /// Refuse this call. Optional `feedback` is forwarded to the model
    /// as a tool-result error so it can adjust subsequent behaviour.
    Reject { feedback: Option<String> },
}

/// Where a permission ask originated. The dispatcher passes
/// `SubAgentSpawn` for the LlmInitiated step-8 ask; future call sites
/// (shell escalation, edit on protected files) will append more
/// variants without breaking the trait surface.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum PermissionAskSource {
    /// The parent agent is about to dispatch a sub-agent via the
    /// `task` tool. `prompt_digest` is a short hash / preview of the
    /// outgoing prompt so the user can recognise the dispatch in logs.
    SubAgentSpawn { name: String, prompt_digest: String },
}

/// One permission request flowing through the service.
///
/// Borrowed shape so the caller does not have to clone the parent's
/// thread / session ids per ask. Extending the request later is a
/// non-breaking change as long as new fields are appended.
pub struct PermissionAskRequest<'a> {
    pub permission: &'a str,
    pub patterns: &'a [String],
    pub thread_id: &'a str,
    pub session_id: &'a SessionId,
    pub source: PermissionAskSource,
}

/// Object-safe trait the [`PermissionService`] delegates to.
///
/// Implementations land in P3.4+ (interactive Code UI prompt, automation
/// API, programmatic always-allow / always-reject for tests). Today the
/// trait is the contract the dispatcher's step-8 call site speaks.
pub trait PermissionAsker: Send + Sync {
    fn ask<'a>(
        &'a self,
        request: PermissionAskRequest<'a>,
    ) -> futures::future::BoxFuture<'a, PermissionReply>;
}

/// Permission service handed into [`DispatchContext`].
///
/// A thin wrapper around an `Arc<dyn PermissionAsker>` so the dispatcher
/// holds a stable reference type while the underlying asker can be
/// swapped without touching the dispatch signature. Constructors that
/// take an explicit asker reflect the doc's contract: there is no safe
/// default, every production caller must supply an asker that matches
/// its surface (Code UI prompt, automation queue, etc.).
pub struct PermissionService {
    asker: std::sync::Arc<dyn PermissionAsker>,
}

impl PermissionService {
    /// Wrap an asker. Use [`Self::with_asker`] for `Arc` convenience.
    pub fn new(asker: std::sync::Arc<dyn PermissionAsker>) -> Self {
        Self { asker }
    }

    /// Convenience constructor that takes any concrete asker type and
    /// boxes it into the service. Useful for tests.
    pub fn with_asker<A: PermissionAsker + 'static>(asker: A) -> Self {
        Self::new(std::sync::Arc::new(asker))
    }

    /// Forward an ask through the underlying asker.
    pub async fn ask(&self, request: PermissionAskRequest<'_>) -> PermissionReply {
        self.asker.ask(request).await
    }
}

/// One permission ask flowing across the channel-backed asker
/// (v0.17.787). The Code UI widget receives this on its consumer side,
/// renders the prompt, and sends a `PermissionReply` back through
/// `reply_tx`. Mirrors the existing exec-approval flow's shape so
/// a future UI integration can reuse the same widget skeleton.
///
/// `permission`, `patterns`, `thread_id`, `session_id`, `source`
/// are owned String/clone copies of the original
/// [`PermissionAskRequest`]'s borrowed shape — the channel needs
/// 'static data because the asker future is detached from the
/// dispatch lifetime.
#[derive(Debug)]
pub struct ChannelPermissionAsk {
    pub permission: String,
    pub patterns: Vec<String>,
    pub thread_id: String,
    pub session_id: SessionId,
    pub source: PermissionAskSource,
    pub reply_tx: tokio::sync::oneshot::Sender<PermissionReply>,
}

/// Channel-backed [`PermissionAsker`] that delegates every ask to
/// a remote session-UI consumer. The dispatcher
/// awaits the consumer's reply via the `oneshot` channel embedded
/// in each [`ChannelPermissionAsk`].
///
/// Failure modes (consumer dropped, channel send error) degrade to
/// `PermissionReply::Reject { feedback: Some("...") }` so a missing
/// consumer behaves like an explicit deny rather than hanging the
/// dispatch. This matches the [`DenyByDefaultPermissionAsker`]
/// posture: the safer failure is to reject, not to allow.
///
/// Construction:
/// ```ignore
/// let (tx, rx) = tokio::sync::mpsc::unbounded_channel();
/// let asker = ChannelPermissionAsker::new(tx);
/// // hand `asker` to PermissionService; consume `rx` in the session UI
/// // thread, render the prompt, send the reply via
/// // `ask.reply_tx.send(PermissionReply::Once)`.
/// ```
pub struct ChannelPermissionAsker {
    tx: tokio::sync::mpsc::UnboundedSender<ChannelPermissionAsk>,
}

impl ChannelPermissionAsker {
    pub fn new(tx: tokio::sync::mpsc::UnboundedSender<ChannelPermissionAsk>) -> Self {
        Self { tx }
    }
}

impl PermissionAsker for ChannelPermissionAsker {
    fn ask<'a>(
        &'a self,
        request: PermissionAskRequest<'a>,
    ) -> futures::future::BoxFuture<'a, PermissionReply> {
        let (reply_tx, reply_rx) = tokio::sync::oneshot::channel();
        let ask = ChannelPermissionAsk {
            permission: request.permission.to_string(),
            patterns: request.patterns.to_vec(),
            thread_id: request.thread_id.to_string(),
            session_id: request.session_id.clone(),
            source: request.source,
            reply_tx,
        };
        let send_result = self.tx.send(ask);
        Box::pin(async move {
            if send_result.is_err() {
                return PermissionReply::Reject {
                    feedback: Some(
                        "permission ask channel closed (session UI consumer dropped); \
                         dispatch rejected — restart `libra code` to reattach the asker"
                            .to_string(),
                    ),
                };
            }
            reply_rx.await.unwrap_or(PermissionReply::Reject {
                feedback: Some(
                    "permission ask reply channel closed before the session UI consumer answered; \
                     dispatch rejected — the session UI may have exited during the ask"
                        .to_string(),
                ),
            })
        })
    }
}

/// Conservative production fallback asker that rejects every
/// permission ask with a generic feedback message. Used by the
/// libra-code session bootstrap (v0.17.776) when sub-agents are
/// enabled but no interactive prompt path is wired yet — this
/// keeps `UserInitiated{bypass_permission_ask:true}` dispatches
/// working (slash-command `/task` paths) while ensuring any
/// LlmInitiated dispatch that needs an escalation fails fast
/// rather than silently allowing an unreviewed permission.
///
/// A full Code UI-bound asker that routes prompts through the same
/// review widget the existing exec-approval flow uses is the
/// follow-up; this fallback is intentionally narrow so that
/// future work has a single replacement target.
#[derive(Debug, Default, Clone, Copy)]
pub struct DenyByDefaultPermissionAsker;

impl PermissionAsker for DenyByDefaultPermissionAsker {
    fn ask<'a>(
        &'a self,
        request: PermissionAskRequest<'a>,
    ) -> futures::future::BoxFuture<'a, PermissionReply> {
        // Log the denied escalation so operators have a discoverable
        // trace of "my sub-agent needs permission X" without needing
        // an interactive prompt. The Code UI follow-up that wires a
        // real PermissionAsker replaces this asker entirely; the
        // trace is the diagnostic bridge until then.
        tracing::warn!(
            permission = request.permission,
            patterns = ?request.patterns,
            thread_id = request.thread_id,
            session_id = %request.session_id,
            source = ?request.source,
            "DenyByDefaultPermissionAsker rejecting permission escalation; \
             add the rule to [code.agents.<name>.permission] in .libra/agents.toml \
             to grant it without an interactive prompt",
        );
        Box::pin(async {
            PermissionReply::Reject {
                feedback: Some(
                    "permission escalation rejected by the default deny-all asker; \
                     either pre-grant the permission via [code.agents.<name>.permission] \
                     in .libra/agents.toml, or wire an interactive PermissionAsker via \
                     libra-code session bootstrap"
                        .to_string(),
                ),
            }
        })
    }
}

impl std::fmt::Debug for PermissionService {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // The asker is opaque; surface only the wrapper's identity so a
        // production debug log does not accidentally include sensitive
        // state from a concrete asker.
        f.debug_struct("PermissionService").finish_non_exhaustive()
    }
}

// ─── Other runtime service placeholders ─────────────────────────────────

/// Loads the latest `ContextFrameEvent` from a session JSONL store so
/// the dispatcher can build a `ContextHandoff` via
/// `ContextHandoffBuilder`.
///
/// Today the loader is a pure scan over the events already on disk —
/// no caching, no projection index. The OC-Phase 3 dispatcher needs
/// "the most recent ContextFrame the parent emitted" and nothing
/// more; richer queries (multi-frame splice, attachment dedup) belong
/// to the orchestrator/projection layer.
///
/// `Default` returns a loader with no fallback frame — every load
/// path consults the supplied session store. A future P4.5 PR may
/// add an in-memory baseline; until then `Default` is what tests use
/// when they don't care about handoff content.
///
#[derive(Debug, Default)]
pub struct ContextFrameLoader {
    _marker: (),
}

impl ContextFrameLoader {
    /// Build a fresh loader. Exists alongside `Default::default()` so
    /// call sites that prefer constructor syntax read consistently
    /// with the rest of the runtime services.
    pub fn new() -> Self {
        Self::default()
    }

    /// Walk the session's JSONL stream and return the most recent
    /// `ContextFrameEvent`, or
    /// `None` if the stream carries no frame events yet (a fresh
    /// session, or one whose frames have been pruned).
    ///
    /// IO errors surface verbatim so the dispatcher can decide
    /// whether to fall through with a degraded handoff or refuse the
    /// dispatch; the loader itself takes no policy stance.
    pub fn latest_frame_for_session(
        &self,
        store: &SessionJsonlStore,
    ) -> std::io::Result<Option<ContextFrameEvent>> {
        let events = store.load_events()?;
        let mut latest = None;
        for event in events {
            if let SessionEvent::ContextFrame(frame) = event {
                latest = Some(frame);
            }
        }
        Ok(latest)
    }
}

// ─── Invocation / Result / Entry Kind ───────────────────────────────────

/// The shape the model emits when it calls the `task` tool.
///
/// Mirrors `TaskInvocation` from `docs/development/commands/_general.md` and the
/// JSON schema returned by `ToolSpec::task()` (OC-Phase 3 P3.1).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TaskInvocation {
    /// Short human-readable summary surfaced in transcripts and budget logs.
    pub description: String,
    /// The user-message body sent to the sub-agent.
    pub prompt: String,
    /// Name of the agent profile to dispatch.
    pub subagent_type: String,
    /// Optional resume token from a prior dispatch under the same parent
    /// thread.
    pub task_id: Option<String>,
}

/// Successful return shape from the dispatcher.
///
/// `usage` is a [`CompletionUsageSummary`] aggregating every model call
/// inside the child run (assistant turns + any nested tool retries).
/// `agent_name` / `provider_id` / `model_id` come from the resolved
/// [`AgentExecutionSpec`]; they are echoed back so the parent transcript
/// records exactly what ran.
#[derive(Clone, Debug, PartialEq)]
pub struct TaskResult {
    pub task_id: String,
    pub agent_name: String,
    pub provider_id: String,
    pub model_id: String,
    pub final_text: String,
    pub steps_used: u32,
    pub usage: CompletionUsageSummary,
}

/// How a `task` dispatch entered the dispatcher.
///
/// `LlmInitiated` is the regular flow: the parent agent's model emitted a
/// `task(...)` tool call. `UserInitiated` is the slash-command / Code
/// Control / SubtaskPart flow where the user explicitly typed the
/// dispatch. Per the doc "Two Entry Points" table:
///
/// - `LlmInitiated` ⇒ runs `permission.ask({permission:"task", ...})`.
/// - `UserInitiated { bypass_permission_ask: true }` ⇒ skips the
///   approval dialog (the user already chose) but still runs every other
///   gate (depth, concurrency, SafetyDecision, escalation).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TaskEntryKind {
    LlmInitiated,
    UserInitiated { bypass_permission_ask: bool },
}

// ─── Failure taxonomy ──────────────────────────────────────────────────

/// Reason an `Always`-reply approval would have been required but the
/// dispatcher refused to dispatch. Wraps a human-readable explanation
/// produced by the SafetyDecision module (Step 1.1) — the placeholder is
/// a string until that module's denial type stabilises.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SafetyDecisionDenial {
    pub reason: String,
}

/// Why a budget gate fired during dispatch. Each variant carries a short
/// human-readable label so UI projections can render the limit; the
/// numeric thresholds live on the budget config.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum BudgetExceededReason {
    /// The dispatcher refused to enter because a hard cost cap is
    /// already at or past its limit.
    CostHardCap,
    /// The dispatcher refused to enter because a hard token cap is
    /// already at or past its limit.
    TokenHardCap,
    /// Wall-clock budget for the parent session has expired before the
    /// child started.
    WallClock,
    /// A sub-agent step budget would be violated.
    Steps,
    /// Budget enforcement itself failed before a safe decision could
    /// be made. The dispatcher fails closed.
    Internal { reason: String },
}

/// Why the dispatcher could not assemble a `ContextHandoff` for the
/// child run. The `SchemaMismatch` variant matches the literal-template
/// validation rule from `opencode.md` (OC-Phase 4 P4.3).
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ContextHandoffError {
    /// The compaction agent's summary did not contain every required
    /// `## ...` heading from the literal SUMMARY_TEMPLATE.
    SchemaMismatch { missing_sections: Vec<String> },
    /// The compaction agent itself failed (provider error, timeout, etc.).
    CompactionFailed { reason: String },
    /// The parent session had no `ContextFrameEvent` available to seed
    /// the handoff from.
    NoFrameAvailable,
}

/// Why the child tool loop returned a non-completion outcome. Wraps the
/// generic completion error today; OC-Phase 4 P4.1 will extend this to
/// the structured `ProviderError` taxonomy.
#[derive(Debug)]
pub enum ToolLoopError {
    Completion(CompletionError),
    /// Step budget hit before a final answer arrived.
    StepBudgetExhausted {
        steps: u32,
    },
}

/// Who triggered a cancellation. Surfaced inside
/// [`TaskFailure::Cancelled`] so the parent can distinguish "user
/// pressed Ctrl-C" from "child hit its own budget hard cap".
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CancellationSource {
    /// The parent session's abort token fired (Ctrl-C, `/cancel`,
    /// Code Control `cancel` RPC).
    ParentAbort,
    /// The child's own budget tripped its hard cap.
    BudgetHardCap,
    /// The child's wall-clock timeout ran out.
    Timeout,
}

/// Structured failure modes returned by the dispatcher.
///
/// Each variant mirrors the doc's `TaskFailure` enum literally so a
/// future opencode-shaped JSON event for these cases is a structural
/// copy. New variants must be added BOTH here AND to `opencode.md`.
#[derive(Debug)]
pub enum TaskFailure {
    /// `code.multi_agent.enabled` is `false` and the dispatcher refused
    /// the spawn at step 1 of the gate flow. Distinct from
    /// [`Self::SafetyDenied`] (which is the step-5 SafetyDecision
    /// rejection) so log analysis and error matchers can disambiguate
    /// "feature off" from "sandbox said no". This variant is **not**
    /// in opencode's original taxonomy — it is a Libra-specific addition
    /// because the feature flag lives in `[code.multi_agent]` config
    /// (OC-Phase 5) instead of being a deploy-time toggle.
    FeatureDisabled,
    /// `subagent_type` did not resolve to a profile, or the profile's
    /// mode was `Primary` (not eligible for sub-agent dispatch).
    UnknownSubagent {
        name: String,
        suggestions: Vec<String>,
    },
    /// `ctx.depth + 1 > max_subagent_depth`. P3.3 enforces the gate.
    DepthExceeded { current: u8, limit: u8 },
    /// More child dispatches in flight than `max_concurrent_subagents`.
    ConcurrencyExceeded { current: u32, limit: u32 },
    /// The escalation gate (Permission Escalation Gate) refused — a
    /// child rule would have flipped a parent `Deny` to `Allow`.
    PermissionEscalationDenied { permission: String, pattern: String },
    /// SafetyDecision module refused the spawn before any other gate.
    SafetyDenied(SafetyDecisionDenial),
    /// The user replied `Reject` to the `LlmInitiated` permission ask;
    /// the optional `feedback` carries the user's typed reason.
    ApprovalRejected { feedback: Option<String> },
    /// Budget gate fired (cost / wall-clock / steps).
    BudgetExceeded(BudgetExceededReason),
    /// Context handoff failed; the dispatcher refused to fall back to
    /// raw transcript so the model never sees an unbounded history.
    ContextHandoffFailed(ContextHandoffError),
    /// The child's own provider call failed (network, auth, malformed
    /// response).
    ProviderError(CompletionError),
    /// The child tool loop returned an error — distinct from a clean
    /// `final_text` outcome.
    ChildToolLoopFailed(ToolLoopError),
    /// Cancellation propagated from `source`.
    Cancelled { source: CancellationSource },
    /// Wall-clock deadline hit before the child finished.
    Timeout { wall_clock_ms: u64 },
}

impl std::fmt::Display for TaskFailure {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::FeatureDisabled => write!(f, "multi-agent dispatch is disabled"),
            Self::UnknownSubagent { name, suggestions } => {
                if suggestions.is_empty() {
                    write!(f, "unknown sub-agent `{name}`")
                } else {
                    write!(
                        f,
                        "unknown sub-agent `{name}`; available sub-agents: {}",
                        suggestions.join(", ")
                    )
                }
            }
            Self::DepthExceeded { current, limit } => write!(
                f,
                "sub-agent depth {current} exceeds configured limit {limit}"
            ),
            Self::ConcurrencyExceeded { current, limit } => write!(
                f,
                "sub-agent concurrency {current} exceeds configured limit {limit}"
            ),
            Self::PermissionEscalationDenied {
                permission,
                pattern,
            } => write!(
                f,
                "permission escalation denied for `{permission}:{pattern}`"
            ),
            Self::SafetyDenied(denial) => {
                write!(f, "safety policy denied sub-agent spawn: {}", denial.reason)
            }
            Self::ApprovalRejected { feedback } => match feedback {
                Some(feedback) if !feedback.trim().is_empty() => {
                    write!(f, "sub-agent dispatch approval rejected: {feedback}")
                }
                _ => write!(f, "sub-agent dispatch approval rejected"),
            },
            Self::BudgetExceeded(reason) => {
                write!(f, "sub-agent budget exceeded: {reason:?}")
            }
            Self::ContextHandoffFailed(reason) => {
                write!(f, "failed to prepare sub-agent context handoff: {reason:?}")
            }
            Self::ProviderError(error) => write!(f, "sub-agent provider error: {error}"),
            Self::ChildToolLoopFailed(ToolLoopError::Completion(error)) => {
                write!(f, "sub-agent tool loop failed: {error}")
            }
            Self::ChildToolLoopFailed(ToolLoopError::StepBudgetExhausted { steps }) => {
                write!(f, "sub-agent step budget exhausted after {steps} step(s)")
            }
            Self::Cancelled { source } => write!(f, "sub-agent cancelled by {source:?}"),
            Self::Timeout { wall_clock_ms } => {
                write!(f, "sub-agent timed out after {wall_clock_ms} ms")
            }
        }
    }
}

impl std::error::Error for TaskFailure {}

// ─── DispatchContext + SubAgentDispatcher trait ─────────────────────────

/// Context the parent session passes to the dispatcher.
///
/// Every reference is borrowed from the live runtime; the dispatcher
/// returns a future whose lifetime ties back to `'a` so the borrow
/// graph is checked at compile time. Once P3.3 lands, the dispatcher
/// implementation will hold each of these references long enough to
/// drive the child run and then drop them.
pub struct DispatchContext<'a> {
    /// Stable thread id under which both parent and child events live.
    pub parent_thread_id: &'a str,
    /// Durable runtime turn that owns this child provider usage. Child rows
    /// retain this turn while `agent_run_id` distinguishes the child run.
    pub parent_turn_id: Option<&'a str>,
    /// Parent session id (the [`SessionId`] alias is `String` today).
    pub parent_session_id: &'a SessionId,
    /// The parent agent's resolved spec — drives permission inheritance
    /// and is forwarded into the child handoff.
    pub parent_agent: &'a AgentExecutionSpec,
    /// Parent ruleset; the child inherits the doc's
    /// `child_ruleset(parent, sub_spec)` projection.
    pub parent_ruleset: &'a PermissionRuleset,
    /// Parent's resolved model binding. Unused by the gates but
    /// surfaced into the child run's handoff so the model knows what
    /// mode the parent was running in.
    pub parent_model_binding: &'a ModelBinding,
    /// The assistant message id that emitted the `task` tool call.
    /// Forwarded to `AgentRunEvent::Spawned` for replay.
    pub parent_message_id: MessageId,
    /// Permission service the dispatcher delegates `Ask` replies to.
    /// **The child's permission service must be the parent's instance**
    /// (not a fresh one) per the doc — children cannot self-approve.
    pub permission_service: &'a PermissionService,
    /// Session JSONL store for the parent. The dispatcher writes the
    /// `Spawned` / `Completed` / `Failed` events on the parent side and
    /// creates the child's JSONL on the same backing store.
    pub session_store: &'a SessionJsonlStore,
    /// Provider factory used to build the child's `AnyCompletionModel`.
    /// Stateless today (OC-Phase 1 P1.2) — the dispatcher just holds a
    /// reference for parity with the doc's contract.
    pub provider_factory: &'a ProviderFactory,
    /// Per-provider build options resolved by the parent runtime
    /// (API key, base URL, fake fixture, compact-tool mode). The
    /// dispatcher must not read process env directly.
    pub provider_build_options: &'a ProviderBuildOptions,
    /// Optional resolver for child-provider-specific build options.
    /// When absent, the dispatcher falls back to
    /// [`provider_build_options`](Self::provider_build_options). The
    /// resolver is what lets a parent and child use different LLM
    /// providers without letting runtime code read env or config files.
    pub provider_build_options_resolver: Option<&'a dyn ProviderBuildOptionsResolver>,
    /// Parent tool registry cloned into the child with schema-level
    /// pre-filtering. This keeps `task` out of normal ToolHandler
    /// dispatch while still letting sub-agents use ordinary tools.
    pub tool_registry: &'a ToolRegistry,
    /// Runtime sandbox / approval / file-history context inherited by
    /// child tool invocations. Child rulesets may narrow visible tools,
    /// but they must not get a fresh approval authority.
    pub runtime_context: Option<ToolRuntimeContext>,
    /// Usage recorder the child run pipes its rows into. The recorder is
    /// the parent's instance with `agent_run_id` bound to the child's
    /// id; OC-Phase 5 adds the `agent_name` column.
    pub usage_recorder: &'a UsageRecorder,
    /// Loader that materialises the parent's most recent
    /// `ContextFrameEvent` into a handoff structure.
    pub context_frame_loader: &'a ContextFrameLoader,
    /// Child's abort token. Created via `ctx.abort_token.child()` so a
    /// parent-side cancel propagates downstream (the placeholder in
    /// P3.2 hands back a fresh token; P3.7 makes the propagation real).
    pub abort_token: AbortToken,
    /// Stack depth of this dispatch. The session driver passes `0` for
    /// the first level; the dispatcher rejects `depth + 1 >
    /// max_subagent_depth`.
    pub depth: u8,
    /// OC-Phase 4 P4.4 compaction model (v0.17.785): when
    /// configured via `[code.compaction]`, the dispatcher tail
    /// routes the parent's latest `ContextFrameEvent` through
    /// `compaction_agent::run_compaction(...)` to produce a
    /// validated `ContextHandoff`, then materialises it via
    /// `ContextHandoff::to_handoff_messages` (v0.17.781). None
    /// falls through to `ContextFrameEvent::to_handoff_messages`
    /// (v0.17.773) — same raw-segment path the dispatcher used
    /// before P4.4 was wired.
    pub compaction_model: Option<&'a crate::internal::ai::providers::AnyCompletionModel>,
    /// OC-Phase 3 S2-INV-13 hook dispatch (v0.17.806): the parent's
    /// `HookRunner` so the child's tool_loop fires
    /// `PreToolUse` / `PostToolUse` hooks before / after every
    /// dispatched tool call. None means hooks are disabled (no
    /// `.libra/hooks.json` present); the child loop simply skips
    /// the hook calls in that case, matching the parent's
    /// behaviour. Forcing the parent's runner (rather than a
    /// fresh one per child) is required by S2-INV-13: child
    /// dispatchers must not have private hooks that the operator
    /// can't see in the project config.
    pub hook_runner: Option<&'a Arc<crate::internal::ai::hooks::HookRunner>>,
}

impl<'a> DispatchContext<'a> {
    /// Resolve [`ProviderBuildOptions`] for a child sub-agent's model
    /// binding. The OC-Phase 3 step 9 plumbing needs to build an
    /// `AnyCompletionModel` for the child via
    /// [`ProviderFactory::build`]; that call needs options. This
    /// helper centralises the "resolver if present, else fall back to
    /// the parent's" rule so the dispatcher tail doesn't have to
    /// repeat the match.
    ///
    /// `Err(reason)` surfaces verbatim from the resolver and is
    /// expected to be mapped by the caller into the right
    /// `TaskFailure` variant (typically `TaskFailure::Provider` once
    /// the P3.4 child-loop PR lands the constructor).
    pub fn resolve_provider_build_options(
        &self,
        binding: &ModelBinding,
    ) -> Result<ProviderBuildOptions, String> {
        match self.provider_build_options_resolver {
            Some(resolver) => resolver.resolve(binding),
            None => Ok(self.provider_build_options.clone()),
        }
    }

    /// Build the child sub-agent's `AnyCompletionModel` for OC-Phase 3
    /// P3.4 step 9 ("model build for child").
    ///
    /// Sequence:
    /// 1. Verify `sub_spec.model` is `Some`; without a binding the
    ///    sub-agent cannot run. Returns
    ///    [`TaskFailure::ProviderError`] with a descriptive message
    ///    rather than panicking — the dispatcher tail propagates this
    ///    verbatim into the parent's transcript.
    /// 2. Resolve [`ProviderBuildOptions`] via
    ///    [`Self::resolve_provider_build_options`] (resolver-first,
    ///    parent-clone fallback).
    /// 3. Call [`ProviderFactory::build`] with the resolved binding +
    ///    options. Factory errors map to
    ///    [`TaskFailure::ProviderError`] carrying a
    ///    [`CompletionError::ProviderError`] with the verbatim factory
    ///    error message so the operator can see exactly which
    ///    provider rejected the binding.
    ///
    /// The helper performs **no** I/O of its own — `ProviderFactory`
    /// is env-free and the factory's `build` is a sync constructor
    /// over already-resolved options. Networked provider clients open
    /// connections lazily on the first request, so the helper can be
    /// called from the dispatcher's sync gate path without making the
    /// future block.
    pub fn build_child_model(
        &self,
        sub_spec: &AgentExecutionSpec,
    ) -> Result<crate::internal::ai::providers::AnyCompletionModel, TaskFailure> {
        let binding = sub_spec.model.as_ref().ok_or_else(|| {
            TaskFailure::ProviderError(CompletionError::ProviderError(format!(
                "sub-agent `{name}` has no `model` binding; cannot build a CompletionModel",
                name = sub_spec.name,
            )))
        })?;

        let options = self
            .resolve_provider_build_options(binding)
            .map_err(|reason| {
                TaskFailure::ProviderError(CompletionError::ProviderError(format!(
                    "failed to resolve provider build options for `{provider}/{model}`: {reason}",
                    provider = binding.provider_id,
                    model = binding.model_id,
                )))
            })?;

        self.provider_factory
            .build(binding, options)
            .map_err(|err| {
                TaskFailure::ProviderError(CompletionError::ProviderError(err.to_string()))
            })
    }
}

pub struct SubAgentChildRunRequest<'a> {
    pub ctx: &'a DispatchContext<'a>,
    pub invocation: &'a TaskInvocation,
    pub sub_spec: &'a AgentExecutionSpec,
    pub effective_ruleset: &'a PermissionRuleset,
    pub task_id: String,
    pub agent_run_id: AgentRunId,
    /// Optional pre-built chat history the runner threads into the
    /// child's tool loop before the user prompt. Today the
    /// dispatcher passes `Vec::new()` (the child sees only the
    /// invocation's prompt), but the field exists so a follow-up
    /// integration can materialise the parent
    /// `ContextHandoff::recent_tail` segments into `Message`s and
    /// hand them in without changing the runner trait's surface.
    /// Empty is the "no handoff yet" baseline.
    pub history: Vec<crate::internal::ai::completion::Message>,
    /// CEX-S2-12 / S2-INV-03 workspace isolation: when the dispatcher
    /// materialized an isolated workspace for this run, the child tool
    /// registry re-rooted onto that workspace (working_dir =
    /// workspace_root, plus an alias mapping the user-facing repo path
    /// into it). `None` means no isolation — the runner falls back to
    /// `ctx.tool_registry` (the parent's, rooted at the main worktree).
    pub workspace_registry: Option<ToolRegistry>,
    /// The inherited [`ToolRuntimeContext`] with its sandbox
    /// `writable_roots` rebased onto the isolated workspace root, so an
    /// absolute-path `shell` write to the main worktree is OS-denied.
    /// `None` means no isolation — the runner falls back to
    /// `ctx.runtime_context` (carrying the main worktree's roots).
    pub workspace_runtime_context: Option<ToolRuntimeContext>,
}

/// Executes the tail of a dispatch after gates and approvals pass.
///
/// Kept as an object-safe seam so tests can exercise dispatcher gate
/// behavior without constructing live providers, while the default
/// production runner can build an `AnyCompletionModel` and re-enter the
/// normal tool loop.
pub trait SubAgentChildRunner: Send + Sync {
    fn run<'a>(
        &'a self,
        request: SubAgentChildRunRequest<'a>,
    ) -> BoxFuture<'a, Result<TaskResult, TaskFailure>>;
}

/// Default production runner that drives a sub-agent through the
/// normal tool loop.
///
/// The runner builds an `AnyCompletionModel` via
/// [`DispatchContext::build_child_model`], threads any dispatcher-built
/// handoff history into `run_tool_loop_with_history_and_observer`,
/// enforces the child tool allow-list, forwards the inherited runtime
/// context, and folds the assistant result into a [`TaskResult`].
///
/// What this runner DOES today:
/// - Builds the child model from `sub_spec.model` using the
///   resolver-aware build helper.
/// - Sends the dispatcher-provided handoff history plus the
///   invocation's `prompt`.
/// - Writes a child session JSONL stream under
///   `SessionJsonlStore::child(agent_run_id)`, with snapshots for the
///   child prompt, assistant step text, tool calls, tool results, and
///   final assistant response.
/// - Aggregates assistant text content into `final_text`.
/// - Lets the child call tools allowed by
///   `ToolRegistry::available_for(sub_spec, effective_ruleset)`.
/// - Surfaces provider errors verbatim as
///   `TaskFailure::ProviderError(CompletionError)`.
/// - Honours `ctx.abort_token.is_cancelled()` pre-call so a parent
///   abort that fires before we hand off to the provider does not
///   leak a stale request to the wire.
///
/// What this runner does NOT do (yet):
/// - No byte-for-byte golden transcript fixture. The child JSONL stream
///   stores session snapshots for each tool step; later schema work may
///   add dedicated `ToolCall` / `ToolResult` event variants.
#[derive(Clone, Copy, Debug, Default)]
pub struct DefaultSubAgentChildRunner;

impl DefaultSubAgentChildRunner {
    /// Construct a fresh runner. Stateless — provided for ergonomics
    /// alongside [`Default::default`].
    pub fn new() -> Self {
        Self
    }
}

/// Lightweight tool-loop observer that accumulates the per-turn
/// count, usage, and child-visible tool transcript seen during a child run.
/// The child runner uses this to surface real `steps_used` / `usage` on the
/// returned `TaskResult`, and to persist per-tool child session snapshots under
/// `SessionJsonlStore::child(agent_run_id)`.
#[derive(Debug)]
struct ChildRunObserver {
    steps_used: u32,
    usage: CompletionUsageSummary,
    child_store: SessionJsonlStore,
    child_session: Arc<Mutex<SessionState>>,
    agent_run_id: AgentRunId,
    subagent_name: String,
}

impl ChildRunObserver {
    fn new(
        child_store: SessionJsonlStore,
        child_session: Arc<Mutex<SessionState>>,
        agent_run_id: AgentRunId,
        subagent_name: String,
    ) -> Self {
        Self {
            steps_used: 0,
            usage: CompletionUsageSummary::default(),
            child_store,
            child_session,
            agent_run_id,
            subagent_name,
        }
    }

    fn push_child_message(&self, role: &'static str, content: String, warning: &'static str) {
        mutate_child_session_snapshot(
            &self.child_store,
            &self.child_session,
            warning,
            self.agent_run_id,
            &self.subagent_name,
            |state| {
                let now = chrono::Utc::now();
                state.messages.push(SessionMessage {
                    role: role.to_string(),
                    content,
                    timestamp: now,
                });
                state.updated_at = now;
            },
        );
    }

    fn append_child_event(&self, event: SessionEvent, warning: &'static str) {
        append_child_session_event(
            &self.child_store,
            event,
            warning,
            self.agent_run_id,
            &self.subagent_name,
        );
    }
}

impl super::tool_loop::ToolLoopObserver for ChildRunObserver {
    fn on_model_turn_start(&mut self, _turn: usize) {
        self.steps_used = self.steps_used.saturating_add(1);
    }

    fn on_model_usage(&mut self, usage: &CompletionUsageSummary) {
        // Delegate to the canonical accumulator — every existing
        // `merge_optional_*` rule applies (saturating add, None
        // collapses to existing).
        self.usage.merge(usage);
    }

    fn on_assistant_step_text(&mut self, text: &str) {
        self.push_child_message(
            "assistant",
            text.to_string(),
            "failed to append sub-agent child assistant step snapshot",
        );
    }

    fn on_tool_call_begin(
        &mut self,
        call_id: &str,
        tool_name: &str,
        arguments: &serde_json::Value,
    ) {
        self.append_child_event(
            SessionEvent::tool_call(SessionToolCallEvent {
                event_id: uuid::Uuid::new_v4(),
                recorded_at: chrono::Utc::now(),
                agent_run_id: self.agent_run_id,
                subagent_name: self.subagent_name.clone(),
                call_id: call_id.to_string(),
                tool_name: tool_name.to_string(),
                arguments: arguments.clone(),
            }),
            "failed to append sub-agent child tool-call event",
        );
        self.push_child_message(
            "tool_call",
            serde_json::json!({
                "id": call_id,
                "name": tool_name,
                "arguments": arguments,
            })
            .to_string(),
            "failed to append sub-agent child tool-call snapshot",
        );
    }

    fn on_tool_call_end(
        &mut self,
        call_id: &str,
        tool_name: &str,
        result: &Result<ToolOutput, String>,
    ) {
        let (status, result_value, error_value) = match result {
            Ok(output) => (
                if output.is_success() {
                    "success".to_string()
                } else {
                    "failure".to_string()
                },
                Some(output.clone().into_response()),
                None,
            ),
            Err(error) => ("error".to_string(), None, Some(error.clone())),
        };
        self.append_child_event(
            SessionEvent::tool_result(SessionToolResultEvent {
                event_id: uuid::Uuid::new_v4(),
                recorded_at: chrono::Utc::now(),
                agent_run_id: self.agent_run_id,
                subagent_name: self.subagent_name.clone(),
                call_id: call_id.to_string(),
                tool_name: tool_name.to_string(),
                status: status.clone(),
                result: result_value.clone(),
                error: error_value.clone(),
            }),
            "failed to append sub-agent child tool-result event",
        );
        let payload = match error_value.as_ref() {
            None => serde_json::json!({
                "id": call_id,
                "name": tool_name,
                "status": status,
                "result": result_value,
            }),
            Some(error) => serde_json::json!({
                "id": call_id,
                "name": tool_name,
                "status": status,
                "error": error,
            }),
        };
        self.push_child_message(
            "tool_result",
            payload.to_string(),
            "failed to append sub-agent child tool-result snapshot",
        );
    }
}

impl SubAgentChildRunner for DefaultSubAgentChildRunner {
    fn run<'a>(
        &'a self,
        request: SubAgentChildRunRequest<'a>,
    ) -> BoxFuture<'a, Result<TaskResult, TaskFailure>> {
        use super::tool_loop::{ToolLoopConfig, run_tool_loop_with_history_and_observer};

        Box::pin(async move {
            // Pre-flight cancel: matches the dispatcher's post-ask
            // cancel check (v0.17.743). If the parent aborted while
            // the dispatcher was writing the Spawned event we'd
            // rather skip the provider call than emit a stale wire
            // request that the parent has already abandoned.
            if request.ctx.abort_token.is_cancelled() {
                return Err(TaskFailure::Cancelled {
                    source: CancellationSource::ParentAbort,
                });
            }

            let model = request.ctx.build_child_model(request.sub_spec)?;
            let provider_id = request
                .sub_spec
                .model
                .as_ref()
                .map(|m| m.provider_id.clone())
                .unwrap_or_default();
            let model_id = request
                .sub_spec
                .model
                .as_ref()
                .map(|m| m.model_id.clone())
                .unwrap_or_default();

            // Compute the child's visible tool surface by intersecting
            // the parent's registry with `(sub_spec, effective_ruleset)`.
            // The names feed `ToolLoopConfig::allowed_tools` so the
            // tool loop blocks anything the child should not see —
            // even if the underlying registry still carries the
            // handler. This is the same `available_for` rule the
            // P3.1 schema-level pre-filter uses; the child sees the
            // intersection at both the request definition and the
            // execution-time gate (`available_for` filters the
            // request's tool list, `allowed_tools` blocks any
            // hallucinated call to a tool outside that list).
            // CEX-S2-12 / S2-INV-03 workspace isolation: when the
            // dispatcher materialized an isolated workspace it hands the
            // child a registry re-rooted onto it and a runtime context
            // whose sandbox `writable_roots` are rebased to it. Fall
            // back to the parent's registry / inherited context when no
            // workspace was materialized (every flag-off path), keeping
            // behaviour identical to before this slice.
            let child_registry = request
                .workspace_registry
                .as_ref()
                .unwrap_or(request.ctx.tool_registry);
            let child_runtime_context = request
                .workspace_runtime_context
                .clone()
                .or_else(|| request.ctx.runtime_context.clone());
            let child_store = request
                .ctx
                .session_store
                .child(&request.agent_run_id.0.to_string());
            let child_session = Arc::new(Mutex::new(build_child_session_state(
                &request,
                child_registry.working_dir(),
                &provider_id,
                &model_id,
            )));
            mutate_child_session_snapshot(
                &child_store,
                &child_session,
                "failed to append sub-agent child session start snapshot",
                request.agent_run_id,
                &request.sub_spec.name,
                |state| state.add_user_message(&request.invocation.prompt),
            );

            let allowed_tools: Vec<String> = child_registry
                .available_for(request.sub_spec, request.effective_ruleset)
                .into_iter()
                .map(|spec| spec.function.name)
                .collect();
            let usage_repo_id = match request.ctx.usage_recorder.canonical_repo_id().await {
                Ok(repo_id) => repo_id,
                Err(error) => {
                    tracing::warn!(
                        %error,
                        "sub-agent usage stats will omit repository attribution because libra.repoid could not be read"
                    );
                    None
                }
            };

            let tool_loop_config = ToolLoopConfig {
                allowed_tools: Some(allowed_tools),
                // Child provider events share the parent's session/thread but
                // receive their own durable agent-run and event identity.
                // This keeps child spend out of the anonymous parent bucket.
                usage_recorder: Some(request.ctx.usage_recorder.clone()),
                usage_context: Some(crate::internal::ai::usage::UsageContext {
                    repo_id: usage_repo_id,
                    session_id: Some(request.ctx.parent_session_id.to_string()),
                    thread_id: Some(request.ctx.parent_thread_id.to_string()),
                    agent_run_id: Some(request.agent_run_id.0.to_string()),
                    run_id: Some(request.agent_run_id.0.to_string()),
                    turn_id: request.ctx.parent_turn_id.map(str::to_string),
                    event_id: Some(format!("sub-agent:{}", request.agent_run_id.0)),
                    provider: provider_id.clone(),
                    model: model_id.clone(),
                    request_kind: "completion".to_string(),
                    intent: None,
                    agent_name: Some(request.sub_spec.name.clone()),
                }),
                // S2-INV-13 hook dispatch (v0.17.806): forward the
                // parent's `HookRunner` so PreToolUse / PostToolUse
                // hooks fire on the child's tool calls — sub-agents
                // are not allowed to bypass project-level hooks.
                hook_runner: request.ctx.hook_runner.cloned(),
                // S2-INV-06 approval-authority inheritance + S2-INV-03
                // workspace isolation: the child's tool invocations run
                // under the parent's runtime sandbox / approval /
                // file-history authority. `DispatchContext::
                // runtime_context` documents this as "inherited by child
                // tool invocations ... they must not get a fresh approval
                // authority"; before it was wired the child silently
                // dropped it (no sandbox, approval defaulting to `Skip`
                // — strictly more permissive than the parent). When an
                // isolated workspace was materialized this is the
                // rebased context (sandbox `writable_roots` =
                // [workspace_root]); otherwise it is the inherited
                // parent context verbatim.
                runtime_context: child_runtime_context,
                ..ToolLoopConfig::default()
            };

            // Drive the history-aware tool loop with our observer so
            // the returned `TaskResult` reports the real per-turn
            // count and accumulated usage instead of a `1` / default
            // placeholder. History starts from the request's
            // optional handoff segments; the loop folds the user
            // prompt + every assistant / tool turn after that.
            //
            // P3.7 mid-flight cancel: race the child loop against
            // the parent's abort token. If the token fires while
            // the child is in a long provider await, the
            // `tokio::select!` short-circuits with
            // `Cancelled { ParentAbort }` instead of waiting for
            // the provider response. The provider future is
            // dropped at that point — cooperative cancel; any
            // in-flight network IO finishes its own way, but we
            // do not wait for it before returning to the parent.
            // The dispatcher's `map_failure_to_terminal_event`
            // (v0.17.757) maps the returned `Cancelled` to
            // `AgentRunEvent::Cancelled { reason: UserRequested }`.
            let mut observer = ChildRunObserver::new(
                child_store.clone(),
                Arc::clone(&child_session),
                request.agent_run_id,
                request.sub_spec.name.clone(),
            );
            let abort_token_clone = request.ctx.abort_token.clone();
            let child_loop = run_tool_loop_with_history_and_observer(
                &model,
                request.history.clone(),
                request.invocation.prompt.clone(),
                child_registry,
                tool_loop_config,
                &mut observer,
            );

            let turn_result = tokio::select! {
                biased;
                _ = abort_token_clone.cancelled() => {
                    mutate_child_session_snapshot(
                        &child_store,
                        &child_session,
                        "failed to append sub-agent child session cancellation snapshot",
                        request.agent_run_id,
                        &request.sub_spec.name,
                        |state| {
                            state.summary = "Sub-agent cancelled by parent abort".to_string();
                            state
                                .metadata
                                .insert("status".to_string(), serde_json::json!("cancelled"));
                        },
                    );
                    return Err(TaskFailure::Cancelled {
                        source: CancellationSource::ParentAbort,
                    });
                }
                result = child_loop => result,
            };
            let turn = match turn_result {
                Ok(turn) => turn,
                Err(error) => {
                    mutate_child_session_snapshot(
                        &child_store,
                        &child_session,
                        "failed to append sub-agent child session failure snapshot",
                        request.agent_run_id,
                        &request.sub_spec.name,
                        |state| {
                            state.summary = format!("Sub-agent failed: {error}");
                            state
                                .metadata
                                .insert("status".to_string(), serde_json::json!("failed"));
                        },
                    );
                    return Err(TaskFailure::ProviderError(error));
                }
            };
            mutate_child_session_snapshot(
                &child_store,
                &child_session,
                "failed to append sub-agent child session completion snapshot",
                request.agent_run_id,
                &request.sub_spec.name,
                |state| {
                    state.add_assistant_message(&turn.final_text);
                    state.summary = "Sub-agent completed".to_string();
                    state
                        .metadata
                        .insert("status".to_string(), serde_json::json!("completed"));
                },
            );

            Ok(TaskResult {
                task_id: request.task_id,
                agent_name: request.sub_spec.name.clone(),
                provider_id,
                model_id,
                final_text: turn.final_text,
                steps_used: observer.steps_used,
                usage: observer.usage,
            })
        })
    }
}

fn build_child_session_state(
    request: &SubAgentChildRunRequest<'_>,
    working_dir: &std::path::Path,
    provider_id: &str,
    model_id: &str,
) -> SessionState {
    let mut state = SessionState::new(working_dir.to_string_lossy().as_ref());
    state.id = request.agent_run_id.0.to_string();
    state.metadata.insert(
        "kind".to_string(),
        serde_json::json!("sub_agent_child_session"),
    );
    state.metadata.insert(
        "agent_run_id".to_string(),
        serde_json::json!(request.agent_run_id.0.to_string()),
    );
    state.metadata.insert(
        "parent_thread_id".to_string(),
        serde_json::json!(request.ctx.parent_thread_id),
    );
    state.metadata.insert(
        "parent_session_id".to_string(),
        serde_json::json!(request.ctx.parent_session_id.as_str()),
    );
    state.metadata.insert(
        "parent_message_id".to_string(),
        serde_json::json!(request.ctx.parent_message_id.as_str()),
    );
    state.metadata.insert(
        "subagent_name".to_string(),
        serde_json::json!(&request.sub_spec.name),
    );
    state.metadata.insert(
        "subagent_type".to_string(),
        serde_json::json!(&request.invocation.subagent_type),
    );
    state
        .metadata
        .insert("task_id".to_string(), serde_json::json!(&request.task_id));
    state
        .metadata
        .insert("provider_id".to_string(), serde_json::json!(provider_id));
    state
        .metadata
        .insert("model_id".to_string(), serde_json::json!(model_id));
    state
        .metadata
        .insert("status".to_string(), serde_json::json!("running"));
    state
}

fn append_child_session_snapshot(
    store: &SessionJsonlStore,
    state: &SessionState,
    warning: &'static str,
    agent_run_id: AgentRunId,
    subagent_name: &str,
) {
    if let Err(error) = store.append(&SessionEvent::snapshot(state.clone())) {
        tracing::warn!(
            error = %error,
            agent_run_id = %agent_run_id.0,
            subagent = %subagent_name,
            "{warning}",
        );
    }
}

fn append_child_session_event(
    store: &SessionJsonlStore,
    event: SessionEvent,
    warning: &'static str,
    agent_run_id: AgentRunId,
    subagent_name: &str,
) {
    if let Err(error) = store.append(&event) {
        tracing::warn!(
            error = %error,
            agent_run_id = %agent_run_id.0,
            subagent = %subagent_name,
            "{warning}",
        );
    }
}

fn mutate_child_session_snapshot(
    store: &SessionJsonlStore,
    state: &Arc<Mutex<SessionState>>,
    warning: &'static str,
    agent_run_id: AgentRunId,
    subagent_name: &str,
    mutate: impl FnOnce(&mut SessionState),
) {
    match state.lock() {
        Ok(mut state) => {
            mutate(&mut state);
            append_child_session_snapshot(store, &state, warning, agent_run_id, subagent_name);
        }
        Err(error) => {
            tracing::warn!(
                error = %error,
                agent_run_id = %agent_run_id.0,
                subagent = %subagent_name,
                "{warning}: child session state lock poisoned",
            );
        }
    }
}

/// Object-safe provider-options resolver used by the dispatcher tail.
///
/// `ProviderFactory` is intentionally env-free. The command layer owns
/// env-file, Vault, and CLI flag resolution, then exposes this narrow
/// interface so a child bound to `deepseek/...` can receive DeepSeek
/// credentials even when the parent model is `ollama/...`.
pub trait ProviderBuildOptionsResolver: Send + Sync {
    fn resolve(&self, binding: &ModelBinding) -> Result<ProviderBuildOptions, String>;
}

/// Object-safe trait the tool loop forwards `task` calls into when
/// `code.multi_agent.enabled = true` (OC-Phase 5).
///
/// The signature uses [`BoxFuture`] (not `async fn`) so the trait can be
/// stored as `Arc<dyn SubAgentDispatcher>` inside a generic
/// `ToolLoopConfig`. This is the explicit reason the doc forbids relying
/// on `async fn in trait` for this specific trait — the runtime needs
/// trait objects.
pub trait SubAgentDispatcher: Send + Sync {
    fn dispatch<'a>(
        &'a self,
        ctx: DispatchContext<'a>,
        invocation: TaskInvocation,
        entry_kind: TaskEntryKind,
    ) -> BoxFuture<'a, Result<TaskResult, TaskFailure>>;
}

/// Owned runtime bundle the parent tool loop uses to intercept the
/// `task` tool and call a [`SubAgentDispatcher`].
///
/// This is deliberately explicit instead of smuggling fields through
/// [`crate::internal::ai::tools::ToolRuntimeContext`]: a sub-agent
/// dispatch needs parent session identity, rules, model binding,
/// approval routing, usage recording, and session JSONL access. Normal
/// tool handlers do not have that authority, which is exactly why
/// `task` is intercepted at the tool-loop layer.
#[derive(Clone)]
pub struct SubAgentToolLoopRuntime {
    pub dispatcher: Arc<dyn SubAgentDispatcher>,
    pub parent_thread_id: String,
    /// Bound from the parent's usage context for each admitted runtime turn.
    pub parent_turn_id: Option<String>,
    pub parent_session_id: SessionId,
    pub parent_agent: AgentExecutionSpec,
    pub parent_ruleset: PermissionRuleset,
    pub parent_model_binding: ModelBinding,
    pub permission_service: Arc<PermissionService>,
    pub session_store: SessionJsonlStore,
    pub provider_factory: Arc<ProviderFactory>,
    pub provider_build_options: ProviderBuildOptions,
    pub provider_build_options_resolver: Option<Arc<dyn ProviderBuildOptionsResolver>>,
    pub tool_registry: ToolRegistry,
    pub runtime_context: Option<ToolRuntimeContext>,
    pub usage_recorder: Arc<UsageRecorder>,
    pub context_frame_loader: Arc<ContextFrameLoader>,
    pub abort_token: AbortToken,
    pub depth: u8,
    /// OC-Phase 4 P4.4 compaction model (v0.17.784): when
    /// configured via `[code.compaction]` in agents.toml, the
    /// dispatcher tail uses this model to convert the parent's
    /// latest `ContextFrameEvent` into a validated
    /// `ContextHandoff` via `run_compaction(...)` before feeding
    /// the child via `ContextHandoff::to_handoff_messages`
    /// (v0.17.781). None falls through to the v0.17.773
    /// raw-segment path — same behaviour as pre-v0.17.784.
    /// `Arc`-wrapped so cloning the runtime stays O(1) struct
    /// clone.
    #[allow(clippy::type_complexity)]
    pub compaction_model: Option<Arc<crate::internal::ai::providers::AnyCompletionModel>>,
    /// OC-Phase 3 S2-INV-13 hook dispatch (v0.17.806): the parent's
    /// `HookRunner`, forwarded into each sub-agent's tool_loop
    /// config so `PreToolUse` / `PostToolUse` hooks fire on
    /// child tool calls. None means no hooks are configured at
    /// the project level (no `.libra/hooks.json`); the child
    /// then runs without hook dispatch, matching the parent.
    pub hook_runner: Option<Arc<crate::internal::ai::hooks::HookRunner>>,
}

impl std::fmt::Debug for SubAgentToolLoopRuntime {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SubAgentToolLoopRuntime")
            .field("parent_thread_id", &self.parent_thread_id)
            .field("parent_turn_id", &self.parent_turn_id)
            .field("parent_session_id", &self.parent_session_id)
            .field("parent_agent", &self.parent_agent.name)
            .field("parent_model_binding", &self.parent_model_binding)
            .field("depth", &self.depth)
            .finish_non_exhaustive()
    }
}

impl SubAgentToolLoopRuntime {
    /// Build a [`DispatchContext`] for one `task` dispatch.
    ///
    /// `live_runtime_context` lets the caller hand the child the
    /// parent tool loop's *current* per-turn runtime context (sandbox /
    /// approval / file-history authority). This matters because the Code UI
    /// attaches per-turn state — most importantly the file-history
    /// batch that drives `apply_patch` undo preimage recording — to the
    /// turn's `ToolLoopConfig.runtime_context`, NOT to the
    /// session-start snapshot stored in [`Self::runtime_context`]. The
    /// tool-loop `task` interception passes that live context here so a
    /// dispatched child inherits the live authority (S2-INV-06);
    /// callers without a per-turn context (e.g. the `/task` slash
    /// command) pass `None` and fall back to the stored snapshot.
    pub fn dispatch_context(
        &self,
        parent_message_id: MessageId,
        live_runtime_context: Option<ToolRuntimeContext>,
    ) -> DispatchContext<'_> {
        DispatchContext {
            parent_thread_id: &self.parent_thread_id,
            parent_turn_id: self.parent_turn_id.as_deref(),
            parent_session_id: &self.parent_session_id,
            parent_agent: &self.parent_agent,
            parent_ruleset: &self.parent_ruleset,
            parent_model_binding: &self.parent_model_binding,
            parent_message_id,
            permission_service: self.permission_service.as_ref(),
            session_store: &self.session_store,
            provider_factory: self.provider_factory.as_ref(),
            provider_build_options: &self.provider_build_options,
            provider_build_options_resolver: self
                .provider_build_options_resolver
                .as_ref()
                .map(|resolver| resolver.as_ref()),
            tool_registry: &self.tool_registry,
            runtime_context: live_runtime_context.or_else(|| self.runtime_context.clone()),
            usage_recorder: self.usage_recorder.as_ref(),
            context_frame_loader: self.context_frame_loader.as_ref(),
            abort_token: self.abort_token.child(),
            depth: self.depth,
            compaction_model: self.compaction_model.as_deref(),
            hook_runner: self.hook_runner.as_ref(),
        }
    }

    /// Return a clone of this runtime with the `abort_token` field
    /// swapped for a turn-scoped token. Used by the App's turn
    /// handler to attach a per-turn cancel signal that's distinct
    /// from the session-level token: `Ctrl-C` during a turn
    /// cancels only the in-flight sub-agent dispatch via this
    /// turn token, while the session token survives for
    /// subsequent turns.
    ///
    /// The returned runtime shares every other field with `self`
    /// (Arc-wrapped or owned by-value), so attaching a per-turn
    /// token costs one clone of the wrapping struct, not of the
    /// state behind it.
    pub fn with_abort_token(&self, abort_token: AbortToken) -> Self {
        let mut clone = self.clone();
        clone.abort_token = abort_token;
        clone
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Scenario: `ChannelPermissionAsker` (v0.17.787) forwards every
    /// ask to its consumer channel and awaits the reply. With a
    /// consumer present that replies `Once`, the asker resolves
    /// with the same reply. With the consumer dropped before
    /// replying, the asker resolves with a `Reject` carrying the
    /// reply-channel-closed feedback string. With the asker
    /// constructed against a dropped receiver, the send fails
    /// immediately and the asker resolves with a `Reject` carrying
    /// the ask-channel-closed feedback string.
    #[tokio::test]
    async fn channel_permission_asker_round_trips_reply_and_handles_dropped_consumer() {
        let session_id: SessionId = "sess".to_string();
        let request = PermissionAskRequest {
            permission: "shell",
            patterns: &["ls".to_string()],
            thread_id: "thread",
            session_id: &session_id,
            source: PermissionAskSource::SubAgentSpawn {
                name: "explore".to_string(),
                prompt_digest: "abc123".to_string(),
            },
        };

        // Happy path: consumer replies with Once.
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
        let asker = ChannelPermissionAsker::new(tx);
        let ask_future = asker.ask(PermissionAskRequest {
            source: request.source.clone(),
            ..request
        });
        let consumer = tokio::spawn(async move {
            let ask = rx.recv().await.expect("consumer should receive ask");
            assert_eq!(ask.permission, "shell");
            assert_eq!(ask.patterns, vec!["ls".to_string()]);
            ask.reply_tx
                .send(PermissionReply::Once)
                .expect("reply send must succeed");
        });
        let reply = ask_future.await;
        consumer.await.expect("consumer task should finish");
        assert_eq!(reply, PermissionReply::Once);

        // Reply-channel dropped: consumer drops the reply_tx
        // without sending. Asker must surface a Reject.
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
        let asker = ChannelPermissionAsker::new(tx);
        let ask_future = asker.ask(PermissionAskRequest {
            source: request.source.clone(),
            ..request
        });
        let consumer = tokio::spawn(async move {
            let ask = rx.recv().await.expect("consumer should receive ask");
            drop(ask.reply_tx);
        });
        let reply = ask_future.await;
        consumer.await.expect("consumer task should finish");
        assert!(
            matches!(
                reply,
                PermissionReply::Reject { feedback: Some(ref feedback) }
                    if feedback.contains("reply channel closed")
            ),
            "expected Reject with reply-channel-closed feedback, got: {reply:?}",
        );

        // Ask-channel dropped: receiver dropped before the asker
        // sends. Asker must surface a Reject without awaiting.
        let (tx, rx) = tokio::sync::mpsc::unbounded_channel::<ChannelPermissionAsk>();
        drop(rx);
        let asker = ChannelPermissionAsker::new(tx);
        let reply = asker
            .ask(PermissionAskRequest {
                source: request.source.clone(),
                ..request
            })
            .await;
        assert!(
            matches!(
                reply,
                PermissionReply::Reject { feedback: Some(ref feedback) }
                    if feedback.contains("ask channel closed")
            ),
            "expected Reject with ask-channel-closed feedback, got: {reply:?}",
        );
    }

    /// Scenario: `AbortToken` round-trips a cancellation flag through
    /// `cancel()` / `is_cancelled()`, and cancellation propagates to
    /// children created before and after the parent is cancelled.
    #[test]
    fn abort_token_propagates_parent_cancellation_to_children() {
        let root = AbortToken::new();
        let child = root.child();
        assert!(!root.is_cancelled());
        assert!(!child.is_cancelled());

        root.cancel();

        assert!(root.is_cancelled());
        assert!(child.is_cancelled());

        let late_child = root.child();
        assert!(late_child.is_cancelled());
    }

    /// Scenario: async waiters are notified when a token is cancelled.
    /// The child-runner uses this to stop waiting for a provider call
    /// once the parent session has been cancelled.
    #[tokio::test]
    async fn abort_token_cancelled_future_resolves() {
        let token = AbortToken::new();
        let waiter = token.clone();
        let (tx, mut rx) = tokio::sync::oneshot::channel();
        let handle = tokio::spawn(async move {
            waiter.cancelled().await;
            let _ = tx.send(waiter.is_cancelled());
        });

        assert!(
            tokio::time::timeout(std::time::Duration::from_millis(20), &mut rx)
                .await
                .is_err(),
            "waiter should stay pending before cancellation"
        );
        token.cancel();

        let observed = tokio::time::timeout(std::time::Duration::from_secs(1), rx)
            .await
            .expect("waiter should resolve after cancellation")
            .expect("waiter task should send");
        assert!(observed);
        handle.await.expect("waiter task should complete");
    }

    /// Scenario: every `TaskEntryKind` variant constructs without
    /// panicking and the `bypass_permission_ask` payload survives.
    #[test]
    fn task_entry_kind_variants_construct() {
        let _llm = TaskEntryKind::LlmInitiated;
        let user = TaskEntryKind::UserInitiated {
            bypass_permission_ask: true,
        };
        match user {
            TaskEntryKind::UserInitiated {
                bypass_permission_ask,
            } => assert!(bypass_permission_ask),
            other => panic!("expected UserInitiated, got {other:?}"),
        }
    }

    /// Scenario: a `TaskInvocation` round-trips through clone +
    /// equality, pinning the field set the doc commits to. Adding a
    /// new required field (or renaming one) breaks this test loudly.
    #[test]
    fn task_invocation_field_set_matches_doc_contract() {
        let invocation = TaskInvocation {
            description: "find TODOs".to_string(),
            prompt: "grep TODO src/".to_string(),
            subagent_type: "explore".to_string(),
            task_id: None,
        };
        let cloned = invocation.clone();
        assert_eq!(cloned, invocation);
        assert_eq!(invocation.description, "find TODOs");
        assert_eq!(invocation.prompt, "grep TODO src/");
        assert_eq!(invocation.subagent_type, "explore");
        assert!(invocation.task_id.is_none());

        let resumed = TaskInvocation {
            task_id: Some("task-42".to_string()),
            ..invocation.clone()
        };
        assert_eq!(resumed.task_id.as_deref(), Some("task-42"));
    }

    /// Scenario: every `TaskFailure` variant the doc enumerates has a
    /// constructible match arm. A regression that drops or renames a
    /// variant fails this test even before any consumer code breaks,
    /// which keeps the in-tree taxonomy in lock-step with the doc.
    #[test]
    fn task_failure_variants_match_doc_taxonomy() {
        let cases: Vec<TaskFailure> = vec![
            TaskFailure::FeatureDisabled,
            TaskFailure::UnknownSubagent {
                name: "?".to_string(),
                suggestions: vec!["explore".to_string()],
            },
            TaskFailure::DepthExceeded {
                current: 1,
                limit: 1,
            },
            TaskFailure::ConcurrencyExceeded {
                current: 1,
                limit: 1,
            },
            TaskFailure::PermissionEscalationDenied {
                permission: "edit".to_string(),
                pattern: "*".to_string(),
            },
            TaskFailure::SafetyDenied(SafetyDecisionDenial {
                reason: "x".to_string(),
            }),
            TaskFailure::ApprovalRejected { feedback: None },
            TaskFailure::BudgetExceeded(BudgetExceededReason::CostHardCap),
            TaskFailure::BudgetExceeded(BudgetExceededReason::TokenHardCap),
            TaskFailure::BudgetExceeded(BudgetExceededReason::Internal {
                reason: "tracker unavailable".to_string(),
            }),
            TaskFailure::ContextHandoffFailed(ContextHandoffError::NoFrameAvailable),
            TaskFailure::ProviderError(CompletionError::ProviderError("x".into())),
            TaskFailure::ChildToolLoopFailed(ToolLoopError::StepBudgetExhausted { steps: 1 }),
            TaskFailure::Cancelled {
                source: CancellationSource::ParentAbort,
            },
            TaskFailure::Timeout { wall_clock_ms: 0 },
        ];
        // Every variant constructs cleanly. Surface a debug print to
        // catch a stray Display/Debug regression too.
        for failure in cases {
            let _ = format!("{failure:?}");
        }
    }

    /// Scenario: user-facing dispatch errors must render as stable,
    /// actionable text rather than raw `Debug` output. `/task` and the
    /// task tool both surface this Display implementation to the
    /// parent transcript.
    #[test]
    fn task_failure_display_is_user_friendly() {
        assert_eq!(
            TaskFailure::FeatureDisabled.to_string(),
            "multi-agent dispatch is disabled"
        );
        assert_eq!(
            TaskFailure::UnknownSubagent {
                name: "ghost".to_string(),
                suggestions: vec!["explore".to_string(), "review".to_string()],
            }
            .to_string(),
            "unknown sub-agent `ghost`; available sub-agents: explore, review"
        );
        assert_eq!(
            TaskFailure::PermissionEscalationDenied {
                permission: "edit".to_string(),
                pattern: "*".to_string(),
            }
            .to_string(),
            "permission escalation denied for `edit:*`"
        );
    }

    /// Scenario: `SubAgentDispatcher` is object-safe (the trait must be
    /// usable as `Arc<dyn SubAgentDispatcher>` so the tool loop can
    /// store it generically). This test compiles the trait object
    /// construction explicitly so a future regression that adds a
    /// generic method to the trait fails compilation here, not only at
    /// the eventual store site.
    #[test]
    fn sub_agent_dispatcher_is_object_safe() {
        // We only need the type to exist and be coercible.
        struct NoopDispatcher;
        impl SubAgentDispatcher for NoopDispatcher {
            fn dispatch<'a>(
                &'a self,
                _ctx: DispatchContext<'a>,
                _invocation: TaskInvocation,
                _entry_kind: TaskEntryKind,
            ) -> BoxFuture<'a, Result<TaskResult, TaskFailure>> {
                Box::pin(async move {
                    Err(TaskFailure::UnknownSubagent {
                        name: "noop".to_string(),
                        suggestions: vec![],
                    })
                })
            }
        }
        let dispatcher: Arc<dyn SubAgentDispatcher> = Arc::new(NoopDispatcher);
        // Sanity: drop-it. The compile-time check is the meaningful
        // assertion; this drop just keeps clippy from flagging an
        // unused binding.
        drop(dispatcher);
    }

    /// Scenario: a session whose JSONL has no `ContextFrame` events
    /// yet returns `None`. Used as the cold-start baseline for the
    /// dispatcher's "no parent context to forward" branch.
    #[test]
    fn context_frame_loader_returns_none_for_empty_session() {
        let temp = tempfile::tempdir().expect("tempdir");
        let store = SessionJsonlStore::new(temp.path().to_path_buf());
        let loader = ContextFrameLoader::new();
        let latest = loader
            .latest_frame_for_session(&store)
            .expect("scan over an empty session must not be an IO error");
        assert!(latest.is_none(), "no frame events should produce None");
    }

    /// Scenario: a session JSONL with multiple `ContextFrame` events
    /// returns the LATEST (last-written) one. The dispatcher relies on
    /// this rule to pick "the parent's most recent context" rather
    /// than a stale one earlier in the session.
    #[test]
    fn context_frame_loader_returns_latest_frame_when_multiple_present() {
        use chrono::Utc;
        use uuid::Uuid;

        use crate::internal::ai::context_budget::frame::{ContextFrameEvent, ContextFrameKind};

        let temp = tempfile::tempdir().expect("tempdir");
        let store = SessionJsonlStore::new(temp.path().to_path_buf());

        let older = ContextFrameEvent {
            event_id: Uuid::new_v4(),
            recorded_at: Utc::now(),
            frame_id: Uuid::new_v4(),
            kind: ContextFrameKind::PromptBuild,
            prompt_id: None,
            segments: Vec::new(),
            omissions: Vec::new(),
            total_candidate_tokens: 0,
            total_selected_tokens: 0,
            budget_exceeded_by: 0,
        };
        let newer = ContextFrameEvent {
            event_id: Uuid::new_v4(),
            recorded_at: Utc::now(),
            frame_id: Uuid::new_v4(),
            kind: ContextFrameKind::ToolResult,
            prompt_id: None,
            segments: Vec::new(),
            omissions: Vec::new(),
            total_candidate_tokens: 0,
            total_selected_tokens: 0,
            budget_exceeded_by: 0,
        };
        store
            .append(&SessionEvent::ContextFrame(older.clone()))
            .unwrap();
        store
            .append(&SessionEvent::ContextFrame(newer.clone()))
            .unwrap();

        let loader = ContextFrameLoader::new();
        let latest = loader
            .latest_frame_for_session(&store)
            .expect("scan must not fail")
            .expect("a frame event was appended");
        assert_eq!(
            latest.frame_id, newer.frame_id,
            "loader must return the latest frame, not the first"
        );
        assert_ne!(latest.frame_id, older.frame_id);
    }

    /// Scenario: non-`ContextFrame` events on the session JSONL
    /// (e.g. `SessionSnapshot`, `AgentRun`) do not get returned. This
    /// pins the loader's "frame-only" filter so a future
    /// `SessionEvent` variant cannot silently leak through.
    #[test]
    fn context_frame_loader_skips_non_frame_session_events() {
        use crate::internal::ai::session::state::SessionState;

        let temp = tempfile::tempdir().expect("tempdir");
        let store = SessionJsonlStore::new(temp.path().to_path_buf());

        store
            .append(&SessionEvent::SessionSnapshot(
                crate::internal::ai::session::jsonl::SessionSnapshotEvent {
                    event_id: uuid::Uuid::new_v4(),
                    recorded_at: chrono::Utc::now(),
                    state: SessionState::new(temp.path().to_string_lossy().as_ref()),
                },
            ))
            .unwrap();

        let loader = ContextFrameLoader::new();
        let latest = loader
            .latest_frame_for_session(&store)
            .expect("scan must not fail");
        assert!(
            latest.is_none(),
            "non-ContextFrame events must not surface as frame loads",
        );
    }

    /// Scenario: a `DispatchContext` without a per-binding resolver
    /// returns a clone of the parent's `provider_build_options` for
    /// any model binding. The P3.4 step 9 plumbing uses this to build
    /// a child `AnyCompletionModel` when the operator did not declare
    /// per-provider credentials.
    #[test]
    fn dispatch_context_resolve_provider_build_options_falls_back_to_parent_clone() {
        use crate::internal::ai::providers::{ProviderBuildOptions, ProviderFactory};

        let parent_options = ProviderBuildOptions {
            api_key: Some("parent-api-key".to_string()),
            api_base: None,
            ollama_compact_tools: false,
            ..ProviderBuildOptions::default()
        };
        // Materialise every reference the context borrows. The
        // helpers from the dispatcher's test harness live in
        // sub_agent_dispatcher.rs and are private; reconstruct only
        // the minimum the resolver call needs.
        let parent_spec = AgentExecutionSpec::default();
        let parent_ruleset: PermissionRuleset = Vec::new();
        let parent_binding = ModelBinding::parse("anthropic/claude-3-5-sonnet-latest")
            .expect("parent binding must parse");
        let permission_service =
            PermissionService::new(Arc::new(AskerThatNeverFires) as Arc<dyn PermissionAsker>);
        let temp = tempfile::tempdir().expect("tempdir");
        let store =
            crate::internal::ai::session::jsonl::SessionJsonlStore::new(temp.path().to_path_buf());
        let provider_factory = ProviderFactory;
        let tool_registry = crate::internal::ai::tools::ToolRegistry::with_working_dir(
            std::path::PathBuf::from("/tmp"),
        );
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("tokio rt");
        let conn = rt
            .block_on(sea_orm::Database::connect("sqlite::memory:"))
            .expect("sqlite memory db");
        let usage_recorder = crate::internal::ai::usage::UsageRecorder::new(conn);
        let context_frame_loader = ContextFrameLoader::default();
        let session_id: SessionId = "session".to_string();

        let context = DispatchContext {
            parent_thread_id: "thread",
            parent_turn_id: None,
            parent_session_id: &session_id,
            parent_agent: &parent_spec,
            parent_ruleset: &parent_ruleset,
            parent_model_binding: &parent_binding,
            parent_message_id: MessageId::from("msg"),
            permission_service: &permission_service,
            session_store: &store,
            provider_factory: &provider_factory,
            provider_build_options: &parent_options,
            provider_build_options_resolver: None,
            tool_registry: &tool_registry,
            runtime_context: None,
            usage_recorder: &usage_recorder,
            context_frame_loader: &context_frame_loader,
            abort_token: AbortToken::new(),
            depth: 0,
            compaction_model: None,
            hook_runner: None,
        };

        let child_binding =
            ModelBinding::parse("deepseek/deepseek-chat").expect("child binding must parse");
        let resolved = context
            .resolve_provider_build_options(&child_binding)
            .expect("fallback path must always succeed");
        assert_eq!(
            resolved.api_key.as_deref(),
            Some("parent-api-key"),
            "absent resolver must hand back a clone of the parent's options",
        );
    }

    /// Scenario: a resolver is registered, so the child binding gets
    /// resolver-supplied credentials, not the parent's. This is the
    /// path the operator uses to bind a child sub-agent to a
    /// different provider (e.g. parent on ollama, child on
    /// deepseek with its own API key).
    #[test]
    fn dispatch_context_resolve_provider_build_options_uses_resolver_when_present() {
        use crate::internal::ai::providers::{ProviderBuildOptions, ProviderFactory};

        let parent_options = ProviderBuildOptions {
            api_key: Some("parent-api-key".to_string()),
            ..ProviderBuildOptions::default()
        };

        struct ChildKeyResolver;
        impl ProviderBuildOptionsResolver for ChildKeyResolver {
            fn resolve(&self, _binding: &ModelBinding) -> Result<ProviderBuildOptions, String> {
                Ok(ProviderBuildOptions {
                    api_key: Some("child-api-key".to_string()),
                    ..ProviderBuildOptions::default()
                })
            }
        }

        let resolver: &dyn ProviderBuildOptionsResolver = &ChildKeyResolver;
        let parent_spec = AgentExecutionSpec::default();
        let parent_ruleset: PermissionRuleset = Vec::new();
        let parent_binding = ModelBinding::parse("anthropic/claude-3-5-sonnet-latest").unwrap();
        let permission_service =
            PermissionService::new(Arc::new(AskerThatNeverFires) as Arc<dyn PermissionAsker>);
        let temp = tempfile::tempdir().unwrap();
        let store =
            crate::internal::ai::session::jsonl::SessionJsonlStore::new(temp.path().to_path_buf());
        let provider_factory = ProviderFactory;
        let tool_registry = crate::internal::ai::tools::ToolRegistry::with_working_dir(
            std::path::PathBuf::from("/tmp"),
        );
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        let conn = rt
            .block_on(sea_orm::Database::connect("sqlite::memory:"))
            .unwrap();
        let usage_recorder = crate::internal::ai::usage::UsageRecorder::new(conn);
        let context_frame_loader = ContextFrameLoader::default();
        let session_id: SessionId = "session".to_string();

        let context = DispatchContext {
            parent_thread_id: "thread",
            parent_turn_id: None,
            parent_session_id: &session_id,
            parent_agent: &parent_spec,
            parent_ruleset: &parent_ruleset,
            parent_model_binding: &parent_binding,
            parent_message_id: MessageId::from("msg"),
            permission_service: &permission_service,
            session_store: &store,
            provider_factory: &provider_factory,
            provider_build_options: &parent_options,
            provider_build_options_resolver: Some(resolver),
            tool_registry: &tool_registry,
            runtime_context: None,
            usage_recorder: &usage_recorder,
            context_frame_loader: &context_frame_loader,
            abort_token: AbortToken::new(),
            depth: 0,
            compaction_model: None,
            hook_runner: None,
        };

        let child_binding = ModelBinding::parse("deepseek/deepseek-chat").unwrap();
        let resolved = context
            .resolve_provider_build_options(&child_binding)
            .expect("resolver must succeed for this fixture");
        assert_eq!(
            resolved.api_key.as_deref(),
            Some("child-api-key"),
            "resolver path must override the parent's options",
        );
    }

    /// Test-only asker placeholder. Constructing a
    /// `PermissionService` requires *some* asker, but neither test
    /// above invokes the dispatcher's step 8 ask path, so the asker
    /// is never called.
    struct AskerThatNeverFires;
    impl PermissionAsker for AskerThatNeverFires {
        fn ask<'a>(
            &'a self,
            _request: PermissionAskRequest<'a>,
        ) -> futures::future::BoxFuture<'a, PermissionReply> {
            Box::pin(async { unreachable!("resolver fixture never reaches the step-8 ask path") })
        }
    }

    /// Build a `DispatchContext` parameterised on the parent options +
    /// resolver, and run the closure with it. Used by the
    /// `build_child_model` tests so the boilerplate doesn't crowd the
    /// scenario assertions.
    fn with_dispatch_context<R>(
        parent_options: ProviderBuildOptions,
        resolver: Option<&dyn ProviderBuildOptionsResolver>,
        f: impl FnOnce(&DispatchContext<'_>) -> R,
    ) -> R {
        use crate::internal::ai::providers::ProviderFactory;

        let parent_spec = AgentExecutionSpec::default();
        let parent_ruleset: PermissionRuleset = Vec::new();
        let parent_binding =
            ModelBinding::parse("anthropic/claude-3-5-sonnet-latest").expect("parent binding");
        let permission_service =
            PermissionService::new(Arc::new(AskerThatNeverFires) as Arc<dyn PermissionAsker>);
        let temp = tempfile::tempdir().expect("tempdir");
        let store =
            crate::internal::ai::session::jsonl::SessionJsonlStore::new(temp.path().to_path_buf());
        let provider_factory = ProviderFactory;
        let tool_registry = crate::internal::ai::tools::ToolRegistry::with_working_dir(
            std::path::PathBuf::from("/tmp"),
        );
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("tokio rt");
        let conn = rt
            .block_on(sea_orm::Database::connect("sqlite::memory:"))
            .expect("sqlite memory db");
        let usage_recorder = crate::internal::ai::usage::UsageRecorder::new(conn);
        let context_frame_loader = ContextFrameLoader::default();
        let session_id: SessionId = "session".to_string();

        let context = DispatchContext {
            parent_thread_id: "thread",
            parent_turn_id: None,
            parent_session_id: &session_id,
            parent_agent: &parent_spec,
            parent_ruleset: &parent_ruleset,
            parent_model_binding: &parent_binding,
            parent_message_id: MessageId::from("msg"),
            permission_service: &permission_service,
            session_store: &store,
            provider_factory: &provider_factory,
            provider_build_options: &parent_options,
            provider_build_options_resolver: resolver,
            tool_registry: &tool_registry,
            runtime_context: None,
            usage_recorder: &usage_recorder,
            context_frame_loader: &context_frame_loader,
            abort_token: AbortToken::new(),
            depth: 0,
            compaction_model: None,
            hook_runner: None,
        };

        f(&context)
    }

    /// Scenario: a sub-agent spec with `model: None` cannot build a
    /// CompletionModel. `build_child_model` surfaces this as a
    /// structured `TaskFailure::ProviderError` rather than panicking
    /// so the dispatcher tail can echo the reason into the parent
    /// transcript.
    #[test]
    fn build_child_model_rejects_sub_spec_with_no_model_binding() {
        use crate::internal::ai::providers::ProviderBuildOptions;

        let sub_spec = AgentExecutionSpec {
            name: "no-binding".to_string(),
            model: None,
            ..AgentExecutionSpec::default()
        };

        with_dispatch_context(ProviderBuildOptions::default(), None, |ctx| {
            let err = ctx
                .build_child_model(&sub_spec)
                .expect_err("None binding must fail");
            let message = err.to_string();
            assert!(
                message.contains("no `model` binding"),
                "expected the missing-binding hint in the error, got: {message}",
            );
            assert!(
                message.contains("no-binding"),
                "expected the sub-spec name to be quoted, got: {message}",
            );
        });
    }

    /// OC-Phase 3 P3.4 step 13 cancel pre-flight: a runner whose
    /// `ctx.abort_token` is already cancelled must short-circuit with
    /// `Cancelled { ParentAbort }` BEFORE any provider call. Without
    /// this guard the runner would emit a stale wire request after
    /// the parent has already abandoned the dispatch.
    #[tokio::test]
    async fn default_child_runner_short_circuits_on_pre_cancelled_abort_token() {
        use crate::internal::ai::providers::{ProviderBuildOptions, ProviderFactory};

        let sub_spec = AgentExecutionSpec {
            name: "cancelled".to_string(),
            model: ModelBinding::parse("anthropic/claude-3-5-haiku-latest"),
            ..AgentExecutionSpec::default()
        };
        let invocation = TaskInvocation {
            description: "should never reach provider".to_string(),
            prompt: "ignored".to_string(),
            subagent_type: "cancelled".to_string(),
            task_id: None,
        };
        let parent_spec = AgentExecutionSpec::default();
        let parent_ruleset: PermissionRuleset = Vec::new();
        let parent_binding = ModelBinding::parse("anthropic/claude-3-5-sonnet-latest").unwrap();
        let permission_service =
            PermissionService::new(Arc::new(AskerThatNeverFires) as Arc<dyn PermissionAsker>);
        let temp = tempfile::tempdir().unwrap();
        let store =
            crate::internal::ai::session::jsonl::SessionJsonlStore::new(temp.path().to_path_buf());
        let provider_factory = ProviderFactory;
        let provider_options = ProviderBuildOptions::default();
        let tool_registry = crate::internal::ai::tools::ToolRegistry::with_working_dir(
            std::path::PathBuf::from("/tmp"),
        );
        let conn = sea_orm::Database::connect("sqlite::memory:").await.unwrap();
        let usage_recorder = crate::internal::ai::usage::UsageRecorder::new(conn);
        let context_frame_loader = ContextFrameLoader::default();
        let session_id: SessionId = "session".to_string();
        let abort_token = AbortToken::new();
        abort_token.cancel();

        let context = DispatchContext {
            parent_thread_id: "thread",
            parent_turn_id: None,
            parent_session_id: &session_id,
            parent_agent: &parent_spec,
            parent_ruleset: &parent_ruleset,
            parent_model_binding: &parent_binding,
            parent_message_id: MessageId::from("msg"),
            permission_service: &permission_service,
            session_store: &store,
            provider_factory: &provider_factory,
            provider_build_options: &provider_options,
            provider_build_options_resolver: None,
            tool_registry: &tool_registry,
            runtime_context: None,
            usage_recorder: &usage_recorder,
            context_frame_loader: &context_frame_loader,
            abort_token,
            depth: 0,
            compaction_model: None,
            hook_runner: None,
        };

        let request = SubAgentChildRunRequest {
            ctx: &context,
            invocation: &invocation,
            sub_spec: &sub_spec,
            effective_ruleset: &parent_ruleset,
            task_id: "task-id".to_string(),
            agent_run_id: AgentRunId::new(),
            history: Vec::new(),
            workspace_registry: None,
            workspace_runtime_context: None,
        };

        let runner = DefaultSubAgentChildRunner::new();
        let err = runner
            .run(request)
            .await
            .expect_err("pre-cancelled token must short-circuit");
        assert!(
            matches!(
                err,
                TaskFailure::Cancelled {
                    source: CancellationSource::ParentAbort
                }
            ),
            "expected Cancelled {{ ParentAbort }}, got: {err:?}",
        );
    }

    /// Scenario: a sub-agent spec with an unknown provider id flows
    /// through `ProviderFactory::build`'s `UnknownProvider` rejection
    /// and surfaces as `TaskFailure::ProviderError` carrying the
    /// factory's verbatim message — including the list of recognised
    /// providers, so the operator can fix the binding without
    /// trial-and-error.
    #[test]
    fn build_child_model_surfaces_provider_factory_unknown_provider() {
        use crate::internal::ai::providers::ProviderBuildOptions;

        let sub_spec = AgentExecutionSpec {
            name: "bad-provider".to_string(),
            model: ModelBinding::parse("definitely-not-a-real-provider/foo"),
            ..AgentExecutionSpec::default()
        };

        with_dispatch_context(ProviderBuildOptions::default(), None, |ctx| {
            let err = ctx
                .build_child_model(&sub_spec)
                .expect_err("unknown provider must fail");
            let message = err.to_string();
            assert!(
                message.contains("unknown provider"),
                "expected the factory's 'unknown provider' phrasing, got: {message}",
            );
            assert!(
                message.contains("definitely-not-a-real-provider"),
                "expected the offending provider id in the message, got: {message}",
            );
        });
    }
}
