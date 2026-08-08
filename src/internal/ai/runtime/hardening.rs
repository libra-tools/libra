//! Phase E hardening contracts for authorization, tool boundary, redaction, and audit.

use std::{collections::BTreeSet, fmt, sync::Arc};

use anyhow::Result;
use async_trait::async_trait;
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use tokio::sync::Mutex;
use uuid::Uuid;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PrincipalRole {
    Owner,
    Contributor,
    Observer,
    System,
}

impl PrincipalRole {
    /// `true` for roles that may execute state-mutating operations.
    /// Observers are read-only and fail-closed against mutation; every
    /// other role passes this gate (the mutating-vs-approval decision
    /// lives downstream in [`is_privileged`](Self::is_privileged)).
    ///
    /// Used by [`ToolBoundaryPolicy::decide`] in place of the inline
    /// `role == Observer && mutates` check so capability rules stay
    /// in one place.
    pub fn can_mutate(self) -> bool {
        !matches!(self, PrincipalRole::Observer)
    }
}

impl PrincipalRole {
    /// `true` for roles that can execute mutating tools **without
    /// runtime-mediated approval**. Today only `System` qualifies —
    /// platform code running on Libra's behalf doesn't go through the
    /// approval pipeline. Owners and Contributors still need approval
    /// for mutations even though they pass [`can_mutate`](Self::can_mutate).
    ///
    /// Used by [`ToolBoundaryPolicy::decide`] in place of the inline
    /// `role != System` check so the privileged-skip rule stays in one
    /// place.
    pub fn is_privileged(self) -> bool {
        matches!(self, PrincipalRole::System)
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct PrincipalContext {
    pub principal_id: String,
    pub role: PrincipalRole,
}

impl PrincipalContext {
    pub fn system() -> Self {
        Self {
            principal_id: "libra-runtime".to_string(),
            role: PrincipalRole::System,
        }
    }

    /// Map a git-internal [`ActorRef`](git_internal::internal::object::types::ActorRef)
    /// onto a [`PrincipalContext`].
    ///
    /// Mapping policy (one-way, lossy — `ActorKind` carries more granularity
    /// than `PrincipalRole`):
    ///
    /// - `ActorKind::System` → `PrincipalRole::System`
    /// - `ActorKind::Human` / `ActorKind::Agent` / `ActorKind::McpClient`
    ///   → `PrincipalRole::Contributor` (all act on behalf of the
    ///   workspace owner, distinct from platform-level System)
    /// - `ActorKind::Other(_)` → `PrincipalRole::Observer` (fail-closed
    ///   to least-privilege for unknown actor categories)
    ///
    /// The `principal_id` is the verbatim
    /// [`ActorRef::id`](git_internal::internal::object::types::ActorRef::id),
    /// so audit pipelines can correlate `PrincipalContext.principal_id`
    /// with the on-object actor identifier without a side table.
    pub fn from_actor(actor: &git_internal::internal::object::types::ActorRef) -> Self {
        use git_internal::internal::object::types::ActorKind;
        let role = match actor.kind() {
            ActorKind::System => PrincipalRole::System,
            ActorKind::Human | ActorKind::Agent | ActorKind::McpClient => {
                PrincipalRole::Contributor
            }
            ActorKind::Other(_) => PrincipalRole::Observer,
        };
        Self {
            principal_id: actor.id().to_string(),
            role,
        }
    }
}

/// A tool-boundary operation the runtime is about to attempt.
///
/// Carries the classifier inputs the policy actually reads
/// (`tool_name`, `mutates_state`, `requires_network`) plus a
/// structured [`ToolOperationDetails`] tag that distinguishes the
/// payload shape — currently a plain tool call vs. a sub-agent spawn
/// (mirrors the `task` tool semantics introduced in OC-Phase 2 P3.3).
/// Construct via the [`ToolOperation::tool`] / [`ToolOperation::sub_agent_spawn`]
/// helpers so the `details` discriminator stays in sync with the
/// shape-determined fields.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ToolOperation {
    pub tool_name: String,
    pub mutates_state: bool,
    pub requires_network: bool,
    #[serde(default)]
    pub details: ToolOperationDetails,
}

/// Shape-tag for a [`ToolOperation`]. `Tool` is the default for the
/// model's normal tool-loop calls; `SubAgentSpawn` carries the
/// child-agent name and a redacted prompt digest so auditors can
/// reconstruct who-asked-what without seeing the verbatim user
/// prompt.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", tag = "kind")]
pub enum ToolOperationDetails {
    /// A regular registry-routed tool call.
    #[default]
    Tool,
    /// A sub-agent spawn issued via the `task` tool.
    SubAgentSpawn { name: String, prompt_digest: String },
}

impl ToolOperation {
    /// Build a regular tool-call operation. Use this in the
    /// registry's pre-execute path; the policy classifies based on
    /// the three boolean/string fields.
    pub fn tool(tool_name: impl Into<String>, mutates_state: bool, requires_network: bool) -> Self {
        Self {
            tool_name: tool_name.into(),
            mutates_state,
            requires_network,
            details: ToolOperationDetails::Tool,
        }
    }

    /// Build a sub-agent spawn operation. Pinned to `tool_name =
    /// "task"`, `mutates_state = true`, `requires_network = false`
    /// because every spawn must route through the approval-mediated
    /// `task` boundary (CEX-S2-12 dispatcher).
    pub fn sub_agent_spawn(name: impl Into<String>, prompt_digest: impl Into<String>) -> Self {
        Self {
            tool_name: "task".to_string(),
            mutates_state: true,
            requires_network: false,
            details: ToolOperationDetails::SubAgentSpawn {
                name: name.into(),
                prompt_digest: prompt_digest.into(),
            },
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct BoundaryDecision {
    pub allowed: bool,
    pub approval_required: bool,
    pub reason: String,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SafetyDisposition {
    Allow,
    Deny,
    NeedsHuman,
}

impl SafetyDisposition {
    pub fn is_allow(self) -> bool {
        self == Self::Allow
    }

    pub fn is_deny(self) -> bool {
        self == Self::Deny
    }

    pub fn needs_human(self) -> bool {
        self == Self::NeedsHuman
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BlastRadius {
    Workspace,
    Repository,
    System,
    Network,
    Unknown,
}

impl fmt::Display for BlastRadius {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let label = match self {
            Self::Workspace => "workspace",
            Self::Repository => "repository",
            Self::System => "system",
            Self::Network => "network",
            Self::Unknown => "unknown",
        };
        f.write_str(label)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CommandSafetySurface {
    Shell,
    LibraVcs,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct SafetyDecision {
    pub disposition: SafetyDisposition,
    pub rule_name: String,
    pub reason: String,
    pub blast_radius: BlastRadius,
}

impl SafetyDecision {
    pub fn allow(
        rule_name: impl Into<String>,
        reason: impl Into<String>,
        blast_radius: BlastRadius,
    ) -> Self {
        Self {
            disposition: SafetyDisposition::Allow,
            rule_name: rule_name.into(),
            reason: reason.into(),
            blast_radius,
        }
    }

    pub fn deny(
        rule_name: impl Into<String>,
        reason: impl Into<String>,
        blast_radius: BlastRadius,
    ) -> Self {
        Self {
            disposition: SafetyDisposition::Deny,
            rule_name: rule_name.into(),
            reason: reason.into(),
            blast_radius,
        }
    }

    pub fn needs_human(
        rule_name: impl Into<String>,
        reason: impl Into<String>,
        blast_radius: BlastRadius,
    ) -> Self {
        Self {
            disposition: SafetyDisposition::NeedsHuman,
            rule_name: rule_name.into(),
            reason: reason.into(),
            blast_radius,
        }
    }

    pub fn is_allow(&self) -> bool {
        self.disposition.is_allow()
    }

    pub fn is_deny(&self) -> bool {
        self.disposition.is_deny()
    }

    pub fn is_needs_human(&self) -> bool {
        self.disposition.needs_human()
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ToolBoundaryPolicy {
    readonly_tools: BTreeSet<String>,
    mutating_tools: BTreeSet<String>,
    allow_network: bool,
    policy_version: String,
}

impl ToolBoundaryPolicy {
    pub fn default_runtime() -> Self {
        Self {
            readonly_tools: [
                "read_file",
                "list_dir",
                "grep_files",
                "search_files",
                "web_search",
                "request_user_input",
                "mcp_read",
                "run_libra_vcs",
            ]
            .into_iter()
            .map(str::to_string)
            .collect(),
            mutating_tools: [
                "shell",
                "apply_patch",
                "update_plan",
                "submit_intent_draft",
                "submit_plan_draft",
                "submit_task_complete",
                "mcp_write",
            ]
            .into_iter()
            .map(str::to_string)
            .collect(),
            allow_network: false,
            policy_version: "tool-boundary:v1".to_string(),
        }
    }

    pub fn policy_version(&self) -> &str {
        &self.policy_version
    }

    /// Conservative mutability classification shared by runtime admission and
    /// cooperative cancellation. Keep the known-tool set here so callers do
    /// not grow parallel mutation lists.
    pub fn operation_may_mutate(&self, operation: &ToolOperation) -> bool {
        operation.mutates_state
            || self.mutating_tools.contains(&operation.tool_name)
            || operation.tool_name.starts_with("create_")
            || operation.tool_name.starts_with("update_")
    }

    pub fn decide(
        &self,
        principal: &PrincipalContext,
        operation: &ToolOperation,
    ) -> BoundaryDecision {
        if operation.requires_network && !self.allow_network {
            return BoundaryDecision {
                allowed: false,
                approval_required: false,
                reason: "network access is disabled by tool boundary policy".to_string(),
            };
        }

        if !principal.role.can_mutate() && operation.mutates_state {
            return BoundaryDecision {
                allowed: false,
                approval_required: false,
                reason: "observer principals cannot run mutating tools".to_string(),
            };
        }

        let known_readonly = self.readonly_tools.contains(&operation.tool_name)
            || operation.tool_name.starts_with("list_");
        let known_mutating = self.operation_may_mutate(operation);

        if known_readonly && !operation.mutates_state {
            return BoundaryDecision {
                allowed: true,
                approval_required: false,
                reason: "readonly tool allowed".to_string(),
            };
        }

        if known_mutating || operation.mutates_state {
            return BoundaryDecision {
                allowed: true,
                approval_required: !principal.role.is_privileged(),
                reason: "mutating tool requires runtime-mediated approval".to_string(),
            };
        }

        BoundaryDecision {
            allowed: false,
            approval_required: false,
            reason: format!("unknown tool '{}' is not allowlisted", operation.tool_name),
        }
    }
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct SecretRedactor {
    markers: Vec<String>,
}

impl SecretRedactor {
    pub fn default_runtime() -> Self {
        Self {
            markers: [
                "api_key:",
                "api_key=",
                "authorization: bearer ",
                "control_token:",
                "control_token=",
                "control-token:",
                "control-token=",
                "password:",
                "password=",
                "token:",
                "token=",
                "x-code-controller-token:",
                "x-code-controller-token=",
                "x-libra-control-token:",
                "x-libra-control-token=",
                // Wave 7 / PR 7 — path-component patterns. Common
                // `LIBRA_LOG_FILE` paths injected by automation
                // clients can embed secret-like substrings as
                // directory segments (e.g.
                // `/tmp/abc-secret-key-xyz/libra.log`). Treating
                // `secret-` / `secret_` as markers ensures the
                // remainder of that path component is replaced
                // with `[REDACTED]` before it reaches the
                // diagnostics response.
                "secret-",
                "secret_",
            ]
            .into_iter()
            .map(str::to_string)
            .collect(),
        }
    }

    pub fn redact(&self, input: &str) -> String {
        let mut output = input.to_string();
        for marker in &self.markers {
            output = redact_marker(&output, marker);
        }
        output
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct AuditEvent {
    pub trace_id: Uuid,
    pub principal_id: String,
    pub action: String,
    pub policy_version: String,
    pub redacted_summary: String,
    pub at: DateTime<Utc>,
}

/// Append-only audit channel.
///
/// **CEX-00.5 contract**: implementors must persist (or otherwise observe)
/// every `AuditEvent` passed to `append`. The two semantic helpers
/// `record_decision` and `record_event` are provided with default
/// implementations that wrap their inputs into an `AuditEvent` and forward to
/// `append`; concrete sinks should not need to override them. Tests for those
/// default flows live in `tests/ai_hardening_contract_test.rs`.
///
/// `flush` exists for sinks that buffer (e.g. file-based JSONL writers); the
/// default `TracingAuditSink` and `InMemoryAuditSink` are unbuffered and
/// return `Ok(())` immediately.
#[async_trait]
pub trait AuditSink: Send + Sync {
    /// Lower-level write of a fully-formed audit event. The semantic helpers
    /// (`record_decision` / `record_event`) call this after constructing the
    /// `AuditEvent`.
    async fn append(&self, event: AuditEvent) -> Result<()>;

    /// Flush any buffered writes.
    async fn flush(&self) -> Result<()>;

    /// Record a `BoundaryDecision` made for a given principal and tool
    /// operation. The default impl builds a summary string, runs it through
    /// the supplied `redactor` so secrets in `decision.reason` or
    /// `operation.tool_name` cannot leak verbatim, and forwards an
    /// `AuditEvent` to `append`.
    ///
    /// **Why an explicit `&SecretRedactor`**: `AuditEvent.redacted_summary`
    /// claims its content is post-redaction. Without an explicit redactor
    /// argument, default-impl callers would silently violate that claim
    /// (CEX-00.5 Codex review P1-a). Pass
    /// `SecretRedactor::default_runtime()` if you have no project-specific
    /// patterns; pass a configured redactor otherwise.
    async fn record_decision(
        &self,
        trace_id: Uuid,
        principal: &PrincipalContext,
        policy_version: &str,
        operation: &ToolOperation,
        decision: &BoundaryDecision,
        redactor: &SecretRedactor,
    ) -> Result<()> {
        let summary = format!(
            "tool={} mutates={} network={} allowed={} approval_required={} reason={}",
            operation.tool_name,
            operation.mutates_state,
            operation.requires_network,
            decision.allowed,
            decision.approval_required,
            decision.reason
        );
        self.append(AuditEvent {
            trace_id,
            principal_id: principal.principal_id.clone(),
            action: "boundary_decision".to_string(),
            policy_version: policy_version.to_string(),
            redacted_summary: redactor.redact(&summary),
            at: Utc::now(),
        })
        .await
    }

    /// Record a domain event (anything implementing the `Event` trait) on
    /// the audit channel. The default impl produces an action string of
    /// `event/<event_kind>`, runs `event_summary()` through `redactor`, and
    /// forwards to `append`.
    ///
    /// **Why an explicit `&SecretRedactor`**: same rationale as
    /// `record_decision` — domain events may carry user prompts or tool
    /// outputs containing secrets, and the `AuditEvent.redacted_summary`
    /// claim must hold (CEX-00.5 Codex review P1-a).
    async fn record_event(
        &self,
        trace_id: Uuid,
        principal: &PrincipalContext,
        policy_version: &str,
        event: &dyn super::event::Event,
        redactor: &SecretRedactor,
    ) -> Result<()> {
        let summary = event.event_summary();
        self.append(AuditEvent {
            trace_id,
            principal_id: principal.principal_id.clone(),
            action: super::event::audit_action_for(event),
            policy_version: policy_version.to_string(),
            redacted_summary: redactor.redact(&summary),
            at: Utc::now(),
        })
        .await
    }
}

#[derive(Clone)]
pub struct ToolBoundaryRuntime {
    trace_id: Uuid,
    principal: PrincipalContext,
    policy: ToolBoundaryPolicy,
    redactor: SecretRedactor,
    audit_sink: Arc<dyn AuditSink>,
}

impl ToolBoundaryRuntime {
    pub fn new(
        trace_id: Uuid,
        principal: PrincipalContext,
        policy: ToolBoundaryPolicy,
        redactor: SecretRedactor,
        audit_sink: Arc<dyn AuditSink>,
    ) -> Self {
        Self {
            trace_id,
            principal,
            policy,
            redactor,
            audit_sink,
        }
    }

    pub fn system(trace_id: Uuid, audit_sink: Arc<dyn AuditSink>) -> Self {
        Self::new(
            trace_id,
            PrincipalContext::system(),
            ToolBoundaryPolicy::default_runtime(),
            SecretRedactor::default_runtime(),
            audit_sink,
        )
    }

    pub fn decide(&self, operation: &ToolOperation) -> BoundaryDecision {
        self.policy.decide(&self.principal, operation)
    }

    /// Return the policy's conservative mutability classification for this
    /// operation without making an authorization decision.
    pub fn operation_may_mutate(&self, operation: &ToolOperation) -> bool {
        self.policy.operation_may_mutate(operation)
    }

    pub async fn append_audit(
        &self,
        action: impl Into<String>,
        summary: impl AsRef<str>,
    ) -> Result<()> {
        self.audit_sink
            .append(AuditEvent {
                trace_id: self.trace_id,
                principal_id: self.principal.principal_id.clone(),
                action: action.into(),
                policy_version: self.policy.policy_version().to_string(),
                redacted_summary: self.redactor.redact(summary.as_ref()),
                at: Utc::now(),
            })
            .await
    }

    pub async fn flush_audit(&self) -> Result<()> {
        self.audit_sink.flush().await
    }
}

#[derive(Debug, Default)]
pub struct TracingAuditSink;

#[async_trait]
impl AuditSink for TracingAuditSink {
    async fn append(&self, event: AuditEvent) -> Result<()> {
        tracing::info!(
            trace_id = %event.trace_id,
            principal = %event.principal_id,
            action = %event.action,
            policy_version = %event.policy_version,
            summary = %event.redacted_summary,
            "ai runtime audit event"
        );
        Ok(())
    }

    async fn flush(&self) -> Result<()> {
        Ok(())
    }
}

#[derive(Debug, Default)]
pub struct InMemoryAuditSink {
    events: Mutex<Vec<AuditEvent>>,
}

impl InMemoryAuditSink {
    pub async fn events(&self) -> Vec<AuditEvent> {
        self.events.lock().await.clone()
    }
}

#[async_trait]
impl AuditSink for InMemoryAuditSink {
    async fn append(&self, event: AuditEvent) -> Result<()> {
        self.events.lock().await.push(event);
        Ok(())
    }

    async fn flush(&self) -> Result<()> {
        Ok(())
    }
}

fn redact_marker(input: &str, marker: &str) -> String {
    let lower = input.to_lowercase();
    let mut cursor = 0;
    let mut output = String::with_capacity(input.len());

    while let Some(relative_start) = lower[cursor..].find(marker) {
        let marker_start = cursor + relative_start;
        let value_start = marker_start + marker.len();
        output.push_str(&input[cursor..value_start]);

        let mut value_cursor = value_start;
        while let Some(ch) = input[value_cursor..].chars().next() {
            if !ch.is_whitespace() {
                break;
            }
            output.push(ch);
            value_cursor += ch.len_utf8();
        }

        let value_end = input[value_cursor..]
            .char_indices()
            .find_map(|(offset, ch)| {
                if ch.is_whitespace() || ch == ',' || ch == ';' {
                    Some(value_cursor + offset)
                } else {
                    None
                }
            })
            .unwrap_or(input.len());

        output.push_str("[REDACTED]");
        cursor = value_end;
    }

    output.push_str(&input[cursor..]);
    output
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn runtime_redactor_masks_local_control_tokens() {
        let redactor = SecretRedactor::default_runtime();
        let input =
            "X-Libra-Control-Token: process-secret X-Code-Controller-Token=lease-secret token: raw";

        let output = redactor.redact(input);

        assert!(!output.contains("process-secret"));
        assert!(!output.contains("lease-secret"));
        assert!(!output.contains(" raw"));
        assert!(output.contains("X-Libra-Control-Token: [REDACTED]"));
        assert!(output.contains("X-Code-Controller-Token=[REDACTED]"));
    }

    /// `PrincipalContext::from_actor` must map every `ActorKind` variant
    /// to the right `PrincipalRole`. Human / Agent / McpClient all
    /// collapse to `Contributor` (they act on behalf of the workspace
    /// owner); System maps to `System`; the open-ended `Other(_)` variant
    /// is fail-closed to `Observer` (least privilege) so a malformed
    /// actor on disk can't accidentally route as System.
    #[test]
    fn principal_context_from_actor_maps_actor_kinds_to_roles() {
        use git_internal::internal::object::types::{ActorKind, ActorRef};

        let human = ActorRef::new(ActorKind::Human, "user@example").unwrap();
        let agent = ActorRef::new(ActorKind::Agent, "libra-coder").unwrap();
        let system = ActorRef::new(ActorKind::System, "libra-orchestrator").unwrap();
        let mcp_client = ActorRef::new(ActorKind::McpClient, "mcp-user").unwrap();
        let other = ActorRef::new(ActorKind::Other("custom".to_string()), "unknown").unwrap();

        assert_eq!(
            PrincipalContext::from_actor(&human),
            PrincipalContext {
                principal_id: "user@example".to_string(),
                role: PrincipalRole::Contributor,
            }
        );
        assert_eq!(
            PrincipalContext::from_actor(&agent),
            PrincipalContext {
                principal_id: "libra-coder".to_string(),
                role: PrincipalRole::Contributor,
            }
        );
        assert_eq!(
            PrincipalContext::from_actor(&system),
            PrincipalContext {
                principal_id: "libra-orchestrator".to_string(),
                role: PrincipalRole::System,
            }
        );
        assert_eq!(
            PrincipalContext::from_actor(&mcp_client),
            PrincipalContext {
                principal_id: "mcp-user".to_string(),
                role: PrincipalRole::Contributor,
            }
        );
        assert_eq!(
            PrincipalContext::from_actor(&other),
            PrincipalContext {
                principal_id: "unknown".to_string(),
                role: PrincipalRole::Observer,
            }
        );
    }

    /// `can_mutate()` must return `false` only for `Observer` so the
    /// rule "observers are read-only" stays in one place. Every other
    /// role passes the gate.
    #[test]
    fn principal_role_can_mutate_rejects_only_observer() {
        assert!(!PrincipalRole::Observer.can_mutate());
        for role in [
            PrincipalRole::Owner,
            PrincipalRole::Contributor,
            PrincipalRole::System,
        ] {
            assert!(role.can_mutate(), "{role:?} must be allowed to mutate",);
        }
    }

    /// `is_privileged()` must return `true` only for `System` — Owners
    /// and Contributors still need approval for mutations even though
    /// they pass `can_mutate()`.
    #[test]
    fn principal_role_is_privileged_only_for_system() {
        assert!(PrincipalRole::System.is_privileged());
        for role in [
            PrincipalRole::Owner,
            PrincipalRole::Contributor,
            PrincipalRole::Observer,
        ] {
            assert!(
                !role.is_privileged(),
                "{role:?} must NOT be privileged (still needs approval)",
            );
        }
    }

    /// `can_mutate()` and `is_privileged()` are not equivalent — every
    /// privileged role also passes `can_mutate()`, but `can_mutate()`
    /// alone is not sufficient to skip approval. This test pins that
    /// asymmetry so a future "simplify the predicates" refactor can't
    /// silently collapse the two.
    #[test]
    fn principal_role_privileged_implies_can_mutate_but_not_vice_versa() {
        // Forward: privileged ⇒ can_mutate.
        for role in [
            PrincipalRole::Owner,
            PrincipalRole::Contributor,
            PrincipalRole::Observer,
            PrincipalRole::System,
        ] {
            if role.is_privileged() {
                assert!(
                    role.can_mutate(),
                    "{role:?} is privileged but failed can_mutate",
                );
            }
        }
        // Reverse must NOT hold: Contributor + Owner are can_mutate but
        // NOT privileged.
        assert!(
            PrincipalRole::Contributor.can_mutate() && !PrincipalRole::Contributor.is_privileged(),
        );
        assert!(PrincipalRole::Owner.can_mutate() && !PrincipalRole::Owner.is_privileged());
    }
}
