//! Context types for tool invocation and execution.

use std::{collections::HashMap, fmt, path::PathBuf};

use serde::{Deserialize, Deserializer, Serialize, de::Error as _};
use serde_json::{Map, Value};
use tokio::sync::oneshot;

use crate::internal::ai::{
    intentspec::IntentDraft,
    sandbox::{SandboxPermissions, ToolRuntimeContext},
};

/// The kind of tool payload.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub enum ToolKind {
    /// Standard function calling with JSON arguments.
    Function,
    /// Model Context Protocol tool call.
    Mcp,
    /// Custom/freeform tool call.
    Custom,
}

/// A tool invocation containing all context needed for execution.
#[derive(Clone)]
pub struct ToolInvocation {
    /// Unique identifier for this specific tool call.
    pub call_id: String,
    /// Name of the tool being invoked.
    pub tool_name: String,
    /// The payload containing arguments or input data.
    pub payload: ToolPayload,
    /// Working directory for file operations.
    pub working_dir: PathBuf,
    /// Optional runtime constraints attached by the orchestrator.
    pub runtime_context: Option<ToolRuntimeContext>,
}

impl ToolInvocation {
    /// Create a new tool invocation.
    pub fn new(
        call_id: impl Into<String>,
        tool_name: impl Into<String>,
        payload: ToolPayload,
        working_dir: PathBuf,
    ) -> Self {
        Self {
            call_id: call_id.into(),
            tool_name: tool_name.into(),
            payload,
            working_dir,
            runtime_context: None,
        }
    }

    /// Attach runtime context to this invocation.
    pub fn with_runtime_context(mut self, runtime_context: ToolRuntimeContext) -> Self {
        self.runtime_context = Some(runtime_context);
        self
    }

    /// Get the payload as a string for logging.
    pub fn log_payload(&self) -> String {
        match &self.payload {
            ToolPayload::Function { arguments } => arguments.clone(),
            ToolPayload::Custom { input } => input.clone(),
            ToolPayload::Mcp { raw_arguments, .. } => raw_arguments.clone(),
        }
    }
}

/// The payload of a tool invocation, containing the input data.
#[derive(Clone, Debug)]
pub enum ToolPayload {
    /// Function-style tool call with JSON arguments.
    Function {
        /// JSON string containing the tool arguments.
        arguments: String,
    },
    /// Custom/freeform tool call with plain text input.
    Custom {
        /// Plain text input for the tool.
        input: String,
    },
    /// Model Context Protocol tool call.
    Mcp {
        /// MCP server name.
        server: String,
        /// Tool name within the MCP server.
        tool: String,
        /// Raw arguments string.
        raw_arguments: String,
    },
}

impl ToolPayload {
    /// Check if this is a Function payload.
    pub fn is_function(&self) -> bool {
        matches!(self, ToolPayload::Function { .. })
    }

    /// Check if this is a Custom payload.
    pub fn is_custom(&self) -> bool {
        matches!(self, ToolPayload::Custom { .. })
    }

    /// Check if this is an Mcp payload.
    pub fn is_mcp(&self) -> bool {
        matches!(self, ToolPayload::Mcp { .. })
    }
}

/// Output from a tool execution.
#[derive(Clone, Debug)]
pub enum ToolOutput {
    /// Function-style output with structured data.
    Function {
        /// The output content as text.
        content: String,
        /// Whether the tool execution succeeded.
        success: Option<bool>,
        /// Optional structured data for UI projection (not sent to the model).
        metadata: Option<serde_json::Value>,
    },
    /// MCP tool output.
    Mcp {
        /// The MCP result as JSON.
        result: serde_json::Value,
    },
}

impl ToolOutput {
    /// Create a successful function output.
    pub fn success(content: impl Into<String>) -> Self {
        Self::Function {
            content: content.into(),
            success: Some(true),
            metadata: None,
        }
    }

    /// Create a failed function output.
    pub fn failure(content: impl Into<String>) -> Self {
        Self::Function {
            content: content.into(),
            success: Some(false),
            metadata: None,
        }
    }

    /// Create a text-only function output (success assumed).
    pub fn text(content: impl Into<String>) -> Self {
        Self::Function {
            content: content.into(),
            success: None,
            metadata: None,
        }
    }

    /// Attach structured metadata (for UI display, not sent to the model).
    pub fn with_metadata(mut self, meta: serde_json::Value) -> Self {
        if let ToolOutput::Function {
            ref mut metadata, ..
        } = self
        {
            *metadata = Some(meta);
        }
        self
    }

    /// Get the text content of this output.
    pub fn as_text(&self) -> Option<&str> {
        match self {
            ToolOutput::Function { content, .. } => Some(content),
            ToolOutput::Mcp { .. } => None,
        }
    }

    /// Get structured metadata attached to this output.
    pub fn metadata(&self) -> Option<&serde_json::Value> {
        match self {
            ToolOutput::Function { metadata, .. } => metadata.as_ref(),
            ToolOutput::Mcp { .. } => None,
        }
    }

    /// Check if the output indicates success.
    pub fn is_success(&self) -> bool {
        match self {
            ToolOutput::Function { success, .. } => success.unwrap_or(true),
            ToolOutput::Mcp { .. } => true,
        }
    }

    /// Convert this output to a JSON value for sending to the model.
    ///
    /// The `metadata` field is intentionally excluded — it is for UI display
    /// only and must not be sent to the model.
    pub fn into_response(self) -> serde_json::Value {
        match self {
            ToolOutput::Function {
                content, success, ..
            } => {
                serde_json::json!({
                    "content": content,
                    "success": success
                })
            }
            ToolOutput::Mcp { result } => result,
        }
    }

    /// Get a preview of the output for logging (truncated if necessary).
    pub fn log_preview(&self) -> String {
        const MAX_PREVIEW_LENGTH: usize = 500;
        match self {
            ToolOutput::Function { content, .. } => log_preview_text(content, MAX_PREVIEW_LENGTH),
            ToolOutput::Mcp { result } => {
                log_preview_text(&format!("{:?}", result), MAX_PREVIEW_LENGTH)
            }
        }
    }
}

fn log_preview_text(content: &str, max_bytes: usize) -> String {
    if content.len() <= max_bytes {
        content.to_string()
    } else {
        format!(
            "{}... (truncated)",
            truncate_to_utf8_boundary(content, max_bytes)
        )
    }
}

fn truncate_to_utf8_boundary(content: &str, max_bytes: usize) -> &str {
    let mut end = max_bytes.min(content.len());
    while !content.is_char_boundary(end) {
        end -= 1;
    }
    &content[..end]
}

/// Arguments for the read_file tool.
#[derive(Clone, Deserialize, Debug)]
pub struct ReadFileArgs {
    /// Absolute path to the file to read.
    pub file_path: String,
    /// 1-indexed line number to start reading from (default: 1).
    #[serde(default = "default_offset")]
    pub offset: usize,
    /// Maximum number of lines to return (default: 2000).
    #[serde(default = "default_limit")]
    pub limit: usize,
}

fn default_offset() -> usize {
    1
}

fn default_limit() -> usize {
    2000
}

/// Arguments for the list_dir tool.
#[derive(Clone, Deserialize, Debug)]
pub struct ListDirArgs {
    /// Absolute path to the directory to list.
    #[serde(
        default = "default_dir_path",
        alias = "path",
        alias = "directory",
        alias = "dir",
        alias = "directory_path"
    )]
    pub dir_path: String,
    /// 1-indexed entry number to start listing from (default: 1).
    #[serde(default = "default_dir_offset")]
    pub offset: usize,
    /// Maximum number of entries to return (default: 25).
    #[serde(default = "default_dir_limit")]
    pub limit: usize,
    /// Maximum directory depth to traverse (default: 2, must be >= 1).
    #[serde(default = "default_depth")]
    pub depth: usize,
}

fn default_dir_path() -> String {
    ".".to_string()
}

fn default_dir_offset() -> usize {
    1
}

fn default_dir_limit() -> usize {
    25
}

fn default_depth() -> usize {
    2
}

/// Arguments for the shell tool.
#[derive(Clone, Deserialize, Debug)]
pub struct ShellArgs {
    /// Shell command or script to execute (runs in the user's default shell).
    pub command: String,
    /// Working directory for the command. May be absolute or sandbox-relative,
    /// and must remain within the sandbox working directory.
    #[serde(default)]
    pub workdir: Option<String>,
    /// Timeout in milliseconds. Defaults to 60,000 ms (60 seconds).
    #[serde(default)]
    pub timeout_ms: Option<u64>,
    /// Controls whether the command uses sandbox defaults or requests escalation.
    #[serde(default)]
    pub sandbox_permissions: SandboxPermissions,
    /// Optional justification for escalated command execution.
    #[serde(default)]
    pub justification: Option<String>,
}

/// Arguments for the grep_files tool.
#[derive(Clone, Deserialize, Debug)]
pub struct GrepFilesArgs {
    /// Regular expression pattern to search for.
    #[serde(alias = "query", alias = "search", alias = "regex")]
    pub pattern: String,
    /// Optional glob limiting which files are searched (e.g. "*.rs" or "*.{ts,tsx}").
    #[serde(default, alias = "glob")]
    pub include: Option<String>,
    /// Directory or file path to search. Defaults to the working directory.
    #[serde(default, alias = "dir_path", alias = "directory", alias = "dir")]
    pub path: Option<String>,
    /// Maximum number of file paths to return (default: 100, max: 2000).
    #[serde(default = "default_grep_limit")]
    pub limit: usize,
}

fn default_grep_limit() -> usize {
    100
}

/// Arguments for the web_search tool.
#[derive(Clone, Deserialize, Debug)]
pub struct WebSearchArgs {
    /// Search query to send to the web search provider.
    #[serde(alias = "q")]
    pub query: String,
    /// Maximum number of results to return (default: 5, max enforced by handler).
    #[serde(default = "default_web_search_limit")]
    pub limit: usize,
}

fn default_web_search_limit() -> usize {
    5
}

// ── update_plan types ──────────────────────────────────────────────────

/// Status of a single plan step.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum StepStatus {
    Pending,
    InProgress,
    Completed,
}

/// A single step in a plan.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct PlanStep {
    /// Human-readable description of the step.
    pub step: String,
    /// Current status of the step.
    pub status: StepStatus,
}

/// Arguments for the `update_plan` tool.
#[derive(Clone, Debug, Deserialize)]
pub struct UpdatePlanArgs {
    /// Optional explanation of what changed since the last plan update.
    #[serde(default)]
    pub explanation: Option<String>,
    /// The full plan, expressed as an ordered list of steps.
    pub plan: Vec<PlanStep>,
}

// ── submit_plan_draft types ───────────────────────────────────────────

/// One provider-proposed step title for an execution plan draft.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct PlanDraftStep {
    /// Human-readable draft step title.
    pub title: String,
}

/// Arguments for the `submit_plan_draft` tool.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct SubmitPlanDraftArgs {
    /// Optional explanation for the proposed draft.
    #[serde(default)]
    pub explanation: Option<String>,
    /// Ordered provider-proposed draft steps. Runtime status is intentionally absent.
    pub steps: Vec<PlanDraftStep>,
}

pub const MAX_SUBMIT_PLAN_DRAFT_ARGUMENT_BYTES: usize = 256 * 1024;
pub const MAX_SUBMIT_PLAN_DRAFT_STEPS: usize = 128;
pub const MAX_SUBMIT_PLAN_DRAFT_TITLE_BYTES: usize = 512;
pub const MAX_SUBMIT_PLAN_DRAFT_EXPLANATION_BYTES: usize = 16 * 1024;
pub const MAX_SUBMIT_PLAN_DRAFT_TEXT_BYTES: usize = 128 * 1024;

pub fn validate_submit_plan_draft_bounds(args: &SubmitPlanDraftArgs) -> Result<(), String> {
    if args.steps.is_empty() {
        return Err("submit_plan_draft requires at least one non-empty step title".to_string());
    }
    if args.steps.len() > MAX_SUBMIT_PLAN_DRAFT_STEPS {
        return Err(format!(
            "submit_plan_draft accepts at most {MAX_SUBMIT_PLAN_DRAFT_STEPS} steps"
        ));
    }
    let explanation_bytes = args.explanation.as_deref().map_or(0, str::len);
    if explanation_bytes > MAX_SUBMIT_PLAN_DRAFT_EXPLANATION_BYTES {
        return Err(format!(
            "submit_plan_draft explanation exceeds {MAX_SUBMIT_PLAN_DRAFT_EXPLANATION_BYTES} bytes"
        ));
    }
    let mut total = explanation_bytes;
    for step in &args.steps {
        let title = step.title.trim();
        if title.is_empty() {
            return Err("submit_plan_draft step titles must not be empty".to_string());
        }
        if title.len() > MAX_SUBMIT_PLAN_DRAFT_TITLE_BYTES {
            return Err(format!(
                "submit_plan_draft step title exceeds {MAX_SUBMIT_PLAN_DRAFT_TITLE_BYTES} bytes"
            ));
        }
        total = total
            .checked_add(title.len())
            .ok_or_else(|| "submit_plan_draft text size exceeds the supported limit".to_string())?;
    }
    if total > MAX_SUBMIT_PLAN_DRAFT_TEXT_BYTES {
        return Err(format!(
            "submit_plan_draft text exceeds {MAX_SUBMIT_PLAN_DRAFT_TEXT_BYTES} bytes"
        ));
    }
    Ok(())
}

pub fn validate_submit_plan_draft_value_bounds(value: &serde_json::Value) -> Result<(), String> {
    let object = value
        .as_object()
        .ok_or_else(|| "submit_plan_draft arguments must be an object".to_string())?;
    if object
        .keys()
        .any(|key| !matches!(key.as_str(), "explanation" | "steps"))
    {
        return Err("submit_plan_draft arguments contain an unknown field".to_string());
    }
    let explanation_bytes = match object.get("explanation") {
        Some(serde_json::Value::String(explanation)) => explanation.len(),
        Some(serde_json::Value::Null) | None => 0,
        Some(_) => return Err("submit_plan_draft explanation must be a string".to_string()),
    };
    if explanation_bytes > MAX_SUBMIT_PLAN_DRAFT_EXPLANATION_BYTES {
        return Err(format!(
            "submit_plan_draft explanation exceeds {MAX_SUBMIT_PLAN_DRAFT_EXPLANATION_BYTES} bytes"
        ));
    }
    let steps = object
        .get("steps")
        .and_then(serde_json::Value::as_array)
        .ok_or_else(|| "submit_plan_draft steps must be an array".to_string())?;
    if steps.is_empty() {
        return Err("submit_plan_draft requires at least one non-empty step title".to_string());
    }
    if steps.len() > MAX_SUBMIT_PLAN_DRAFT_STEPS {
        return Err(format!(
            "submit_plan_draft accepts at most {MAX_SUBMIT_PLAN_DRAFT_STEPS} steps"
        ));
    }
    let mut total = explanation_bytes;
    for step in steps {
        let step = step
            .as_object()
            .ok_or_else(|| "submit_plan_draft step must be an object".to_string())?;
        if step.keys().any(|key| key != "title") {
            return Err("submit_plan_draft step contains an unknown field".to_string());
        }
        let title = step
            .get("title")
            .and_then(serde_json::Value::as_str)
            .ok_or_else(|| "submit_plan_draft step title must be a string".to_string())?
            .trim();
        if title.is_empty() {
            return Err("submit_plan_draft step titles must not be empty".to_string());
        }
        if title.len() > MAX_SUBMIT_PLAN_DRAFT_TITLE_BYTES {
            return Err(format!(
                "submit_plan_draft step title exceeds {MAX_SUBMIT_PLAN_DRAFT_TITLE_BYTES} bytes"
            ));
        }
        total = total
            .checked_add(title.len())
            .ok_or_else(|| "submit_plan_draft text size exceeds the supported limit".to_string())?;
    }
    if total > MAX_SUBMIT_PLAN_DRAFT_TEXT_BYTES {
        return Err(format!(
            "submit_plan_draft text exceeds {MAX_SUBMIT_PLAN_DRAFT_TEXT_BYTES} bytes"
        ));
    }
    Ok(())
}

// ── submit_intent_draft types ─────────────────────────────────────────

/// Arguments for the `submit_intent_draft` tool.
#[derive(Clone, Debug, Deserialize)]
pub struct SubmitIntentDraftArgs {
    /// Structured draft used by the program to resolve a complete IntentSpec.
    pub draft: IntentDraft,
}

// ── submit_task_complete types ────────────────────────────────────────

/// Final outcome of a task declared via `submit_task_complete`.
#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum TaskCompleteResult {
    /// All acceptance criteria are verified to pass.
    Pass,
    /// Task is blocked or at least one required criterion failed.
    Fail,
    /// Existing workspace state already satisfies the criteria; no edits applied.
    NoChangesNeeded,
}

/// One acceptance-check evidence entry attached to `submit_task_complete`.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct TaskCompleteEvidence {
    /// The exact shell command that was executed for verification.
    pub command: String,
    /// Exit code returned by the command.
    pub exit_code: i32,
    /// Short excerpt of stdout/stderr supporting the verdict.
    #[serde(default)]
    pub output_excerpt: Option<String>,
}

/// Arguments for the `submit_task_complete` tool.
#[derive(Clone, Debug, Deserialize)]
pub struct SubmitTaskCompleteArgs {
    /// Final outcome of the task.
    pub result: TaskCompleteResult,
    /// Human-readable summary of what was done and what evidence supports `result`.
    pub summary: String,
    /// Optional acceptance-check evidence. Empty for `no_changes_needed`/analysis tasks.
    #[serde(default)]
    pub evidence: Vec<TaskCompleteEvidence>,
}

// ── request_user_input types ───────────────────────────────────────────

/// A selectable option presented to the user.
#[derive(Clone, Debug, Serialize)]
pub struct UserInputOption {
    /// Short label for the option (1-5 words).
    pub label: String,
    /// Longer description of the impact/tradeoff.
    pub description: String,
}

impl<'de> Deserialize<'de> for UserInputOption {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let value = Value::deserialize(deserializer)?;
        user_input_option_from_value(value).map_err(D::Error::custom)
    }
}

fn user_input_option_from_value(value: Value) -> Result<UserInputOption, String> {
    match value {
        Value::String(label) => Ok(UserInputOption {
            label,
            description: String::new(),
        }),
        Value::Number(number) => Ok(UserInputOption {
            label: number.to_string(),
            description: String::new(),
        }),
        Value::Bool(flag) => Ok(UserInputOption {
            label: flag.to_string(),
            description: String::new(),
        }),
        Value::Array(items) => {
            let label = items
                .first()
                .and_then(scalar_value_to_string)
                .unwrap_or_else(|| "Option".to_string());
            let description = items
                .get(1)
                .and_then(scalar_value_to_string)
                .unwrap_or_default();
            Ok(UserInputOption { label, description })
        }
        Value::Object(map) => user_input_option_from_object(&map),
        Value::Null => Err("option must not be null".to_string()),
    }
}

fn user_input_option_from_object(map: &Map<String, Value>) -> Result<UserInputOption, String> {
    let label = first_object_string(
        map,
        ["label", "value", "text", "name", "title", "id", "key"],
    )
    .ok_or_else(|| {
        "option object must include a label, value, text, name, title, id, or key".to_string()
    })?;
    let description =
        first_object_string(map, ["description", "desc", "detail", "help"]).unwrap_or_default();
    Ok(UserInputOption { label, description })
}

fn first_object_string<const N: usize>(
    map: &Map<String, Value>,
    keys: [&str; N],
) -> Option<String> {
    keys.into_iter()
        .filter_map(|key| map.get(key))
        .find_map(scalar_value_to_string)
        .filter(|value| !value.trim().is_empty())
}

fn scalar_value_to_string(value: &Value) -> Option<String> {
    match value {
        Value::String(text) => Some(text.clone()),
        Value::Number(number) => Some(number.to_string()),
        Value::Bool(flag) => Some(flag.to_string()),
        Value::Null | Value::Array(_) | Value::Object(_) => None,
    }
}

/// A single question to present to the user.
#[derive(Clone, Debug, Serialize)]
pub struct UserInputQuestion {
    /// Machine-readable identifier for the question (unique within a request).
    pub id: String,
    /// Short header displayed above the question (≤12 chars).
    pub header: String,
    /// The full question text (single sentence).
    pub question: String,
    /// Whether to auto-add a "None of the above" option.
    #[serde(default)]
    pub is_other: bool,
    /// Whether to mask user-typed text with '*' (for secrets).
    #[serde(default)]
    pub is_secret: bool,
    /// Predefined options the user can choose from.
    /// When `None` or empty, the question is freeform (text input only).
    #[serde(default)]
    pub options: Option<Vec<UserInputOption>>,
}

impl<'de> Deserialize<'de> for UserInputQuestion {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        #[derive(Deserialize)]
        struct UserInputQuestionInput {
            #[serde(default)]
            id: Option<String>,
            #[serde(default)]
            header: Option<String>,
            #[serde(default, alias = "title")]
            title: Option<String>,
            #[serde(default)]
            question: Option<String>,
            #[serde(default, alias = "message")]
            prompt: Option<String>,
            #[serde(default)]
            is_other: bool,
            #[serde(default)]
            is_secret: bool,
            #[serde(default)]
            options: Option<Vec<UserInputOption>>,
        }

        let input = UserInputQuestionInput::deserialize(deserializer)?;
        let id = first_non_empty([input.id.as_deref()])
            .map(str::to_string)
            .unwrap_or_else(|| "input".to_string());
        let header = first_non_empty([input.header.as_deref(), input.title.as_deref()])
            .map(str::to_string)
            .unwrap_or_else(|| humanize_question_id(&id));
        let question = first_non_empty([
            input.question.as_deref(),
            input.prompt.as_deref(),
            input.title.as_deref(),
        ])
        .map(str::to_string)
        .unwrap_or_else(|| format!("Please provide {}.", header.to_ascii_lowercase()));

        Ok(Self {
            id,
            header: truncate_chars(&header, 12),
            question,
            is_other: input.is_other,
            is_secret: input.is_secret,
            options: input.options,
        })
    }
}

fn first_non_empty<const N: usize>(values: [Option<&str>; N]) -> Option<&str> {
    values
        .into_iter()
        .flatten()
        .find(|value| !value.trim().is_empty())
}

fn humanize_question_id(id: &str) -> String {
    let mut out = String::new();
    for (index, part) in id
        .split(['_', '-', ' '])
        .filter(|part| !part.is_empty())
        .enumerate()
    {
        if index > 0 {
            out.push(' ');
        }
        if index == 0 {
            let mut chars = part.chars();
            if let Some(first) = chars.next() {
                out.extend(first.to_uppercase());
                out.push_str(chars.as_str());
            }
        } else {
            out.push_str(part);
        }
    }

    if out.is_empty() {
        "Input".to_string()
    } else {
        out
    }
}

fn truncate_chars(value: &str, max_chars: usize) -> String {
    value.chars().take(max_chars).collect()
}

/// Arguments for the `request_user_input` tool.
#[derive(Clone, Debug, Deserialize)]
pub struct RequestUserInputArgs {
    /// Questions to present to the user (1-3, prefer 1).
    pub questions: Vec<UserInputQuestion>,
}

/// A single answer, potentially containing the selected option label
/// and/or user notes.
///
/// - If an option was selected: `answers[0]` = selected label
/// - If notes were added: last entry = `"user_note: {text}"`
/// - For freeform questions: `answers[0]` = user-typed text
/// - Empty `answers` means the user skipped the question
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct UserInputAnswer {
    pub answers: Vec<String>,
}

/// User's responses to a set of questions, keyed by question id.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct UserInputResponse {
    pub answers: HashMap<String, UserInputAnswer>,
}

/// A request sent from the tool handler to the session UI, carrying the questions
/// and a one-shot channel for the user's response.
pub struct UserInputRequest {
    /// The call_id of the originating tool invocation (for correlation).
    pub call_id: String,
    /// Questions to display.
    pub questions: Vec<UserInputQuestion>,
    /// Channel the session UI uses to send back the response.
    pub response_tx: oneshot::Sender<UserInputResponse>,
}

impl fmt::Debug for UserInputRequest {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("UserInputRequest")
            .field("call_id", &self.call_id)
            .field("questions", &self.questions)
            .field("response_tx", &"<oneshot::Sender>")
            .finish()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_tool_invocation_creation() {
        let invocation = ToolInvocation::new(
            "call-123",
            "read_file",
            ToolPayload::Function {
                arguments: r#"{"file_path":"/tmp/test.txt"}"#.to_string(),
            },
            PathBuf::from("/work"),
        );
        assert_eq!(invocation.call_id, "call-123");
        assert_eq!(invocation.tool_name, "read_file");
        assert_eq!(invocation.working_dir, PathBuf::from("/work"));
    }

    #[test]
    fn test_tool_output_success() {
        let output = ToolOutput::success("File content here");
        assert!(output.is_success());
        assert_eq!(output.as_text(), Some("File content here"));
    }

    #[test]
    fn test_tool_output_failure() {
        let output = ToolOutput::failure("Error reading file");
        assert!(!output.is_success());
        assert_eq!(output.as_text(), Some("Error reading file"));
    }

    #[test]
    fn test_tool_payload_kinds() {
        let func = ToolPayload::Function {
            arguments: "{}".to_string(),
        };
        assert!(func.is_function());
        assert!(!func.is_custom());

        let custom = ToolPayload::Custom {
            input: "test".to_string(),
        };
        assert!(custom.is_custom());
        assert!(!custom.is_function());
    }

    #[test]
    fn test_log_preview_truncation() {
        let long_content = "x".repeat(1000);
        let output = ToolOutput::success(long_content.clone());
        let preview = output.log_preview();
        assert!(preview.len() < long_content.len());
        assert!(preview.contains("truncated"));
    }

    #[test]
    fn test_log_preview_truncates_at_utf8_boundary() {
        let content = format!("{}├suffix", "x".repeat(498));
        let output = ToolOutput::success(content);
        let preview = output.log_preview();

        assert!(preview.contains("truncated"));
        assert!(preview.is_char_boundary(preview.len()));
    }

    #[test]
    fn test_read_file_args_defaults() {
        let args: Result<ReadFileArgs, _> =
            serde_json::from_str(r#"{"file_path":"/tmp/test.txt"}"#);
        assert!(args.is_ok());
        let args = args.unwrap();
        assert_eq!(args.offset, 1);
        assert_eq!(args.limit, 2000);
    }
}
