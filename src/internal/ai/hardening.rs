//! Phase E hardening contracts for authorization, tool boundary, redaction, and audit.

use std::{collections::BTreeSet, fmt, sync::Arc};

use anyhow::{Context, Result, bail};
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
    /// - `ActorKind::Human` / `ActorKind::Agent`
    ///   → `PrincipalRole::Contributor` (all act on behalf of the
    ///   workspace owner, distinct from platform-level System)
    /// - `ActorKind::Other(_)` (and any actor kind beyond Human/Agent/System)
    ///   → `PrincipalRole::Observer` (fail-closed
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
            ActorKind::Human | ActorKind::Agent => PrincipalRole::Contributor,
            ActorKind::Other(_) | ActorKind::McpClient => PrincipalRole::Observer,
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
            ]
            .into_iter()
            .map(str::to_string)
            .collect(),
            allow_network: false,
            policy_version: "tool-boundary:v1".to_string(),
        }
    }

    /// Toggle network access for this policy copy (plan-execution Allow/Deny).
    pub fn with_network_access(mut self, allow_network: bool) -> Self {
        self.allow_network = allow_network;
        self
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
    /// Exact secret values (e.g. `--env-file` provider keys) scrubbed
    /// wherever they appear in projected strings. Built via
    /// [`SecretRedactor::with_forbidden_env_values`] using A0-08
    /// [`crate::internal::ai::observed_agents::trust::env_name_is_forbidden`].
    literals: Vec<String>,
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
            literals: Vec::new(),
        }
    }

    /// Register exact values from env entries whose names are forbidden by
    /// A0-08 (`env_name_is_forbidden`). Non-forbidden keys (e.g. model names)
    /// are ignored so Code Web does not invent a second secret-name table.
    pub fn with_forbidden_env_values<I, K, V>(mut self, entries: I) -> Self
    where
        I: IntoIterator<Item = (K, V)>,
        K: AsRef<str>,
        V: AsRef<str>,
    {
        use crate::internal::ai::observed_agents::trust::env_name_is_forbidden;

        for (name, value) in entries {
            if !env_name_is_forbidden(name.as_ref()) {
                continue;
            }
            let value = value.as_ref().trim();
            if value.is_empty() {
                continue;
            }
            if !self.literals.iter().any(|existing| existing == value) {
                self.literals.push(value.to_string());
            }
        }
        self
    }

    /// Fail closed when this redactor has no rules (GC-07): never project
    /// unredacted content through an empty rule set.
    pub fn ensure_configured(&self) -> Result<()> {
        if self.markers.is_empty() && self.literals.is_empty() {
            bail!(
                "secret redactor has no markers or literal secrets; \
                 refusing to project unredacted content"
            );
        }
        Ok(())
    }

    pub fn redact(&self, input: &str) -> String {
        let mut output = input.to_string();
        // Longest literals first so a short suffix of a longer key cannot
        // leave a partial secret behind after replacement.
        let mut literals: Vec<&str> = self.literals.iter().map(String::as_str).collect();
        literals.sort_by_key(|value| std::cmp::Reverse(value.len()));
        for literal in &literals {
            if !literal.is_empty() {
                output = output.replace(literal, "[REDACTED]");
            }
        }
        for marker in &self.markers {
            output = redact_marker(&output, marker);
        }
        output
    }

    /// Redact streaming / free-text surfaces where a secret may arrive across
    /// multiple deltas. Omit trailing proper prefixes first, then apply full
    /// literal/marker redaction. Ordering matters: skipping omit whenever
    /// `[REDACTED]` already appears would leave partial secrets next to
    /// unrelated marker hits (W3-12 Codex r12).
    pub fn redact_streaming_text(&self, input: &str) -> String {
        let mut literals: Vec<&str> = self.literals.iter().map(String::as_str).collect();
        literals.sort_by_key(|value| std::cmp::Reverse(value.len()));
        let withheld = redact_trailing_literal_prefixes(input, &literals);
        self.redact(&withheld)
    }
}

/// Recursively scrub string leaves (and object keys) in a JSON value.
/// Returns `Err` when two keys redact to the same name (fail closed).
pub fn redact_json_value(value: &mut serde_json::Value, redactor: &SecretRedactor) -> Result<()> {
    match value {
        serde_json::Value::String(text) => {
            *text = redactor.redact(text);
            Ok(())
        }
        serde_json::Value::Array(items) => {
            for item in items {
                redact_json_value(item, redactor)?;
            }
            Ok(())
        }
        serde_json::Value::Object(map) => {
            // Rebuild so object keys that embed registered secrets (or
            // marker-shaped prefixes) are scrubbed too — walking only values
            // would leave secret-bearing keys on the wire (W3-12 Codex r1).
            // Keys use `redact` (no 1-char trailing prefixes) so wire field
            // names like `status` / `plans` stay intact when env secrets
            // start with common letters (W3-12 Codex r4).
            let original = std::mem::take(map);
            for (key, mut child) in original {
                redact_json_value(&mut child, redactor)?;
                let redacted_key = redactor.redact(&key);
                if map.contains_key(&redacted_key) {
                    bail!(
                        "redacted JSON object key collision for `{redacted_key}`; \
                         refusing to project ambiguous metadata"
                    );
                }
                map.insert(redacted_key, child);
            }
            // Free-text projection fields may grow mid-secret across SSE
            // deltas. Only apply trailing-prefix withholding while the entry
            // is still `streaming: true`; finalized text uses full-literal
            // redaction only so ordinary endings like `yes` survive.
            let is_streaming = map
                .get("streaming")
                .and_then(serde_json::Value::as_bool)
                .unwrap_or(false);
            if is_streaming {
                for field in [
                    "content",
                    "status",
                    "details",
                    "title",
                    "summary",
                    "description",
                    "prompt",
                    "note",
                    "lastError",
                    "logFile",
                ] {
                    if let Some(serde_json::Value::String(text)) = map.get_mut(field) {
                        *text = redactor.redact_streaming_text(text);
                    }
                }
            }
            Ok(())
        }
        serde_json::Value::Null | serde_json::Value::Bool(_) | serde_json::Value::Number(_) => {
            Ok(())
        }
    }
}

/// Serialize `value` then redact every string leaf. Returns `Err` when the
/// redactor is unconfigured (fail closed), serialization fails, or redacted
/// object keys collide.
pub fn project_json_for_wire<T: serde::Serialize>(
    value: &T,
    redactor: &SecretRedactor,
) -> Result<serde_json::Value> {
    redactor.ensure_configured()?;
    let mut projected = serde_json::to_value(value)
        .context("failed to serialize value for redacted wire projection")?;
    redact_json_value(&mut projected, redactor).context("failed to redact wire projection")?;
    Ok(projected)
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
        Self::system_with_redactor(trace_id, audit_sink, SecretRedactor::default_runtime())
    }

    /// System principal with an explicit projection redactor (W3-12: attach
    /// `--env-file` forbidden values so tool-boundary audit summaries cannot
    /// leak bare provider keys).
    pub fn system_with_redactor(
        trace_id: Uuid,
        audit_sink: Arc<dyn AuditSink>,
        redactor: SecretRedactor,
    ) -> Self {
        Self::new(
            trace_id,
            PrincipalContext::system(),
            ToolBoundaryPolicy::default_runtime(),
            redactor,
            audit_sink,
        )
    }

    /// Return a copy whose policy matches the approved network setting for this
    /// execution (Phase 1 Allow/Deny), without changing the shared worker
    /// boundary used for other turns.
    pub fn with_network_access(&self, allow_network: bool) -> Self {
        Self {
            trace_id: self.trace_id,
            principal: self.principal.clone(),
            policy: self.policy.clone().with_network_access(allow_network),
            redactor: self.redactor.clone(),
            audit_sink: self.audit_sink.clone(),
        }
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

fn redact_trailing_literal_prefixes(input: &str, literals: &[&str]) -> String {
    // Omit at most one trailing proper prefix of any length ≥1. Used only for
    // `streaming: true` free-text fields so finalized copy (e.g. `yes`) is not
    // clipped when an `sk-...` env secret is registered (W3-12 Codex r8/r10).
    // If the full literal is already a suffix, leave it for `redact()` rather
    // than omitting a near-complete proper prefix that would leave one char.
    let mut best_prefix: Option<String> = None;
    for literal in literals {
        if literal.is_empty() || input.ends_with(literal) {
            continue;
        }
        let literal_chars: Vec<char> = literal.chars().collect();
        if literal_chars.len() < 2 {
            continue;
        }
        let max_prefix = literal_chars.len() - 1;
        for prefix_len in (1..=max_prefix).rev() {
            let prefix: String = literal_chars[..prefix_len].iter().collect();
            if prefix == "[REDACTED]" {
                continue;
            }
            if !input.ends_with(&prefix) {
                continue;
            }
            let take = best_prefix
                .as_ref()
                .is_none_or(|current| prefix.len() > current.len());
            if take {
                best_prefix = Some(prefix);
            }
            break;
        }
    }
    let Some(prefix) = best_prefix else {
        return input.to_string();
    };
    let mut output = input.to_string();
    output.truncate(output.len() - prefix.len());
    output
}

fn find_ascii_ignore_case(haystack: &str, needle: &str) -> Option<usize> {
    let needle_bytes = needle.as_bytes();
    if needle_bytes.is_empty() {
        return Some(0);
    }
    let hay_bytes = haystack.as_bytes();
    if hay_bytes.len() < needle_bytes.len() {
        return None;
    }
    let last_start = hay_bytes.len() - needle_bytes.len();
    let mut start = 0;
    while start <= last_start {
        if !haystack.is_char_boundary(start) {
            start += 1;
            continue;
        }
        let end = start + needle_bytes.len();
        if !haystack.is_char_boundary(end) {
            start += 1;
            continue;
        }
        let mut matched = true;
        for (offset, needle_byte) in needle_bytes.iter().enumerate() {
            if !hay_bytes[start + offset].eq_ignore_ascii_case(needle_byte) {
                matched = false;
                break;
            }
        }
        if matched {
            return Some(start);
        }
        start += 1;
    }
    None
}

fn redact_marker(input: &str, marker: &str) -> String {
    // Markers are ASCII. Search with ASCII case-folding on the original
    // bytes so Unicode characters whose lowercase form changes length
    // (e.g. `İ`) cannot shift offsets into the secret tail (W3-12 Codex r3).
    let mut cursor = 0;
    let mut output = String::with_capacity(input.len());

    while let Some(relative_start) = find_ascii_ignore_case(&input[cursor..], marker) {
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

    #[test]
    fn with_forbidden_env_values_registers_only_a0_08_forbidden_names() {
        let redactor = SecretRedactor::default_runtime().with_forbidden_env_values([
            ("OPENAI_API_KEY", "sk-live-envfile-secret-value"),
            ("LIBRA_MODEL", "should-not-be-redacted"),
            ("MOONSHOT_API_KEY", ""),
        ]);

        let output = redactor.redact(
            "provider failed with sk-live-envfile-secret-value while model=should-not-be-redacted",
        );
        assert!(
            !output.contains("sk-live-envfile-secret-value"),
            "forbidden env value must be scrubbed: {output}"
        );
        assert!(
            output.contains("should-not-be-redacted"),
            "non-forbidden env values must not invent a second secret table: {output}"
        );
        assert!(redactor.ensure_configured().is_ok());
    }

    #[test]
    fn ensure_configured_fails_closed_when_empty() {
        let empty = SecretRedactor::default();
        assert!(empty.ensure_configured().is_err());
    }

    #[test]
    fn project_json_for_wire_scrubs_nested_strings() {
        let redactor = SecretRedactor::default_runtime()
            .with_forbidden_env_values([("OPENAI_API_KEY", "sk-nested-secret-999")]);
        let value = serde_json::json!({
            "transcript": [{"content": "leak sk-nested-secret-999 here"}],
            "meta": {"token:": "ignored-prefix-shape api_key: visible-key"}
        });
        let projected = project_json_for_wire(&value, &redactor).expect("project");
        let rendered = projected.to_string();
        assert!(!rendered.contains("sk-nested-secret-999"));
        assert!(!rendered.contains("visible-key"));
        assert!(rendered.contains("[REDACTED]"));
    }

    #[test]
    fn project_json_for_wire_scrubs_object_keys() {
        let redactor = SecretRedactor::default_runtime()
            .with_forbidden_env_values([("OPENAI_API_KEY", "sk-as-object-key")]);
        let value = serde_json::json!({
            "metadata": {
                "sk-as-object-key": "nested-ok",
                "safe": "sk-as-object-key"
            }
        });
        let projected = project_json_for_wire(&value, &redactor).expect("project");
        let rendered = projected.to_string();
        assert!(
            !rendered.contains("sk-as-object-key"),
            "secret must not survive as a JSON key or value: {rendered}"
        );
        assert!(rendered.contains("[REDACTED]"));
    }

    #[test]
    fn redact_scrubs_trailing_partial_literal_prefixes() {
        let secret = "sk-stream-partial-abcdef";
        let redactor = SecretRedactor::default_runtime()
            .with_forbidden_env_values([("OPENAI_API_KEY", secret)]);
        // Mid-stream omit (any length ≥1), including after alphanumeric text.
        assert_eq!(redactor.redact_streaming_text("prefixsk-st"), "prefix");
        // Full secret still marker-replaces.
        let full = redactor.redact_streaming_text(&format!("prefix{secret}"));
        assert!(!full.contains(secret));
        assert!(full.contains("[REDACTED]"));
        // Finalized path does not use streaming omit — `yes` / keys survive.
        assert_eq!(redactor.redact("yes"), "yes");
        assert_eq!(redactor.redact("status"), "status");
    }

    #[test]
    fn project_json_preserves_wire_field_names_with_sk_env_secret() {
        let redactor = SecretRedactor::default_runtime()
            .with_forbidden_env_values([("OPENAI_API_KEY", "sk-live-common-prefix")]);
        let value = serde_json::json!({
            "status": "idle",
            "plans": [],
            "transcript": [
                {"content": "yes", "kind": "assistant_message", "streaming": false},
                {"content": "hello sk-li", "kind": "assistant_message", "streaming": true}
            ]
        });
        let projected = project_json_for_wire(&value, &redactor).expect("project");
        assert!(projected.get("status").is_some(), "status key must survive");
        assert!(projected.get("plans").is_some(), "plans key must survive");
        assert_eq!(projected["transcript"][0]["content"], "yes");
        assert_eq!(projected["transcript"][1]["content"], "hello ");
    }

    #[test]
    fn redact_streaming_text_terminates_when_literal_starts_with_redacted_token() {
        let redactor = SecretRedactor::default_runtime()
            .with_forbidden_env_values([("OPENAI_API_KEY", "[REDACTED]x-extra")]);
        let scrubbed = redactor.redact_streaming_text("leak [REDACTED]x-extra here");
        assert!(scrubbed.contains("[REDACTED]"));
        assert!(!scrubbed.contains("x-extra"));
    }

    #[test]
    fn redact_streaming_text_masks_partial_secret_next_to_unrelated_marker() {
        let redactor = SecretRedactor::default_runtime()
            .with_forbidden_env_values([("OPENAI_API_KEY", "sk-live-secret-value")]);
        let scrubbed = redactor.redact_streaming_text("token: dummy sk-liv");
        assert!(
            !scrubbed.contains("sk-liv"),
            "partial env-file secret must be omitted even beside another marker: {scrubbed}"
        );
    }

    #[test]
    fn redact_marker_preserves_offsets_around_unicode_casefold_expanders() {
        let redactor = SecretRedactor::default_runtime();
        // U+0130 LATIN CAPITAL LETTER I WITH DOT ABOVE lowercases to two
        // bytes (`i` + combining dot) in Unicode casefold; ASCII-offset
        // search must still redact the full secret tail.
        let input = "İ api_key:secret-tail-xyz";
        let output = redactor.redact(input);
        assert!(
            !output.contains("secret-tail-xyz"),
            "unicode-prefixed marker must not leak secret bytes: {output}"
        );
        assert!(output.contains("[REDACTED]"));
    }

    #[test]
    fn project_json_for_wire_fails_closed_on_redacted_key_collision() {
        let redactor = SecretRedactor::default_runtime();
        let value = serde_json::json!({
            "api_key: first": 1,
            "api_key: second": 2
        });
        let err = project_json_for_wire(&value, &redactor).expect_err("collision");
        assert!(
            err.to_string().contains("collision") || err.to_string().contains("redact"),
            "expected collision failure, got {err}"
        );
    }

    /// `PrincipalContext::from_actor` must map every `ActorKind` variant
    /// to the right `PrincipalRole`. Human / Agent
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

    #[test]
    fn with_network_access_controls_requires_network_tools() {
        let denied =
            ToolBoundaryRuntime::system(Uuid::nil(), Arc::new(InMemoryAuditSink::default()));
        let network_op = ToolOperation::tool("web_search", false, true);
        assert!(
            !denied.decide(&network_op).allowed,
            "system default must deny requires_network tools"
        );
        let allowed = denied.with_network_access(true);
        assert!(
            allowed.decide(&network_op).allowed,
            "Allow network gate must permit requires_network tools"
        );
        let redenied = allowed.with_network_access(false);
        assert!(
            !redenied.decide(&network_op).allowed,
            "Deny network gate must reject requires_network tools again"
        );
    }
}
