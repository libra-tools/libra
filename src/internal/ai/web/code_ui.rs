//! Code UI projection server helpers for exposing AI thread state to the local web UI.
//!
//! Boundary: this file translates internal projection records into HTTP/websocket
//! views; it does not execute tools or mutate repository state. Projection resolver
//! tests cover missing threads, event ordering, and replayed snapshots.

use std::{
    collections::HashMap,
    sync::{
        Arc,
        atomic::{AtomicU64, Ordering},
    },
};

use anyhow::anyhow;
use async_trait::async_trait;
use chrono::{DateTime, Duration, Utc};
use serde::{Deserialize, Serialize};
use serde_json::json;
use tokio::sync::{Mutex, RwLock, broadcast};
use uuid::Uuid;

use crate::internal::ai::{
    projection::{PlanHeadRef, ThreadBundle},
    runtime::hardening::SecretRedactor,
};

const DEFAULT_BROWSER_CONTROLLER_LEASE_SECS: i64 = 120;

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Default)]
#[serde(rename_all = "snake_case")]
pub enum CodeUiSessionStatus {
    #[default]
    Idle,
    Thinking,
    ExecutingTool,
    AwaitingInteraction,
    Completed,
    Error,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Default)]
#[serde(rename_all = "camelCase")]
pub struct CodeUiCapabilities {
    pub message_input: bool,
    pub streaming_text: bool,
    pub plan_updates: bool,
    pub tool_calls: bool,
    pub patchsets: bool,
    pub interactive_approvals: bool,
    pub structured_questions: bool,
    pub provider_session_resume: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Default)]
#[serde(rename_all = "camelCase")]
pub struct CodeUiProviderInfo {
    pub provider: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub mode: Option<String>,
    #[serde(default)]
    pub managed: bool,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Default)]
#[serde(rename_all = "snake_case")]
pub enum CodeUiControllerKind {
    #[default]
    None,
    Browser,
    /// Local automation writer. Automation requires both the process-level
    /// `X-Libra-Control-Token` and the lease-level `X-Code-Controller-Token`;
    /// existing browser controllers keep using only the lease token for
    /// backward compatibility.
    Automation,
    Tui,
    Cli,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Default)]
#[serde(rename_all = "camelCase")]
pub struct CodeUiControllerState {
    pub kind: CodeUiControllerKind,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub owner_label: Option<String>,
    pub can_write: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub lease_expires_at: Option<DateTime<Utc>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
    pub loopback_only: bool,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Default)]
#[serde(rename_all = "snake_case")]
pub enum CodeUiTranscriptEntryKind {
    #[default]
    UserMessage,
    AssistantMessage,
    ToolCall,
    PlanSummary,
    Diff,
    InfoNote,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct CodeUiTranscriptEntry {
    pub id: String,
    pub kind: CodeUiTranscriptEntryKind,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub content: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub status: Option<String>,
    #[serde(default)]
    pub streaming: bool,
    #[serde(default = "default_metadata")]
    pub metadata: serde_json::Value,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Default)]
#[serde(rename_all = "snake_case")]
pub enum CodeUiInteractionKind {
    #[default]
    Approval,
    SandboxApproval,
    RequestUserInput,
    IntentReviewChoice,
    PostPlanChoice,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Default)]
#[serde(rename_all = "snake_case")]
pub enum CodeUiInteractionStatus {
    #[default]
    Pending,
    Resolved,
    Cancelled,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Default)]
#[serde(rename_all = "camelCase")]
pub struct CodeUiInteractionOption {
    pub id: String,
    pub label: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct CodeUiInteractionRequest {
    pub id: String,
    pub kind: CodeUiInteractionKind,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub prompt: Option<String>,
    #[serde(default)]
    pub options: Vec<CodeUiInteractionOption>,
    pub status: CodeUiInteractionStatus,
    #[serde(default = "default_metadata")]
    pub metadata: serde_json::Value,
    pub requested_at: DateTime<Utc>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub resolved_at: Option<DateTime<Utc>>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Default)]
#[serde(rename_all = "snake_case")]
pub enum CodeUiApplyToFuture {
    #[default]
    No,
    AcceptAll,
    DeclineAll,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct CodeUiInteractionResponse {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub approved: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub apply_to_future: Option<CodeUiApplyToFuture>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub selected_option: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub note: Option<String>,
    #[serde(default)]
    pub answers: HashMap<String, Vec<String>>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct CodeUiPlanStep {
    pub step: String,
    pub status: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct CodeUiPlanSnapshot {
    pub id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub summary: Option<String>,
    pub status: String,
    #[serde(default)]
    pub steps: Vec<CodeUiPlanStep>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct CodeUiTaskSnapshot {
    pub id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    pub status: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub details: Option<String>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct CodeUiToolCallSnapshot {
    pub id: String,
    pub tool_name: String,
    pub status: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub summary: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub details: Option<String>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct CodeUiPatchChange {
    pub path: String,
    pub change_type: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub diff: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct CodeUiPatchsetSnapshot {
    pub id: String,
    pub status: String,
    #[serde(default)]
    pub changes: Vec<CodeUiPatchChange>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CodeUiSessionSnapshot {
    pub session_id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub thread_id: Option<String>,
    pub working_dir: String,
    pub provider: CodeUiProviderInfo,
    pub capabilities: CodeUiCapabilities,
    pub controller: CodeUiControllerState,
    pub status: CodeUiSessionStatus,
    pub transcript: Vec<CodeUiTranscriptEntry>,
    pub plans: Vec<CodeUiPlanSnapshot>,
    pub tasks: Vec<CodeUiTaskSnapshot>,
    pub tool_calls: Vec<CodeUiToolCallSnapshot>,
    pub patchsets: Vec<CodeUiPatchsetSnapshot>,
    pub interactions: Vec<CodeUiInteractionRequest>,
    pub updated_at: DateTime<Utc>,
}

impl Default for CodeUiSessionSnapshot {
    fn default() -> Self {
        Self {
            session_id: Uuid::new_v4().to_string(),
            thread_id: None,
            working_dir: String::new(),
            provider: CodeUiProviderInfo::default(),
            capabilities: CodeUiCapabilities::default(),
            controller: CodeUiControllerState::default(),
            status: CodeUiSessionStatus::Idle,
            transcript: Vec::new(),
            plans: Vec::new(),
            tasks: Vec::new(),
            tool_calls: Vec::new(),
            patchsets: Vec::new(),
            interactions: Vec::new(),
            updated_at: Utc::now(),
        }
    }
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Default)]
#[serde(rename_all = "snake_case")]
pub enum CodeUiEventType {
    #[default]
    SessionUpdated,
    StatusChanged,
    ControllerChanged,
}

impl CodeUiEventType {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::SessionUpdated => "session_updated",
            Self::StatusChanged => "status_changed",
            Self::ControllerChanged => "controller_changed",
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CodeUiEventEnvelope {
    pub seq: u64,
    #[serde(rename = "type")]
    pub event_type: CodeUiEventType,
    pub at: DateTime<Utc>,
    pub data: CodeUiSessionSnapshot,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CodeUiControllerAttachRequest {
    pub client_id: String,
    #[serde(default = "default_controller_attach_kind")]
    pub kind: CodeUiControllerKind,
}

fn default_controller_attach_kind() -> CodeUiControllerKind {
    CodeUiControllerKind::Browser
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CodeUiControllerAttachResponse {
    pub controller_token: String,
    pub lease_expires_at: DateTime<Utc>,
    pub controller: CodeUiControllerState,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CodeUiControllerDetachRequest {
    pub client_id: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CodeUiMessageRequest {
    pub text: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CodeUiAckResponse {
    pub accepted: bool,
}

/// `POST /api/code/task/dispatch` body. This is the Code Control
/// equivalent of `/task <agent> <prompt>` and enters the dispatcher as
/// `UserInitiated { bypass_permission_ask: true }`.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CodeUiTaskDispatchRequest {
    pub agent: String,
    pub prompt: String,
}

/// `POST /api/code/goal/start` body. The objective is validated
/// at the App layer against the same `GoalSpec::new` shape rules
/// (non-empty after trim, ≤ MAX_OBJECTIVE_LEN bytes); the wire
/// shape itself is permissive so the validator's error messages
/// surface verbatim through the response.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CodeUiGoalStartRequest {
    pub objective: String,
}

/// `POST /api/code/goal/cancel` body. The reason flows into the
/// `GoalEvent::Cancelled` envelope's audit-log payload.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CodeUiGoalCancelRequest {
    pub reason: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Default)]
#[serde(rename_all = "camelCase")]
pub struct CodeUiDiagnosticsPorts {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub web: Option<u16>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub mcp: Option<u16>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct CodeUiDiagnostics {
    pub pid: u32,
    pub provider: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub thread_id: Option<String>,
    pub status: CodeUiSessionStatus,
    pub controller: CodeUiControllerState,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub ports: Option<CodeUiDiagnosticsPorts>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub log_file: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub active_interaction_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub last_error: Option<String>,
}

impl CodeUiDiagnostics {
    /// Wave 7 / PR 7 — exposed `pub(crate)` so the
    /// `code_diagnostics_handler` in `mod.rs` can apply it before
    /// serialising the response. Internal-only — automation
    /// clients never construct this themselves.
    pub(crate) fn redact(mut self, redactor: &SecretRedactor) -> Self {
        redact_string(&mut self.provider, redactor);
        redact_option_string(&mut self.model, redactor);
        redact_option_string(&mut self.thread_id, redactor);
        redact_option_string(&mut self.controller.owner_label, redactor);
        redact_option_string(&mut self.controller.reason, redactor);
        redact_option_string(&mut self.log_file, redactor);
        redact_option_string(&mut self.active_interaction_id, redactor);
        redact_option_string(&mut self.last_error, redactor);
        self
    }
}

fn redact_string(value: &mut String, redactor: &SecretRedactor) {
    let redacted = redactor.redact(value.as_str());
    *value = redacted;
}

fn redact_option_string(value: &mut Option<String>, redactor: &SecretRedactor) {
    if let Some(value) = value.as_mut() {
        redact_string(value, redactor);
    }
}

fn default_metadata() -> serde_json::Value {
    json!({})
}

#[derive(Debug)]
pub struct CodeUiSession {
    snapshot: RwLock<CodeUiSessionSnapshot>,
    tx: broadcast::Sender<CodeUiEventEnvelope>,
    next_seq: AtomicU64,
}

impl CodeUiSession {
    pub fn new(snapshot: CodeUiSessionSnapshot) -> Arc<Self> {
        let (tx, _) = broadcast::channel(256);
        Arc::new(Self {
            snapshot: RwLock::new(snapshot),
            tx,
            next_seq: AtomicU64::new(1),
        })
    }

    pub async fn snapshot(&self) -> CodeUiSessionSnapshot {
        self.snapshot.read().await.clone()
    }

    pub fn subscribe(&self) -> broadcast::Receiver<CodeUiEventEnvelope> {
        self.tx.subscribe()
    }

    pub async fn mutate<F>(&self, event_type: CodeUiEventType, f: F)
    where
        F: FnOnce(&mut CodeUiSessionSnapshot),
    {
        let snapshot = {
            let mut snapshot = self.snapshot.write().await;
            f(&mut snapshot);
            snapshot.updated_at = Utc::now();
            snapshot.clone()
        };
        self.broadcast_snapshot(event_type, &snapshot);
    }

    pub async fn replace_snapshot(
        &self,
        event_type: CodeUiEventType,
        snapshot: CodeUiSessionSnapshot,
    ) {
        {
            let mut current = self.snapshot.write().await;
            *current = snapshot;
        }
        let snapshot = self.snapshot().await;
        self.broadcast_snapshot(event_type, &snapshot);
    }

    pub async fn set_controller_state(&self, controller: CodeUiControllerState) {
        self.mutate(CodeUiEventType::ControllerChanged, |snapshot| {
            snapshot.controller = controller;
        })
        .await;
    }

    pub async fn set_status(&self, status: CodeUiSessionStatus) {
        self.mutate(CodeUiEventType::StatusChanged, |snapshot| {
            snapshot.status = status;
        })
        .await;
    }

    pub async fn cancel_active_turn(&self, message: impl Into<String>) {
        let message = message.into();
        self.mutate(CodeUiEventType::SessionUpdated, move |snapshot| {
            let now = Utc::now();
            snapshot.status = CodeUiSessionStatus::Idle;
            for tool_call in &mut snapshot.tool_calls {
                if matches!(tool_call.status.as_str(), "preview" | "running") {
                    tool_call.status = "failed".to_string();
                    tool_call.details = Some(message.clone());
                    tool_call.updated_at = now;
                }
            }
            for entry in &mut snapshot.transcript {
                match entry.kind {
                    CodeUiTranscriptEntryKind::AssistantMessage if entry.streaming => {
                        entry.content = Some(message.clone());
                        entry.status = Some("cancelled".to_string());
                        entry.streaming = false;
                        entry.updated_at = now;
                    }
                    CodeUiTranscriptEntryKind::ToolCall
                        if matches!(entry.status.as_deref(), Some("preview" | "running")) =>
                    {
                        entry.content = Some(message.clone());
                        entry.status = Some("failed".to_string());
                        entry.streaming = false;
                        entry.updated_at = now;
                    }
                    _ => {}
                }
            }
        })
        .await;
    }

    pub async fn upsert_transcript_entry(&self, entry: CodeUiTranscriptEntry) {
        self.mutate(CodeUiEventType::SessionUpdated, |snapshot| {
            upsert_by_id(&mut snapshot.transcript, entry, |item| item.id.as_str());
        })
        .await;
    }

    pub async fn append_assistant_delta(&self, entry_id: &str, delta: &str) {
        self.mutate(CodeUiEventType::SessionUpdated, |snapshot| {
            if let Some(entry) = snapshot
                .transcript
                .iter_mut()
                .find(|item| item.id == entry_id)
            {
                // Skip late-arriving deltas for entries that have already been
                // finalized (e.g. by `cancel_turn` flipping the status to
                // `cancelled`). Re-flagging a settled entry as `streaming`
                // would resurrect the perpetual typing indicator we just
                // cleared. The TUI flow uses live statuses like `thinking`
                // alongside `streaming: true` while the agent is still
                // producing output, so we only short-circuit on terminal
                // statuses (`completed`, `error`, `cancelled`).
                if let Some(status) = entry.status.as_deref()
                    && matches!(status, "completed" | "error" | "cancelled")
                {
                    return;
                }
                let content = entry.content.get_or_insert_with(String::new);
                content.push_str(delta);
                entry.streaming = true;
                entry.updated_at = Utc::now();
            }
        })
        .await;
    }

    pub async fn upsert_interaction(&self, request: CodeUiInteractionRequest) {
        self.mutate(CodeUiEventType::SessionUpdated, |snapshot| {
            upsert_by_id(&mut snapshot.interactions, request, |item| item.id.as_str());
        })
        .await;
    }

    pub async fn resolve_interaction(&self, interaction_id: &str) {
        let interaction_id = interaction_id.to_string();
        self.mutate(CodeUiEventType::SessionUpdated, |snapshot| {
            if let Some(interaction) = snapshot
                .interactions
                .iter_mut()
                .find(|item| item.id == interaction_id)
            {
                interaction.status = CodeUiInteractionStatus::Resolved;
                interaction.resolved_at = Some(Utc::now());
            }
        })
        .await;
    }

    pub async fn clear_interaction(&self, interaction_id: &str) {
        let interaction_id = interaction_id.to_string();
        self.mutate(CodeUiEventType::SessionUpdated, |snapshot| {
            snapshot
                .interactions
                .retain(|interaction| interaction.id != interaction_id);
        })
        .await;
    }

    pub async fn upsert_plan(&self, plan: CodeUiPlanSnapshot) {
        self.mutate(CodeUiEventType::SessionUpdated, |snapshot| {
            // Monotonic plan status: a plan that has already reached a terminal
            // status ("completed"/"failed") must not be regressed to a
            // non-terminal one ("running"/"pending") by a late-arriving event.
            // `on_tool_call_start` (running) and `on_tool_call_end` (completed)
            // each `tokio::spawn` an independent task with no ordering
            // guarantee; without this guard a late "running" upsert clobbers an
            // already-"completed" plan and strands it (the source of a flaky
            // "plan stuck at running" projection).
            if let Some(existing) = snapshot.plans.iter().find(|item| item.id == plan.id)
                && is_terminal_plan_status(&existing.status)
                && !is_terminal_plan_status(&plan.status)
            {
                return;
            }
            upsert_by_id(&mut snapshot.plans, plan, |item| item.id.as_str());
        })
        .await;
    }

    pub async fn upsert_task(&self, task: CodeUiTaskSnapshot) {
        self.mutate(CodeUiEventType::SessionUpdated, |snapshot| {
            upsert_by_id(&mut snapshot.tasks, task, |item| item.id.as_str());
        })
        .await;
    }

    pub async fn upsert_tool_call(&self, tool_call: CodeUiToolCallSnapshot) {
        self.mutate(CodeUiEventType::SessionUpdated, |snapshot| {
            upsert_by_id(&mut snapshot.tool_calls, tool_call, |item| item.id.as_str());
        })
        .await;
    }

    pub async fn upsert_patchset(&self, patchset: CodeUiPatchsetSnapshot) {
        self.mutate(CodeUiEventType::SessionUpdated, |snapshot| {
            upsert_by_id(&mut snapshot.patchsets, patchset, |item| item.id.as_str());
        })
        .await;
    }

    pub async fn emit_current_snapshot(&self, event_type: CodeUiEventType) {
        let snapshot = self.snapshot().await;
        self.broadcast_snapshot(event_type, &snapshot);
    }

    fn broadcast_snapshot(&self, event_type: CodeUiEventType, snapshot: &CodeUiSessionSnapshot) {
        let seq = self.next_seq.fetch_add(1, Ordering::Relaxed);
        let event = CodeUiEventEnvelope {
            seq,
            event_type,
            at: Utc::now(),
            data: snapshot.clone(),
        };
        let _ = self.tx.send(event);
    }
}

fn upsert_by_id<T, F>(items: &mut Vec<T>, incoming: T, id_fn: F)
where
    F: Fn(&T) -> &str,
{
    let incoming_id = id_fn(&incoming).to_string();
    if let Some(existing) = items.iter_mut().find(|item| id_fn(item) == incoming_id) {
        *existing = incoming;
    } else {
        items.push(incoming);
    }
}

/// A plan status is terminal once the tool call that produced it has ended.
/// Used to keep [`CodeUiSession::upsert_plan`] monotonic so a late-arriving
/// non-terminal upsert cannot regress a finished plan.
fn is_terminal_plan_status(status: &str) -> bool {
    matches!(status, "completed" | "failed")
}

#[async_trait]
pub trait CodeUiReadModel: Send + Sync {
    fn session(&self) -> Arc<CodeUiSession>;

    async fn snapshot(&self) -> CodeUiSessionSnapshot {
        self.session().snapshot().await
    }

    fn subscribe(&self) -> broadcast::Receiver<CodeUiEventEnvelope> {
        self.session().subscribe()
    }
}

#[async_trait]
pub trait CodeUiCommandAdapter: Send + Sync {
    fn capabilities(&self) -> CodeUiCapabilities;

    async fn submit_message(&self, text: String) -> anyhow::Result<()>;

    async fn respond_interaction(
        &self,
        interaction_id: &str,
        response: CodeUiInteractionResponse,
    ) -> anyhow::Result<()>;

    async fn cancel_turn(&self) -> anyhow::Result<()> {
        Err(anyhow!(
            "This libra code session does not support turn cancel"
        ))
    }

    /// `task.dispatch` — explicitly run a sub-agent from automation.
    /// Default implementation returns "not supported" for adapters
    /// that do not expose the local TUI sub-agent runtime.
    async fn task_dispatch(&self, _agent: String, _prompt: String) -> anyhow::Result<String> {
        Err(anyhow!(
            "This libra code session does not support task.dispatch"
        ))
    }

    /// `goal.start` — create an active Goal in this session
    /// (OC-Phase 6 P6.6). Returns the rendered status of the new
    /// Goal so callers can echo it without a follow-up
    /// `goal.status`. Default implementation returns "not
    /// supported" so non-TUI adapters (headless, web-only Codex)
    /// don't have to opt in until they grow Goal mode support.
    async fn goal_start(&self, _objective: String) -> anyhow::Result<String> {
        Err(anyhow!(
            "This libra code session does not support Goal mode"
        ))
    }

    /// `goal.status` — render the active Goal's snapshot, or an
    /// error if none. Default implementation returns "not
    /// supported".
    async fn goal_status(&self) -> anyhow::Result<String> {
        Err(anyhow!(
            "This libra code session does not support Goal mode"
        ))
    }

    /// `goal.cancel` — explicit user-driven cancellation of the
    /// active Goal. Returns the rendered status post-cancel.
    /// Default implementation returns "not supported".
    async fn goal_cancel(&self, _reason: String) -> anyhow::Result<String> {
        Err(anyhow!(
            "This libra code session does not support Goal mode"
        ))
    }

    async fn shutdown(&self) -> anyhow::Result<()> {
        Ok(())
    }
}

pub trait CodeUiProviderAdapter: CodeUiReadModel + CodeUiCommandAdapter {}

impl<T> CodeUiProviderAdapter for T where T: CodeUiReadModel + CodeUiCommandAdapter {}

#[derive(Debug, Clone)]
pub enum CodeUiInitialController {
    Unclaimed,
    Fixed {
        kind: CodeUiControllerKind,
        owner_label: String,
        reason: Option<String>,
    },
    LocalTui {
        owner_label: String,
        reason: Option<String>,
    },
}

#[derive(Debug)]
struct FixedController {
    kind: CodeUiControllerKind,
    owner_label: String,
    reason: Option<String>,
}

#[derive(Debug, Clone)]
pub struct ControllerLease {
    pub kind: CodeUiControllerKind,
    pub client_id: String,
    pub token: String,
    pub expires_at: DateTime<Utc>,
}

#[derive(Debug)]
struct CodeUiControllerRuntimeState {
    fixed: Option<FixedController>,
    local_tui_owner: Option<FixedController>,
    active_lease: Option<ControllerLease>,
}

#[derive(Clone)]
pub struct CodeUiRuntimeHandle {
    adapter: Arc<dyn CodeUiProviderAdapter>,
    browser_write_enabled: bool,
    automation_write_enabled: bool,
    controller_state: Arc<Mutex<CodeUiControllerRuntimeState>>,
    controller_lease_duration: Duration,
}

/// Bag of constructor options for [`CodeUiRuntimeHandle::build_with_options`].
///
/// Existing call sites continue to use [`CodeUiRuntimeHandle::build`] /
/// [`CodeUiRuntimeHandle::build_with_control`] with the default 120 s lease
/// TTL. Tests that need to exercise lease expiry without sleeping for two
/// minutes pass a custom `lease_duration` through this struct.
#[derive(Debug, Clone)]
pub struct CodeUiRuntimeOptions {
    pub browser_write_enabled: bool,
    pub automation_write_enabled: bool,
    pub initial_controller: CodeUiInitialController,
    /// Override for the controller-lease TTL. `None` keeps the production
    /// default (`DEFAULT_BROWSER_CONTROLLER_LEASE_SECS` = 120 s). Only set
    /// from `cfg(feature = "test-provider")` paths.
    pub lease_duration: Option<Duration>,
}

impl CodeUiRuntimeOptions {
    pub fn new(
        browser_write_enabled: bool,
        automation_write_enabled: bool,
        initial_controller: CodeUiInitialController,
    ) -> Self {
        Self {
            browser_write_enabled,
            automation_write_enabled,
            initial_controller,
            lease_duration: None,
        }
    }
}

/// Test-only override for the controller-lease TTL.
///
/// Production builds always return `Ok(None)` so the runtime keeps the
/// default 120 s lease. Under `cfg(feature = "test-provider")`, the helper
/// reads `LIBRA_CODE_LEASE_DURATION_MS` from the environment and rejects
/// bogus inputs (zero, negative, non-integer) so a typo'd test fixture
/// fails loudly at session spawn instead of silently keeping the
/// production default.
///
/// The error type is `String` so callers in both `CliResult` and
/// `anyhow::Result` flows can wrap it — neither dependency is brought in
/// by `code_ui.rs`.
pub fn test_lease_duration_override() -> Result<Option<Duration>, String> {
    #[cfg(feature = "test-provider")]
    {
        let raw = match std::env::var("LIBRA_CODE_LEASE_DURATION_MS") {
            Ok(value) => value,
            Err(_) => return Ok(None),
        };
        let trimmed = raw.trim();
        if trimmed.is_empty() {
            return Ok(None);
        }
        let millis: i64 = trimmed.parse().map_err(|_| {
            format!(
                "LIBRA_CODE_LEASE_DURATION_MS must be a positive integer in milliseconds (got '{raw}')",
            )
        })?;
        if millis <= 0 {
            return Err(format!(
                "LIBRA_CODE_LEASE_DURATION_MS must be greater than zero (got '{raw}')",
            ));
        }
        Ok(Some(Duration::milliseconds(millis)))
    }
    #[cfg(not(feature = "test-provider"))]
    {
        Ok(None)
    }
}

impl CodeUiRuntimeHandle {
    pub async fn build(
        adapter: Arc<dyn CodeUiProviderAdapter>,
        browser_write_enabled: bool,
        initial_controller: CodeUiInitialController,
    ) -> Arc<Self> {
        Self::build_with_control(adapter, browser_write_enabled, false, initial_controller).await
    }

    pub async fn build_with_control(
        adapter: Arc<dyn CodeUiProviderAdapter>,
        browser_write_enabled: bool,
        automation_write_enabled: bool,
        initial_controller: CodeUiInitialController,
    ) -> Arc<Self> {
        Self::build_with_options(
            adapter,
            CodeUiRuntimeOptions::new(
                browser_write_enabled,
                automation_write_enabled,
                initial_controller,
            ),
        )
        .await
    }

    pub async fn build_with_options(
        adapter: Arc<dyn CodeUiProviderAdapter>,
        options: CodeUiRuntimeOptions,
    ) -> Arc<Self> {
        let (fixed, local_tui_owner) = match options.initial_controller {
            CodeUiInitialController::Unclaimed => (None, None),
            CodeUiInitialController::Fixed {
                kind,
                owner_label,
                reason,
            } => (
                Some(FixedController {
                    kind,
                    owner_label,
                    reason,
                }),
                None,
            ),
            CodeUiInitialController::LocalTui {
                owner_label,
                reason,
            } => (
                None,
                Some(FixedController {
                    kind: CodeUiControllerKind::Tui,
                    owner_label,
                    reason,
                }),
            ),
        };

        let handle = Arc::new(Self {
            adapter,
            browser_write_enabled: options.browser_write_enabled,
            automation_write_enabled: options.automation_write_enabled,
            controller_state: Arc::new(Mutex::new(CodeUiControllerRuntimeState {
                fixed,
                local_tui_owner,
                active_lease: None,
            })),
            controller_lease_duration: options
                .lease_duration
                .unwrap_or_else(|| Duration::seconds(DEFAULT_BROWSER_CONTROLLER_LEASE_SECS)),
        });
        handle.sync_controller_snapshot().await;
        handle
    }

    pub fn adapter(&self) -> Arc<dyn CodeUiProviderAdapter> {
        self.adapter.clone()
    }

    pub async fn snapshot(&self) -> CodeUiSessionSnapshot {
        self.adapter.snapshot().await
    }

    pub async fn diagnostics(&self) -> CodeUiDiagnostics {
        self.sync_controller_snapshot().await;
        let snapshot = self.snapshot().await;
        let redactor = SecretRedactor::default_runtime();
        CodeUiDiagnostics {
            pid: std::process::id(),
            provider: snapshot.provider.provider,
            model: snapshot.provider.model,
            thread_id: snapshot.thread_id,
            status: snapshot.status,
            controller: snapshot.controller,
            ports: None,
            log_file: std::env::var("LIBRA_LOG_FILE")
                .ok()
                .filter(|value| !value.trim().is_empty()),
            active_interaction_id: snapshot
                .interactions
                .iter()
                .find(|interaction| interaction.status == CodeUiInteractionStatus::Pending)
                .map(|interaction| interaction.id.clone()),
            last_error: None,
        }
        .redact(&redactor)
    }

    pub fn subscribe(&self) -> broadcast::Receiver<CodeUiEventEnvelope> {
        self.adapter.subscribe()
    }

    pub async fn attach_browser_controller(
        &self,
        client_id: &str,
    ) -> Result<CodeUiControllerAttachResponse, CodeUiApiError> {
        self.attach_controller(CodeUiControllerKind::Browser, client_id)
            .await
    }

    /// Request a controller lease.
    ///
    /// `kind` may be `Browser` or `Automation`. `Automation` requires
    /// `automation_write_enabled` to be true (i.e. `--control write`).
    ///
    /// Errors:
    /// - `BROWSER_CONTROL_DISABLED` / `CONTROL_DISABLED` when the kind is not enabled.
    /// - `CONTROLLER_CONFLICT` when another client already holds an active lease.
    /// - `INVALID_CONTROLLER_KIND` for `None`, `Tui`, or `Cli`.
    ///
    /// The lease TTL defaults to `DEFAULT_BROWSER_CONTROLLER_LEASE_SECS` (120s).
    /// Renew by calling again with the same `client_id`.
    pub async fn attach_controller(
        &self,
        kind: CodeUiControllerKind,
        client_id: &str,
    ) -> Result<CodeUiControllerAttachResponse, CodeUiApiError> {
        match kind {
            CodeUiControllerKind::Browser if !self.browser_write_enabled => {
                return Err(CodeUiApiError::forbidden(
                    "BROWSER_CONTROL_DISABLED",
                    "Browser control is disabled for this code session",
                ));
            }
            CodeUiControllerKind::Automation if !self.automation_write_enabled => {
                return Err(CodeUiApiError::forbidden(
                    "CONTROL_DISABLED",
                    "Local TUI automation write control is not enabled; start with --control write",
                ));
            }
            CodeUiControllerKind::Browser | CodeUiControllerKind::Automation => {}
            _ => {
                return Err(CodeUiApiError::bad_request(
                    "INVALID_CONTROLLER_KIND",
                    format!("Controller kind '{}' cannot attach", kind.as_str()),
                ));
            }
        }

        let mut state = self.controller_state.lock().await;
        if let Some(fixed) = state.fixed.as_ref() {
            return Err(CodeUiApiError::conflict(
                "CONTROLLER_CONFLICT",
                format!(
                    "The active controller is {} ({})",
                    fixed.kind.as_str(),
                    fixed.owner_label
                ),
            ));
        }

        let now = Utc::now();
        if state
            .active_lease
            .as_ref()
            .is_some_and(|lease| lease.expires_at <= now)
        {
            state.active_lease = None;
        }

        let lease = if let Some(existing) = state.active_lease.as_mut() {
            if existing.client_id != client_id || existing.kind != kind {
                return Err(CodeUiApiError::conflict(
                    "CONTROLLER_CONFLICT",
                    format!(
                        "Another {} currently controls this session",
                        existing.kind.as_str()
                    ),
                ));
            }
            existing.expires_at = now + self.controller_lease_duration;
            existing.clone()
        } else {
            let lease = ControllerLease {
                kind,
                client_id: client_id.to_string(),
                token: Uuid::new_v4().to_string(),
                expires_at: now + self.controller_lease_duration,
            };
            state.active_lease = Some(lease.clone());
            lease
        };
        drop(state);

        self.sync_controller_snapshot().await;

        Ok(CodeUiControllerAttachResponse {
            controller_token: lease.token,
            lease_expires_at: lease.expires_at,
            controller: self.current_controller_state().await,
        })
    }

    pub async fn detach_browser_controller(
        &self,
        client_id: &str,
        token: &str,
    ) -> Result<(), CodeUiApiError> {
        self.detach_controller(CodeUiControllerKind::Browser, client_id, token, false)
            .await
    }

    /// Release an active controller lease.
    ///
    /// `force` is reserved for local TUI reclaim (e.g. `/control reclaim`).
    /// When `force` is `false`, both `client_id` and `token` must match the
    /// active lease. HTTP handlers should not expose `force` to remote clients.
    ///
    /// Thin wrappers (`detach_browser_controller`) hard-code `kind` and `force`
    /// to preserve backward compatibility for existing browser callers.
    pub async fn detach_controller(
        &self,
        kind: CodeUiControllerKind,
        client_id: &str,
        token: &str,
        force: bool,
    ) -> Result<(), CodeUiApiError> {
        let mut state = self.controller_state.lock().await;
        let Some(existing) = state.active_lease.as_ref() else {
            return Ok(());
        };
        if existing.kind != kind {
            return Ok(());
        }
        if !force && (existing.client_id != client_id || existing.token != token) {
            return Err(CodeUiApiError::forbidden(
                "INVALID_CONTROLLER_TOKEN",
                "The controller token does not match the active controller",
            ));
        }
        state.active_lease = None;
        drop(state);
        self.sync_controller_snapshot().await;
        Ok(())
    }

    pub async fn submit_message(
        &self,
        token: Option<&str>,
        text: String,
    ) -> Result<(), CodeUiApiError> {
        self.ensure_controller_write_access(token).await?;
        self.adapter
            .submit_message(text)
            .await
            .map_err(CodeUiApiError::unsupported_from_error)
    }

    pub async fn respond_interaction(
        &self,
        token: Option<&str>,
        interaction_id: &str,
        response: CodeUiInteractionResponse,
    ) -> Result<(), CodeUiApiError> {
        self.ensure_controller_write_access(token).await?;
        self.adapter
            .respond_interaction(interaction_id, response)
            .await
            .map_err(CodeUiApiError::unsupported_from_error)
    }

    pub async fn cancel_turn(&self, token: Option<&str>) -> Result<(), CodeUiApiError> {
        self.ensure_controller_write_access(token).await?;
        self.adapter
            .cancel_turn()
            .await
            .map_err(CodeUiApiError::unsupported_from_error)
    }

    /// `task.dispatch { agent, prompt }` — user-initiated sub-agent
    /// dispatch. Requires controller write-access because it mutates
    /// the session transcript and may run tools.
    pub async fn task_dispatch(
        &self,
        token: Option<&str>,
        agent: String,
        prompt: String,
    ) -> Result<String, CodeUiApiError> {
        self.ensure_controller_write_access(token).await?;
        self.adapter
            .task_dispatch(agent, prompt)
            .await
            .map_err(CodeUiApiError::unsupported_from_error)
    }

    /// `goal.start { objective }` — open an active Goal in this
    /// session. Requires controller write-access (a controller
    /// token validated against the active lease) because creating
    /// a Goal is a session-mutating operation. Returns the freshly
    /// rendered status string so callers don't need a follow-up
    /// `goal.status` (OC-Phase 6 P6.6).
    pub async fn goal_start(
        &self,
        token: Option<&str>,
        objective: String,
    ) -> Result<String, CodeUiApiError> {
        self.ensure_controller_write_access(token).await?;
        self.adapter
            .goal_start(objective)
            .await
            .map_err(CodeUiApiError::unsupported_from_error)
    }

    /// `goal.status` — return the active Goal's rendered snapshot.
    /// **Read-only**, so no controller token is required at this
    /// layer; the HTTP handler still loopback-gates the request.
    pub async fn goal_status(&self) -> Result<String, CodeUiApiError> {
        self.adapter
            .goal_status()
            .await
            .map_err(CodeUiApiError::unsupported_from_error)
    }

    /// `goal.cancel { reason }` — explicit cancellation of the
    /// active Goal. Requires controller write-access; mirrors
    /// `cancel_turn` in shape and audit policy.
    pub async fn goal_cancel(
        &self,
        token: Option<&str>,
        reason: String,
    ) -> Result<String, CodeUiApiError> {
        self.ensure_controller_write_access(token).await?;
        self.adapter
            .goal_cancel(reason)
            .await
            .map_err(CodeUiApiError::unsupported_from_error)
    }

    pub async fn shutdown(&self) -> anyhow::Result<()> {
        self.adapter.shutdown().await
    }

    /// Validate a controller token and return the active lease.
    ///
    /// Checks that the token is present, non-empty, matches the active lease,
    /// and that the lease has not expired. Expired leases are cleared on check.
    ///
    /// Errors:
    /// - `MISSING_CONTROLLER_TOKEN` when `token` is missing or empty.
    /// - `CONTROLLER_CONFLICT` when no lease is active.
    /// - `INVALID_CONTROLLER_TOKEN` when the token does not match the active lease.
    ///
    /// Thin wrappers (`ensure_browser_write_access`) hard-code the kind check
    /// for backward compatibility.
    pub async fn ensure_controller_write_access(
        &self,
        token: Option<&str>,
    ) -> Result<ControllerLease, CodeUiApiError> {
        let Some(token) = token.filter(|token| !token.trim().is_empty()) else {
            return Err(CodeUiApiError::forbidden(
                "MISSING_CONTROLLER_TOKEN",
                "A controller token is required for write operations",
            ));
        };

        let mut should_sync_after_error = false;
        let lease = {
            let mut state = self.controller_state.lock().await;
            let now = Utc::now();
            if state
                .active_lease
                .as_ref()
                .is_some_and(|lease| lease.expires_at <= now)
            {
                state.active_lease = None;
                should_sync_after_error = true;
            }

            let Some(lease) = state.active_lease.as_mut() else {
                drop(state);
                if should_sync_after_error {
                    self.sync_controller_snapshot().await;
                }
                return Err(CodeUiApiError::conflict(
                    "CONTROLLER_CONFLICT",
                    "No client currently controls this session",
                ));
            };
            if lease.token != token {
                return Err(CodeUiApiError::forbidden(
                    "INVALID_CONTROLLER_TOKEN",
                    "The controller token does not match the active controller",
                ));
            }
            lease.expires_at = now + self.controller_lease_duration;
            lease.clone()
        };
        self.sync_controller_snapshot().await;
        Ok(lease)
    }

    pub async fn reclaim_local_tui_controller(&self) -> Result<(), CodeUiApiError> {
        let mut state = self.controller_state.lock().await;
        if state.local_tui_owner.is_none() {
            return Err(CodeUiApiError::conflict(
                "CONTROLLER_CONFLICT",
                "This session does not have a local TUI controller to reclaim",
            ));
        }
        state.active_lease = None;
        drop(state);
        self.sync_controller_snapshot().await;
        Ok(())
    }

    async fn current_controller_state(&self) -> CodeUiControllerState {
        let mut state = self.controller_state.lock().await;
        let now = Utc::now();
        if state
            .active_lease
            .as_ref()
            .is_some_and(|lease| lease.expires_at <= now)
        {
            state.active_lease = None;
        }

        if let Some(lease) = state.active_lease.as_ref() {
            return CodeUiControllerState {
                kind: lease.kind,
                owner_label: Some(lease.client_id.clone()),
                can_write: true,
                lease_expires_at: Some(lease.expires_at),
                reason: None,
                loopback_only: true,
            };
        }

        if let Some(local) = state.local_tui_owner.as_ref() {
            return CodeUiControllerState {
                kind: local.kind,
                owner_label: Some(local.owner_label.clone()),
                can_write: false,
                lease_expires_at: None,
                reason: local.reason.clone(),
                loopback_only: true,
            };
        }

        if let Some(fixed) = state.fixed.as_ref() {
            return CodeUiControllerState {
                kind: fixed.kind,
                owner_label: Some(fixed.owner_label.clone()),
                can_write: false,
                lease_expires_at: None,
                reason: fixed.reason.clone(),
                loopback_only: true,
            };
        }

        CodeUiControllerState {
            kind: CodeUiControllerKind::None,
            owner_label: None,
            can_write: false,
            lease_expires_at: None,
            reason: if self.browser_write_enabled {
                Some("No controller attached".to_string())
            } else {
                Some("Browser control is disabled".to_string())
            },
            loopback_only: true,
        }
    }

    async fn sync_controller_snapshot(&self) {
        let controller = self.current_controller_state().await;
        self.adapter
            .session()
            .set_controller_state(controller)
            .await;
    }
}

#[derive(Debug, Clone)]
pub struct CodeUiApiError {
    pub status: u16,
    pub code: String,
    pub message: String,
}

impl CodeUiApiError {
    pub fn not_found(code: impl Into<String>, message: impl Into<String>) -> Self {
        Self {
            status: 404,
            code: code.into(),
            message: message.into(),
        }
    }

    pub fn conflict(code: impl Into<String>, message: impl Into<String>) -> Self {
        Self {
            status: 409,
            code: code.into(),
            message: message.into(),
        }
    }

    pub fn forbidden(code: impl Into<String>, message: impl Into<String>) -> Self {
        Self {
            status: 403,
            code: code.into(),
            message: message.into(),
        }
    }

    pub fn bad_request(code: impl Into<String>, message: impl Into<String>) -> Self {
        Self {
            status: 400,
            code: code.into(),
            message: message.into(),
        }
    }

    pub fn unsupported_from_error(error: anyhow::Error) -> Self {
        if let Some(control_error) =
            error.downcast_ref::<crate::internal::tui::control::TuiControlError>()
        {
            return Self {
                status: control_error.status(),
                code: control_error.code().to_string(),
                message: control_error.message(),
            };
        }

        Self {
            status: 422,
            code: "UNSUPPORTED_OPERATION".to_string(),
            message: error.to_string(),
        }
    }

    pub fn unavailable() -> Self {
        Self::not_found(
            "CODE_UI_UNAVAILABLE",
            "No active libra code session is available",
        )
    }
}

/// Wave 2 / PR 2 — single source-of-truth catalogue of every
/// Code UI error code the API exposes, paired with the HTTP status
/// it MUST resolve to. Per `docs/development/commands/_general.md` §5.20, this
/// list is enforced by `code_ui_error_code_contract*` in `tests`
/// below: any new error code added by a constructor OR emitted by
/// a route handler as an inline `WebApiError {…}` literal must be
/// appended here, otherwise the test fails. The list is also the
/// reference for `docs/commands/code.md` and the Worker frontend
/// error rendering.
///
/// Codex pass-1 P3: the list is grouped by gate-rejection layer
/// (loopback first, then body limit, then control-token, then
/// controller lease, then read/runtime) so a reviewer can see at
/// a glance which check produced a given code. Do NOT re-sort
/// alphabetically — the gate ordering is part of the contract
/// and matches the §5.3 / §5.4 specification.
pub fn code_ui_error_codes() -> &'static [(&'static str, u16)] {
    &[
        // Layer ordering: route handlers reject non-loopback first.
        ("LOOPBACK_REQUIRED", 403),
        // Then the body-limit middleware (write surface only).
        ("PAYLOAD_TOO_LARGE", 413),
        // Then automation control-token gate.
        ("CONTROL_DISABLED", 403),
        ("MISSING_CONTROL_TOKEN", 403),
        ("INVALID_CONTROL_TOKEN", 403),
        // Then controller (lease) state machine.
        ("MISSING_CONTROLLER_TOKEN", 403),
        ("INVALID_CONTROLLER_TOKEN", 403),
        ("INVALID_CONTROLLER_KIND", 400),
        ("CONTROLLER_CONFLICT", 409),
        ("BROWSER_CONTROL_DISABLED", 403),
        ("AUTOMATION_CONTROLLER_REQUIRED", 403),
        // Tail: read-side and runtime-availability errors.
        ("CODE_UI_UNAVAILABLE", 404),
        ("INVALID_QUERY_PARAM", 400),
        ("STORAGE_PATH_INVALID", 500),
        ("STATUS_UNAVAILABLE", 500),
        ("THREAD_LIST_FAILED", 500),
        ("DB_UNAVAILABLE", 500),
        ("INTERNAL_ERROR", 500),
        ("UNSUPPORTED_OPERATION", 422),
    ]
}

#[derive(Clone)]
pub struct ReadOnlyCodeUiAdapter {
    session: Arc<CodeUiSession>,
    capabilities: CodeUiCapabilities,
}

impl ReadOnlyCodeUiAdapter {
    pub fn new(session: Arc<CodeUiSession>, capabilities: CodeUiCapabilities) -> Arc<Self> {
        Arc::new(Self {
            session,
            capabilities,
        })
    }
}

#[async_trait]
impl CodeUiReadModel for ReadOnlyCodeUiAdapter {
    fn session(&self) -> Arc<CodeUiSession> {
        self.session.clone()
    }
}

#[async_trait]
impl CodeUiCommandAdapter for ReadOnlyCodeUiAdapter {
    fn capabilities(&self) -> CodeUiCapabilities {
        self.capabilities.clone()
    }

    async fn submit_message(&self, _text: String) -> anyhow::Result<()> {
        Err(anyhow!(
            "This libra code session is read-only from the browser"
        ))
    }

    async fn respond_interaction(
        &self,
        _interaction_id: &str,
        _response: CodeUiInteractionResponse,
    ) -> anyhow::Result<()> {
        Err(anyhow!(
            "This libra code session is read-only from the browser"
        ))
    }
}

pub fn initial_snapshot(
    working_dir: impl Into<String>,
    provider: CodeUiProviderInfo,
    capabilities: CodeUiCapabilities,
) -> CodeUiSessionSnapshot {
    CodeUiSessionSnapshot {
        session_id: Uuid::new_v4().to_string(),
        thread_id: None,
        working_dir: working_dir.into(),
        provider,
        capabilities,
        controller: CodeUiControllerState::default(),
        status: CodeUiSessionStatus::Idle,
        transcript: Vec::new(),
        plans: Vec::new(),
        tasks: Vec::new(),
        tool_calls: Vec::new(),
        patchsets: Vec::new(),
        interactions: Vec::new(),
        updated_at: Utc::now(),
    }
}

pub fn snapshot_from_thread_bundle(
    working_dir: impl Into<String>,
    provider: CodeUiProviderInfo,
    capabilities: CodeUiCapabilities,
    bundle: &ThreadBundle,
) -> CodeUiSessionSnapshot {
    let mut snapshot = initial_snapshot(working_dir, provider, capabilities);
    apply_thread_bundle_to_snapshot(&mut snapshot, bundle);
    snapshot
}

pub fn apply_thread_bundle_to_snapshot(
    snapshot: &mut CodeUiSessionSnapshot,
    bundle: &ThreadBundle,
) {
    let thread_id = bundle.thread.thread_id.to_string();
    snapshot.session_id = thread_id.clone();
    snapshot.thread_id = Some(thread_id);
    snapshot.status = if bundle.scheduler.active_run_id.is_some() {
        CodeUiSessionStatus::ExecutingTool
    } else if bundle.scheduler.active_task_id.is_some() {
        CodeUiSessionStatus::Thinking
    } else {
        CodeUiSessionStatus::Idle
    };
    snapshot.plans = code_ui_plan_snapshots(
        &bundle.scheduler.selected_plan_ids,
        bundle.scheduler.updated_at,
    );
    snapshot.tasks = bundle
        .scheduler
        .active_task_id
        .map(|task_id| CodeUiTaskSnapshot {
            id: task_id.to_string(),
            title: None,
            status: "active".to_string(),
            details: Some("Active scheduler task".to_string()),
            updated_at: bundle.scheduler.updated_at,
        })
        .into_iter()
        .collect();
    snapshot.updated_at = bundle.thread.updated_at.max(bundle.scheduler.updated_at);
}

/// Build the [`CodeUiPlanSnapshot`] list for a snapshot from the
/// scheduler's selected-plan heads.
///
/// `scheduler_updated_at` is the upstream `SchedulerState::updated_at`
/// — *not* `Utc::now()` — so every plan entry surfaces the same
/// projection revision timestamp as the rest of the snapshot. Using
/// `Utc::now()` here would make every render emit a different
/// `updatedAt` even when the underlying projection is unchanged, which
/// breaks browser change-detection heuristics and makes contract
/// snapshot tests non-deterministic.
fn code_ui_plan_snapshots(
    plan_heads: &[PlanHeadRef],
    scheduler_updated_at: DateTime<Utc>,
) -> Vec<CodeUiPlanSnapshot> {
    plan_heads
        .iter()
        .map(|plan| CodeUiPlanSnapshot {
            id: plan.plan_id.to_string(),
            title: None,
            summary: Some(format!("Selected plan ordinal {}", plan.ordinal)),
            status: "selected".to_string(),
            steps: Vec::new(),
            updated_at: scheduler_updated_at,
        })
        .collect()
}

pub fn browser_controller_token_from_headers(headers: &axum::http::HeaderMap) -> Option<String> {
    headers
        .get("x-code-controller-token")
        .and_then(|value| value.to_str().ok())
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
}

pub fn snapshot_from_event(event: &CodeUiEventEnvelope) -> anyhow::Result<CodeUiSessionSnapshot> {
    Ok(event.data.clone())
}

impl CodeUiControllerKind {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::None => "none",
            Self::Browser => "browser",
            Self::Automation => "automation",
            Self::Tui => "tui",
            Self::Cli => "cli",
        }
    }
}

pub fn ensure_session_updated_event(
    snapshot: &CodeUiSessionSnapshot,
) -> anyhow::Result<CodeUiEventEnvelope> {
    Ok(CodeUiEventEnvelope {
        seq: 0,
        event_type: CodeUiEventType::SessionUpdated,
        at: Utc::now(),
        data: snapshot.clone(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_session() -> Arc<CodeUiSession> {
        CodeUiSession::new(initial_snapshot(
            "/tmp/libra",
            CodeUiProviderInfo {
                provider: "test".to_string(),
                model: Some("test-model".to_string()),
                mode: Some("test".to_string()),
                managed: false,
            },
            CodeUiCapabilities {
                message_input: true,
                ..CodeUiCapabilities::default()
            },
        ))
    }

    #[test]
    fn terminal_plan_status_classification() {
        assert!(is_terminal_plan_status("completed"));
        assert!(is_terminal_plan_status("failed"));
        assert!(!is_terminal_plan_status("running"));
        assert!(!is_terminal_plan_status("pending"));
    }

    #[tokio::test]
    async fn upsert_plan_does_not_regress_terminal_status_to_running() {
        let session = test_session();
        let plan = |status: &str| CodeUiPlanSnapshot {
            id: "call_1".to_string(),
            title: Some("t".to_string()),
            summary: None,
            status: status.to_string(),
            steps: Vec::new(),
            updated_at: Utc::now(),
        };
        let stored_status = |snap: &CodeUiSessionSnapshot| {
            snap.plans
                .iter()
                .find(|p| p.id == "call_1")
                .map(|p| p.status.clone())
        };

        // Normal order (running → completed) still lands on completed.
        session.upsert_plan(plan("running")).await;
        session.upsert_plan(plan("completed")).await;
        assert_eq!(
            stored_status(&session.snapshot().await).as_deref(),
            Some("completed")
        );

        // The race: a late "running" upsert (from on_tool_call_start losing the
        // spawn race with on_tool_call_end) must NOT clobber the completed plan.
        session.upsert_plan(plan("running")).await;
        assert_eq!(
            stored_status(&session.snapshot().await).as_deref(),
            Some("completed"),
            "a late non-terminal upsert must not regress a completed plan"
        );

        // Terminal→terminal is still allowed (completed → failed), and a
        // subsequent late "running" is likewise ignored.
        session.upsert_plan(plan("failed")).await;
        session.upsert_plan(plan("running")).await;
        assert_eq!(
            stored_status(&session.snapshot().await).as_deref(),
            Some("failed")
        );
    }

    #[test]
    fn code_ui_error_code_listing_matches_authoritative_doc() {
        let doc_path =
            std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("docs/commands/code.md");
        let doc = std::fs::read_to_string(&doc_path).expect("read docs/commands/code.md");
        let mut doc_pairs: Vec<(String, u16)> = Vec::new();
        for line in doc.lines() {
            // Markdown table rows look like `| \`CODE\` | 403 | gate description |`.
            // Skip header / separator rows and any row whose first
            // cell isn't a backtick-wrapped identifier.
            let trimmed = line.trim_start();
            if !trimmed.starts_with('|') {
                continue;
            }
            let cells: Vec<&str> = trimmed.split('|').map(str::trim).collect();
            // Expected shape: ["", code, status, description, ""].
            if cells.len() < 4 {
                continue;
            }
            let code_cell = cells[1];
            if !(code_cell.starts_with('`') && code_cell.ends_with('`')) {
                continue;
            }
            let code = code_cell.trim_matches('`');
            // Reject the header separator (`| --- | --- | --- |`).
            if code.is_empty() || code.chars().all(|c| c == '-' || c.is_whitespace()) {
                continue;
            }
            let status: u16 = match cells[2].parse() {
                Ok(value) => value,
                Err(_) => continue,
            };
            doc_pairs.push((code.to_string(), status));
        }
        let source_pairs: Vec<(String, u16)> = code_ui_error_codes()
            .iter()
            .map(|(code, status)| ((*code).to_string(), *status))
            .collect();
        assert!(
            !doc_pairs.is_empty(),
            "Code UI API error table not found in docs/commands/code.md",
        );
        assert_eq!(
            doc_pairs, source_pairs,
            "docs/commands/code.md Code UI API error table is out of sync with code_ui_error_codes(); regenerate the table to match (order matters — the table mirrors the runtime gate ordering).",
        );
    }

    #[test]
    fn attach_request_defaults_to_browser_kind() {
        let request: CodeUiControllerAttachRequest =
            serde_json::from_value(serde_json::json!({ "clientId": "browser-1" })).unwrap();

        assert_eq!(request.kind, CodeUiControllerKind::Browser);
    }

    #[tokio::test]
    async fn interaction_update_broadcasts_typed_session_snapshot_event() {
        let session = test_session();
        let mut rx = session.subscribe();
        let requested_at = Utc::now();

        session
            .upsert_interaction(CodeUiInteractionRequest {
                id: "interaction-1".to_string(),
                kind: CodeUiInteractionKind::RequestUserInput,
                title: Some("Pick one".to_string()),
                description: None,
                prompt: Some("Continue?".to_string()),
                options: vec![CodeUiInteractionOption {
                    id: "yes".to_string(),
                    label: "Yes".to_string(),
                    description: None,
                }],
                status: CodeUiInteractionStatus::Pending,
                metadata: json!({"source": "test"}),
                requested_at,
                resolved_at: None,
            })
            .await;

        let event = rx.recv().await.expect("interaction update event");
        assert_eq!(event.event_type, CodeUiEventType::SessionUpdated);
        assert_eq!(event.data.interactions.len(), 1);
        let interaction = &event.data.interactions[0];
        assert_eq!(interaction.id, "interaction-1");
        assert_eq!(interaction.kind, CodeUiInteractionKind::RequestUserInput);
        assert_eq!(interaction.status, CodeUiInteractionStatus::Pending);
    }

    /// Wave 2 / PR 2 — error code source-of-truth contract.
    ///
    /// `code_ui_error_codes()` lists every Code UI error code the
    /// API may return. Per `docs/development/commands/_general.md` §5.20 we
    /// pin both:
    ///
    /// 1. the (code, status) tuples themselves are stable — any
    ///    drift breaks the documented HTTP contract; and
    /// 2. each documented constructor (`CodeUiApiError::*`) and
    ///    runtime path that produces a code in the list still
    ///    resolves to the listed status. Adding a new constructor
    ///    that produces an unlisted code makes the
    ///    `produced_codes_are_listed` assertion fail.
    #[test]
    fn code_ui_error_code_contract_pins_status_per_code() {
        // 1. Status mapping must be deterministic.
        let catalogue = code_ui_error_codes();
        // The catalogue must be free of duplicates so callers can
        // index it as a map without losing entries.
        let mut seen = std::collections::HashSet::new();
        for (code, _status) in catalogue {
            assert!(
                seen.insert(*code),
                "code_ui_error_codes() duplicates the entry for '{code}'",
            );
        }

        // 2. Walk the constructors that produce a fixed (code,
        //    status) pair and assert each one matches the
        //    catalogue. Adding a new producer requires extending
        //    both the catalogue AND this list — a missing entry
        //    fails on the lookup.
        let map: std::collections::HashMap<&'static str, u16> = catalogue.iter().copied().collect();
        let cases: Vec<(CodeUiApiError, &'static str)> = vec![
            (CodeUiApiError::unavailable(), "CODE_UI_UNAVAILABLE"),
            (
                CodeUiApiError::forbidden("LOOPBACK_REQUIRED", "remote"),
                "LOOPBACK_REQUIRED",
            ),
            (
                CodeUiApiError::forbidden("CONTROL_DISABLED", "no token"),
                "CONTROL_DISABLED",
            ),
            (
                CodeUiApiError::forbidden("MISSING_CONTROL_TOKEN", "missing"),
                "MISSING_CONTROL_TOKEN",
            ),
            (
                CodeUiApiError::forbidden("INVALID_CONTROL_TOKEN", "bad"),
                "INVALID_CONTROL_TOKEN",
            ),
            (
                CodeUiApiError::forbidden("MISSING_CONTROLLER_TOKEN", "missing lease"),
                "MISSING_CONTROLLER_TOKEN",
            ),
            (
                CodeUiApiError::forbidden("INVALID_CONTROLLER_TOKEN", "bad lease"),
                "INVALID_CONTROLLER_TOKEN",
            ),
            (
                CodeUiApiError::bad_request("INVALID_CONTROLLER_KIND", "kind"),
                "INVALID_CONTROLLER_KIND",
            ),
            (
                CodeUiApiError::conflict("CONTROLLER_CONFLICT", "held"),
                "CONTROLLER_CONFLICT",
            ),
            (
                CodeUiApiError::forbidden("BROWSER_CONTROL_DISABLED", "off"),
                "BROWSER_CONTROL_DISABLED",
            ),
            (
                CodeUiApiError::forbidden("AUTOMATION_CONTROLLER_REQUIRED", "lease"),
                "AUTOMATION_CONTROLLER_REQUIRED",
            ),
            (
                CodeUiApiError::bad_request("INVALID_QUERY_PARAM", "limit"),
                "INVALID_QUERY_PARAM",
            ),
            (
                CodeUiApiError::unsupported_from_error(anyhow::anyhow!("operation refused")),
                "UNSUPPORTED_OPERATION",
            ),
        ];
        for (err, expected_code) in cases {
            assert_eq!(
                err.code, expected_code,
                "constructor produced code '{}' but caller expected '{}'",
                err.code, expected_code,
            );
            let expected_status = map.get(expected_code).copied().unwrap_or_else(|| {
                panic!(
                    "code '{expected_code}' is missing from code_ui_error_codes(); \
                     update the catalogue when adding new error codes",
                )
            });
            assert_eq!(
                err.status, expected_status,
                "code '{expected_code}' resolved to status {} but the catalogue says {}",
                err.status, expected_status,
            );
        }
    }

    /// Codex pass-1 P2 — inline-producer coverage for the
    /// catalogue. The codes listed below are emitted as inline
    /// `WebApiError { … }` literals from `web::mod` rather than
    /// via the `CodeUiApiError` constructors above. Pinning their
    /// (code, status) shape here makes the catalogue's
    /// "single-source-of-truth" claim true for the WHOLE error
    /// surface, not just the constructor surface.
    ///
    /// The literal shapes mirror the inline producers in
    /// `src/internal/ai/web/mod.rs` (search for the code string
    /// to find the producer). When refactoring an inline literal
    /// into a named helper, move the corresponding case into
    /// `code_ui_error_code_contract_pins_status_per_code` above.
    #[test]
    fn code_ui_error_code_contract_pins_status_for_inline_producers() {
        let catalogue = code_ui_error_codes();
        let map: std::collections::HashMap<&'static str, u16> = catalogue.iter().copied().collect();
        // (code string, observed status) pairs — assert both halves
        // appear in the catalogue and resolve to the listed status.
        let inline_cases: &[(&str, u16)] = &[
            // mod.rs `enforce_code_write_body_limit` /
            // `code_control_body_too_large_response`.
            ("PAYLOAD_TOO_LARGE", 413),
            // mod.rs `parse_optional_u64` (?limit/?offset parser).
            ("INVALID_QUERY_PARAM", 400),
            // mod.rs `code_threads_handler` storage path build.
            ("STORAGE_PATH_INVALID", 500),
            // mod.rs `code_threads_handler` thread-list query path.
            ("DB_UNAVAILABLE", 500),
            ("THREAD_LIST_FAILED", 500),
            // mod.rs `code_goal_status_handler` response coerce.
            ("STATUS_UNAVAILABLE", 500),
            // mod.rs `WebApiError::From<serde_json::Error>` fallback.
            ("INTERNAL_ERROR", 500),
        ];
        for (code, observed_status) in inline_cases {
            let expected = map.get(code).copied().unwrap_or_else(|| {
                panic!(
                    "code '{code}' is in the inline-producer test list but missing from \
                     code_ui_error_codes(); update the catalogue when adding inline producers",
                )
            });
            assert_eq!(
                expected, *observed_status,
                "inline producer for '{code}' emits status {observed_status} but the catalogue says {expected}",
            );
        }
    }

    #[derive(Clone)]
    struct RecordingCodeUiAdapter {
        session: Arc<CodeUiSession>,
        submitted_messages: Arc<Mutex<Vec<String>>>,
    }

    impl RecordingCodeUiAdapter {
        fn new(session: Arc<CodeUiSession>) -> Arc<Self> {
            Arc::new(Self {
                session,
                submitted_messages: Arc::new(Mutex::new(Vec::new())),
            })
        }

        async fn submitted_messages(&self) -> Vec<String> {
            self.submitted_messages.lock().await.clone()
        }
    }

    #[async_trait]
    impl CodeUiReadModel for RecordingCodeUiAdapter {
        fn session(&self) -> Arc<CodeUiSession> {
            self.session.clone()
        }
    }

    #[async_trait]
    impl CodeUiCommandAdapter for RecordingCodeUiAdapter {
        fn capabilities(&self) -> CodeUiCapabilities {
            CodeUiCapabilities {
                message_input: true,
                interactive_approvals: true,
                ..CodeUiCapabilities::default()
            }
        }

        async fn submit_message(&self, text: String) -> anyhow::Result<()> {
            self.submitted_messages.lock().await.push(text);
            Ok(())
        }

        async fn respond_interaction(
            &self,
            _interaction_id: &str,
            _response: CodeUiInteractionResponse,
        ) -> anyhow::Result<()> {
            Ok(())
        }
    }

    #[tokio::test]
    async fn browser_controller_attach_and_detach_updates_snapshot() {
        let session = test_session();
        let runtime = CodeUiRuntimeHandle::build(
            ReadOnlyCodeUiAdapter::new(session.clone(), CodeUiCapabilities::default()),
            true,
            CodeUiInitialController::Unclaimed,
        )
        .await;

        let attach = runtime
            .attach_browser_controller("browser-a")
            .await
            .expect("browser controller should attach");
        assert_eq!(attach.controller.kind, CodeUiControllerKind::Browser);
        assert!(attach.controller.can_write);

        let snapshot = runtime.snapshot().await;
        assert_eq!(snapshot.controller.kind, CodeUiControllerKind::Browser);
        assert_eq!(
            snapshot.controller.owner_label.as_deref(),
            Some("browser-a")
        );

        runtime
            .detach_browser_controller("browser-a", &attach.controller_token)
            .await
            .expect("browser controller should detach");

        let detached_snapshot = runtime.snapshot().await;
        assert_eq!(
            detached_snapshot.controller.kind,
            CodeUiControllerKind::None
        );
        assert!(!detached_snapshot.controller.can_write);
    }

    #[tokio::test]
    async fn expired_browser_controller_lease_is_cleaned_before_attach() {
        let session = test_session();
        let runtime = CodeUiRuntimeHandle::build(
            ReadOnlyCodeUiAdapter::new(session.clone(), CodeUiCapabilities::default()),
            true,
            CodeUiInitialController::Unclaimed,
        )
        .await;

        let expired_attach = runtime
            .attach_browser_controller("browser-a")
            .await
            .expect("browser controller should attach");
        {
            let mut state = runtime.controller_state.lock().await;
            let lease = state
                .active_lease
                .as_mut()
                .expect("browser lease should be active");
            lease.expires_at = Utc::now() - Duration::seconds(1);
        }

        let replacement_attach = runtime
            .attach_browser_controller("browser-b")
            .await
            .expect("expired lease should not block a new browser");

        assert_ne!(
            expired_attach.controller_token,
            replacement_attach.controller_token
        );
        let snapshot = runtime.snapshot().await;
        assert_eq!(snapshot.controller.kind, CodeUiControllerKind::Browser);
        assert_eq!(
            snapshot.controller.owner_label.as_deref(),
            Some("browser-b")
        );

        let stale_error = runtime
            .ensure_controller_write_access(Some(&expired_attach.controller_token))
            .await
            .expect_err("stale token must not keep write access");
        assert_eq!(stale_error.status, 403);
        assert_eq!(stale_error.code, "INVALID_CONTROLLER_TOKEN");
    }

    #[tokio::test]
    async fn expired_browser_controller_write_failure_syncs_snapshot() {
        let session = test_session();
        let mut options =
            CodeUiRuntimeOptions::new(true, false, CodeUiInitialController::Unclaimed);
        options.lease_duration = Some(Duration::milliseconds(1));
        let runtime = CodeUiRuntimeHandle::build_with_options(
            ReadOnlyCodeUiAdapter::new(session.clone(), CodeUiCapabilities::default()),
            options,
        )
        .await;

        let attach = runtime
            .attach_browser_controller("browser-a")
            .await
            .expect("browser controller should attach");
        let attached_snapshot = runtime.snapshot().await;
        assert_eq!(
            attached_snapshot.controller.kind,
            CodeUiControllerKind::Browser
        );

        tokio::time::sleep(std::time::Duration::from_millis(5)).await;
        let error = runtime
            .submit_message(Some(&attach.controller_token), "after expiry".to_string())
            .await
            .expect_err("expired browser token must not write");
        assert_eq!(error.status, 409);
        assert_eq!(error.code, "CONTROLLER_CONFLICT");

        let stale_snapshot = runtime.snapshot().await;
        assert_eq!(stale_snapshot.controller.kind, CodeUiControllerKind::None);
        assert!(!stale_snapshot.controller.can_write);
    }

    #[tokio::test]
    async fn concurrent_browser_attach_allows_only_one_owner() {
        let runtime = CodeUiRuntimeHandle::build(
            ReadOnlyCodeUiAdapter::new(test_session(), CodeUiCapabilities::default()),
            true,
            CodeUiInitialController::Unclaimed,
        )
        .await;

        let runtime_a = runtime.clone();
        let runtime_b = runtime.clone();
        let (first, second) = tokio::join!(
            async move { runtime_a.attach_browser_controller("browser-a").await },
            async move { runtime_b.attach_browser_controller("browser-b").await },
        );

        let successes = [first.as_ref().ok(), second.as_ref().ok()]
            .into_iter()
            .flatten()
            .count();
        let conflicts = [first.as_ref().err(), second.as_ref().err()]
            .into_iter()
            .flatten()
            .filter(|error| error.status == 409 && error.code == "CONTROLLER_CONFLICT")
            .count();

        assert_eq!(successes, 1);
        assert_eq!(conflicts, 1);
    }

    #[tokio::test]
    async fn invalid_detach_does_not_clear_browser_controller() {
        let runtime = CodeUiRuntimeHandle::build(
            ReadOnlyCodeUiAdapter::new(test_session(), CodeUiCapabilities::default()),
            true,
            CodeUiInitialController::Unclaimed,
        )
        .await;
        let attach = runtime
            .attach_browser_controller("browser-a")
            .await
            .expect("browser controller should attach");

        let error = runtime
            .detach_browser_controller("browser-b", &attach.controller_token)
            .await
            .expect_err("wrong client id must not detach");
        assert_eq!(error.status, 403);
        assert_eq!(error.code, "INVALID_CONTROLLER_TOKEN");

        let snapshot = runtime.snapshot().await;
        assert_eq!(snapshot.controller.kind, CodeUiControllerKind::Browser);
        assert_eq!(
            snapshot.controller.owner_label.as_deref(),
            Some("browser-a")
        );
    }

    #[tokio::test]
    async fn concurrent_detach_and_submit_message_leaves_stale_token_rejected() {
        let adapter = RecordingCodeUiAdapter::new(test_session());
        let runtime =
            CodeUiRuntimeHandle::build(adapter.clone(), true, CodeUiInitialController::Unclaimed)
                .await;
        let attach = runtime
            .attach_browser_controller("browser-a")
            .await
            .expect("browser controller should attach");

        let detach_token = attach.controller_token.clone();
        let submit_token = attach.controller_token.clone();
        let runtime_for_detach = runtime.clone();
        let runtime_for_submit = runtime.clone();
        let (detach_result, submit_result) = tokio::join!(
            async move {
                runtime_for_detach
                    .detach_browser_controller("browser-a", &detach_token)
                    .await
            },
            async move {
                runtime_for_submit
                    .submit_message(Some(&submit_token), "hello".to_string())
                    .await
            },
        );

        detach_result.expect("detach should succeed");
        if let Err(error) = submit_result {
            assert!(
                error.status == 403 || error.status == 409,
                "submit should either win the race or fail authorization, got {error:?}"
            );
        }

        let stale_error = runtime
            .submit_message(Some(&attach.controller_token), "after detach".to_string())
            .await
            .expect_err("detached token must not submit again");
        assert_eq!(stale_error.status, 409);
        assert_eq!(stale_error.code, "CONTROLLER_CONFLICT");
        assert!(adapter.submitted_messages().await.len() <= 1);
    }

    #[tokio::test]
    async fn fixed_controller_rejects_browser_attach() {
        let runtime = CodeUiRuntimeHandle::build(
            ReadOnlyCodeUiAdapter::new(test_session(), CodeUiCapabilities::default()),
            true,
            CodeUiInitialController::Fixed {
                kind: CodeUiControllerKind::Cli,
                owner_label: "CLI".to_string(),
                reason: Some("Terminal control is active".to_string()),
            },
        )
        .await;

        let error = runtime
            .attach_browser_controller("browser-a")
            .await
            .expect_err("fixed controller must block browser attach");
        assert_eq!(error.status, 409);
        assert_eq!(error.code, "CONTROLLER_CONFLICT");
    }

    #[tokio::test]
    async fn local_tui_owner_allows_automation_takeover_and_reclaim() {
        let runtime = CodeUiRuntimeHandle::build_with_control(
            ReadOnlyCodeUiAdapter::new(test_session(), CodeUiCapabilities::default()),
            false,
            true,
            CodeUiInitialController::LocalTui {
                owner_label: "Terminal UI".to_string(),
                reason: Some("Local TUI owns this session".to_string()),
            },
        )
        .await;

        let initial = runtime.snapshot().await;
        assert_eq!(initial.controller.kind, CodeUiControllerKind::Tui);
        assert!(!initial.controller.can_write);

        let attach = runtime
            .attach_controller(CodeUiControllerKind::Automation, "automation-a")
            .await
            .expect("automation should attach");
        assert_eq!(attach.controller.kind, CodeUiControllerKind::Automation);
        assert!(attach.controller.can_write);

        let lease = runtime
            .ensure_controller_write_access(Some(&attach.controller_token))
            .await
            .expect("automation token should authorize writes");
        assert_eq!(lease.kind, CodeUiControllerKind::Automation);

        runtime
            .reclaim_local_tui_controller()
            .await
            .expect("local TUI should reclaim controller");

        let reclaimed = runtime.snapshot().await;
        assert_eq!(reclaimed.controller.kind, CodeUiControllerKind::Tui);
        assert!(!reclaimed.controller.can_write);

        let stale = runtime
            .ensure_controller_write_access(Some(&attach.controller_token))
            .await
            .expect_err("automation token must be invalid after reclaim");
        assert_eq!(stale.status, 409);
        assert_eq!(stale.code, "CONTROLLER_CONFLICT");
    }

    #[tokio::test]
    async fn automation_attach_is_disabled_without_control_mode() {
        let runtime = CodeUiRuntimeHandle::build(
            ReadOnlyCodeUiAdapter::new(test_session(), CodeUiCapabilities::default()),
            false,
            CodeUiInitialController::LocalTui {
                owner_label: "Terminal UI".to_string(),
                reason: None,
            },
        )
        .await;

        let error = runtime
            .attach_controller(CodeUiControllerKind::Automation, "automation-a")
            .await
            .expect_err("automation should be disabled by default");
        assert_eq!(error.status, 403);
        assert_eq!(error.code, "CONTROL_DISABLED");
    }

    #[tokio::test]
    async fn diagnostics_exposes_snapshot_summary_without_secret_material() {
        let session = test_session();
        session
            .set_controller_state(CodeUiControllerState {
                kind: CodeUiControllerKind::Automation,
                owner_label: Some("local-script".to_string()),
                can_write: true,
                lease_expires_at: Some(Utc::now() + Duration::seconds(60)),
                reason: None,
                loopback_only: true,
            })
            .await;
        session
            .upsert_interaction(CodeUiInteractionRequest {
                id: "interaction-1".to_string(),
                kind: CodeUiInteractionKind::Approval,
                title: Some("Approve command".to_string()),
                status: CodeUiInteractionStatus::Pending,
                requested_at: Utc::now(),
                ..CodeUiInteractionRequest::default()
            })
            .await;
        let runtime = CodeUiRuntimeHandle::build_with_control(
            ReadOnlyCodeUiAdapter::new(session, CodeUiCapabilities::default()),
            false,
            true,
            CodeUiInitialController::Unclaimed,
        )
        .await;
        let attach = runtime
            .attach_controller(CodeUiControllerKind::Automation, "local-script")
            .await
            .expect("automation should attach");
        runtime
            .ensure_controller_write_access(Some(&attach.controller_token))
            .await
            .expect("automation token should refresh lease");

        let diagnostics = runtime.diagnostics().await;
        let serialized = serde_json::to_string(&diagnostics).unwrap();

        assert_eq!(diagnostics.provider, "test");
        assert_eq!(diagnostics.model.as_deref(), Some("test-model"));
        assert_eq!(
            diagnostics.active_interaction_id.as_deref(),
            Some("interaction-1")
        );
        assert_eq!(
            diagnostics.controller.kind,
            CodeUiControllerKind::Automation
        );
        assert!(!serialized.contains(&attach.controller_token));
        assert!(!serialized.contains("x-libra-control-token"));
        assert!(!serialized.contains("authorization"));
        assert!(!serialized.contains("api_key"));
    }
}
