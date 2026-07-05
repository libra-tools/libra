//! Default `SubAgentDispatcher` implementation — gates 1–4, 6–8.
//!
//! This module ships the dispatcher across OC-Phase 3 P3.3 and P3.4 from
//! `docs/development/commands/_general.md`. It implements the **gate + ask** half
//! of the 14-step dispatcher main flow:
//!
//! 1. validate feature flag (`code.multi_agent.enabled`) — P3.3 implemented
//! 2. validate `ctx.depth + 1 <= max_subagent_depth` — P3.3 implemented
//! 3. validate `concurrent_count + 1 <= max_concurrent_subagents`
//!    via atomic `fetch_add` claim — P3.3 implemented
//! 4. resolve `subagent_type` via the spec registry; reject `Primary`
//!    profiles — P3.3 implemented
//! 5. `SafetyDecision::evaluate(SubAgentSpawn { name, prompt_digest })`
//!    via `ToolBoundaryRuntime::decide` and `ToolOperation::sub_agent_spawn`.
//!    Registry-level hardening can reject a spawn before it reaches step 8.
//! 6. compute `effective_ruleset` via `child_ruleset(parent, sub_spec)`
//!    — P3.3 implemented
//! 7. assert no permission escalation (Permission Escalation Gate)
//!    — P3.3 implemented; v0.17.743 layered a parent-abort cancel
//!    check (pre-gate + post-ask) returning
//!    `TaskFailure::Cancelled { ParentAbort }` if the parent
//!    short-circuited mid-dispatch. — P3.7 partial
//! 8. `PermissionService.ask(...)` for `LlmInitiated` only;
//!    `UserInitiated { bypass_permission_ask: true }` skips the
//!    dialog. `Reject{feedback}` surfaces as
//!    [`TaskFailure::ApprovalRejected`]. — P3.4 implemented
//!
//! `Spawned` / `Completed` AgentRun lifecycle events are written into
//! the parent session JSONL as soon as gates clear — P3.5 partial
//! (v0.17.739). When a [`SubAgentChildRunner`] is attached via
//! [`DefaultSubAgentDispatcher::with_child_runner`] (v0.17.756), the
//! dispatcher delegates the post-gate work to the runner and maps
//! its `TaskFailure` into the matching `AgentRunEvent` terminal
//! variant — `Cancelled` / `TimedOut` / `BudgetExceeded` /
//! `Failed { reason }` — via [`map_failure_to_terminal_event`]
//! (v0.17.757). The runner trait itself is the OC-Phase 3 P3.4
//! entry seam.
//!
//! Steps 9–13 (model build, handoff, child run) now route through
//! [`DefaultSubAgentChildRunner`], which drives
//! `run_tool_loop_with_history_and_observer` with dispatcher-built
//! handoff history, child tool filtering, inherited runtime context,
//! parent-abort cancellation, and child session snapshots keyed by the
//! `agent_run_id`. The remaining S3/P3.4 gap is the byte-for-byte
//! per-tool child transcript fixture; the parent side already writes
//! `Spawned` plus a typed terminal event.
//!
//! [`DefaultSubAgentChildRunner`]: super::sub_agent::DefaultSubAgentChildRunner
//! [`SubAgentChildRunner`]: super::sub_agent::SubAgentChildRunner
//! [`DispatchContext::resolve_provider_build_options`]: super::sub_agent::DispatchContext::resolve_provider_build_options
//! [`DispatchContext::build_child_model`]: super::sub_agent::DispatchContext::build_child_model
//! [`ContextFrameLoader::latest_frame_for_session`]: super::sub_agent::ContextFrameLoader::latest_frame_for_session
//! Callers that pass step 8 still see the placeholder
//! [`TaskResult`] from P3.3 — empty `final_text`, zero `steps_used`,
//! the spec-derived agent / provider / model identities. Tests pin
//! that shape so a future regression that drops the placeholder
//! before steps 9–13 land is loud.

use std::sync::{
    Arc,
    atomic::{AtomicU32, Ordering},
};

use futures::future::BoxFuture;

use super::sub_agent::{
    CancellationSource, DispatchContext, PermissionAskRequest, PermissionAskSource,
    PermissionReply, SafetyDecisionDenial, SubAgentDispatcher, TaskEntryKind, TaskFailure,
    TaskInvocation, TaskResult,
};
use crate::internal::ai::{
    agent::profile::AgentExecutionSpec,
    agent_run::{AgentRunEvent, AgentRunEventEnvelope, AgentRunId},
    completion::CompletionUsageSummary,
    permission::{
        EDIT_TOOLS, PermissionRuleset, agent_permission_spec_to_ruleset, assert_no_escalation,
        child_ruleset,
    },
    runtime::ToolOperation,
    session::jsonl::SessionEvent,
};

/// Runtime configuration for the multi-agent feature gate.
///
/// `enabled` mirrors `code.multi_agent.enabled` from the doc's
/// configuration section (OC-Phase 5 will wire the TOML loader; today
/// the default is `false` so the gate is loud whenever the dispatcher
/// is invoked under flag-off).
///
/// Limits default to the doc's `max_subagent_depth = 1` and
/// `max_concurrent_subagents = 1` — even when the feature flag flips,
/// the runtime starts conservative.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct MultiAgentConfig {
    pub enabled: bool,
    pub max_subagent_depth: u8,
    pub max_concurrent_subagents: u32,
}

impl Default for MultiAgentConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            max_subagent_depth: 1,
            max_concurrent_subagents: 1,
        }
    }
}

/// Registry the dispatcher consults to resolve a `subagent_type` string
/// into the agent's [`AgentExecutionSpec`].
///
/// The trait stays minimal so callers can plug in either an
/// `AgentProfileRouter` adapter or a synthetic test registry without
/// pulling the entire profile loader through. The two methods are
/// `lookup` (the resolve path the dispatcher uses) and
/// `registered_names` (so error suggestions match the live registry).
pub trait AgentSpecRegistry: Send + Sync {
    fn lookup(&self, name: &str) -> Option<AgentExecutionSpec>;
    fn registered_names(&self) -> Vec<String>;
}

/// Default dispatcher implementation. Holds a registry, a config, and
/// a shared concurrency counter that subsequent dispatches increment +
/// decrement around the gate.
pub struct DefaultSubAgentDispatcher {
    registry: Arc<dyn AgentSpecRegistry>,
    config: MultiAgentConfig,
    in_flight: Arc<AtomicU32>,
    /// Optional child runner the dispatcher delegates to after the
    /// gates clear. When `Some`, the dispatcher tail constructs a
    /// [`SubAgentChildRunRequest`] from the current dispatch state
    /// and calls `runner.run(...).await` instead of synthesising the
    /// P3.3-era placeholder `TaskResult`. When `None`, the dispatcher
    /// falls back to the placeholder so existing call sites (and the
    /// gate-only tests) keep working unchanged.
    ///
    /// The field is plumbed now so the P3.4 child-loop PR is purely
    /// additive: that PR ships the `RealChildRunner` implementation
    /// and a `with_child_runner` constructor; nothing else in the
    /// dispatcher needs to change.
    child_runner: Option<Arc<dyn super::sub_agent::SubAgentChildRunner>>,
    /// CEX-S2-12 / S2-INV-03 workspace isolation inputs. When `Some`,
    /// the dispatch tail materializes a per-run isolated workspace,
    /// re-roots the child tool registry onto it, and rebases the
    /// inherited sandbox `writable_roots` to it before invoking the
    /// child runner — so sub-agent writes land in the workspace, not
    /// the main worktree. `None` (every gate-only test and any flag-off
    /// path) means no isolation: behaviour is unchanged.
    workspace_isolation: Option<super::sub_agent::WorkspaceIsolationConfig>,
}

impl DefaultSubAgentDispatcher {
    pub fn new(registry: Arc<dyn AgentSpecRegistry>, config: MultiAgentConfig) -> Self {
        Self {
            registry,
            config,
            in_flight: Arc::new(AtomicU32::new(0)),
            child_runner: None,
            workspace_isolation: None,
        }
    }

    /// Attach workspace-isolation inputs (CEX-S2-12 / S2-INV-03). When
    /// set, the dispatch tail materializes an isolated workspace for
    /// each sub-agent run and confines the child's writes to it. Wired
    /// by `libra code`'s `build_subagent_runtime_for_session`; left
    /// unset by gate-only tests so their behaviour is unchanged.
    pub fn with_workspace_isolation(
        mut self,
        isolation: super::sub_agent::WorkspaceIsolationConfig,
    ) -> Self {
        self.workspace_isolation = Some(isolation);
        self
    }

    /// Attach a [`SubAgentChildRunner`] to the dispatcher. The runner
    /// is consulted after every gate (1-8) clears and the Spawned
    /// event is written; its `TaskResult` (or `TaskFailure`) becomes
    /// the dispatch's outcome, and the dispatcher writes the
    /// matching `Completed` / `Failed` lifecycle event before
    /// returning. Production wires the OC-Phase 3 P3.4 implementation
    /// via this seam; tests can supply a deterministic stub.
    ///
    /// [`SubAgentChildRunner`]: super::sub_agent::SubAgentChildRunner
    pub fn with_child_runner(
        mut self,
        runner: Arc<dyn super::sub_agent::SubAgentChildRunner>,
    ) -> Self {
        self.child_runner = Some(runner);
        self
    }

    /// Convenience wrapper that attaches the production
    /// [`DefaultSubAgentChildRunner`] (single-shot model invocation
    /// with `run_tool_loop` integration). This is the dispatcher
    /// shape libra code's session bootstrap should call when
    /// `code.sub_agents.enabled = true`: gate behaviour stays
    /// unchanged, and the dispatch tail actually runs the child
    /// model instead of synthesising the P3.3-era placeholder
    /// result.
    ///
    /// [`DefaultSubAgentChildRunner`]: super::sub_agent::DefaultSubAgentChildRunner
    pub fn with_default_child_runner(self) -> Self {
        self.with_child_runner(Arc::new(super::sub_agent::DefaultSubAgentChildRunner::new()))
    }

    /// Number of dispatches currently running (test introspection only).
    #[cfg(test)]
    pub fn in_flight(&self) -> u32 {
        self.in_flight.load(Ordering::Acquire)
    }

    /// Run the seven gates in order, returning either the resolved
    /// `(sub_spec, effective_ruleset)` pair the dispatcher tail
    /// consumes or the first [`TaskFailure`] that fires. Step 8
    /// (permission ask) and the P3.5 lifecycle event writes run in
    /// the dispatcher proper; steps 9-13 (child model build,
    /// `ContextHandoff` build, child JSONL, child `run_tool_loop`)
    /// still wait for P3.4 follow-ups.
    fn run_capability_gates(
        &self,
        ctx: &DispatchContext<'_>,
        invocation: &TaskInvocation,
        _entry_kind: TaskEntryKind,
    ) -> Result<(AgentExecutionSpec, PermissionRuleset), TaskFailure> {
        // Step 1: feature flag. A dedicated `FeatureDisabled` variant
        // keeps log analysis distinct from the step-5 SafetyDenied path
        // that lands in P3.4.
        if !self.config.enabled {
            return Err(TaskFailure::FeatureDisabled);
        }

        // Step 2: depth gate.
        let next_depth = ctx.depth.saturating_add(1);
        if next_depth > self.config.max_subagent_depth {
            return Err(TaskFailure::DepthExceeded {
                current: ctx.depth,
                limit: self.config.max_subagent_depth,
            });
        }

        // Step 3 lives in dispatch() so the slot increment happens
        // atomically with the check (avoiding a TOCTOU race where two
        // concurrent dispatches both pass step 3 with `current = 0`).

        // Step 4: resolve subagent_type. `Primary`-only profiles cannot
        // be dispatched as sub-agents — they must be either `Subagent`
        // or `All`.
        let sub_spec = match self.registry.lookup(&invocation.subagent_type) {
            Some(spec) if spec.mode.is_subagent_eligible() => spec,
            Some(_unsuitable) => {
                return Err(TaskFailure::UnknownSubagent {
                    name: invocation.subagent_type.clone(),
                    suggestions: self.subagent_eligible_suggestions(),
                });
            }
            None => {
                return Err(TaskFailure::UnknownSubagent {
                    name: invocation.subagent_type.clone(),
                    suggestions: self.subagent_eligible_suggestions(),
                });
            }
        };

        // Step 5: SafetyDecision evaluate.
        //
        // Tool-boundary policy is attached to the parent tool registry.
        // If absent, this layer currently falls back to pass-through,
        // preserving historical behavior for tests and other callers that
        // do not yet inject a runtime policy.
        if let Some(hardening) = ctx.tool_registry.hardening() {
            let decision = hardening.decide(&ToolOperation::sub_agent_spawn(
                invocation.subagent_type.as_str(),
                digest_for_prompt(&invocation.prompt),
            ));
            if !decision.allowed {
                return Err(TaskFailure::SafetyDenied(SafetyDecisionDenial {
                    reason: decision.reason,
                }));
            }
        }

        // Step 6: compute effective ruleset for the child.
        let effective = child_ruleset(ctx.parent_ruleset, &sub_spec.permission);

        // Step 7: escalation gate. The doc spec calls for (builtin tool
        // names ∪ sub-spec permission keys) × ("*" ∪ sub-spec patterns).
        // Both sample sets are computed dynamically so a future
        // `AgentPermissionSpec` schema that grows non-`"*"` patterns
        // does not silently lose coverage.
        let permission_keys = collect_permission_keys(&sub_spec.permission);
        let permission_key_refs: Vec<&str> = permission_keys.iter().map(String::as_str).collect();
        let pattern_samples = collect_pattern_samples(&sub_spec.permission);
        let pattern_sample_refs: Vec<&str> = pattern_samples.iter().map(String::as_str).collect();
        if let Err((permission, pattern)) = assert_no_escalation(
            ctx.parent_ruleset,
            &effective,
            &permission_key_refs,
            &pattern_sample_refs,
        ) {
            return Err(TaskFailure::PermissionEscalationDenied {
                permission,
                pattern,
            });
        }

        Ok((sub_spec, effective))
    }

    fn subagent_eligible_suggestions(&self) -> Vec<String> {
        let mut names: Vec<String> = self
            .registry
            .registered_names()
            .into_iter()
            .filter(|name| {
                self.registry
                    .lookup(name)
                    .is_some_and(|spec| spec.mode.is_subagent_eligible())
            })
            .collect();
        names.sort();
        names
    }
}

impl SubAgentDispatcher for DefaultSubAgentDispatcher {
    fn dispatch<'a>(
        &'a self,
        ctx: DispatchContext<'a>,
        invocation: TaskInvocation,
        entry_kind: TaskEntryKind,
    ) -> BoxFuture<'a, Result<TaskResult, TaskFailure>> {
        Box::pin(async move {
            // P3.7 cancel propagation pre-check: if the parent's abort
            // token has already been cancelled before the call even
            // reaches us, refuse the dispatch up front rather than
            // claiming a concurrency slot, writing a `Spawned` event,
            // or invoking the asker. This matches opencode PR #25798's
            // "parent abort short-circuits the whole subtree"
            // semantics — running a now-stale dispatch through to
            // `Completed` would let the parent observe a successful
            // child run after the user already pressed `Ctrl-C`.
            if ctx.abort_token.is_cancelled() {
                return Err(TaskFailure::Cancelled {
                    source: CancellationSource::ParentAbort,
                });
            }

            // Steps 1, 2: feature flag + depth. These cannot mutate
            // shared state, so they run before any concurrency slot
            // is claimed. (Step 3 follows with an atomic claim.)
            if !self.config.enabled {
                return Err(TaskFailure::FeatureDisabled);
            }
            if ctx.depth.saturating_add(1) > self.config.max_subagent_depth {
                return Err(TaskFailure::DepthExceeded {
                    current: ctx.depth,
                    limit: self.config.max_subagent_depth,
                });
            }

            // Step 3: claim a concurrency slot ATOMICALLY. `fetch_add`
            // unconditionally increments and returns the previous
            // value; if that was already at the limit we roll back
            // and surface `ConcurrencyExceeded`. This avoids the
            // load-then-add TOCTOU race where two concurrent
            // dispatches could both pass a `load == 0, limit == 1`
            // check and end up with `in_flight == 2`.
            let prev = self.in_flight.fetch_add(1, Ordering::AcqRel);
            if prev >= self.config.max_concurrent_subagents {
                self.in_flight.fetch_sub(1, Ordering::AcqRel);
                return Err(TaskFailure::ConcurrencyExceeded {
                    current: prev,
                    limit: self.config.max_concurrent_subagents,
                });
            }

            // RAII guard: from here on every exit path (early-return on
            // a TaskFailure from steps 4-7, panic, or normal success at
            // the end) decrements the counter exactly once. P3.4 will
            // put real I/O between this guard's creation and the
            // placeholder result; the guard is what prevents a panic
            // in that I/O from orphaning the slot.
            let _slot = ConcurrencyGuard {
                counter: Arc::clone(&self.in_flight),
            };

            // Steps 4-7: capability + permission gates that don't
            // touch the concurrency counter.
            let (sub_spec, _effective) =
                self.run_capability_gates(&ctx, &invocation, entry_kind)?;

            // The same prompt digest is used both by the LlmInitiated
            // permission ask and by the `Spawned` event below, so
            // compute it once and reuse.
            let prompt_digest = digest_for_prompt(&invocation.prompt);

            // Step 8: permission ask. Per the doc's "Two Entry Points"
            // table, only `LlmInitiated` triggers the ask. **All**
            // `UserInitiated` variants — both `bypass_permission_ask:
            // true` and `false` — currently skip the dialog because
            // today's only `UserInitiated` call sites (slash command,
            // Code Control RPC, SubtaskPart payload arriving in P3.6)
            // set `bypass: true` by construction. P3.6 reviews
            // whether a `bypass: false` slash-command path is
            // actually meaningful; if so, this match arm widens to
            // include it.
            if let TaskEntryKind::LlmInitiated = entry_kind {
                let patterns = vec![invocation.subagent_type.clone()];
                let request = PermissionAskRequest {
                    permission: "task",
                    patterns: &patterns,
                    thread_id: ctx.parent_thread_id,
                    session_id: ctx.parent_session_id,
                    source: PermissionAskSource::SubAgentSpawn {
                        name: invocation.subagent_type.clone(),
                        prompt_digest: prompt_digest.clone(),
                    },
                };
                match ctx.permission_service.ask(request).await {
                    PermissionReply::Once | PermissionReply::Always { .. } => {
                        // The dispatcher itself does not persist
                        // `Always` patterns — that is the responsibility
                        // of the asker implementation, which has access
                        // to the project's `ApprovedRulesetStore`.
                    }
                    PermissionReply::Reject { feedback } => {
                        return Err(TaskFailure::ApprovalRejected { feedback });
                    }
                }
            }

            // P3.7: a second cancel check after step 8 covers the
            // window where the asker awaited a human reply long
            // enough that the parent aborted in between. Failing
            // closed here means we never write a `Spawned` event for
            // a dispatch that the caller has already abandoned.
            if ctx.abort_token.is_cancelled() {
                return Err(TaskFailure::Cancelled {
                    source: CancellationSource::ParentAbort,
                });
            }

            // P3.5: emit the `Spawned` lifecycle event into the parent
            // session JSONL immediately after every dispatch gate
            // (capability + concurrency + permission) has cleared.
            // This is the earliest point at which a child run is
            // semantically committed; tests and replay tooling rely on
            // `Spawned` preceding any child-side event. The event is a
            // best-effort fire-and-forget write — propagating an IO
            // error here would force the dispatcher to fail dispatches
            // that have already passed every safety gate, so we log
            // and continue.
            let agent_run_id = AgentRunId::new();
            let provider_id = sub_spec
                .model
                .as_ref()
                .map(|m| m.provider_id.clone())
                .unwrap_or_default();
            let model_id = sub_spec
                .model
                .as_ref()
                .map(|m| m.model_id.clone())
                .unwrap_or_default();
            if let Err(err) =
                ctx.session_store
                    .append(&SessionEvent::AgentRun(AgentRunEventEnvelope::from(
                        AgentRunEvent::Spawned {
                            agent_run_id,
                            parent_thread_id: ctx.parent_thread_id.to_string(),
                            parent_session_id: ctx.parent_session_id.clone(),
                            parent_message_id: ctx.parent_message_id.clone(),
                            subagent_name: invocation.subagent_type.clone(),
                            provider_id: provider_id.clone(),
                            model_id: model_id.clone(),
                            depth: ctx.depth.saturating_add(1),
                            prompt_digest,
                        },
                    )))
            {
                tracing::warn!(
                    error = %err,
                    agent_run_id = %agent_run_id.0,
                    subagent = %invocation.subagent_type,
                    "failed to append AgentRunEvent::Spawned to parent session JSONL"
                );
            }

            // Bind the task id to the run id so future call sites
            // that grep the JSONL stream can correlate the dispatch
            // back to its `Spawned` event. Both the P3.4 child runner
            // and the legacy placeholder tail consume the same id.
            let task_id = invocation
                .task_id
                .clone()
                .unwrap_or_else(|| format!("task-placeholder-{}", agent_run_id.0));

            // Steps 9-13: when a child runner is registered, delegate
            // to it. Otherwise fall back to the P3.3-era placeholder
            // so existing gate-only tests keep working unchanged. The
            // runner branch is the seam OC-Phase 3 P3.4 fills in;
            // today every test path takes the placeholder branch.
            // CEX-S2-12 / S2-INV-03: holds the materialized isolated
            // workspace (if any) so it survives the child run and is
            // cleaned up after the terminal event below. The RAII guard
            // is a panic backstop: if the child run unwinds before the
            // explicit post-terminal cleanup, the guard's `Drop` still
            // tears the workspace down (no leak). The happy path
            // `take()`s the workspace out of the guard first, so its
            // `Drop` is a no-op there.
            let mut workspace_guard = WorkspaceCleanupGuard { workspace: None };
            let outcome = if let Some(runner) = self.child_runner.as_ref() {
                // OC-Phase 4 minimum-viable handoff (v0.17.773) +
                // P4.4 compacted handoff (v0.17.785): load the
                // parent's latest `ContextFrameEvent` from the
                // session JSONL and materialise it into the
                // child's history before the user prompt lands.
                //
                // Routing rule:
                //   - parent frame present + `ctx.compaction_model`
                //     present: run the compaction agent and feed
                //     the validated `ContextHandoff` via
                //     `to_handoff_messages()` (v0.17.781).
                //   - parent frame present + no compaction model:
                //     fall back to the v0.17.773 raw-segment
                //     dump.
                //   - no parent frame: empty history.
                //
                // Compaction failures (provider error, malformed
                // SUMMARY template) emit `tracing::warn!` and
                // degrade to the raw-segment path. The dispatch
                // never blocks on a compaction failure — the
                // child still runs.
                let parent_frame = ctx
                    .context_frame_loader
                    .latest_frame_for_session(ctx.session_store)
                    .ok()
                    .flatten();
                let history = match (parent_frame.as_ref(), ctx.compaction_model) {
                    (Some(frame), Some(compaction_model)) => {
                        let frame_text = frame
                            .segments
                            .iter()
                            .filter_map(|seg| seg.content.as_deref())
                            .collect::<Vec<_>>()
                            .join("\n\n");
                        let attachment_refs = frame.attachment_refs();
                        let system_prompt =
                            crate::internal::ai::context_budget::embedded_compaction_system_prompt(
                            );
                        match crate::internal::ai::context_budget::run_compaction(
                            compaction_model,
                            system_prompt,
                            &frame_text,
                            frame.frame_id,
                            attachment_refs,
                            Vec::new(),
                            0,
                        )
                        .await
                        {
                            Ok(handoff) => handoff.to_handoff_messages(),
                            Err(err) => {
                                tracing::warn!(
                                    %err,
                                    "compaction agent failed; falling back to raw-segment handoff",
                                );
                                frame.to_handoff_messages()
                            }
                        }
                    }
                    (Some(frame), None) => frame.to_handoff_messages(),
                    (None, _) => Vec::new(),
                };

                // CEX-S2-12 / S2-INV-03: when isolation is configured,
                // materialize a per-run workspace and hand the child a
                // registry re-rooted onto it + the inherited runtime
                // context with sandbox `writable_roots` rebased to it.
                //
                // FAIL CLOSED: if isolation is configured but the
                // workspace cannot be materialized, refuse the dispatch
                // (`SafetyDenied`) rather than running the sub-agent
                // unsandboxed against the main worktree — a mutating
                // child must never fall back to the parent worktree when
                // isolation was required (S2-INV-03). When isolation is
                // NOT configured (`workspace_isolation == None`: every
                // flag-off / gate-only path) the child runs with the
                // parent registry/context exactly as before.
                let isolation_overrides: Result<
                    (
                        Option<crate::internal::ai::tools::ToolRegistry>,
                        Option<crate::internal::ai::sandbox::ToolRuntimeContext>,
                    ),
                    TaskFailure,
                > = match self.workspace_isolation.as_ref() {
                    Some(isolation) => {
                        match materialize_isolated_dispatch_workspace(&ctx, agent_run_id, isolation)
                        {
                            Ok((registry, runtime_context, workspace)) => {
                                workspace_guard.workspace = Some(workspace);
                                Ok((Some(registry), runtime_context))
                            }
                            Err(err) => {
                                tracing::warn!(
                                    error = %err,
                                    agent_run_id = %agent_run_id.0,
                                    subagent = %invocation.subagent_type,
                                    "refusing to dispatch sub-agent: isolated workspace could \
                                     not be materialized and isolation is required",
                                );
                                Err(TaskFailure::SafetyDenied(SafetyDecisionDenial {
                                    reason: format!(
                                        "sub-agent workspace isolation could not be \
                                         materialized ({err}); refusing to run the sub-agent \
                                         unsandboxed against the main worktree"
                                    ),
                                }))
                            }
                        }
                    }
                    None => Ok((None, None)),
                };

                match isolation_overrides {
                    Ok((workspace_registry, workspace_runtime_context)) => {
                        let request = super::sub_agent::SubAgentChildRunRequest {
                            ctx: &ctx,
                            invocation: &invocation,
                            sub_spec: &sub_spec,
                            effective_ruleset: &_effective,
                            task_id: task_id.clone(),
                            agent_run_id,
                            history,
                            workspace_registry,
                            workspace_runtime_context,
                        };
                        runner.run(request).await
                    }
                    Err(failure) => Err(failure),
                }
            } else {
                Ok(TaskResult {
                    task_id,
                    agent_name: sub_spec.name.clone(),
                    provider_id,
                    model_id,
                    final_text: String::new(),
                    steps_used: 0,
                    usage: CompletionUsageSummary::default(),
                })
            };

            // P3.5: mirror the dispatch tail with the matching
            // lifecycle event. `Completed` for success; for failures
            // the doc spec distinguishes structurally between
            // Failed / Cancelled / TimedOut / BudgetExceeded so
            // replay tooling can branch on the variant tag without
            // string-matching the reason. Free-form `Failed {
            // reason }` is the catch-all for everything else (e.g.
            // ProviderError, ChildToolLoopFailed) — the reason text
            // is the TaskFailure's Display so the parent transcript
            // and the persisted event agree byte-for-byte.
            // Best-effort: append failures degrade to tracing::warn
            // rather than overriding the outcome.
            let terminal_event = match &outcome {
                Ok(_) => AgentRunEvent::Completed { agent_run_id },
                Err(failure) => map_failure_to_terminal_event(agent_run_id, failure),
            };
            if let Err(err) =
                ctx.session_store
                    .append(&SessionEvent::AgentRun(AgentRunEventEnvelope::from(
                        terminal_event,
                    )))
            {
                tracing::warn!(
                    error = %err,
                    agent_run_id = %agent_run_id.0,
                    subagent = %invocation.subagent_type,
                    outcome_ok = outcome.is_ok(),
                    "failed to append AgentRunEvent::Completed/Failed to parent session JSONL"
                );
            }

            // CEX-S2-12 / S2-INV-03 + CEX-S2-11 (5): tear down the
            // isolated workspace now that the child run is done so no
            // workspace leaks. FUSE cleanup blocks on the runtime
            // (`Handle::block_on`), so route it through
            // `spawn_blocking`; awaiting keeps the teardown observable
            // (a test can assert the workspace is gone) without
            // blocking the async runtime thread.
            //
            // TODO(CEX-S2-12-cancel): on a parent abort, the child
            // tool-loop future is dropped cooperatively but a tool
            // handler's detached `spawn_blocking` write (e.g.
            // `apply_patch`) may still be running against the workspace
            // when this teardown fires. This is a cleanup/lifecycle
            // race, not an S2-INV-03 breach (the detached write is
            // confined to the workspace, never the main worktree), and
            // on unix the teardown is benign (the dir entry is removed
            // and the in-flight write completes against the unlinked
            // inode). The correct fix — propagating cancellation into
            // the blocking tool handlers and awaiting in-flight tasks
            // before teardown — is a cross-cutting tool-loop change
            // tracked separately, not part of this isolation slice.
            if let Some(workspace) = workspace_guard.workspace.take() {
                match tokio::task::spawn_blocking(move || workspace.cleanup()).await {
                    Ok(Ok(())) => {}
                    Ok(Err(err)) => tracing::warn!(
                        error = %err,
                        agent_run_id = %agent_run_id.0,
                        "failed to clean up isolated sub-agent workspace",
                    ),
                    Err(join_err) => tracing::warn!(
                        error = %join_err,
                        agent_run_id = %agent_run_id.0,
                        "isolated sub-agent workspace cleanup task panicked",
                    ),
                }
            }

            // `_slot` drops here, releasing the concurrency slot.
            outcome
        })
    }
}

/// Materialize a per-run isolated workspace and record the
/// `workspace_materialized` audit event (CEX-S2-12 / S2-INV-03).
///
/// **AG-22 / plan.md Task A7 public seam — this is the mandatory
/// reviewer isolation path.** External review agents (`libra review`)
/// must run inside a workspace materialized through *this* function,
/// never in the main worktree: the materialization walk honours ignore
/// rules (`WalkBuilder::git_ignore(true)`), so gitignored secret files
/// (e.g. `.env.test`) are excluded from what a reviewer process can
/// read — the first line of defense against secret exfiltration
/// (redaction of persisted reviewer output is only the on-disk
/// fallback). Exposure choice: the fn is `pub` in place here, beside
/// the sub-agent dispatcher that owns the isolation mechanics, and is
/// re-exported at [`crate::internal::ai::review`] as the
/// reviewer-facing path.
///
/// `main_working_dir` is the repo worktree to mirror; `thread_id` only
/// names the audit transcript path
/// `.libra/sessions/{thread_id}/agents/{run_id}.jsonl` the
/// `workspace_materialized` event is appended to (rooted at
/// `isolation.sessions_root`).
///
/// The returned [`SubAgentWorkspace`] must be held by the caller until
/// the run completes and then torn down via
/// [`SubAgentWorkspace::cleanup`] (no leaked workspaces,
/// CEX-S2-11 (5)). When isolation is required, materialization failure
/// is terminal for the caller: falling back to the main worktree would
/// violate S2-INV-03 (and, for reviewers, plan.md:946).
///
/// [`SubAgentWorkspace`]: crate::internal::ai::orchestrator::workspace::SubAgentWorkspace
/// [`SubAgentWorkspace::cleanup`]: crate::internal::ai::orchestrator::workspace::SubAgentWorkspace::cleanup
pub fn materialize_isolated_workspace(
    main_working_dir: &std::path::Path,
    thread_id: uuid::Uuid,
    agent_run_id: AgentRunId,
    isolation: &super::sub_agent::WorkspaceIsolationConfig,
) -> Result<
    crate::internal::ai::orchestrator::workspace::SubAgentWorkspace,
    crate::internal::ai::orchestrator::workspace::SubAgentWorkspaceError,
> {
    use crate::internal::ai::{
        agent_run::{event_store::AgentRunEventStore, workspace_sizing::measure_workspace_sizing},
        orchestrator::workspace::materialize_sub_agent_workspace,
    };

    let sizing = measure_workspace_sizing(
        &main_working_dir.join(crate::utils::util::ROOT_DIR),
        main_working_dir,
    );
    let store = AgentRunEventStore::new(isolation.sessions_root.clone());

    materialize_sub_agent_workspace(
        main_working_dir,
        sizing,
        thread_id,
        agent_run_id,
        isolation.allow_full_copy,
        &isolation.fuse_state,
        &store,
    )
}

/// Materialize an isolated workspace for a sub-agent run and return the
/// re-rooted child tool registry + the inherited runtime context with
/// its sandbox `writable_roots` rebased onto the workspace
/// (CEX-S2-12 / S2-INV-03).
///
/// Dispatcher-internal wrapper over the public
/// [`materialize_isolated_workspace`] seam: the workspace mechanics are
/// shared with the AG-22 reviewer path; only the registry re-root and
/// sandbox rebase below are sub-agent-dispatch specific.
///
/// The returned [`SubAgentWorkspace`] must be held by the caller until
/// the child run completes and then cleaned up (no leaked workspaces,
/// CEX-S2-11 (5)). When isolation is configured, materialization
/// failure is terminal for the dispatch: running a mutating child
/// against the main worktree would violate S2-INV-03.
///
/// [`SubAgentWorkspace`]: crate::internal::ai::orchestrator::workspace::SubAgentWorkspace
fn materialize_isolated_dispatch_workspace(
    ctx: &DispatchContext<'_>,
    agent_run_id: AgentRunId,
    isolation: &super::sub_agent::WorkspaceIsolationConfig,
) -> Result<
    (
        crate::internal::ai::tools::ToolRegistry,
        Option<crate::internal::ai::sandbox::ToolRuntimeContext>,
        crate::internal::ai::orchestrator::workspace::SubAgentWorkspace,
    ),
    crate::internal::ai::orchestrator::workspace::SubAgentWorkspaceError,
> {
    let main_working_dir = ctx.tool_registry.working_dir().to_path_buf();
    // `thread_id` only names the `WorkspaceMaterialized` transcript
    // path; `parent_thread_id` is a free-form `String` today, so parse
    // with a fresh-uuid fallback.
    let thread_id =
        uuid::Uuid::parse_str(ctx.parent_thread_id).unwrap_or_else(|_| uuid::Uuid::new_v4());

    let workspace =
        materialize_isolated_workspace(&main_working_dir, thread_id, agent_run_id, isolation)?;

    let workspace_root = workspace.root().to_path_buf();
    // Re-root the child registry onto the workspace, aliasing the
    // user-facing repo path into it so provider calls that reuse the
    // main-worktree absolute path still resolve inside the workspace.
    let registry = ctx
        .tool_registry
        .clone_with_working_dir_and_alias(workspace_root.clone(), main_working_dir);
    // Rebase the inherited sandbox `writable_roots` onto the workspace
    // so an absolute-path `shell` write to the main worktree is denied.
    let runtime_context = ctx.runtime_context.clone().map(|mut rc| {
        if let Some(sandbox) = rc.sandbox.as_mut() {
            sandbox.policy = sandbox.policy.rebased_to_workspace(&workspace_root);
        }
        rc
    });

    Ok((registry, runtime_context, workspace))
}

/// Panic backstop for a materialized sub-agent workspace
/// (CEX-S2-12 / S2-INV-03 + CEX-S2-11 (5): no leaked workspaces).
///
/// The dispatch tail cleans the workspace up explicitly (and
/// observably, via an awaited `spawn_blocking`) after the terminal
/// lifecycle event, `take()`ing it out of the guard first. This guard's
/// `Drop` only fires when the dispatch *unwinds* before that point —
/// e.g. a child-runner panic — so the workspace is still torn down.
///
/// `Drop` must never block the current thread (a FUSE unmount uses
/// `Handle::block_on`), so on unwind it schedules the teardown on the
/// blocking pool when a runtime is available, falling back to an inline
/// best-effort removal otherwise.
struct WorkspaceCleanupGuard {
    workspace: Option<crate::internal::ai::orchestrator::workspace::SubAgentWorkspace>,
}

impl Drop for WorkspaceCleanupGuard {
    fn drop(&mut self) {
        use crate::internal::ai::orchestrator::types::TaskWorkspaceBackend;

        let Some(workspace) = self.workspace.take() else {
            return;
        };
        // A FUSE teardown unmounts via `Handle::block_on`, which must
        // NOT run on the async runtime thread (this `Drop` may fire
        // mid-unwind inside the dispatch future) — route it to the
        // blocking pool when a runtime is available. A copy-backend
        // teardown is plain filesystem removal, safe to run inline even
        // during an unwind, so we do it synchronously (this is also what
        // makes a panicking-child regression test deterministic under
        // the copy backend).
        if matches!(workspace.backend(), TaskWorkspaceBackend::Fuse)
            && let Ok(handle) = tokio::runtime::Handle::try_current()
        {
            handle.spawn_blocking(move || {
                if let Err(err) = workspace.cleanup() {
                    tracing::warn!(
                        error = %err,
                        "failed to clean up sub-agent workspace during unwind",
                    );
                }
            });
            return;
        }
        if let Err(err) = workspace.cleanup() {
            tracing::warn!(
                error = %err,
                "failed to clean up sub-agent workspace during unwind",
            );
        }
    }
}

/// RAII handle for a concurrency slot claimed via [`AtomicU32::fetch_add`].
///
/// Dropping the guard decrements the counter once, regardless of
/// whether the dispatch returned `Ok`, returned `Err`, or panicked.
/// This is the doc-promised "decrement happens in dispatch's
/// drop-guarded scope" invariant. The guard holds an `Arc` to the
/// counter so it can outlive the dispatcher's borrow if a future
/// refactor moves the dispatcher behind a different ownership model.
struct ConcurrencyGuard {
    counter: Arc<AtomicU32>,
}

impl Drop for ConcurrencyGuard {
    fn drop(&mut self) {
        self.counter.fetch_sub(1, Ordering::AcqRel);
    }
}

/// Collect every permission key referenced by a sub-spec, plus the
/// canonical defense-in-depth set the doc requires (`task`,
/// `todowrite`, `edit`, every member of `EDIT_TOOLS`). The result
/// feeds into the escalation gate's cartesian product.
fn collect_permission_keys(
    spec: &crate::internal::ai::agent::profile::AgentPermissionSpec,
) -> Vec<String> {
    use std::collections::BTreeSet;
    let mut set: BTreeSet<String> = BTreeSet::new();
    for tool in &spec.allowed_tools {
        set.insert(tool.clone());
    }
    for tool in &spec.denied_tools {
        set.insert(tool.clone());
    }
    set.insert("task".to_string());
    set.insert("todowrite".to_string());
    set.insert("edit".to_string());
    for tool in EDIT_TOOLS {
        set.insert((*tool).to_string());
    }
    set.into_iter().collect()
}

/// Produce a short, human-readable preview of a prompt for the
/// permission ask UI. Cap at the first line and 80 characters so the
/// digest fits in a one-line prompt header. Not a cryptographic hash —
/// the goal is "enough to recognise the dispatch in a log", not
/// uniqueness.
/// Translate a `TaskFailure` into the matching `AgentRunEvent`
/// terminal variant.
///
/// The OC-Phase 3 P3.5 contract distinguishes between Failed /
/// Cancelled / TimedOut / BudgetExceeded at the event level so
/// replay tooling can branch on the variant tag without scanning
/// `Failed.reason` for substrings. Variants outside this
/// structural taxonomy (e.g. provider error, child-tool-loop
/// failure) fall through to `Failed { reason }` with the
/// `TaskFailure`'s `Display` text as the reason — that text is the
/// same one the parent transcript shows, so a downstream reader
/// can correlate the event with the parent's diagnostic byte-for-byte.
fn map_failure_to_terminal_event(
    agent_run_id: crate::internal::ai::agent_run::AgentRunId,
    failure: &TaskFailure,
) -> AgentRunEvent {
    use crate::internal::ai::agent_run::{BudgetDimension, CancellationReason};

    match failure {
        TaskFailure::Cancelled {
            source: CancellationSource::ParentAbort,
        } => AgentRunEvent::Cancelled {
            agent_run_id,
            reason: CancellationReason::UserRequested,
        },
        TaskFailure::Cancelled {
            source: CancellationSource::Timeout,
        } => AgentRunEvent::Cancelled {
            agent_run_id,
            reason: CancellationReason::LayerOneTimeout,
        },
        TaskFailure::Cancelled {
            source: CancellationSource::BudgetHardCap,
        } => AgentRunEvent::Cancelled {
            agent_run_id,
            reason: CancellationReason::Other("budget_hard_cap".to_string()),
        },
        TaskFailure::Timeout { .. } => AgentRunEvent::TimedOut { agent_run_id },
        TaskFailure::BudgetExceeded(super::sub_agent::BudgetExceededReason::CostHardCap) => {
            AgentRunEvent::BudgetExceeded {
                agent_run_id,
                dimension: BudgetDimension::Cost,
            }
        }
        TaskFailure::BudgetExceeded(super::sub_agent::BudgetExceededReason::TokenHardCap) => {
            AgentRunEvent::BudgetExceeded {
                agent_run_id,
                dimension: BudgetDimension::Token,
            }
        }
        TaskFailure::BudgetExceeded(super::sub_agent::BudgetExceededReason::WallClock) => {
            AgentRunEvent::BudgetExceeded {
                agent_run_id,
                dimension: BudgetDimension::WallClock,
            }
        }
        TaskFailure::BudgetExceeded(super::sub_agent::BudgetExceededReason::Steps) => {
            // No dedicated "Steps" dimension — use ToolCall as the
            // structural neighbour and preserve the Display reason
            // in the event semantics via the variant tag itself.
            AgentRunEvent::BudgetExceeded {
                agent_run_id,
                dimension: BudgetDimension::ToolCall,
            }
        }
        // Everything else (FeatureDisabled / UnknownSubagent /
        // DepthExceeded / ConcurrencyExceeded /
        // PermissionEscalationDenied / SafetyDenied /
        // ApprovalRejected / BudgetExceeded(Internal) /
        // ContextHandoffFailed / ProviderError /
        // ChildToolLoopFailed) goes through Failed with the
        // Display text. Pre-gate failures never reach this helper
        // because they return Err before the Spawned event fires.
        _ => AgentRunEvent::Failed {
            agent_run_id,
            reason: failure.to_string(),
        },
    }
}

fn digest_for_prompt(prompt: &str) -> String {
    let first_line = prompt.lines().next().unwrap_or("").trim();
    if first_line.chars().count() <= 80 {
        first_line.to_string()
    } else {
        let truncated: String = first_line.chars().take(77).collect();
        format!("{truncated}…")
    }
}

/// Collect every pattern referenced by a sub-spec's converted ruleset,
/// always including `"*"` as a defense-in-depth sample. The doc
/// requires `("*" ∪ child_spec.permission.iter().map(|r| &r.pattern))`
/// for the escalation gate's cartesian product; computing this from
/// the converted ruleset future-proofs the dispatcher against a
/// schema evolution that grows non-`"*"` patterns on
/// `AgentPermissionSpec`.
fn collect_pattern_samples(
    spec: &crate::internal::ai::agent::profile::AgentPermissionSpec,
) -> Vec<String> {
    use std::collections::BTreeSet;
    let mut set: BTreeSet<String> = BTreeSet::new();
    set.insert("*".to_string());
    for rule in agent_permission_spec_to_ruleset(spec) {
        set.insert(rule.pattern);
    }
    set.into_iter().collect()
}

#[cfg(test)]
mod tests {
    use std::{
        collections::{BTreeSet, HashMap},
        sync::{Mutex, OnceLock},
    };

    use futures::future::BoxFuture;
    use sea_orm::Database;

    use super::*;
    use crate::internal::ai::{
        agent::{
            profile::{
                AgentExecutionSpec, AgentMode, AgentPermissionSpec, ApprovalRoutingSpec,
                ModelBinding, ToolSelection,
            },
            runtime::sub_agent::{
                AbortToken, ContextFrameLoader, DispatchContext, MessageId, PermissionAskRequest,
                PermissionAsker, PermissionReply, PermissionService, SubAgentDispatcher,
                TaskEntryKind, TaskFailure, TaskInvocation,
            },
        },
        permission::{PermissionAction, PermissionRule, PermissionRuleset},
        providers::{ProviderBuildOptions, ProviderFactory},
        runtime::{
            InMemoryAuditSink, PrincipalContext, PrincipalRole, SecretRedactor, ToolBoundaryPolicy,
            ToolBoundaryRuntime,
        },
        session::SessionId,
        tools::ToolRegistry,
        usage::UsageRecorder,
    };

    /// Process-wide empty `ProviderBuildOptions` used by every
    /// `DispatchContext` test fixture. The gates exercised here never
    /// read these fields, but the struct shape requires the borrow,
    /// so a single shared static keeps every `ctx()` call site free of
    /// per-test allocation noise.
    fn default_provider_build_options() -> &'static ProviderBuildOptions {
        static OPTS: OnceLock<ProviderBuildOptions> = OnceLock::new();
        OPTS.get_or_init(ProviderBuildOptions::default)
    }

    /// Process-wide empty `ToolRegistry` used by every
    /// `DispatchContext` test fixture. Construction uses
    /// `with_working_dir(".")` so the helper never panics on
    /// CWD-resolution like `ToolRegistry::new()` could under a
    /// concurrent harness.
    fn default_tool_registry() -> &'static ToolRegistry {
        static REG: OnceLock<ToolRegistry> = OnceLock::new();
        REG.get_or_init(|| ToolRegistry::with_working_dir(std::path::PathBuf::from(".")))
    }

    fn observer_tool_registry() -> ToolRegistry {
        let hardening = ToolBoundaryRuntime::new(
            uuid::Uuid::new_v4(),
            PrincipalContext {
                principal_id: "observer".to_string(),
                role: PrincipalRole::Observer,
            },
            ToolBoundaryPolicy::default_runtime(),
            SecretRedactor::default_runtime(),
            std::sync::Arc::new(InMemoryAuditSink::default()),
        );
        ToolRegistry::with_working_dir(std::path::PathBuf::from(".")).with_hardening(hardening)
    }

    /// Test asker that replies with a pre-canned [`PermissionReply`]
    /// and counts how many times `ask()` was invoked. The counter
    /// pins the doc's "ask only on `LlmInitiated`" rule.
    struct TestAsker {
        reply: PermissionReply,
        ask_calls: Mutex<u32>,
    }

    impl TestAsker {
        fn always(reply: PermissionReply) -> Self {
            Self {
                reply,
                ask_calls: Mutex::new(0),
            }
        }
        fn ask_call_count(&self) -> u32 {
            *self.ask_calls.lock().unwrap()
        }
    }

    impl PermissionAsker for TestAsker {
        fn ask<'a>(&'a self, _request: PermissionAskRequest<'a>) -> BoxFuture<'a, PermissionReply> {
            *self.ask_calls.lock().unwrap() += 1;
            let reply = self.reply.clone();
            Box::pin(async move { reply })
        }
    }

    /// Build a [`PermissionService`] backed by a fresh `TestAsker` and
    /// hand back a clone of the asker so the test can assert the call
    /// count after dispatch.
    fn allow_once_service() -> (PermissionService, Arc<TestAsker>) {
        let asker = Arc::new(TestAsker::always(PermissionReply::Once));
        let service = PermissionService::new(asker.clone());
        (service, asker)
    }

    /// Load the known `AgentRunEvent`s the dispatcher wrote to the
    /// parent session JSONL, in append order (skips non-AgentRun /
    /// unknown envelopes). Used by the lifecycle-fixture tests to assert
    /// the Spawned + terminal event pair.
    fn agent_run_events(
        store: &crate::internal::ai::session::jsonl::SessionJsonlStore,
    ) -> Vec<AgentRunEvent> {
        store
            .load_events()
            .expect("JSONL readable")
            .into_iter()
            .filter_map(|envelope| match envelope {
                crate::internal::ai::session::jsonl::SessionEvent::AgentRun(known) => {
                    known.known().cloned()
                }
                _ => None,
            })
            .collect()
    }

    /// Test-only registry storing specs in a HashMap; the doc says any
    /// registry works as long as it implements the trait.
    #[derive(Default)]
    struct TestRegistry {
        specs: Mutex<HashMap<String, AgentExecutionSpec>>,
    }

    impl TestRegistry {
        fn insert(&self, spec: AgentExecutionSpec) {
            self.specs.lock().unwrap().insert(spec.name.clone(), spec);
        }
    }

    impl AgentSpecRegistry for TestRegistry {
        fn lookup(&self, name: &str) -> Option<AgentExecutionSpec> {
            self.specs.lock().unwrap().get(name).cloned()
        }
        fn registered_names(&self) -> Vec<String> {
            self.specs.lock().unwrap().keys().cloned().collect()
        }
    }

    fn explore_subagent() -> AgentExecutionSpec {
        let mut spec = AgentExecutionSpec {
            name: "explore".to_string(),
            description: "Read-only explorer".to_string(),
            mode: AgentMode::Subagent,
            ..AgentExecutionSpec::default()
        };
        let mut allowed = BTreeSet::new();
        allowed.insert("read_file".to_string());
        spec.permission = AgentPermissionSpec {
            allowed_tools: allowed,
            ..AgentPermissionSpec::default()
        };
        spec
    }

    fn primary_only_agent() -> AgentExecutionSpec {
        AgentExecutionSpec {
            name: "planner".to_string(),
            description: "Primary planner".to_string(),
            mode: AgentMode::Primary,
            ..AgentExecutionSpec::default()
        }
    }

    fn parent_spec() -> AgentExecutionSpec {
        AgentExecutionSpec {
            name: "parent".to_string(),
            description: "Parent driver".to_string(),
            mode: AgentMode::Primary,
            tools: ToolSelection::Inherit,
            permission: AgentPermissionSpec {
                approval_routing: ApprovalRoutingSpec::Layer1Human,
                ..AgentPermissionSpec::default()
            },
            ..AgentExecutionSpec::default()
        }
    }

    fn parent_binding() -> ModelBinding {
        ModelBinding::parse("anthropic/claude-3-5-sonnet-latest").unwrap()
    }

    /// Build a `DispatchContext` for tests. The placeholder service
    /// shells are intentionally `Default::default()`; the gates we
    /// exercise here do not touch them.
    #[allow(clippy::too_many_arguments)]
    fn ctx<'a>(
        parent_thread_id: &'a str,
        parent_session_id: &'a SessionId,
        parent_agent: &'a AgentExecutionSpec,
        parent_ruleset: &'a PermissionRuleset,
        parent_binding: &'a ModelBinding,
        permission_service: &'a PermissionService,
        session_store: &'a crate::internal::ai::session::jsonl::SessionJsonlStore,
        provider_factory: &'a ProviderFactory,
        usage_recorder: &'a UsageRecorder,
        context_frame_loader: &'a ContextFrameLoader,
        depth: u8,
    ) -> DispatchContext<'a> {
        DispatchContext {
            parent_thread_id,
            parent_session_id,
            parent_agent,
            parent_ruleset,
            parent_model_binding: parent_binding,
            parent_message_id: MessageId::from("msg-1"),
            permission_service,
            session_store,
            provider_factory,
            provider_build_options: default_provider_build_options(),
            provider_build_options_resolver: None,
            tool_registry: default_tool_registry(),
            runtime_context: None,
            usage_recorder,
            context_frame_loader,
            abort_token: AbortToken::new(),
            depth,
            compaction_model: None,
            hook_runner: None,
        }
    }

    /// Helper to async-build the runtime services tests need.
    async fn dispatcher_test_harness(
        config: MultiAgentConfig,
    ) -> (
        DefaultSubAgentDispatcher,
        Arc<TestRegistry>,
        UsageRecorder,
        crate::internal::ai::session::jsonl::SessionJsonlStore,
    ) {
        let registry = Arc::new(TestRegistry::default());
        let dispatcher = DefaultSubAgentDispatcher::new(registry.clone(), config);
        let conn = Database::connect("sqlite::memory:").await.unwrap();
        let usage_recorder = UsageRecorder::new(conn);
        let temp = tempfile::tempdir().unwrap();
        let store =
            crate::internal::ai::session::jsonl::SessionJsonlStore::new(temp.path().to_path_buf());
        // Leak the temp dir so the SessionJsonlStore reference remains
        // valid for the test duration.
        std::mem::forget(temp);
        (dispatcher, registry, usage_recorder, store)
    }

    fn invocation(subagent_type: &str) -> TaskInvocation {
        TaskInvocation {
            description: "test invocation".to_string(),
            prompt: "do a thing".to_string(),
            subagent_type: subagent_type.to_string(),
            task_id: None,
        }
    }

    /// Scenario: with `multi_agent.enabled = false`, the dispatcher
    /// rejects every dispatch with `FeatureDisabled`. This is the
    /// flag-off invariant — even if the tool slipped past the
    /// registry-level filter, the dispatcher still refuses with a
    /// dedicated variant (not `SafetyDenied`, which is reserved for
    /// step-5 sandbox rejections in P3.4).
    #[tokio::test]
    async fn dispatch_rejects_when_feature_flag_disabled() {
        let (dispatcher, registry, usage, store) =
            dispatcher_test_harness(MultiAgentConfig::default()).await;
        registry.insert(explore_subagent());

        let parent = parent_spec();
        let parent_ruleset: PermissionRuleset = Vec::new();
        let parent_binding = parent_binding();
        let (permission_service, _asker) = allow_once_service();
        let provider_factory = ProviderFactory;
        let context_frame_loader = ContextFrameLoader::default();
        let parent_thread = "thread-1".to_string();
        let parent_session: SessionId = "session-1".to_string();

        let context = ctx(
            &parent_thread,
            &parent_session,
            &parent,
            &parent_ruleset,
            &parent_binding,
            &permission_service,
            &store,
            &provider_factory,
            &usage,
            &context_frame_loader,
            0,
        );

        let result = dispatcher
            .dispatch(context, invocation("explore"), TaskEntryKind::LlmInitiated)
            .await;
        assert!(
            matches!(result, Err(TaskFailure::FeatureDisabled)),
            "expected FeatureDisabled when multi_agent.enabled = false, got {:?}",
            result.as_ref().err()
        );

        // P3.8 byte-level flag-off regression contract: a refused
        // dispatch must NOT mutate the parent session JSONL. If the
        // events.jsonl file does not exist that is the strongest
        // possible "no side effects" signal — the dispatcher rejected
        // on the feature flag before any append could even create the
        // file. If the file exists from a prior write in the same
        // test harness, then no new bytes may be appended after the
        // rejected dispatch.
        let events_path = store.events_path();
        let bytes_after = std::fs::read(&events_path).unwrap_or_default();
        assert!(
            bytes_after.is_empty(),
            "flag-off dispatch must NOT mutate parent session JSONL; \
             found {} bytes at '{}': {:?}",
            bytes_after.len(),
            events_path.display(),
            String::from_utf8_lossy(&bytes_after),
        );
    }

    /// Scenario: depth gate fires when `ctx.depth + 1 > limit`. The
    /// default config sets depth=1 so a depth-1 ctx must be rejected.
    #[tokio::test]
    async fn dispatch_rejects_when_depth_exceeded() {
        let config = MultiAgentConfig {
            enabled: true,
            max_subagent_depth: 1,
            max_concurrent_subagents: 4,
        };
        let (dispatcher, registry, usage, store) = dispatcher_test_harness(config).await;
        registry.insert(explore_subagent());

        let parent = parent_spec();
        let parent_ruleset: PermissionRuleset = Vec::new();
        let parent_binding = parent_binding();
        let (permission_service, _asker) = allow_once_service();
        let provider_factory = ProviderFactory;
        let context_frame_loader = ContextFrameLoader::default();
        let parent_thread = "thread-1".to_string();
        let parent_session: SessionId = "session-1".to_string();

        let context = ctx(
            &parent_thread,
            &parent_session,
            &parent,
            &parent_ruleset,
            &parent_binding,
            &permission_service,
            &store,
            &provider_factory,
            &usage,
            &context_frame_loader,
            1, // depth + 1 = 2 > limit 1
        );

        let result = dispatcher
            .dispatch(context, invocation("explore"), TaskEntryKind::LlmInitiated)
            .await;
        assert!(matches!(
            result,
            Err(TaskFailure::DepthExceeded {
                current: 1,
                limit: 1
            })
        ));
    }

    /// Scenario: concurrency gate fires when the in-flight counter is
    /// already at the limit. We seed the counter directly to emulate a
    /// parallel dispatch.
    #[tokio::test]
    async fn dispatch_rejects_when_concurrency_exceeded() {
        let config = MultiAgentConfig {
            enabled: true,
            max_subagent_depth: 4,
            max_concurrent_subagents: 1,
        };
        let (dispatcher, registry, usage, store) = dispatcher_test_harness(config).await;
        registry.insert(explore_subagent());
        // Pre-occupy the only slot.
        dispatcher.in_flight.fetch_add(1, Ordering::AcqRel);

        let parent = parent_spec();
        let parent_ruleset: PermissionRuleset = Vec::new();
        let parent_binding = parent_binding();
        let (permission_service, _asker) = allow_once_service();
        let provider_factory = ProviderFactory;
        let context_frame_loader = ContextFrameLoader::default();
        let parent_thread = "thread-1".to_string();
        let parent_session: SessionId = "session-1".to_string();

        let context = ctx(
            &parent_thread,
            &parent_session,
            &parent,
            &parent_ruleset,
            &parent_binding,
            &permission_service,
            &store,
            &provider_factory,
            &usage,
            &context_frame_loader,
            0,
        );

        let result = dispatcher
            .dispatch(context, invocation("explore"), TaskEntryKind::LlmInitiated)
            .await;
        assert!(matches!(
            result,
            Err(TaskFailure::ConcurrencyExceeded {
                current: 1,
                limit: 1
            })
        ));
    }

    /// Scenario: an unknown subagent_type errors with suggestions
    /// drawn from the subagent-eligible registry entries. A
    /// `Primary`-only profile in the registry must NOT appear in
    /// suggestions — the doc explicitly forbids dispatching primary
    /// agents through `task`.
    #[tokio::test]
    async fn dispatch_rejects_unknown_subagent_with_eligible_suggestions_only() {
        let config = MultiAgentConfig {
            enabled: true,
            max_subagent_depth: 4,
            max_concurrent_subagents: 4,
        };
        let (dispatcher, registry, usage, store) = dispatcher_test_harness(config).await;
        registry.insert(explore_subagent()); // mode = Subagent
        registry.insert(primary_only_agent()); // mode = Primary

        let parent = parent_spec();
        let parent_ruleset: PermissionRuleset = Vec::new();
        let parent_binding = parent_binding();
        let (permission_service, _asker) = allow_once_service();
        let provider_factory = ProviderFactory;
        let context_frame_loader = ContextFrameLoader::default();
        let parent_thread = "thread-1".to_string();
        let parent_session: SessionId = "session-1".to_string();

        let context = ctx(
            &parent_thread,
            &parent_session,
            &parent,
            &parent_ruleset,
            &parent_binding,
            &permission_service,
            &store,
            &provider_factory,
            &usage,
            &context_frame_loader,
            0,
        );

        let result = dispatcher
            .dispatch(
                context,
                invocation("does-not-exist"),
                TaskEntryKind::LlmInitiated,
            )
            .await;
        match result {
            Err(TaskFailure::UnknownSubagent { name, suggestions }) => {
                assert_eq!(name, "does-not-exist");
                assert!(suggestions.contains(&"explore".to_string()));
                assert!(
                    !suggestions.contains(&"planner".to_string()),
                    "primary-only agents must NOT appear in subagent suggestions"
                );
            }
            other => panic!("expected UnknownSubagent, got {other:?}"),
        }
    }

    /// Scenario: a sub-spec that opts into `edit` while the parent
    /// denies `edit: *` is refused by the escalation gate. The
    /// returned `(permission, pattern)` pair must name `edit`.
    #[tokio::test]
    async fn dispatch_rejects_permission_escalation() {
        let config = MultiAgentConfig {
            enabled: true,
            max_subagent_depth: 4,
            max_concurrent_subagents: 4,
        };
        let (dispatcher, registry, usage, store) = dispatcher_test_harness(config).await;

        // Sub-spec opts into edit.
        let mut sub = explore_subagent();
        sub.name = "edit-explorer".to_string();
        let mut allowed = BTreeSet::new();
        allowed.insert("edit".to_string());
        sub.permission = AgentPermissionSpec {
            allowed_tools: allowed,
            ..AgentPermissionSpec::default()
        };
        registry.insert(sub);

        // Parent denies edit globally.
        let parent_ruleset: PermissionRuleset =
            vec![PermissionRule::new("edit", "*", PermissionAction::Deny)];
        let parent = parent_spec();
        let parent_binding = parent_binding();
        let (permission_service, _asker) = allow_once_service();
        let provider_factory = ProviderFactory;
        let context_frame_loader = ContextFrameLoader::default();
        let parent_thread = "thread-1".to_string();
        let parent_session: SessionId = "session-1".to_string();

        let context = ctx(
            &parent_thread,
            &parent_session,
            &parent,
            &parent_ruleset,
            &parent_binding,
            &permission_service,
            &store,
            &provider_factory,
            &usage,
            &context_frame_loader,
            0,
        );

        let result = dispatcher
            .dispatch(
                context,
                invocation("edit-explorer"),
                TaskEntryKind::LlmInitiated,
            )
            .await;
        match result {
            Err(TaskFailure::PermissionEscalationDenied {
                permission,
                pattern: _,
            }) => {
                assert_eq!(permission, "edit");
            }
            other => panic!("expected PermissionEscalationDenied, got {other:?}"),
        }
    }

    /// Scenario (Step 5): Tool-boundary hardening can reject
    /// `SubAgentSpawn` before permission ask. An observer principal
    /// does not allow mutating operations, and spawning a sub-agent is
    /// modeled as a mutating spawn operation, so this dispatch must
    /// fail fast with `SafetyDenied`.
    #[tokio::test]
    async fn dispatch_rejects_when_safety_decision_denies_spawn() {
        let config = MultiAgentConfig {
            enabled: true,
            max_subagent_depth: 4,
            max_concurrent_subagents: 4,
        };
        let (dispatcher, registry, usage, store) = dispatcher_test_harness(config).await;
        registry.insert(explore_subagent());

        let parent = parent_spec();
        let parent_ruleset: PermissionRuleset = Vec::new();
        let parent_binding = parent_binding();
        let asker = Arc::new(TestAsker::always(PermissionReply::Once));
        let permission_service = PermissionService::new(asker.clone());
        let provider_factory = ProviderFactory;
        let context_frame_loader = ContextFrameLoader::default();
        let parent_thread = "thread-safety-denied".to_string();
        let parent_session: SessionId = "session-safety-denied".to_string();
        let tool_registry = observer_tool_registry();

        let context = DispatchContext {
            parent_thread_id: &parent_thread,
            parent_session_id: &parent_session,
            parent_agent: &parent,
            parent_ruleset: &parent_ruleset,
            parent_model_binding: &parent_binding,
            parent_message_id: MessageId::from("msg-safety-denied"),
            permission_service: &permission_service,
            session_store: &store,
            provider_factory: &provider_factory,
            provider_build_options: default_provider_build_options(),
            provider_build_options_resolver: None,
            tool_registry: &tool_registry,
            runtime_context: None,
            usage_recorder: &usage,
            context_frame_loader: &context_frame_loader,
            abort_token: AbortToken::new(),
            depth: 0,
            compaction_model: None,
            hook_runner: None,
        };

        let result = dispatcher
            .dispatch(context, invocation("explore"), TaskEntryKind::LlmInitiated)
            .await;
        match result {
            Err(TaskFailure::SafetyDenied(SafetyDecisionDenial { reason })) => {
                assert!(
                    reason.contains("observer principals cannot run mutating tools"),
                    "unexpected safety reason: {reason}",
                );
            }
            other => panic!("expected SafetyDenied, got {other:?}"),
        }
        assert_eq!(
            asker.ask_call_count(),
            0,
            "safety deny must happen before permission ask"
        );
    }

    /// Scenario (TOCTOU regression guard): with the only slot already
    /// held, two concurrent dispatches must BOTH receive
    /// `ConcurrencyExceeded` and the counter must remain at the
    /// pre-test value (the rejected `fetch_add` calls rolled back).
    /// A naive load-then-add gate would let both pass step 3 and end
    /// up with `in_flight == 3`; the atomic `fetch_add` + rollback
    /// pattern keeps the invariant tight under contention.
    #[tokio::test]
    async fn dispatch_concurrent_calls_against_held_slot_both_reject() {
        let config = MultiAgentConfig {
            enabled: true,
            max_subagent_depth: 4,
            max_concurrent_subagents: 1,
        };
        let (dispatcher, registry, usage, store) = dispatcher_test_harness(config).await;
        registry.insert(explore_subagent());
        // Hold the only slot for the entire test — both concurrent
        // dispatches will see `prev = 1, limit = 1` and roll back.
        // (Acquiring the slot via fetch_add reproduces what a real
        // in-flight dispatch would do.)
        dispatcher.in_flight.fetch_add(1, Ordering::AcqRel);

        let parent = parent_spec();
        let parent_ruleset: PermissionRuleset = Vec::new();
        let parent_binding = parent_binding();
        let (permission_service, _asker) = allow_once_service();
        let provider_factory = ProviderFactory;
        let context_frame_loader = ContextFrameLoader::default();
        let parent_thread = "thread-1".to_string();
        let parent_session: SessionId = "session-1".to_string();
        let context_a = ctx(
            &parent_thread,
            &parent_session,
            &parent,
            &parent_ruleset,
            &parent_binding,
            &permission_service,
            &store,
            &provider_factory,
            &usage,
            &context_frame_loader,
            0,
        );
        let context_b = ctx(
            &parent_thread,
            &parent_session,
            &parent,
            &parent_ruleset,
            &parent_binding,
            &permission_service,
            &store,
            &provider_factory,
            &usage,
            &context_frame_loader,
            0,
        );

        let (result_a, result_b) = tokio::join!(
            dispatcher.dispatch(
                context_a,
                invocation("explore"),
                TaskEntryKind::LlmInitiated,
            ),
            dispatcher.dispatch(
                context_b,
                invocation("explore"),
                TaskEntryKind::LlmInitiated,
            ),
        );
        // Both calls observed the held slot → both must reject.
        assert!(matches!(
            result_a,
            Err(TaskFailure::ConcurrencyExceeded {
                current: 1,
                limit: 1
            })
        ));
        assert!(matches!(
            result_b,
            Err(TaskFailure::ConcurrencyExceeded {
                current: 1,
                limit: 1
            })
        ));
        // Counter still at the held value (1); rejected calls rolled
        // their fetch_add back.
        assert_eq!(dispatcher.in_flight(), 1);
    }

    /// Scenario (OC-Phase 3 P3.4 step 8 — Reject path): a
    /// `LlmInitiated` dispatch whose permission ask returns `Reject`
    /// surfaces `TaskFailure::ApprovalRejected`, with the user's
    /// optional feedback forwarded so the caller can show it to the
    /// model. The concurrency counter releases via the RAII guard.
    #[tokio::test]
    async fn dispatch_returns_approval_rejected_when_asker_rejects() {
        let config = MultiAgentConfig {
            enabled: true,
            max_subagent_depth: 4,
            max_concurrent_subagents: 4,
        };
        let (dispatcher, registry, usage, store) = dispatcher_test_harness(config).await;
        registry.insert(explore_subagent());

        let asker = Arc::new(TestAsker::always(PermissionReply::Reject {
            feedback: Some("budget concerns".to_string()),
        }));
        let permission_service = PermissionService::new(asker.clone());

        let parent = parent_spec();
        let parent_ruleset: PermissionRuleset = Vec::new();
        let parent_binding = parent_binding();
        let provider_factory = ProviderFactory;
        let context_frame_loader = ContextFrameLoader::default();
        let parent_thread = "thread-1".to_string();
        let parent_session: SessionId = "session-1".to_string();

        let context = ctx(
            &parent_thread,
            &parent_session,
            &parent,
            &parent_ruleset,
            &parent_binding,
            &permission_service,
            &store,
            &provider_factory,
            &usage,
            &context_frame_loader,
            0,
        );

        let result = dispatcher
            .dispatch(context, invocation("explore"), TaskEntryKind::LlmInitiated)
            .await;
        match result {
            Err(TaskFailure::ApprovalRejected { feedback }) => {
                assert_eq!(feedback.as_deref(), Some("budget concerns"));
            }
            other => panic!("expected ApprovalRejected, got {other:?}"),
        }
        assert_eq!(asker.ask_call_count(), 1);
        // RAII guard must have released the slot.
        assert_eq!(dispatcher.in_flight(), 0);
    }

    /// Scenario (P3.4 step 8 — Once allow path): an asker that replies
    /// `Once` lets the dispatch through to the placeholder tail. The
    /// asker is invoked exactly once, regardless of `Once` vs
    /// `Always` (the asker, not the dispatcher, persists `Always`
    /// rules).
    #[tokio::test]
    async fn dispatch_proceeds_when_asker_replies_once() {
        let config = MultiAgentConfig {
            enabled: true,
            max_subagent_depth: 4,
            max_concurrent_subagents: 4,
        };
        let (dispatcher, registry, usage, store) = dispatcher_test_harness(config).await;
        registry.insert(explore_subagent());

        let (permission_service, asker) = allow_once_service();
        let parent = parent_spec();
        let parent_ruleset: PermissionRuleset = Vec::new();
        let parent_binding = parent_binding();
        let provider_factory = ProviderFactory;
        let context_frame_loader = ContextFrameLoader::default();
        let parent_thread = "thread-1".to_string();
        let parent_session: SessionId = "session-1".to_string();

        let context = ctx(
            &parent_thread,
            &parent_session,
            &parent,
            &parent_ruleset,
            &parent_binding,
            &permission_service,
            &store,
            &provider_factory,
            &usage,
            &context_frame_loader,
            0,
        );

        let result = dispatcher
            .dispatch(context, invocation("explore"), TaskEntryKind::LlmInitiated)
            .await
            .expect("Once should let the dispatch through");
        assert_eq!(result.agent_name, "explore");
        assert_eq!(asker.ask_call_count(), 1, "ask must fire on LlmInitiated");
    }

    /// Scenario (P3.4 step 8 — UserInitiated bypass path): a
    /// `UserInitiated { bypass_permission_ask: true }` dispatch
    /// MUST NOT call the asker. The user already chose the dispatch
    /// (slash command, Code Control RPC, SubtaskPart payload), so the
    /// dialog would be redundant. Even an asker that always rejects
    /// would not fire.
    #[tokio::test]
    async fn dispatch_user_initiated_bypass_skips_permission_ask() {
        let config = MultiAgentConfig {
            enabled: true,
            max_subagent_depth: 4,
            max_concurrent_subagents: 4,
        };
        let (dispatcher, registry, usage, store) = dispatcher_test_harness(config).await;
        registry.insert(explore_subagent());

        let asker = Arc::new(TestAsker::always(PermissionReply::Reject {
            feedback: Some("would have rejected".to_string()),
        }));
        let permission_service = PermissionService::new(asker.clone());

        let parent = parent_spec();
        let parent_ruleset: PermissionRuleset = Vec::new();
        let parent_binding = parent_binding();
        let provider_factory = ProviderFactory;
        let context_frame_loader = ContextFrameLoader::default();
        let parent_thread = "thread-1".to_string();
        let parent_session: SessionId = "session-1".to_string();

        let context = ctx(
            &parent_thread,
            &parent_session,
            &parent,
            &parent_ruleset,
            &parent_binding,
            &permission_service,
            &store,
            &provider_factory,
            &usage,
            &context_frame_loader,
            0,
        );

        let result = dispatcher
            .dispatch(
                context,
                invocation("explore"),
                TaskEntryKind::UserInitiated {
                    bypass_permission_ask: true,
                },
            )
            .await
            .expect("UserInitiated bypass must not fail at step 8");
        assert_eq!(result.agent_name, "explore");
        assert_eq!(
            asker.ask_call_count(),
            0,
            "UserInitiated bypass must NOT call the asker"
        );
    }

    /// Scenario: every gate passes → the placeholder TaskResult flows
    /// through with the resolved provider/model bound to the agent's
    /// spec. The concurrency counter returns to 0 after the call.
    #[tokio::test]
    async fn dispatch_returns_placeholder_result_when_every_gate_passes() {
        let config = MultiAgentConfig {
            enabled: true,
            max_subagent_depth: 4,
            max_concurrent_subagents: 4,
        };
        let (dispatcher, registry, usage, store) = dispatcher_test_harness(config).await;

        let mut sub = explore_subagent();
        sub.model = ModelBinding::parse("anthropic/claude-3-5-haiku-latest");
        registry.insert(sub);

        let parent = parent_spec();
        let parent_ruleset: PermissionRuleset = Vec::new();
        let parent_binding = parent_binding();
        let (permission_service, _asker) = allow_once_service();
        let provider_factory = ProviderFactory;
        let context_frame_loader = ContextFrameLoader::default();
        let parent_thread = "thread-1".to_string();
        let parent_session: SessionId = "session-1".to_string();

        let context = ctx(
            &parent_thread,
            &parent_session,
            &parent,
            &parent_ruleset,
            &parent_binding,
            &permission_service,
            &store,
            &provider_factory,
            &usage,
            &context_frame_loader,
            0,
        );

        let result = dispatcher
            .dispatch(context, invocation("explore"), TaskEntryKind::LlmInitiated)
            .await
            .expect("every gate should pass");

        assert_eq!(result.agent_name, "explore");
        assert_eq!(result.provider_id, "anthropic");
        assert_eq!(result.model_id, "claude-3-5-haiku-latest");
        // Placeholder tail still leaves these empty/zero — steps
        // 9–13 (handoff + model build + child loop) fill them in
        // subsequent OC-Phase 3 sub-PRs.
        assert_eq!(result.final_text, "");
        assert_eq!(result.steps_used, 0);

        // Concurrency counter must return to 0 after the call.
        assert_eq!(dispatcher.in_flight(), 0);
    }

    /// P3.7 cancel propagation: a dispatch whose context carries an
    /// already-cancelled `abort_token` short-circuits with
    /// `TaskFailure::Cancelled { source: ParentAbort }` BEFORE any
    /// gate runs. Neither the concurrency slot nor the session JSONL
    /// must be touched, otherwise a `Ctrl-C` between the parent
    /// awaiting the asker and the dispatcher returning would leak a
    /// half-committed dispatch.
    #[tokio::test]
    async fn dispatch_short_circuits_when_parent_abort_already_fired() {
        let config = MultiAgentConfig {
            enabled: true,
            max_subagent_depth: 4,
            max_concurrent_subagents: 4,
        };
        let (dispatcher, registry, usage, store) = dispatcher_test_harness(config).await;
        registry.insert(explore_subagent());

        let parent = parent_spec();
        let parent_ruleset: PermissionRuleset = Vec::new();
        let parent_binding = parent_binding();
        let asker = Arc::new(TestAsker::always(PermissionReply::Once));
        let permission_service = PermissionService::new(asker.clone());
        let provider_factory = ProviderFactory;
        let context_frame_loader = ContextFrameLoader::default();
        let parent_thread = "thread-cancelled".to_string();
        let parent_session: SessionId = "session-cancelled".to_string();

        // Build a context whose abort token is already cancelled.
        let pre_cancelled = AbortToken::new();
        pre_cancelled.cancel();
        let context = DispatchContext {
            parent_thread_id: &parent_thread,
            parent_session_id: &parent_session,
            parent_agent: &parent,
            parent_ruleset: &parent_ruleset,
            parent_model_binding: &parent_binding,
            parent_message_id: MessageId::from("msg-cancelled"),
            permission_service: &permission_service,
            session_store: &store,
            provider_factory: &provider_factory,
            provider_build_options: default_provider_build_options(),
            provider_build_options_resolver: None,
            tool_registry: default_tool_registry(),
            runtime_context: None,
            usage_recorder: &usage,
            context_frame_loader: &context_frame_loader,
            abort_token: pre_cancelled,
            depth: 0,
            compaction_model: None,
            hook_runner: None,
        };

        let result = dispatcher
            .dispatch(context, invocation("explore"), TaskEntryKind::LlmInitiated)
            .await;
        assert!(
            matches!(
                result,
                Err(TaskFailure::Cancelled {
                    source: CancellationSource::ParentAbort,
                }),
            ),
            "expected Cancelled{{ParentAbort}} when abort already fired, got {:?}",
            result.as_ref().err()
        );

        assert_eq!(
            asker.ask_call_count(),
            0,
            "pre-cancelled dispatch must NOT call the asker"
        );
        assert_eq!(
            dispatcher.in_flight(),
            0,
            "pre-cancelled dispatch must NOT claim a concurrency slot"
        );

        let events_path = store.events_path();
        let bytes = std::fs::read(&events_path).unwrap_or_default();
        assert!(
            bytes.is_empty(),
            "pre-cancelled dispatch must NOT write any Spawned/Completed bytes; \
             found {} bytes at '{}': {:?}",
            bytes.len(),
            events_path.display(),
            String::from_utf8_lossy(&bytes),
        );
    }

    /// P3.5 wire-up: a successful dispatch writes `Spawned` followed
    /// immediately by `Completed` into the parent session JSONL. Both
    /// events share the same `agent_run_id` and carry the spec-resolved
    /// `provider_id` / `model_id` so replay tooling can correlate the
    /// pair without re-resolving the registry.
    #[tokio::test]
    async fn dispatch_writes_spawned_then_completed_events_to_parent_session() {
        let config = MultiAgentConfig {
            enabled: true,
            max_subagent_depth: 4,
            max_concurrent_subagents: 4,
        };
        let (dispatcher, registry, usage, store) = dispatcher_test_harness(config).await;

        let mut sub = explore_subagent();
        sub.model = ModelBinding::parse("anthropic/claude-3-5-haiku-latest");
        registry.insert(sub);

        let parent = parent_spec();
        let parent_ruleset: PermissionRuleset = Vec::new();
        let parent_binding = parent_binding();
        let (permission_service, _asker) = allow_once_service();
        let provider_factory = ProviderFactory;
        let context_frame_loader = ContextFrameLoader::default();
        let parent_thread = "thread-events".to_string();
        let parent_session: SessionId = "session-events".to_string();

        let context = ctx(
            &parent_thread,
            &parent_session,
            &parent,
            &parent_ruleset,
            &parent_binding,
            &permission_service,
            &store,
            &provider_factory,
            &usage,
            &context_frame_loader,
            0,
        );

        dispatcher
            .dispatch(context, invocation("explore"), TaskEntryKind::LlmInitiated)
            .await
            .expect("every gate should pass");

        let events: Vec<_> = store
            .load_events()
            .expect("session JSONL must be readable after dispatch")
            .into_iter()
            .filter_map(|envelope| match envelope {
                crate::internal::ai::session::jsonl::SessionEvent::AgentRun(known) => {
                    known.known().cloned()
                }
                _ => None,
            })
            .collect();

        assert_eq!(
            events.len(),
            2,
            "dispatch should emit exactly Spawned + Completed"
        );

        let (spawned_id, recorded_provider, recorded_model, recorded_depth, recorded_digest) =
            match &events[0] {
                AgentRunEvent::Spawned {
                    agent_run_id,
                    parent_thread_id,
                    parent_session_id,
                    subagent_name,
                    provider_id,
                    model_id,
                    depth,
                    prompt_digest,
                    ..
                } => {
                    assert_eq!(parent_thread_id, &parent_thread);
                    assert_eq!(parent_session_id, &parent_session);
                    assert_eq!(subagent_name, "explore");
                    (
                        *agent_run_id,
                        provider_id.clone(),
                        model_id.clone(),
                        *depth,
                        prompt_digest.clone(),
                    )
                }
                other => panic!("first event must be Spawned, got {other:?}"),
            };
        assert_eq!(recorded_provider, "anthropic");
        assert_eq!(recorded_model, "claude-3-5-haiku-latest");
        assert_eq!(
            recorded_depth, 1,
            "Spawned.depth should be parent depth + 1 (parent was 0)"
        );
        assert_eq!(
            recorded_digest, "do a thing",
            "prompt digest must equal the invocation's first-line preview"
        );

        match &events[1] {
            AgentRunEvent::Completed { agent_run_id } => {
                assert_eq!(
                    agent_run_id, &spawned_id,
                    "Completed must reuse the agent_run_id minted for Spawned"
                );
            }
            other => panic!("second event must be Completed, got {other:?}"),
        }
    }

    /// OC-Phase 3 P3.4 seam: when a `SubAgentChildRunner` is attached
    /// via `with_child_runner`, the dispatcher delegates the result
    /// to it instead of synthesising the legacy placeholder. The
    /// `Spawned` event still fires up front, and the runner's
    /// outcome (Ok or Err) flips the terminal event between
    /// `Completed` and `Failed`.
    #[tokio::test]
    async fn dispatch_delegates_to_child_runner_when_attached_and_writes_completed() {
        let config = MultiAgentConfig {
            enabled: true,
            max_subagent_depth: 4,
            max_concurrent_subagents: 4,
        };

        // A deterministic runner that returns a recognisable
        // TaskResult so the test can assert it propagated through.
        struct ConstantRunner;
        impl crate::internal::ai::agent::runtime::SubAgentChildRunner for ConstantRunner {
            fn run<'a>(
                &'a self,
                request: crate::internal::ai::agent::runtime::SubAgentChildRunRequest<'a>,
            ) -> futures::future::BoxFuture<'a, Result<TaskResult, TaskFailure>> {
                let task_id = request.task_id.clone();
                let agent_name = request.sub_spec.name.clone();
                Box::pin(async move {
                    Ok(TaskResult {
                        task_id,
                        agent_name,
                        provider_id: "runner-provider".to_string(),
                        model_id: "runner-model".to_string(),
                        final_text: "runner produced this".to_string(),
                        steps_used: 7,
                        usage: CompletionUsageSummary::default(),
                    })
                })
            }
        }

        let (dispatcher, registry, usage, store) = dispatcher_test_harness(config).await;
        let dispatcher = dispatcher.with_child_runner(Arc::new(ConstantRunner));

        let mut sub = explore_subagent();
        sub.model = ModelBinding::parse("anthropic/claude-3-5-haiku-latest");
        registry.insert(sub);

        let parent = parent_spec();
        let parent_ruleset: PermissionRuleset = Vec::new();
        let parent_binding = parent_binding();
        let (permission_service, _asker) = allow_once_service();
        let provider_factory = ProviderFactory;
        let context_frame_loader = ContextFrameLoader::default();
        let parent_thread = "thread-runner".to_string();
        let parent_session: SessionId = "session-runner".to_string();

        let context = ctx(
            &parent_thread,
            &parent_session,
            &parent,
            &parent_ruleset,
            &parent_binding,
            &permission_service,
            &store,
            &provider_factory,
            &usage,
            &context_frame_loader,
            0,
        );

        let result = dispatcher
            .dispatch(context, invocation("explore"), TaskEntryKind::LlmInitiated)
            .await
            .expect("runner returned Ok");
        assert_eq!(result.final_text, "runner produced this");
        assert_eq!(result.steps_used, 7);
        assert_eq!(result.provider_id, "runner-provider");

        let events: Vec<_> = store
            .load_events()
            .expect("JSONL readable")
            .into_iter()
            .filter_map(|envelope| match envelope {
                crate::internal::ai::session::jsonl::SessionEvent::AgentRun(known) => {
                    known.known().cloned()
                }
                _ => None,
            })
            .collect();
        assert_eq!(
            events.len(),
            2,
            "runner success must still emit Spawned + Completed"
        );
        assert!(matches!(events[0], AgentRunEvent::Spawned { .. }));
        assert!(matches!(events[1], AgentRunEvent::Completed { .. }));
    }

    /// Symmetric counterpart: a runner that returns
    /// `TaskFailure::Timeout` produces a structurally-typed
    /// `AgentRunEvent::TimedOut` terminal (not Failed). The P3.5
    /// taxonomy distinguishes Failed / Cancelled / TimedOut /
    /// BudgetExceeded at the event level so replay tooling can
    /// branch on the variant tag without scanning Failed.reason
    /// substrings.
    #[tokio::test]
    async fn dispatch_runner_error_emits_failed_event_with_reason() {
        let config = MultiAgentConfig {
            enabled: true,
            max_subagent_depth: 4,
            max_concurrent_subagents: 4,
        };

        struct FailingRunner;
        impl crate::internal::ai::agent::runtime::SubAgentChildRunner for FailingRunner {
            fn run<'a>(
                &'a self,
                _request: crate::internal::ai::agent::runtime::SubAgentChildRunRequest<'a>,
            ) -> futures::future::BoxFuture<'a, Result<TaskResult, TaskFailure>> {
                Box::pin(async {
                    Err(TaskFailure::Timeout {
                        wall_clock_ms: 60_000,
                    })
                })
            }
        }

        let (dispatcher, registry, usage, store) = dispatcher_test_harness(config).await;
        let dispatcher = dispatcher.with_child_runner(Arc::new(FailingRunner));
        registry.insert(explore_subagent());

        let parent = parent_spec();
        let parent_ruleset: PermissionRuleset = Vec::new();
        let parent_binding = parent_binding();
        let (permission_service, _asker) = allow_once_service();
        let provider_factory = ProviderFactory;
        let context_frame_loader = ContextFrameLoader::default();
        let parent_thread = "thread-fail".to_string();
        let parent_session: SessionId = "session-fail".to_string();

        let context = ctx(
            &parent_thread,
            &parent_session,
            &parent,
            &parent_ruleset,
            &parent_binding,
            &permission_service,
            &store,
            &provider_factory,
            &usage,
            &context_frame_loader,
            0,
        );

        let err = dispatcher
            .dispatch(context, invocation("explore"), TaskEntryKind::LlmInitiated)
            .await
            .expect_err("runner returned Err must surface from dispatch");
        assert!(matches!(err, TaskFailure::Timeout { .. }));

        let events: Vec<_> = store
            .load_events()
            .expect("JSONL readable")
            .into_iter()
            .filter_map(|envelope| match envelope {
                crate::internal::ai::session::jsonl::SessionEvent::AgentRun(known) => {
                    known.known().cloned()
                }
                _ => None,
            })
            .collect();
        assert_eq!(
            events.len(),
            2,
            "runner failure must still emit Spawned + TimedOut"
        );
        assert!(matches!(events[0], AgentRunEvent::Spawned { .. }));
        assert!(
            matches!(events[1], AgentRunEvent::TimedOut { .. }),
            "TaskFailure::Timeout must map to AgentRunEvent::TimedOut, got: {:?}",
            events[1],
        );
    }

    /// CEX-S2-12 criterion (3) — generic-failure fixture: a child run
    /// that returns a non-typed `TaskFailure` (here
    /// `ChildToolLoopFailed`) is recorded end-to-end as
    /// `AgentRunEvent::Failed` (the catch-all terminal), distinct from
    /// the structurally-typed Timeout / Cancelled / BudgetExceeded
    /// fixtures.
    #[tokio::test]
    async fn dispatch_runner_generic_failure_emits_failed_event() {
        use crate::internal::ai::agent::runtime::ToolLoopError;

        let config = MultiAgentConfig {
            enabled: true,
            max_subagent_depth: 4,
            max_concurrent_subagents: 4,
        };

        struct GenericFailingRunner;
        impl crate::internal::ai::agent::runtime::SubAgentChildRunner for GenericFailingRunner {
            fn run<'a>(
                &'a self,
                _request: crate::internal::ai::agent::runtime::SubAgentChildRunRequest<'a>,
            ) -> futures::future::BoxFuture<'a, Result<TaskResult, TaskFailure>> {
                Box::pin(async {
                    Err(TaskFailure::ChildToolLoopFailed(
                        ToolLoopError::StepBudgetExhausted { steps: 48 },
                    ))
                })
            }
        }

        let (dispatcher, registry, usage, store) = dispatcher_test_harness(config).await;
        let dispatcher = dispatcher.with_child_runner(Arc::new(GenericFailingRunner));
        registry.insert(explore_subagent());

        let parent = parent_spec();
        let parent_ruleset: PermissionRuleset = Vec::new();
        let parent_binding = parent_binding();
        let (permission_service, _asker) = allow_once_service();
        let provider_factory = ProviderFactory;
        let context_frame_loader = ContextFrameLoader::default();
        let parent_thread = "thread-generic-fail".to_string();
        let parent_session: SessionId = "session-generic-fail".to_string();

        let context = ctx(
            &parent_thread,
            &parent_session,
            &parent,
            &parent_ruleset,
            &parent_binding,
            &permission_service,
            &store,
            &provider_factory,
            &usage,
            &context_frame_loader,
            0,
        );

        let err = dispatcher
            .dispatch(context, invocation("explore"), TaskEntryKind::LlmInitiated)
            .await
            .expect_err("runner Err must surface from dispatch");
        assert!(matches!(err, TaskFailure::ChildToolLoopFailed(_)));

        let events = agent_run_events(&store);
        assert_eq!(
            events.len(),
            2,
            "a generic runner failure must still emit Spawned + Failed"
        );
        assert!(matches!(events[0], AgentRunEvent::Spawned { .. }));
        assert!(
            matches!(events[1], AgentRunEvent::Failed { .. }),
            "a non-typed TaskFailure must map to AgentRunEvent::Failed, got: {:?}",
            events[1],
        );
    }

    /// CEX-S2-12 criterion (3) — budget fixture: a child run that
    /// returns `TaskFailure::BudgetExceeded` is recorded end-to-end as
    /// `AgentRunEvent::BudgetExceeded` carrying the mapped dimension.
    #[tokio::test]
    async fn dispatch_runner_budget_exceeded_emits_budget_exceeded_event() {
        use crate::internal::ai::{
            agent::runtime::BudgetExceededReason, agent_run::BudgetDimension,
        };

        let config = MultiAgentConfig {
            enabled: true,
            max_subagent_depth: 4,
            max_concurrent_subagents: 4,
        };

        struct BudgetFailingRunner;
        impl crate::internal::ai::agent::runtime::SubAgentChildRunner for BudgetFailingRunner {
            fn run<'a>(
                &'a self,
                _request: crate::internal::ai::agent::runtime::SubAgentChildRunRequest<'a>,
            ) -> futures::future::BoxFuture<'a, Result<TaskResult, TaskFailure>> {
                Box::pin(async {
                    Err(TaskFailure::BudgetExceeded(
                        BudgetExceededReason::CostHardCap,
                    ))
                })
            }
        }

        let (dispatcher, registry, usage, store) = dispatcher_test_harness(config).await;
        let dispatcher = dispatcher.with_child_runner(Arc::new(BudgetFailingRunner));
        registry.insert(explore_subagent());

        let parent = parent_spec();
        let parent_ruleset: PermissionRuleset = Vec::new();
        let parent_binding = parent_binding();
        let (permission_service, _asker) = allow_once_service();
        let provider_factory = ProviderFactory;
        let context_frame_loader = ContextFrameLoader::default();
        let parent_thread = "thread-budget".to_string();
        let parent_session: SessionId = "session-budget".to_string();

        let context = ctx(
            &parent_thread,
            &parent_session,
            &parent,
            &parent_ruleset,
            &parent_binding,
            &permission_service,
            &store,
            &provider_factory,
            &usage,
            &context_frame_loader,
            0,
        );

        let err = dispatcher
            .dispatch(context, invocation("explore"), TaskEntryKind::LlmInitiated)
            .await
            .expect_err("runner Err must surface from dispatch");
        assert!(matches!(
            err,
            TaskFailure::BudgetExceeded(BudgetExceededReason::CostHardCap)
        ));

        let events = agent_run_events(&store);
        assert_eq!(
            events.len(),
            2,
            "a budget failure must still emit Spawned + BudgetExceeded"
        );
        assert!(matches!(events[0], AgentRunEvent::Spawned { .. }));
        assert!(
            matches!(
                events[1],
                AgentRunEvent::BudgetExceeded {
                    dimension: BudgetDimension::Cost,
                    ..
                }
            ),
            "TaskFailure::BudgetExceeded(CostHardCap) must map to AgentRunEvent::BudgetExceeded{{Cost}}, got: {:?}",
            events[1],
        );
    }

    /// `with_default_child_runner` attaches the production runner
    /// without forcing call sites to import the runner type. The
    /// dispatcher's behaviour after attachment is identical to
    /// `with_child_runner(Arc::new(DefaultSubAgentChildRunner))`;
    /// this test pins the equivalence so the convenience wrapper
    /// cannot silently drift from the explicit form.
    #[tokio::test]
    async fn with_default_child_runner_attaches_the_production_runner() {
        let config = MultiAgentConfig {
            enabled: true,
            max_subagent_depth: 4,
            max_concurrent_subagents: 4,
        };
        let (dispatcher, _registry, _usage, _store) = dispatcher_test_harness(config).await;
        let with_explicit = dispatcher.with_child_runner(Arc::new(
            crate::internal::ai::agent::runtime::DefaultSubAgentChildRunner::new(),
        ));
        // The explicit + convenience wrappers attach equivalent
        // runners. The dispatcher does not expose its runner field
        // directly for inspection (private), but the inability to
        // distinguish the two at any caller surface IS the
        // contract — a future refactor that drops the convenience
        // wrapper must keep `with_child_runner(Arc::new(...))`
        // working for the public production path.
        drop(with_explicit);

        let (dispatcher2, _registry2, _usage2, _store2) =
            dispatcher_test_harness(MultiAgentConfig {
                enabled: true,
                max_subagent_depth: 4,
                max_concurrent_subagents: 4,
            })
            .await;
        let with_convenience = dispatcher2.with_default_child_runner();
        drop(with_convenience);
    }

    /// P3.7 wire-up: a runner that returns `TaskFailure::Cancelled
    /// { ParentAbort }` produces `AgentRunEvent::Cancelled { reason:
    /// UserRequested }` — the schema variant tag distinguishes
    /// human-driven aborts from `LayerOneTimeout` (timeout-driven)
    /// and the `Other` catch-all (budget-hard-cap, etc.).
    #[tokio::test]
    async fn dispatch_runner_cancel_emits_cancelled_event_with_user_requested_reason() {
        use crate::internal::ai::agent_run::CancellationReason;

        let config = MultiAgentConfig {
            enabled: true,
            max_subagent_depth: 4,
            max_concurrent_subagents: 4,
        };

        struct CancellingRunner;
        impl crate::internal::ai::agent::runtime::SubAgentChildRunner for CancellingRunner {
            fn run<'a>(
                &'a self,
                _request: crate::internal::ai::agent::runtime::SubAgentChildRunRequest<'a>,
            ) -> futures::future::BoxFuture<'a, Result<TaskResult, TaskFailure>> {
                Box::pin(async {
                    Err(TaskFailure::Cancelled {
                        source: CancellationSource::ParentAbort,
                    })
                })
            }
        }

        let (dispatcher, registry, usage, store) = dispatcher_test_harness(config).await;
        let dispatcher = dispatcher.with_child_runner(Arc::new(CancellingRunner));
        registry.insert(explore_subagent());

        let parent = parent_spec();
        let parent_ruleset: PermissionRuleset = Vec::new();
        let parent_binding = parent_binding();
        let (permission_service, _asker) = allow_once_service();
        let provider_factory = ProviderFactory;
        let context_frame_loader = ContextFrameLoader::default();
        let parent_thread = "thread-cancel".to_string();
        let parent_session: SessionId = "session-cancel".to_string();

        let context = ctx(
            &parent_thread,
            &parent_session,
            &parent,
            &parent_ruleset,
            &parent_binding,
            &permission_service,
            &store,
            &provider_factory,
            &usage,
            &context_frame_loader,
            0,
        );

        dispatcher
            .dispatch(context, invocation("explore"), TaskEntryKind::LlmInitiated)
            .await
            .expect_err("runner returned Cancelled");

        let events: Vec<_> = store
            .load_events()
            .expect("JSONL readable")
            .into_iter()
            .filter_map(|envelope| match envelope {
                crate::internal::ai::session::jsonl::SessionEvent::AgentRun(known) => {
                    known.known().cloned()
                }
                _ => None,
            })
            .collect();
        assert_eq!(events.len(), 2);
        match &events[1] {
            AgentRunEvent::Cancelled { reason, .. } => {
                assert!(
                    matches!(reason, CancellationReason::UserRequested),
                    "ParentAbort must map to CancellationReason::UserRequested, got: {reason:?}",
                );
            }
            other => panic!("expected Cancelled terminal event, got {other:?}"),
        }
    }

    /// CEX-S2-12 / S2-INV-03: materializing an isolated workspace must
    /// hand the child a tool registry re-rooted onto the workspace
    /// (NOT the main worktree) and a runtime context whose sandbox
    /// `writable_roots` are rebased to it, and the workspace must mirror
    /// the main worktree's files. This is the deterministic core of the
    /// isolation mechanics (copy backend, since `fuse_disabled_by_default`
    /// is `true` under `#[cfg(test)]`).
    #[tokio::test]
    async fn materialize_isolated_workspace_reroots_registry_and_rebases_sandbox() {
        use crate::internal::ai::{
            sandbox::{SandboxPermissions, SandboxPolicy, ToolRuntimeContext, ToolSandboxContext},
            tools::{ToolInvocation, ToolPayload, handlers::ApplyPatchHandler},
        };

        const TARGET_BEFORE: &str = "line 1\nline 2\nline 3\n";

        let main = tempfile::tempdir().expect("tempdir");
        let main_dir = main.path().to_path_buf();
        std::fs::write(main_dir.join("target.txt"), TARGET_BEFORE).expect("seed file");

        let (_dispatcher, _registry, usage, store) =
            dispatcher_test_harness(MultiAgentConfig::default()).await;
        let parent = parent_spec();
        let parent_ruleset: PermissionRuleset = Vec::new();
        let parent_binding = parent_binding();
        let permission_service =
            PermissionService::new(Arc::new(TestAsker::always(PermissionReply::Once)));
        let provider_factory = ProviderFactory;
        let context_frame_loader = ContextFrameLoader::default();
        let parent_thread = "thread-iso-materialize".to_string();
        let parent_session: SessionId = "session-iso-materialize".to_string();
        // Register the real apply_patch handler so the re-rooted child
        // registry (cloned from this one) can exercise the production
        // write path against the workspace.
        let mut tool_registry = ToolRegistry::with_working_dir(main_dir.clone());
        tool_registry.register("apply_patch", Arc::new(ApplyPatchHandler));
        let runtime_context = Some(ToolRuntimeContext {
            sandbox: Some(ToolSandboxContext {
                policy: SandboxPolicy::WorkspaceWrite {
                    writable_roots: vec![main_dir.clone()],
                    network_access: Default::default(),
                    exclude_tmpdir_env_var: false,
                    exclude_slash_tmp: false,
                },
                permissions: SandboxPermissions::UseDefault,
            }),
            ..ToolRuntimeContext::default()
        });

        let context = DispatchContext {
            parent_thread_id: &parent_thread,
            parent_session_id: &parent_session,
            parent_agent: &parent,
            parent_ruleset: &parent_ruleset,
            parent_model_binding: &parent_binding,
            parent_message_id: MessageId::from("msg-iso-materialize"),
            permission_service: &permission_service,
            session_store: &store,
            provider_factory: &provider_factory,
            provider_build_options: default_provider_build_options(),
            provider_build_options_resolver: None,
            tool_registry: &tool_registry,
            runtime_context,
            usage_recorder: &usage,
            context_frame_loader: &context_frame_loader,
            abort_token: AbortToken::new(),
            depth: 0,
            compaction_model: None,
            hook_runner: None,
        };

        let isolation = crate::internal::ai::agent::runtime::WorkspaceIsolationConfig {
            fuse_state: crate::internal::ai::orchestrator::workspace::FuseProvisionState::default(),
            sessions_root: main_dir.join(".libra").join("sessions"),
            allow_full_copy: true,
        };

        let (registry, rebased_ctx, workspace) =
            materialize_isolated_dispatch_workspace(&context, AgentRunId::new(), &isolation)
                .expect("materialization must succeed on a plain temp worktree");

        let workspace_root = workspace.root().to_path_buf();
        assert_ne!(
            registry.working_dir(),
            main_dir.as_path(),
            "the child registry must NOT be rooted at the main worktree",
        );
        assert_eq!(
            registry.working_dir(),
            workspace_root.as_path(),
            "the child registry must be re-rooted onto the isolated workspace",
        );
        assert!(
            workspace_root.join("target.txt").exists(),
            "the workspace must mirror the main worktree's files",
        );

        let sandbox = rebased_ctx
            .expect("the inherited runtime context must be present")
            .sandbox
            .expect("the inherited sandbox context must be present");
        match sandbox.policy {
            SandboxPolicy::WorkspaceWrite { writable_roots, .. } => assert_eq!(
                writable_roots,
                vec![workspace_root.clone()],
                "sandbox writable_roots must be rebased onto the workspace, denying \
                 absolute-path writes to the main worktree",
            ),
            other => panic!("expected WorkspaceWrite, got {other:?}"),
        }

        // Drive a REAL apply_patch through the re-rooted child registry
        // and confirm BOTH halves of isolation deterministically: the
        // write SUCCEEDS in the workspace, and it does NOT touch the
        // main worktree. (`registry.dispatch` forces the invocation's
        // working_dir to the registry's — the workspace root.)
        let patch = "*** Begin Patch\n*** Update File: target.txt\n@@\n line 1\n-line 2\n+line 2 modified\n line 3\n*** End Patch";
        let invocation = ToolInvocation::new(
            "call-iso-patch",
            "apply_patch",
            ToolPayload::Function {
                arguments: serde_json::json!({ "input": patch }).to_string(),
            },
            main_dir.clone(),
        );
        registry
            .dispatch(invocation)
            .await
            .expect("apply_patch via the re-rooted registry must succeed in the workspace");

        assert_eq!(
            std::fs::read_to_string(workspace_root.join("target.txt"))
                .expect("read workspace target.txt"),
            "line 1\nline 2 modified\nline 3\n",
            "the child's apply_patch must have applied INSIDE the workspace",
        );
        assert_eq!(
            std::fs::read_to_string(main_dir.join("target.txt")).expect("read main target.txt"),
            TARGET_BEFORE,
            "the child's apply_patch must NOT touch the main worktree (S2-INV-03)",
        );

        // Tear the workspace down through the RAII `WorkspaceCleanupGuard`
        // (the same path that fires on an unwind) and assert the
        // workspace directory is actually gone — CEX-S2-11 (5): no
        // leaked workspaces.
        assert!(
            workspace_root.exists(),
            "workspace must exist before cleanup"
        );
        let guard = WorkspaceCleanupGuard {
            workspace: Some(workspace),
        };
        drop(guard);
        assert!(
            !workspace_root.exists(),
            "WorkspaceCleanupGuard::drop must remove the materialized workspace, but {} remains",
            workspace_root.display(),
        );
    }

    /// CEX-S2-12 / S2-INV-03 acceptance test (`flag_on_does_not_touch_main_worktree`):
    /// a dispatched sub-agent that calls the real `ApplyPatchHandler`
    /// must leave the MAIN worktree byte-for-byte unchanged — its write
    /// lands in the materialized isolated workspace instead. The
    /// sibling `child_apply_patch_records_undo_preimage_under_inherited_batch`
    /// integration test proves the same `apply_patch` DOES modify the
    /// file when NOT isolated, so this "unchanged" assertion genuinely
    /// proves the write was redirected, not that it silently failed.
    #[cfg(feature = "test-provider")]
    #[tokio::test]
    async fn flag_on_does_not_touch_main_worktree() {
        use crate::internal::ai::{
            providers::ProviderBuildOptions,
            sandbox::{SandboxPermissions, SandboxPolicy, ToolRuntimeContext, ToolSandboxContext},
            tools::handlers::ApplyPatchHandler,
        };

        const TARGET_BEFORE: &str = "line 1\nline 2\nline 3\n";

        let main = tempfile::tempdir().expect("tempdir");
        let main_dir = main.path().to_path_buf();
        std::fs::write(main_dir.join("target.txt"), TARGET_BEFORE).expect("seed target.txt");

        let conn = Database::connect("sqlite::memory:").await.unwrap();
        let usage = UsageRecorder::new(conn);
        let context_frame_loader = ContextFrameLoader::default();
        let store = crate::internal::ai::session::jsonl::SessionJsonlStore::new(
            main_dir
                .join(".libra")
                .join("sessions")
                .join("session-iso-e2e"),
        );
        let permission_service =
            PermissionService::new(Arc::new(TestAsker::always(PermissionReply::Once)));
        let provider_factory = ProviderFactory;

        let mut fixture_path = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"));
        fixture_path.push("tests/fixtures/sub_agent/apply_patch_then_done.json");
        let provider_options = ProviderBuildOptions {
            fake_fixture_path: Some(fixture_path),
            ..ProviderBuildOptions::default()
        };

        // Real apply_patch handler so the child exercises the production
        // write path; the parent registry is rooted at the main worktree.
        let mut tool_registry = ToolRegistry::with_working_dir(main_dir.clone());
        tool_registry.register("apply_patch", Arc::new(ApplyPatchHandler));

        let patcher = AgentExecutionSpec {
            name: "patcher".to_string(),
            description: "patches target.txt".to_string(),
            mode: AgentMode::Subagent,
            model: ModelBinding::parse("fake/some-model"),
            tools: ToolSelection::Inherit,
            ..AgentExecutionSpec::default()
        };
        let registry = Arc::new(TestRegistry::default());
        registry.insert(patcher);
        let dispatcher = DefaultSubAgentDispatcher::new(
            registry,
            MultiAgentConfig {
                enabled: true,
                max_subagent_depth: 4,
                max_concurrent_subagents: 4,
            },
        )
        .with_default_child_runner()
        .with_workspace_isolation(
            crate::internal::ai::agent::runtime::WorkspaceIsolationConfig {
                fuse_state:
                    crate::internal::ai::orchestrator::workspace::FuseProvisionState::default(),
                sessions_root: main_dir.join(".libra").join("sessions"),
                allow_full_copy: true,
            },
        );

        let parent = parent_spec();
        let parent_ruleset: PermissionRuleset = Vec::new();
        let parent_binding = parent_binding();
        let parent_thread = "thread-iso-e2e".to_string();
        let parent_session: SessionId = "session-iso-e2e".to_string();
        // WorkspaceWrite rooted at the main worktree, to exercise the
        // sandbox rebase onto the workspace.
        let runtime_context = Some(ToolRuntimeContext {
            sandbox: Some(ToolSandboxContext {
                policy: SandboxPolicy::WorkspaceWrite {
                    writable_roots: vec![main_dir.clone()],
                    network_access: Default::default(),
                    exclude_tmpdir_env_var: false,
                    exclude_slash_tmp: false,
                },
                permissions: SandboxPermissions::UseDefault,
            }),
            ..ToolRuntimeContext::default()
        });

        let context = DispatchContext {
            parent_thread_id: &parent_thread,
            parent_session_id: &parent_session,
            parent_agent: &parent,
            parent_ruleset: &parent_ruleset,
            parent_model_binding: &parent_binding,
            parent_message_id: MessageId::from("msg-iso-e2e"),
            permission_service: &permission_service,
            session_store: &store,
            provider_factory: &provider_factory,
            provider_build_options: &provider_options,
            provider_build_options_resolver: None,
            tool_registry: &tool_registry,
            runtime_context,
            usage_recorder: &usage,
            context_frame_loader: &context_frame_loader,
            abort_token: AbortToken::new(),
            depth: 0,
            compaction_model: None,
            hook_runner: None,
        };

        let invocation = TaskInvocation {
            description: "patch target.txt".to_string(),
            prompt: "please apply the patch to target.txt".to_string(),
            subagent_type: "patcher".to_string(),
            task_id: None,
        };

        let result = dispatcher
            .dispatch(
                context,
                invocation,
                TaskEntryKind::UserInitiated {
                    bypass_permission_ask: true,
                },
            )
            .await
            .expect("isolated child run must complete against the fake provider");
        assert_eq!(result.agent_name, "patcher");

        let agent_run_id = store
            .load_events()
            .expect("parent JSONL readable")
            .into_iter()
            .filter_map(|event| match event {
                SessionEvent::AgentRun(envelope) => envelope.known().cloned(),
                _ => None,
            })
            .find_map(|event| match event {
                AgentRunEvent::Spawned { agent_run_id, .. } => Some(agent_run_id),
                _ => None,
            })
            .expect("dispatch must write a Spawned event with the child agent_run_id");
        let agent_run_id_string = agent_run_id.0.to_string();
        let child_events = store
            .child(&agent_run_id_string)
            .load_events()
            .expect("child session JSONL events readable");
        let child_event_kinds: Vec<_> = child_events
            .iter()
            .map(crate::internal::ai::runtime::Event::event_kind)
            .collect();
        assert_eq!(
            child_event_kinds,
            vec![
                "session_snapshot",
                "tool_call",
                "session_snapshot",
                "tool_result",
                "session_snapshot",
                "session_snapshot",
            ],
            "child JSONL should persist dedicated tool_call/tool_result events between snapshots",
        );
        let child_tool_call = child_events
            .iter()
            .find_map(|event| match event {
                SessionEvent::ToolCall(tool_call) => Some(tool_call),
                _ => None,
            })
            .expect("child JSONL should include a dedicated tool_call event");
        assert_eq!(child_tool_call.agent_run_id, agent_run_id);
        assert_eq!(child_tool_call.tool_name, "apply_patch");
        assert_eq!(child_tool_call.call_id, "call_apply_patch_1");
        let child_tool_result = child_events
            .iter()
            .find_map(|event| match event {
                SessionEvent::ToolResult(tool_result) => Some(tool_result),
                _ => None,
            })
            .expect("child JSONL should include a dedicated tool_result event");
        assert_eq!(child_tool_result.agent_run_id, agent_run_id);
        assert_eq!(child_tool_result.tool_name, "apply_patch");
        assert_eq!(child_tool_result.call_id, "call_apply_patch_1");
        assert_eq!(child_tool_result.status, "success");
        let parent_transcript: Vec<_> = store
            .load_events()
            .expect("parent JSONL readable")
            .into_iter()
            .filter_map(|event| match event {
                SessionEvent::AgentRun(envelope) => envelope.known().cloned(),
                _ => None,
            })
            .map(|event| match event {
                AgentRunEvent::Spawned { .. } => "agent_run.spawned",
                AgentRunEvent::Completed { .. } => "agent_run.completed",
                other => panic!("unexpected parent agent-run event in S3 fixture: {other:?}"),
            })
            .collect();
        let child_transcript: Vec<_> = child_events
            .iter()
            .map(|event| match event {
                SessionEvent::SessionSnapshot(snapshot) => {
                    let roles: Vec<_> = snapshot
                        .state
                        .messages
                        .iter()
                        .map(|message| message.role.as_str())
                        .collect();
                    serde_json::json!({
                        "kind": "session_snapshot",
                        "roles": roles,
                    })
                }
                SessionEvent::ToolCall(tool_call) => serde_json::json!({
                    "kind": "tool_call",
                    "call_id": tool_call.call_id,
                    "tool_name": tool_call.tool_name,
                }),
                SessionEvent::ToolResult(tool_result) => serde_json::json!({
                    "kind": "tool_result",
                    "call_id": tool_result.call_id,
                    "tool_name": tool_result.tool_name,
                    "status": tool_result.status,
                }),
                other => panic!("unexpected child transcript event in S3 fixture: {other:?}"),
            })
            .collect();
        let normalized_transcript = serde_json::json!({
            "parent": parent_transcript,
            "child": child_transcript,
        });
        let fixture_path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("tests/fixtures/sub_agent/child_tool_transcript_sequence.json");
        let expected_transcript = std::fs::read_to_string(&fixture_path).unwrap_or_else(|error| {
            panic!(
                "S3 normalized transcript fixture must exist at {}: {error}",
                fixture_path.display()
            )
        });
        let actual_transcript = format!(
            "{}\n",
            serde_json::to_string_pretty(&normalized_transcript)
                .expect("S3 normalized transcript must serialize as JSON"),
        );
        assert_eq!(
            actual_transcript, expected_transcript,
            "parent/child transcript sequence must match the S3 fixture byte-for-byte",
        );
        let child_state = store
            .child(&agent_run_id_string)
            .load_state()
            .expect("child session JSONL readable")
            .expect("default child runner must persist a child session snapshot");
        assert_eq!(
            child_state.messages.len(),
            4,
            "child session JSONL should retain prompt, tool call, tool result, and final assistant reply",
        );
        assert_eq!(child_state.messages[0].role, "user");
        assert_eq!(
            child_state.messages[0].content,
            "please apply the patch to target.txt"
        );
        assert_eq!(child_state.messages[1].role, "tool_call");
        let tool_call: serde_json::Value = serde_json::from_str(&child_state.messages[1].content)
            .expect("tool_call message must carry JSON content");
        assert_eq!(tool_call["id"], "call_apply_patch_1");
        assert_eq!(tool_call["name"], "apply_patch");
        assert!(
            tool_call["arguments"]["input"]
                .as_str()
                .expect("input argument must be a string")
                .contains("line 2 modified"),
            "tool_call snapshot should retain apply_patch input, got {tool_call:?}",
        );
        assert_eq!(child_state.messages[2].role, "tool_result");
        let tool_result: serde_json::Value = serde_json::from_str(&child_state.messages[2].content)
            .expect("tool_result message must carry JSON content");
        assert_eq!(tool_result["id"], "call_apply_patch_1");
        assert_eq!(tool_result["name"], "apply_patch");
        assert_eq!(tool_result["status"], "success");
        assert_eq!(child_state.messages[3].role, "assistant");
        assert!(
            child_state.messages[3]
                .content
                .contains("patcher sub-agent done"),
            "child assistant reply should be persisted, got {:?}",
            child_state.messages[3].content,
        );
        assert_eq!(
            child_state
                .metadata
                .get("agent_run_id")
                .and_then(serde_json::Value::as_str),
            Some(agent_run_id_string.as_str()),
            "child session metadata must link back to the parent Spawned event",
        );

        // The acceptance criterion: the MAIN worktree is byte-for-byte
        // unchanged after the mutating sub-agent run — the child's
        // apply_patch wrote into the isolated workspace, not here.
        assert_eq!(
            std::fs::read_to_string(main_dir.join("target.txt")).expect("read main target.txt"),
            TARGET_BEFORE,
            "the sub-agent's apply_patch must NOT touch the main worktree (S2-INV-03)",
        );
    }

    /// CEX-S2-12 / S2-INV-03 panic safety: if the child runner panics
    /// after the isolated workspace is materialized, the dispatch must
    /// unwind cleanly (the panic propagates and is catchable) rather
    /// than aborting the process — proving `WorkspaceCleanupGuard`'s
    /// `Drop` backstop tears the workspace down without itself panicking
    /// or blocking the async runtime thread. (Under `#[cfg(test)]` the
    /// copy backend is used, so the guard's teardown runs inline.)
    #[tokio::test]
    async fn workspace_guard_is_panic_safe_when_child_runner_panics() {
        use futures::FutureExt;

        struct PanickingRunner;
        impl crate::internal::ai::agent::runtime::SubAgentChildRunner for PanickingRunner {
            fn run<'a>(
                &'a self,
                _request: crate::internal::ai::agent::runtime::SubAgentChildRunRequest<'a>,
            ) -> futures::future::BoxFuture<'a, Result<TaskResult, TaskFailure>> {
                Box::pin(async { panic!("simulated child-runner panic after materialization") })
            }
        }

        let main = tempfile::tempdir().expect("tempdir");
        let main_dir = main.path().to_path_buf();
        std::fs::write(main_dir.join("file.txt"), "x\n").unwrap();

        let conn = Database::connect("sqlite::memory:").await.unwrap();
        let usage = UsageRecorder::new(conn);
        let context_frame_loader = ContextFrameLoader::default();
        let store = crate::internal::ai::session::jsonl::SessionJsonlStore::new(
            main_dir
                .join(".libra")
                .join("sessions")
                .join("session-panic"),
        );
        let permission_service =
            PermissionService::new(Arc::new(TestAsker::always(PermissionReply::Once)));
        let provider_factory = ProviderFactory;

        let registry = Arc::new(TestRegistry::default());
        registry.insert(explore_subagent());
        let dispatcher = DefaultSubAgentDispatcher::new(
            registry,
            MultiAgentConfig {
                enabled: true,
                max_subagent_depth: 4,
                max_concurrent_subagents: 4,
            },
        )
        .with_child_runner(Arc::new(PanickingRunner))
        .with_workspace_isolation(
            crate::internal::ai::agent::runtime::WorkspaceIsolationConfig {
                fuse_state:
                    crate::internal::ai::orchestrator::workspace::FuseProvisionState::default(),
                sessions_root: main_dir.join(".libra").join("sessions"),
                allow_full_copy: true,
            },
        );

        let parent = parent_spec();
        let parent_ruleset: PermissionRuleset = Vec::new();
        let parent_binding = parent_binding();
        let parent_thread = "thread-panic".to_string();
        let parent_session: SessionId = "session-panic".to_string();
        let tool_registry = ToolRegistry::with_working_dir(main_dir.clone());

        let context = DispatchContext {
            parent_thread_id: &parent_thread,
            parent_session_id: &parent_session,
            parent_agent: &parent,
            parent_ruleset: &parent_ruleset,
            parent_model_binding: &parent_binding,
            parent_message_id: MessageId::from("msg-panic"),
            permission_service: &permission_service,
            session_store: &store,
            provider_factory: &provider_factory,
            provider_build_options: default_provider_build_options(),
            provider_build_options_resolver: None,
            tool_registry: &tool_registry,
            runtime_context: None,
            usage_recorder: &usage,
            context_frame_loader: &context_frame_loader,
            abort_token: AbortToken::new(),
            depth: 0,
            compaction_model: None,
            hook_runner: None,
        };

        let dispatch = std::panic::AssertUnwindSafe(dispatcher.dispatch(
            context,
            invocation("explore"),
            TaskEntryKind::UserInitiated {
                bypass_permission_ask: true,
            },
        ));
        let result = dispatch.catch_unwind().await;
        assert!(
            result.is_err(),
            "the child-runner panic must unwind through dispatch (caught here); reaching this \
             with Ok would mean the panic was swallowed, and a process abort would mean the \
             workspace guard's Drop double-panicked or blocked",
        );
    }

    /// CEX-S2-12 / S2-INV-03 fail-closed: when isolation is configured
    /// but the workspace cannot be materialized, the dispatch must be
    /// refused with `SafetyDenied` rather than running the sub-agent
    /// unsandboxed against the main worktree. Failure is injected by
    /// rooting the parent tool registry at a non-existent directory so
    /// `snapshot_workspace` (hence materialization) errors.
    #[tokio::test]
    async fn dispatch_fails_closed_when_isolation_materialization_fails() {
        let temp = tempfile::tempdir().expect("tempdir");
        // Intentionally NOT created: makes materialization fail.
        let missing_main = temp.path().join("does-not-exist");

        let conn = Database::connect("sqlite::memory:").await.unwrap();
        let usage = UsageRecorder::new(conn);
        let context_frame_loader = ContextFrameLoader::default();
        let store = crate::internal::ai::session::jsonl::SessionJsonlStore::new(
            temp.path()
                .join(".libra")
                .join("sessions")
                .join("session-fail-closed"),
        );
        let permission_service =
            PermissionService::new(Arc::new(TestAsker::always(PermissionReply::Once)));
        let provider_factory = ProviderFactory;

        let registry = Arc::new(TestRegistry::default());
        registry.insert(explore_subagent());
        let dispatcher = DefaultSubAgentDispatcher::new(
            registry,
            MultiAgentConfig {
                enabled: true,
                max_subagent_depth: 4,
                max_concurrent_subagents: 4,
            },
        )
        .with_default_child_runner()
        .with_workspace_isolation(
            crate::internal::ai::agent::runtime::WorkspaceIsolationConfig {
                fuse_state:
                    crate::internal::ai::orchestrator::workspace::FuseProvisionState::default(),
                sessions_root: temp.path().join(".libra").join("sessions"),
                allow_full_copy: true,
            },
        );

        let parent = parent_spec();
        let parent_ruleset: PermissionRuleset = Vec::new();
        let parent_binding = parent_binding();
        let parent_thread = "thread-fail-closed".to_string();
        let parent_session: SessionId = "session-fail-closed".to_string();
        let tool_registry = ToolRegistry::with_working_dir(missing_main.clone());

        let context = DispatchContext {
            parent_thread_id: &parent_thread,
            parent_session_id: &parent_session,
            parent_agent: &parent,
            parent_ruleset: &parent_ruleset,
            parent_model_binding: &parent_binding,
            parent_message_id: MessageId::from("msg-fail-closed"),
            permission_service: &permission_service,
            session_store: &store,
            provider_factory: &provider_factory,
            provider_build_options: default_provider_build_options(),
            provider_build_options_resolver: None,
            tool_registry: &tool_registry,
            runtime_context: None,
            usage_recorder: &usage,
            context_frame_loader: &context_frame_loader,
            abort_token: AbortToken::new(),
            depth: 0,
            compaction_model: None,
            hook_runner: None,
        };

        let result = dispatcher
            .dispatch(
                context,
                invocation("explore"),
                TaskEntryKind::UserInitiated {
                    bypass_permission_ask: true,
                },
            )
            .await;
        match result {
            Err(TaskFailure::SafetyDenied(SafetyDecisionDenial { reason })) => assert!(
                reason.contains("isolation could not be materialized"),
                "fail-closed denial must explain the isolation failure, got: {reason}",
            ),
            other => panic!("expected SafetyDenied (fail closed), got {other:?}"),
        }
        // The main worktree path was never created, and no isolated
        // workspace ran, so nothing was written there.
        assert!(
            !missing_main.exists(),
            "fail-closed must not create the main worktree"
        );
    }

    /// CEX-S2-12 criterion (3): every post-Spawned `TaskFailure` must
    /// map to the structurally-correct terminal `AgentRunEvent` so
    /// replay tooling can branch on the variant tag without
    /// string-matching the reason. Pins `map_failure_to_terminal_event`
    /// across all distinct mappings (the full-dispatch tests cover only
    /// Failed/TimedOut/Cancelled-ParentAbort; this pins the
    /// Cancelled-source matrix and every BudgetExceeded dimension).
    #[test]
    fn map_failure_to_terminal_event_pins_every_lifecycle_mapping() {
        use crate::internal::ai::{
            agent::runtime::{
                BudgetExceededReason, CancellationSource, ContextHandoffError, ToolLoopError,
            },
            agent_run::{BudgetDimension, CancellationReason},
            completion::CompletionError,
        };

        let id = AgentRunId::new();

        // `map_failure_to_terminal_event` only ever emits these four
        // variants; pull the id back out so every case confirms the
        // `agent_run_id` is threaded into the terminal event unchanged.
        fn run_id_of(event: &AgentRunEvent) -> AgentRunId {
            match event {
                AgentRunEvent::Cancelled { agent_run_id, .. }
                | AgentRunEvent::TimedOut { agent_run_id, .. }
                | AgentRunEvent::BudgetExceeded { agent_run_id, .. }
                | AgentRunEvent::Failed { agent_run_id, .. } => *agent_run_id,
                other => panic!(
                    "map_failure_to_terminal_event produced an unexpected variant: {other:?}"
                ),
            }
        }

        // Map a failure AND assert the agent_run_id round-trips, then
        // return the event for the caller's variant assertion.
        let ev = |failure: &TaskFailure| {
            let event = map_failure_to_terminal_event(id, failure);
            assert_eq!(
                run_id_of(&event),
                id,
                "agent_run_id must be threaded into the terminal event for {failure:?}",
            );
            event
        };

        // Cancellation source → mapped CancellationReason.
        assert!(matches!(
            ev(&TaskFailure::Cancelled {
                source: CancellationSource::ParentAbort,
            }),
            AgentRunEvent::Cancelled {
                reason: CancellationReason::UserRequested,
                ..
            }
        ));
        assert!(matches!(
            ev(&TaskFailure::Cancelled {
                source: CancellationSource::Timeout,
            }),
            AgentRunEvent::Cancelled {
                reason: CancellationReason::LayerOneTimeout,
                ..
            }
        ));
        match ev(&TaskFailure::Cancelled {
            source: CancellationSource::BudgetHardCap,
        }) {
            AgentRunEvent::Cancelled {
                reason: CancellationReason::Other(reason),
                ..
            } => assert_eq!(reason, "budget_hard_cap"),
            other => {
                panic!("BudgetHardCap cancel must map to Other(\"budget_hard_cap\"), got {other:?}")
            }
        }

        // Timeout → TimedOut.
        assert!(matches!(
            ev(&TaskFailure::Timeout {
                wall_clock_ms: 1_000
            }),
            AgentRunEvent::TimedOut { .. }
        ));

        // BudgetExceeded(reason) → BudgetExceeded{dimension}.
        assert!(matches!(
            ev(&TaskFailure::BudgetExceeded(
                BudgetExceededReason::CostHardCap
            )),
            AgentRunEvent::BudgetExceeded {
                dimension: BudgetDimension::Cost,
                ..
            }
        ));
        assert!(matches!(
            ev(&TaskFailure::BudgetExceeded(
                BudgetExceededReason::TokenHardCap
            )),
            AgentRunEvent::BudgetExceeded {
                dimension: BudgetDimension::Token,
                ..
            }
        ));
        assert!(matches!(
            ev(&TaskFailure::BudgetExceeded(
                BudgetExceededReason::WallClock
            )),
            AgentRunEvent::BudgetExceeded {
                dimension: BudgetDimension::WallClock,
                ..
            }
        ));
        assert!(matches!(
            ev(&TaskFailure::BudgetExceeded(BudgetExceededReason::Steps)),
            AgentRunEvent::BudgetExceeded {
                dimension: BudgetDimension::ToolCall,
                ..
            }
        ));

        // Catch-all: every remaining variant that can reach this helper
        // → Failed carrying the failure's Display text verbatim. Covers
        // a representative spread of the wildcard arm, including
        // `BudgetExceeded::Internal` (the budget reason that is NOT a
        // dimension), a structured-field variant, a tuple variant, and a
        // unit variant.
        for failure in [
            TaskFailure::BudgetExceeded(BudgetExceededReason::Internal {
                reason: "enforcement unavailable".to_string(),
            }),
            TaskFailure::FeatureDisabled,
            TaskFailure::UnknownSubagent {
                name: "ghost".to_string(),
                suggestions: vec!["explore".to_string()],
            },
            TaskFailure::DepthExceeded {
                current: 2,
                limit: 1,
            },
            TaskFailure::ConcurrencyExceeded {
                current: 4,
                limit: 1,
            },
            TaskFailure::PermissionEscalationDenied {
                permission: "edit".to_string(),
                pattern: "*".to_string(),
            },
            TaskFailure::SafetyDenied(SafetyDecisionDenial {
                reason: "denied for test".to_string(),
            }),
            TaskFailure::ApprovalRejected {
                feedback: Some("rejected for test".to_string()),
            },
            TaskFailure::ContextHandoffFailed(ContextHandoffError::NoFrameAvailable),
            TaskFailure::ProviderError(CompletionError::ProviderError("boom".to_string())),
            TaskFailure::ChildToolLoopFailed(ToolLoopError::StepBudgetExhausted { steps: 48 }),
        ] {
            match ev(&failure) {
                AgentRunEvent::Failed { reason, .. } => assert_eq!(
                    reason,
                    failure.to_string(),
                    "catch-all must surface the failure's Display text verbatim",
                ),
                other => panic!("{failure:?} must map to Failed, got {other:?}"),
            }
        }
    }
}
