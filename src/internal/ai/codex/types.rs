//! Data types for Agent Codex command.
//! Contains all data structures used for Codex protocol communication and session management.

use std::collections::HashMap;

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

// =============================================================================
// Protocol Messages
// =============================================================================

/// Codex JSON-RPC message
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CodexMessage {
    pub jsonrpc: String,
    #[serde(default)]
    pub id: Option<u64>,
    pub method: Option<String>,
    pub params: Option<serde_json::Value>,
    pub result: Option<serde_json::Value>,
    pub error: Option<serde_json::Value>,
}

impl CodexMessage {
    pub fn new_request(id: u64, method: &str, params: serde_json::Value) -> Self {
        Self {
            jsonrpc: "2.0".to_string(),
            id: Some(id),
            method: Some(method.to_string()),
            params: Some(params),
            result: None,
            error: None,
        }
    }

    pub fn to_json(&self) -> String {
        serde_json::to_string(self).unwrap_or_default()
    }
}

// =============================================================================
// Status Enums
// =============================================================================

/// Thread status
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "lowercase")]
pub enum ThreadStatus {
    Pending,
    Running,
    Completed,
    Archived,
    Closed,
}

/// Run status
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "lowercase")]
pub enum RunStatus {
    Pending,
    InProgress,
    Completed,
    Failed,
    /// Turn ended by cancel/abort; distinct from Failed so Code UI can idle
    /// instead of surfacing an error after snapshot rebuild (W3-04).
    Cancelled,
}

/// Task status
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "lowercase")]
pub enum TaskStatus {
    Pending,
    InProgress,
    Completed,
    Failed,
}

/// Plan status
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "lowercase")]
pub enum PlanStatus {
    Pending,
    InProgress,
    Completed,
}

/// Tool invocation status
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "lowercase")]
pub enum ToolStatus {
    Pending,
    InProgress,
    Completed,
    Failed,
}

impl std::fmt::Display for ToolStatus {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let s = match self {
            ToolStatus::Pending => "pending",
            ToolStatus::InProgress => "in_progress",
            ToolStatus::Completed => "completed",
            ToolStatus::Failed => "failed",
        };
        write!(f, "{s}")
    }
}

/// Patch apply status
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "lowercase")]
pub enum PatchStatus {
    Pending,
    InProgress,
    Completed,
    Failed,
    Declined,
}

/// Approval type
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "snake_case")]
pub enum ApprovalType {
    CommandExecution,
    FileChange,
    ApplyPatch,
    Unknown,
}

// =============================================================================
// Data Models
// =============================================================================

/// Thread - represents a conversation session
/// Corresponds to: `Thread[L]` in docs/ai/object-model-reference.md
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CodexThread {
    pub id: String,
    pub status: ThreadStatus,
    pub name: Option<String>,
    pub current_turn_id: Option<String>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

impl Default for CodexThread {
    fn default() -> Self {
        let now = Utc::now();
        Self {
            id: String::new(),
            status: ThreadStatus::Pending,
            name: None,
            current_turn_id: None,
            created_at: now,
            updated_at: now,
        }
    }
}

/// Intent - user request snapshot
/// Corresponds to: `Intent[S]` in docs/ai/object-model-reference.md
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Intent {
    pub id: String,
    pub content: String,
    pub thread_id: String,
    pub created_at: DateTime<Utc>,
}

/// Plan - strategy and steps snapshot
/// Corresponds to: `Plan[S]` in docs/ai/object-model-reference.md
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Plan {
    pub id: String,
    pub text: String,
    pub intent_id: Option<String>,
    pub thread_id: String,
    pub turn_id: Option<String>,
    pub status: PlanStatus,
    pub created_at: DateTime<Utc>,
}

/// Task - work unit definition
/// Corresponds to: `Task[S]` in docs/ai/object-model-reference.md
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Task {
    pub id: String,
    pub tool_name: Option<String>,
    pub plan_id: Option<String>,
    pub thread_id: String,
    pub turn_id: Option<String>,
    pub status: TaskStatus,
    pub created_at: DateTime<Utc>,
}

/// Run - execution attempt
/// Corresponds to: `Run[S]` in docs/ai/object-model-reference.md
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Run {
    pub id: String,
    pub thread_id: String,
    pub status: RunStatus,
    pub started_at: DateTime<Utc>,
    pub completed_at: Option<DateTime<Utc>>,
}

/// Token usage breakdown
/// Corresponds to: `RunUsage[E]` in docs/ai/object-model-reference.md
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct TokenUsage {
    pub cached_input_tokens: Option<i64>,
    pub input_tokens: Option<i64>,
    pub output_tokens: Option<i64>,
    pub reasoning_output_tokens: Option<i64>,
    pub total_tokens: Option<i64>,
}

/// Token usage for a turn
/// Corresponds to: `RunUsage[E]` in docs/ai/object-model-reference.md
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TurnTokenUsage {
    pub thread_id: String,
    pub turn_id: String,
    pub last: TokenUsage,
    pub total: TokenUsage,
    pub model_context_window: Option<i64>,
    pub updated_at: DateTime<Utc>,
}

/// ToolInvocation - tool call record
/// Corresponds to: `ToolInvocation[E]` in docs/ai/object-model-reference.md
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolInvocation {
    pub id: String,
    pub run_id: String,
    pub thread_id: String,
    pub tool_name: String,
    pub server: Option<String>,
    pub arguments: Option<serde_json::Value>,
    pub result: Option<serde_json::Value>,
    pub error: Option<String>,
    pub status: ToolStatus,
    pub duration_ms: Option<i64>,
    pub created_at: DateTime<Utc>,
}

/// Reasoning - agent reasoning process
/// Corresponds to: reasoning items in Codex protocol
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Reasoning {
    pub id: String,
    pub run_id: String,
    pub thread_id: String,
    pub summary: Vec<String>,
    pub text: Option<String>,
    pub created_at: DateTime<Utc>,
}

/// FileChange - represents a file change
/// Corresponds to: `PatchSet[S]` in docs/ai/object-model-reference.md
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct FileChange {
    pub path: String,
    pub diff: String,
    pub change_type: String, // add, delete, update
}

/// PatchSet - candidate patch snapshot
/// Corresponds to: `PatchSet[S]` in docs/ai/object-model-reference.md
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PatchSet {
    pub id: String,
    pub run_id: String,
    pub thread_id: String,
    pub changes: Vec<FileChange>,
    pub status: PatchStatus,
    pub created_at: DateTime<Utc>,
}

#[derive(Debug, Clone)]
pub struct CommandExecutionBaseline {
    pub cwd: String,
    pub files: HashMap<String, String>,
}

/// ApprovalRequest - approval request record
/// Corresponds to: `Decision[E]` in docs/ai/object-model-reference.md (pre-decision)
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ApprovalRequest {
    pub id: String,
    pub approval_type: ApprovalType,
    pub item_id: String,
    pub thread_id: String,
    pub run_id: Option<String>,
    pub command: Option<String>,
    pub changes: Option<Vec<String>>,
    pub description: Option<String>,
    pub decision: Option<bool>,
    pub requested_at: DateTime<Utc>,
    pub resolved_at: Option<DateTime<Utc>>,
}

/// AgentMessage - agent response content
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AgentMessage {
    pub id: String,
    pub run_id: String,
    pub thread_id: String,
    pub content: String,
    pub created_at: DateTime<Utc>,
}

// =============================================================================
// Session Management
// =============================================================================

/// Complete session data that can be queried by MCP
/// This is the main data container that MCP will query
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CodexSession {
    pub thread: CodexThread,
    pub intents: Vec<Intent>,
    pub plans: Vec<Plan>,
    pub tasks: Vec<Task>,
    pub runs: Vec<Run>,
    pub tool_invocations: Vec<ToolInvocation>,
    pub reasonings: Vec<Reasoning>,
    pub patchsets: Vec<PatchSet>,
    #[serde(skip)]
    pub command_baselines: HashMap<String, CommandExecutionBaseline>,
    pub approval_requests: Vec<ApprovalRequest>,
    pub agent_messages: Vec<AgentMessage>,
    pub token_usages: Vec<TurnTokenUsage>,
    pub debug: bool,
}

#[allow(clippy::derivable_impls)]
impl Default for CodexSession {
    fn default() -> Self {
        Self {
            thread: CodexThread::default(),
            intents: Vec::new(),
            plans: Vec::new(),
            tasks: Vec::new(),
            runs: Vec::new(),
            tool_invocations: Vec::new(),
            reasonings: Vec::new(),
            patchsets: Vec::new(),
            command_baselines: HashMap::new(),
            approval_requests: Vec::new(),
            agent_messages: Vec::new(),
            token_usages: Vec::new(),
            debug: false,
        }
    }
}

impl CodexSession {
    pub fn new() -> Self {
        Self::default()
    }

    /// Print summary of current session (for debug)
    pub fn print_summary(&self) {
        eprintln!("\n========== SESSION DATA ==========");
        eprintln!(
            "Thread: id={}, status={:?}, name={:?}, current_turn={:?}",
            self.thread.id, self.thread.status, self.thread.name, self.thread.current_turn_id
        );
        eprintln!("Intents: {}", self.intents.len());
        for intent in &self.intents {
            eprintln!(
                "  - Intent: {} - {}",
                intent.id,
                &intent.content[..intent.content.len().min(50)]
            );
        }
        eprintln!("Plans: {}", self.plans.len());
        for plan in &self.plans {
            eprintln!(
                "  - Plan: {} - {} (status: {:?})",
                plan.id,
                &plan.text[..plan.text.len().min(30)],
                plan.status
            );
        }
        eprintln!("Runs: {}", self.runs.len());
        for run in &self.runs {
            eprintln!("  - Run: {} (status: {:?})", run.id, run.status);
        }
        eprintln!("ToolInvocations: {}", self.tool_invocations.len());
        for inv in &self.tool_invocations {
            eprintln!(
                "  - Tool: {} - {} (status: {:?})",
                inv.id, inv.tool_name, inv.status
            );
        }
        eprintln!("Reasonings: {}", self.reasonings.len());
        eprintln!("PatchSets: {}", self.patchsets.len());
        for ps in &self.patchsets {
            eprintln!(
                "  - PatchSet: {} (status: {:?}, {} changes)",
                ps.id,
                ps.status,
                ps.changes.len()
            );
        }
        eprintln!("ApprovalRequests: {}", self.approval_requests.len());
        for ar in &self.approval_requests {
            eprintln!(
                "  - Approval: {} - {:?} (decision: {:?})",
                ar.id, ar.approval_type, ar.decision
            );
        }
        eprintln!("AgentMessages: {}", self.agent_messages.len());
        eprintln!("TokenUsages: {}", self.token_usages.len());
        for usage in &self.token_usages {
            eprintln!(
                "  - TokenUsage: turn={}, total_tokens={}",
                usage.turn_id,
                usage.total.total_tokens.unwrap_or(0)
            );
        }
        eprintln!("================================\n");
    }

    /// Add a new intent
    pub fn add_intent(&mut self, intent: Intent) {
        if self.debug {
            eprintln!(
                "[DEBUG] Added Intent: {} - {}",
                intent.id,
                &intent.content[..intent.content.len().min(50)]
            );
        }
        self.intents.push(intent);
    }

    /// Add a new plan
    pub fn add_plan(&mut self, plan: Plan) {
        if self.debug {
            eprintln!(
                "[DEBUG] Added Plan: {} - {} (status: {:?})",
                plan.id,
                &plan.text[..plan.text.len().min(30)],
                plan.status
            );
        }
        if let Some(existing) = self
            .plans
            .iter_mut()
            .find(|existing| existing.id == plan.id)
        {
            *existing = plan;
        } else {
            self.plans.push(plan);
        }
    }

    /// Add a new task
    pub fn add_task(&mut self, task: Task) {
        if self.debug {
            eprintln!("[DEBUG] Added Task: {}", task.id);
        }
        if let Some(existing) = self
            .tasks
            .iter_mut()
            .find(|existing| existing.id == task.id)
        {
            *existing = task;
        } else {
            self.tasks.push(task);
        }
    }

    /// Add a new run
    pub fn add_run(&mut self, run: Run) {
        if self.debug {
            eprintln!("[DEBUG] Added Run: {} (status: {:?})", run.id, run.status);
        }
        if let Some(existing) = self.runs.iter_mut().find(|existing| existing.id == run.id) {
            *existing = run;
        } else {
            self.runs.push(run);
        }
    }

    /// Add a new tool invocation
    pub fn add_tool_invocation(&mut self, invocation: ToolInvocation) {
        if self.debug {
            eprintln!(
                "[DEBUG] Added ToolInvocation: {} - {}",
                invocation.id, invocation.tool_name
            );
        }
        if let Some(existing) = self
            .tool_invocations
            .iter_mut()
            .find(|existing| existing.id == invocation.id)
        {
            *existing = invocation;
        } else {
            self.tool_invocations.push(invocation);
        }
    }

    /// Add a new reasoning
    pub fn add_reasoning(&mut self, reasoning: Reasoning) {
        if self.debug {
            eprintln!(
                "[DEBUG] Added Reasoning: {} (summary: {:?})",
                reasoning.id,
                reasoning.summary.first().map(|s| &s[..s.len().min(30)])
            );
        }
        if let Some(existing) = self
            .reasonings
            .iter_mut()
            .find(|existing| existing.id == reasoning.id)
        {
            *existing = reasoning;
        } else {
            self.reasonings.push(reasoning);
        }
    }

    /// Add a new patchset
    pub fn add_patchset(&mut self, patchset: PatchSet) {
        if self.debug {
            eprintln!(
                "[DEBUG] Added PatchSet: {} (status: {:?}, {} changes)",
                patchset.id,
                patchset.status,
                patchset.changes.len()
            );
        }
        if let Some(existing) = self
            .patchsets
            .iter_mut()
            .find(|existing| existing.id == patchset.id)
        {
            *existing = patchset;
        } else {
            self.patchsets.push(patchset);
        }
    }

    /// Add a new approval request
    pub fn add_approval_request(&mut self, request: ApprovalRequest) {
        if self.debug {
            eprintln!(
                "[DEBUG] Added ApprovalRequest: {} - {:?}",
                request.id, request.approval_type
            );
        }
        if let Some(existing) = self
            .approval_requests
            .iter_mut()
            .find(|existing| existing.id == request.id)
        {
            *existing = request;
        } else {
            self.approval_requests.push(request);
        }
    }

    /// Add a new agent message
    pub fn add_agent_message(&mut self, message: AgentMessage) {
        if self.debug {
            eprintln!(
                "[DEBUG] Added AgentMessage: {} - {}",
                message.id,
                &message.content[..message.content.len().min(50)]
            );
        }
        if let Some(existing) = self
            .agent_messages
            .iter_mut()
            .find(|existing| existing.id == message.id)
        {
            *existing = message;
        } else {
            self.agent_messages.push(message);
        }
    }

    /// Add token usage
    pub fn add_token_usage(&mut self, usage: TurnTokenUsage) {
        if self.debug {
            eprintln!(
                "[DEBUG] Added TokenUsage: turn={}, total={}",
                usage.turn_id,
                usage.total.total_tokens.unwrap_or(0)
            );
        }
        self.token_usages.push(usage);
    }

    /// Update the thread
    pub fn update_thread(&mut self, thread: CodexThread) {
        self.thread = thread;
    }

    /// Get current active run
    pub fn get_active_run(&self) -> Option<&Run> {
        self.runs.iter().find(|r| r.status == RunStatus::InProgress)
    }

    /// Get current active turn
    pub fn get_current_turn_id(&self) -> Option<&str> {
        self.thread.current_turn_id.as_deref()
    }
}
