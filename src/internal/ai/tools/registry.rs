//! Tool registry for managing and dispatching tool handlers.

use std::{
    collections::HashMap,
    path::{Path, PathBuf},
    sync::Arc,
};

use async_trait::async_trait;

use super::{
    context::{ToolInvocation, ToolKind, ToolOutput, ToolPayload},
    error::{ToolError, ToolResult},
    spec::ToolSpec,
};
use crate::internal::ai::{
    agent::TaskIntent,
    runtime::{ToolBoundaryRuntime, ToolOperation},
};

/// Handler trait that all tools must implement.
///
/// This trait defines the interface for tools that can be invoked by an AI agent.
/// Tools are registered in the ToolRegistry and dispatched based on their name.
///
/// AI user story: each handler should make one capability available to the
/// model with explicit mutability/network classification so the runtime can
/// decide whether the call is safe, needs approval, or must be blocked.
#[async_trait]
pub trait ToolHandler: Send + Sync {
    /// Returns the kind of tool (Function, Mcp, or Custom).
    fn kind(&self) -> ToolKind;

    /// Check if this handler matches the given payload kind.
    fn matches_kind(&self, payload: &ToolPayload) -> bool {
        matches!(
            (self.kind(), payload),
            (ToolKind::Function, ToolPayload::Function { .. })
                | (ToolKind::Custom, ToolPayload::Custom { .. })
                | (ToolKind::Mcp, ToolPayload::Mcp { .. })
        )
    }

    /// Returns `true` if the tool invocation *might* mutate the environment.
    /// This function should be defensive and return `true` if there's any doubt.
    async fn is_mutating(&self, _invocation: &ToolInvocation) -> bool {
        false
    }

    /// Returns `true` if the invocation requires direct network access.
    async fn requires_network(&self, _invocation: &ToolInvocation) -> bool {
        false
    }

    /// Execute the tool with the given invocation context.
    ///
    /// Returns a ToolOutput containing the result to send back to the model.
    async fn handle(&self, invocation: ToolInvocation) -> ToolResult<ToolOutput>;

    /// Returns the JSON Schema for this tool's parameters.
    fn schema(&self) -> ToolSpec;
}

/// Registry for managing tool handlers and dispatching tool calls.
///
/// The ToolRegistry maintains a mapping of tool names to their handlers
/// and provides methods to register, retrieve, and dispatch tools.
///
/// AI user story: the registry is the agent's capability boundary. Adding a
/// handler here changes what an AI can do in `libra code`, so new tools should
/// document the task they enable, whether they mutate state, and what evidence
/// they return to the model.
#[derive(Clone)]
pub struct ToolRegistry {
    /// Map of tool name to handler implementation.
    handlers: HashMap<String, Arc<dyn ToolHandler>>,
    /// Working directory for file operations.
    working_dir: PathBuf,
    /// Absolute path aliases that should be rebased into `working_dir`.
    path_aliases: Vec<(PathBuf, PathBuf)>,
    /// Optional runtime boundary policy and audit pipeline.
    hardening: Option<ToolBoundaryRuntime>,
}

impl ToolRegistry {
    /// Create a new empty ToolRegistry.
    pub fn new() -> Self {
        Self::try_new().unwrap_or_else(|err| {
            panic!("failed to resolve current working directory for ToolRegistry::new(): {err}")
        })
    }

    /// Try to create a new empty ToolRegistry from the current working directory.
    pub fn try_new() -> std::io::Result<Self> {
        Ok(Self {
            handlers: HashMap::new(),
            working_dir: std::env::current_dir()?,
            path_aliases: Vec::new(),
            hardening: None,
        })
    }

    /// Create a new ToolRegistry with a specific working directory.
    pub fn with_working_dir(working_dir: PathBuf) -> Self {
        Self {
            handlers: HashMap::new(),
            working_dir,
            path_aliases: Vec::new(),
            hardening: None,
        }
    }

    /// Clone this registry while rebasing all tool dispatch onto a new working directory.
    pub fn clone_with_working_dir(&self, working_dir: PathBuf) -> Self {
        Self {
            handlers: self.handlers.clone(),
            working_dir,
            path_aliases: self.path_aliases.clone(),
            hardening: self.hardening.clone(),
        }
    }

    /// Clone this registry while allowing one outside absolute path to resolve
    /// to the new working directory. This keeps tools sandboxed while handling
    /// provider calls that reuse the user-facing repository path inside an
    /// isolated task worktree.
    pub fn clone_with_working_dir_and_alias(
        &self,
        working_dir: PathBuf,
        alias_from: PathBuf,
    ) -> Self {
        let mut path_aliases = self.path_aliases.clone();
        path_aliases.push((alias_from, working_dir.clone()));
        Self {
            handlers: self.handlers.clone(),
            working_dir,
            path_aliases,
            hardening: self.hardening.clone(),
        }
    }

    /// Attach runtime tool-boundary policy, audit, and redaction.
    pub fn with_hardening(mut self, hardening: ToolBoundaryRuntime) -> Self {
        self.hardening = Some(hardening);
        self
    }

    /// Runtime boundary policy and audit channel attached to this registry.
    pub fn hardening(&self) -> Option<&ToolBoundaryRuntime> {
        self.hardening.as_ref()
    }

    /// Register a tool handler with the given name.
    pub fn register(&mut self, name: impl Into<String>, handler: Arc<dyn ToolHandler>) {
        let name = name.into();
        if self.handlers.insert(name.clone(), handler).is_some() {
            tracing::warn!("Overwriting handler for tool: {name}");
        }
    }

    /// Register multiple tool handlers from a map.
    pub fn register_all(&mut self, handlers: HashMap<String, Arc<dyn ToolHandler>>) {
        for (name, handler) in handlers {
            self.register(name, handler);
        }
    }

    /// Get a handler by name.
    pub fn handler(&self, name: &str) -> Option<Arc<dyn ToolHandler>> {
        self.handlers.get(name).cloned()
    }

    /// Get all registered tool names.
    pub fn tool_names(&self) -> Vec<String> {
        self.handlers.keys().cloned().collect()
    }

    /// Return the registered tool names allowed for a classified task intent.
    pub fn filter_by_intent(&self, intent: TaskIntent) -> Vec<String> {
        let mut tools = self
            .handlers
            .keys()
            .filter(|name| tool_allowed_for_intent(name, intent))
            .cloned()
            .collect::<Vec<_>>();
        tools.sort();
        tools
    }

    /// Get all tool specs as a vector of JSON values.
    pub fn tool_specs(&self) -> Vec<ToolSpec> {
        self.handlers
            .values()
            .map(|handler| handler.schema())
            .collect()
    }

    /// Pre-filter the registry's tool specs against an [`AgentExecutionSpec`]
    /// and a [`PermissionRuleset`] (OC-Phase 2 P2.3).
    ///
    /// Filtering proceeds in two stages:
    ///
    /// 1. **Spec gating**: the agent's [`ToolSelection`] decides which tool
    ///    names are even candidates.
    ///    - `Inherit`: every registered tool is a candidate (the runtime
    ///      treats Inherit as "use whatever the session would normally
    ///      expose"; for sub-agents OC-Phase 3 narrows this to deny-everything,
    ///      but at the registry layer Inherit means "no spec-level filter").
    ///    - `Allow(tools)`: only the listed names pass the gate.
    ///    - `Deny(tools)`: every registered tool except the listed names
    ///      passes the gate.
    /// 2. **Ruleset pre-filter**: candidates are stripped via
    ///    [`crate::internal::ai::permission::disabled`] so any tool covered
    ///    by a `pattern == "*"` deny rule is removed from the model's
    ///    schema entirely. The opencode rationale: once a tool name reaches
    ///    the schema, the model will keep retrying to use it; pre-filtering
    ///    avoids the noise + token spend of a deny-after-call response.
    ///
    /// Returns the surviving [`ToolSpec`]s. The order is unspecified — the
    /// registry stores handlers in a [`HashMap`], so callers that need a
    /// stable order must sort the result themselves (the tests do).
    ///
    /// Compatibility: with an empty ruleset and `ToolSelection::Inherit` the
    /// returned set of tool names equals [`Self::tool_specs`]'s set; only
    /// the iteration order is unspecified. That is the flag-off baseline
    /// OC-Phase 2 P2.3 must preserve.
    pub fn available_for(
        &self,
        spec: &crate::internal::ai::agent::profile::AgentExecutionSpec,
        ruleset: &crate::internal::ai::permission::PermissionRuleset,
    ) -> Vec<ToolSpec> {
        use crate::internal::ai::agent::profile::ToolSelection;

        let candidates: Vec<(&String, &Arc<dyn ToolHandler>)> = match &spec.tools {
            ToolSelection::Inherit => self.handlers.iter().collect(),
            ToolSelection::Allow(names) => self
                .handlers
                .iter()
                .filter(|(tool_name, _)| names.iter().any(|n| n == *tool_name))
                .collect(),
            ToolSelection::Deny(names) => self
                .handlers
                .iter()
                .filter(|(tool_name, _)| !names.iter().any(|n| n == *tool_name))
                .collect(),
        };

        // Compute the schema-level disabled set across all candidates.
        let candidate_names: Vec<&str> = candidates.iter().map(|(name, _)| name.as_str()).collect();
        let disabled = crate::internal::ai::permission::disabled(&candidate_names, ruleset);

        candidates
            .into_iter()
            .filter(|(name, _)| !disabled.contains(name.as_str()))
            .map(|(_, handler)| handler.schema())
            .collect()
    }

    /// Get all tool specs as JSON values for API requests.
    pub fn tool_specs_json(&self) -> Vec<serde_json::Value> {
        self.tool_specs()
            .into_iter()
            .map(|spec| spec.to_json())
            .collect()
    }

    /// Dispatch a tool invocation to the appropriate handler.
    ///
    /// This method validates the tool name, checks payload compatibility,
    /// and executes the tool.
    pub async fn dispatch(&self, mut invocation: ToolInvocation) -> ToolResult<ToolOutput> {
        let tool_name = invocation.tool_name.clone();

        let handler = self
            .handler(&tool_name)
            .ok_or_else(|| ToolError::ToolNotFound(tool_name.clone()))?;

        if !handler.matches_kind(&invocation.payload) {
            return Err(ToolError::IncompatiblePayload(format!(
                "Tool {tool_name} received incompatible payload type"
            )));
        }

        // The registry working directory is the single source of truth for sandboxing.
        // Ignore any caller-provided working_dir to prevent sandbox bypass.
        invocation.working_dir = self.working_dir.clone();
        invocation.payload = rebase_payload_path_aliases(invocation.payload, &self.path_aliases)?;

        let mutates_state = handler.is_mutating(&invocation).await;
        let requires_network = handler.requires_network(&invocation).await;

        if let Some(hardening) = &self.hardening {
            let operation = ToolOperation::tool(tool_name.clone(), mutates_state, requires_network);
            let decision = hardening.decide(&operation);
            hardening
                .append_audit(
                    format!("tool_boundary.{}", tool_name),
                    format!(
                        "decision={} approval_required={} reason={} payload={}",
                        if decision.allowed { "allow" } else { "deny" },
                        decision.approval_required,
                        decision.reason,
                        invocation.log_payload()
                    ),
                )
                .await
                .map_err(|error| {
                    ToolError::ExecutionFailed(format!(
                        "failed to persist tool boundary audit event: {error}"
                    ))
                })?;

            if !decision.allowed {
                return Err(ToolError::ExecutionFailed(decision.reason));
            }

            let result = handler.handle(invocation).await.map(|output| {
                redact_workspace_paths_in_output(output, &self.working_dir, &self.path_aliases)
            });
            let summary = match &result {
                Ok(output) => format!(
                    "success={} output={}",
                    output.is_success(),
                    output.log_preview()
                ),
                Err(error) => format!("error={error}"),
            };
            hardening
                .append_audit(format!("tool_result.{}", tool_name), summary)
                .await
                .map_err(|error| {
                    ToolError::ExecutionFailed(format!(
                        "failed to persist tool result audit event: {error}"
                    ))
                })?;
            hardening.flush_audit().await.map_err(|error| {
                ToolError::ExecutionFailed(format!("failed to flush tool audit sink: {error}"))
            })?;
            return result;
        }

        handler.handle(invocation).await.map(|output| {
            redact_workspace_paths_in_output(output, &self.working_dir, &self.path_aliases)
        })
    }

    /// Return the handler's conservative mutability classification without
    /// dispatching it. Runtime owners use this immediately before invocation
    /// to decide whether a cancellation token may interrupt the call.
    pub async fn invocation_may_mutate(&self, invocation: &ToolInvocation) -> ToolResult<bool> {
        let tool_name = invocation.tool_name.clone();
        let handler = self
            .handler(&tool_name)
            .ok_or_else(|| ToolError::ToolNotFound(tool_name.clone()))?;
        if !handler.matches_kind(&invocation.payload) {
            return Err(ToolError::IncompatiblePayload(format!(
                "Tool {tool_name} received incompatible payload type"
            )));
        }
        let handler_mutates = handler.is_mutating(invocation).await;
        let boundary_mutates = self.hardening.as_ref().is_some_and(|hardening| {
            hardening.operation_may_mutate(&ToolOperation::tool(tool_name, handler_mutates, false))
        });
        Ok(handler_mutates || boundary_mutates)
    }

    /// Get the current working directory.
    pub fn working_dir(&self) -> &std::path::Path {
        &self.working_dir
    }

    /// Set the working directory.
    pub fn set_working_dir(&mut self, dir: std::path::PathBuf) {
        self.working_dir = dir;
    }

    /// Replace or install runtime hardening policy for subsequent dispatch calls.
    pub fn set_hardening(&mut self, hardening: ToolBoundaryRuntime) {
        self.hardening = Some(hardening);
    }

    /// Check if a tool is registered.
    pub fn contains_tool(&self, name: &str) -> bool {
        self.handlers.contains_key(name)
    }

    /// Get the number of registered tools.
    pub fn len(&self) -> usize {
        self.handlers.len()
    }

    /// Check if the registry is empty.
    pub fn is_empty(&self) -> bool {
        self.handlers.is_empty()
    }
}

fn tool_allowed_for_intent(tool_name: &str, intent: TaskIntent) -> bool {
    match intent {
        TaskIntent::Question | TaskIntent::Review => is_read_only_or_semantic_tool(tool_name),
        TaskIntent::Command => is_read_only_or_semantic_tool(tool_name) || tool_name == "shell",
        TaskIntent::BugFix
        | TaskIntent::Feature
        | TaskIntent::Refactor
        | TaskIntent::Test
        | TaskIntent::Documentation
        | TaskIntent::Chore
        | TaskIntent::Unknown => true,
    }
}

fn is_read_only_or_semantic_tool(tool_name: &str) -> bool {
    matches!(
        tool_name,
        "read_file"
            | "list_dir"
            | "grep_files"
            | "search_files"
            | "web_search"
            | "list_symbols"
            | "read_symbol"
            | "find_references"
            | "trace_callers"
            | "request_user_input"
            | "list_tasks"
            | "list_runs"
            | "list_plans"
            | "list_evidences"
            | "list_patchsets"
            | "list_decisions"
            | "list_context_snapshots"
            | "list_tool_invocations"
            | "list_provenances"
    )
}

fn rebase_payload_path_aliases(
    payload: ToolPayload,
    aliases: &[(PathBuf, PathBuf)],
) -> ToolResult<ToolPayload> {
    if aliases.is_empty() {
        return Ok(payload);
    }

    let ToolPayload::Function { arguments } = payload else {
        return Ok(payload);
    };

    let Ok(mut value) = serde_json::from_str::<serde_json::Value>(&arguments) else {
        return Ok(ToolPayload::Function { arguments });
    };
    rebase_path_fields(&mut value, aliases);
    let arguments = serde_json::to_string(&value).map_err(|err| {
        ToolError::InvalidArguments(format!("failed to rewrite path aliases: {err}"))
    })?;
    Ok(ToolPayload::Function { arguments })
}

fn rebase_path_fields(value: &mut serde_json::Value, aliases: &[(PathBuf, PathBuf)]) {
    let Some(object) = value.as_object_mut() else {
        return;
    };

    for key in ["file_path", "dir_path", "path", "workdir"] {
        let Some(field) = object.get_mut(key) else {
            continue;
        };
        let Some(raw) = field.as_str() else {
            continue;
        };
        if let Some(rebased) = rebase_path_alias(raw, aliases) {
            *field = serde_json::Value::String(rebased);
        }
    }
}

fn rebase_path_alias(raw: &str, aliases: &[(PathBuf, PathBuf)]) -> Option<String> {
    let path = Path::new(raw);
    if !path.is_absolute() {
        return None;
    }

    for (from, to) in aliases {
        if let Ok(suffix) = path.strip_prefix(from) {
            return Some(to.join(suffix).display().to_string());
        }
    }
    None
}

fn redact_workspace_paths_in_output(
    output: ToolOutput,
    working_dir: &Path,
    aliases: &[(PathBuf, PathBuf)],
) -> ToolOutput {
    if aliases.is_empty() {
        return output;
    }

    match output {
        ToolOutput::Function {
            content,
            success,
            metadata,
        } => {
            let content = redact_workspace_path_mentions(&content, working_dir);
            ToolOutput::Function {
                content,
                success,
                metadata,
            }
        }
        other => other,
    }
}

fn redact_workspace_path_mentions(content: &str, working_dir: &Path) -> String {
    let mut needles = vec![working_dir.display().to_string()];
    if let Ok(canonical) = std::fs::canonicalize(working_dir) {
        let canonical = canonical.display().to_string();
        if !needles.iter().any(|needle| needle == &canonical) {
            needles.push(canonical);
        }
    }
    needles.sort_by_key(|needle| std::cmp::Reverse(needle.len()));

    let mut redacted = content.to_string();
    for needle in needles {
        if !needle.is_empty() {
            redacted = redacted.replace(&needle, ".");
        }
    }
    redacted
}

impl Default for ToolRegistry {
    fn default() -> Self {
        Self::new()
    }
}

/// Builder for constructing a ToolRegistry with multiple handlers.
pub struct ToolRegistryBuilder {
    registry: ToolRegistry,
}

impl ToolRegistryBuilder {
    /// Create a new ToolRegistryBuilder.
    pub fn new() -> Self {
        Self::try_new().unwrap_or_else(|err| {
            panic!(
                "failed to resolve current working directory for ToolRegistryBuilder::new(): {err}"
            )
        })
    }

    /// Try to create a new ToolRegistryBuilder.
    pub fn try_new() -> std::io::Result<Self> {
        Ok(Self {
            registry: ToolRegistry::try_new()?,
        })
    }

    /// Create a new ToolRegistryBuilder with a specific working directory.
    pub fn with_working_dir(working_dir: std::path::PathBuf) -> Self {
        Self {
            registry: ToolRegistry::with_working_dir(working_dir),
        }
    }

    /// Register a tool handler.
    pub fn register(mut self, name: impl Into<String>, handler: Arc<dyn ToolHandler>) -> Self {
        self.registry.register(name, handler);
        self
    }

    /// Set the working directory.
    pub fn working_dir(mut self, dir: std::path::PathBuf) -> Self {
        self.registry.set_working_dir(dir);
        self
    }

    /// Attach runtime hardening policy to the built registry.
    pub fn hardening(mut self, hardening: ToolBoundaryRuntime) -> Self {
        self.registry.set_hardening(hardening);
        self
    }

    /// Build the ToolRegistry.
    pub fn build(self) -> ToolRegistry {
        self.registry
    }
}

impl Default for ToolRegistryBuilder {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use tempfile::TempDir;

    use super::*;
    use crate::internal::ai::tools::{
        context::ToolPayload,
        handlers::{ListDirHandler, ReadFileHandler},
    };

    // Mock handler for testing
    struct MockHandler;

    #[async_trait]
    impl ToolHandler for MockHandler {
        fn kind(&self) -> ToolKind {
            ToolKind::Function
        }

        async fn handle(&self, _invocation: ToolInvocation) -> ToolResult<ToolOutput> {
            Ok(ToolOutput::success("mock result"))
        }

        fn schema(&self) -> ToolSpec {
            ToolSpec::new("mock", "A mock tool")
        }
    }

    struct MutatingMockHandler;

    #[async_trait]
    impl ToolHandler for MutatingMockHandler {
        fn kind(&self) -> ToolKind {
            ToolKind::Function
        }

        async fn is_mutating(&self, _invocation: &ToolInvocation) -> bool {
            true
        }

        async fn handle(&self, _invocation: ToolInvocation) -> ToolResult<ToolOutput> {
            Ok(ToolOutput::success("token=handler-secret"))
        }

        fn schema(&self) -> ToolSpec {
            ToolSpec::new("mutating_mock", "A mutating mock tool")
        }
    }

    #[tokio::test]
    async fn test_registry_registration() {
        let mut registry = ToolRegistry::with_working_dir(std::path::PathBuf::from("/tmp"));
        registry.register("mock", Arc::new(MockHandler));

        assert!(registry.contains_tool("mock"));
        assert!(!registry.contains_tool("nonexistent"));
        assert_eq!(registry.len(), 1);
    }

    #[tokio::test]
    async fn test_registry_dispatch() {
        let mut registry = ToolRegistry::with_working_dir(std::path::PathBuf::from("/tmp"));
        registry.register("mock", Arc::new(MockHandler));

        let invocation = ToolInvocation::new(
            "call-1",
            "mock",
            ToolPayload::Function {
                arguments: "{}".to_string(),
            },
            std::path::PathBuf::from("/tmp"),
        );

        let result = registry.dispatch(invocation).await;
        assert!(result.is_ok());
        assert_eq!(result.unwrap().as_text(), Some("mock result"));
    }

    #[tokio::test]
    async fn test_registry_tool_not_found() {
        let registry = ToolRegistry::with_working_dir(std::path::PathBuf::from("/tmp"));

        let invocation = ToolInvocation::new(
            "call-1",
            "nonexistent",
            ToolPayload::Function {
                arguments: "{}".to_string(),
            },
            std::path::PathBuf::from("/tmp"),
        );

        let result = registry.dispatch(invocation).await;
        assert!(matches!(result, Err(ToolError::ToolNotFound(_))));
    }

    #[tokio::test]
    async fn test_registry_incompatible_payload() {
        let mut registry = ToolRegistry::with_working_dir(std::path::PathBuf::from("/tmp"));
        registry.register("mock", Arc::new(MockHandler));

        // MockHandler is Function kind, but we send Custom payload
        let invocation = ToolInvocation::new(
            "call-1",
            "mock",
            ToolPayload::Custom {
                input: "test".to_string(),
            },
            std::path::PathBuf::from("/tmp"),
        );

        let result = registry.dispatch(invocation).await;
        assert!(matches!(result, Err(ToolError::IncompatiblePayload(_))));
    }

    #[test]
    fn test_tool_specs() {
        let mut registry = ToolRegistry::with_working_dir(std::path::PathBuf::from("/tmp"));
        registry.register("mock", Arc::new(MockHandler));

        let specs = registry.tool_specs();
        assert_eq!(specs.len(), 1);
        assert_eq!(specs[0].function.name, "mock");

        let specs_json = registry.tool_specs_json();
        assert_eq!(specs_json.len(), 1);
        assert_eq!(specs_json[0]["function"]["name"], "mock");
    }

    #[test]
    fn test_registry_builder() {
        let registry = ToolRegistryBuilder::with_working_dir(std::path::PathBuf::from("/tmp"))
            .register("mock", Arc::new(MockHandler))
            .working_dir("/tmp".into())
            .build();

        assert!(registry.contains_tool("mock"));
        assert_eq!(registry.working_dir(), std::path::Path::new("/tmp"));
    }

    #[test]
    fn test_clone_with_working_dir_preserves_handlers() {
        let mut registry =
            ToolRegistry::with_working_dir(std::path::PathBuf::from("/tmp/original"));
        registry.register("mock", Arc::new(MockHandler));

        let cloned = registry.clone_with_working_dir(std::path::PathBuf::from("/tmp/cloned"));

        assert!(cloned.contains_tool("mock"));
        assert_eq!(cloned.working_dir(), std::path::Path::new("/tmp/cloned"));
    }

    #[tokio::test]
    async fn dispatch_rebases_original_workspace_alias_into_task_worktree() {
        let original = TempDir::new().unwrap();
        let task_worktree = TempDir::new().unwrap();
        tokio::fs::create_dir_all(task_worktree.path().join("src"))
            .await
            .unwrap();
        tokio::fs::write(task_worktree.path().join("src/lib.rs"), "pub fn ok() {}")
            .await
            .unwrap();

        let mut registry = ToolRegistry::with_working_dir(original.path().to_path_buf());
        registry.register("list_dir", Arc::new(ListDirHandler));
        let task_registry = registry.clone_with_working_dir_and_alias(
            task_worktree.path().to_path_buf(),
            original.path().to_path_buf(),
        );

        let invocation = ToolInvocation::new(
            "call-list",
            "list_dir",
            ToolPayload::Function {
                arguments: serde_json::json!({
                    "dir_path": original.path().join("src"),
                    "offset": 1,
                    "limit": 25,
                    "depth": 1
                })
                .to_string(),
            },
            original.path().to_path_buf(),
        );

        let output = task_registry.dispatch(invocation).await.unwrap();

        let text = output.as_text().unwrap();
        assert!(text.contains("lib.rs"));
        assert!(text.contains("Absolute path: ./src"));
        assert!(!text.contains(&task_worktree.path().display().to_string()));
        assert!(!text.contains(&original.path().join("src").display().to_string()));
    }

    #[tokio::test]
    async fn test_registry_dispatch_real_handlers() {
        let temp_dir = TempDir::new().unwrap();
        let working_dir = temp_dir.path().to_path_buf();
        let file_path = working_dir.join("test.txt");
        tokio::fs::write(&file_path, "hello").await.unwrap();

        let mut registry = ToolRegistry::with_working_dir(working_dir.clone());
        registry.register("read_file", Arc::new(ReadFileHandler));
        registry.register("list_dir", Arc::new(ListDirHandler));

        let read_invocation = ToolInvocation::new(
            "call-1",
            "read_file",
            ToolPayload::Function {
                arguments: serde_json::json!({
                    "file_path": file_path,
                    "offset": 1,
                    "limit": 10
                })
                .to_string(),
            },
            working_dir.clone(),
        );

        let read_result = registry.dispatch(read_invocation).await;
        assert!(read_result.is_ok());
        let read_text = read_result.unwrap().as_text().unwrap().to_string();
        assert!(read_text.contains("hello"));

        let list_invocation = ToolInvocation::new(
            "call-2",
            "list_dir",
            ToolPayload::Function {
                arguments: serde_json::json!({
                    "dir_path": working_dir,
                    "max_depth": 1
                })
                .to_string(),
            },
            temp_dir.path().to_path_buf(),
        );

        let list_result = registry.dispatch(list_invocation).await;
        assert!(list_result.is_ok());
    }

    #[tokio::test]
    async fn registry_hardening_denies_observer_mutating_tools() {
        let sink = Arc::new(crate::internal::ai::runtime::InMemoryAuditSink::default());
        let hardening = ToolBoundaryRuntime::new(
            uuid::Uuid::new_v4(),
            crate::internal::ai::runtime::PrincipalContext {
                principal_id: "observer".to_string(),
                role: crate::internal::ai::runtime::PrincipalRole::Observer,
            },
            crate::internal::ai::runtime::ToolBoundaryPolicy::default_runtime(),
            crate::internal::ai::runtime::SecretRedactor::default_runtime(),
            sink.clone(),
        );
        let mut registry = ToolRegistry::with_working_dir(std::path::PathBuf::from("/tmp"))
            .with_hardening(hardening);
        registry.register("apply_patch", Arc::new(MutatingMockHandler));

        let result = registry
            .dispatch(ToolInvocation::new(
                "call-1",
                "apply_patch",
                ToolPayload::Function {
                    arguments: "{}".to_string(),
                },
                std::path::PathBuf::from("/tmp"),
            ))
            .await;

        assert!(matches!(result, Err(ToolError::ExecutionFailed(_))));
        let events = sink.events().await;
        assert_eq!(events.len(), 1);
        assert!(events[0].redacted_summary.contains("decision=deny"));
    }

    #[tokio::test]
    async fn registry_hardening_audits_and_redacts_tool_payload_and_result() {
        let sink = Arc::new(crate::internal::ai::runtime::InMemoryAuditSink::default());
        let hardening = ToolBoundaryRuntime::system(uuid::Uuid::new_v4(), sink.clone());
        let mut registry = ToolRegistry::with_working_dir(std::path::PathBuf::from("/tmp"))
            .with_hardening(hardening);
        registry.register("shell", Arc::new(MutatingMockHandler));

        let result = registry
            .dispatch(ToolInvocation::new(
                "call-1",
                "shell",
                ToolPayload::Function {
                    arguments: serde_json::json!({"command":"echo token=payload-secret"})
                        .to_string(),
                },
                std::path::PathBuf::from("/tmp"),
            ))
            .await
            .unwrap();

        assert!(result.is_success());
        let events = sink.events().await;
        assert_eq!(events.len(), 2);
        assert!(
            events
                .iter()
                .all(|event| !event.redacted_summary.contains("secret"))
        );
        assert!(
            events
                .iter()
                .any(|event| event.redacted_summary.contains("[REDACTED]"))
        );
    }

    // ─── OC-Phase 2 P2.3: ToolRegistry::available_for ─────────────────────

    /// Build a registry containing every member of `permission::EDIT_TOOLS`
    /// (`apply_patch`, `write_file`, `patch`), a read-only tool, and `grep`.
    /// Registering all three edit aliases lets the pre-filter scenarios
    /// pin opencode.md's full claim that "[{edit:deny}], apply_patch /
    /// write_file / patch 全部不出现在 schema 中" rather than only the
    /// `apply_patch` member.
    fn available_for_registry() -> ToolRegistry {
        struct EditMock(&'static str);
        #[async_trait]
        impl ToolHandler for EditMock {
            fn kind(&self) -> ToolKind {
                ToolKind::Function
            }
            async fn handle(&self, _invocation: ToolInvocation) -> ToolResult<ToolOutput> {
                Ok(ToolOutput::success("ok"))
            }
            fn schema(&self) -> ToolSpec {
                ToolSpec::new(self.0, "Edit-class mock")
            }
        }
        struct ReadFileMock;
        #[async_trait]
        impl ToolHandler for ReadFileMock {
            fn kind(&self) -> ToolKind {
                ToolKind::Function
            }
            async fn handle(&self, _invocation: ToolInvocation) -> ToolResult<ToolOutput> {
                Ok(ToolOutput::success("ok"))
            }
            fn schema(&self) -> ToolSpec {
                ToolSpec::new("read_file", "Read-only file access")
            }
        }
        struct GrepMock;
        #[async_trait]
        impl ToolHandler for GrepMock {
            fn kind(&self) -> ToolKind {
                ToolKind::Function
            }
            async fn handle(&self, _invocation: ToolInvocation) -> ToolResult<ToolOutput> {
                Ok(ToolOutput::success("ok"))
            }
            fn schema(&self) -> ToolSpec {
                ToolSpec::new("grep", "Search text")
            }
        }
        let mut registry = ToolRegistry::with_working_dir(std::path::PathBuf::from("/tmp"));
        registry.register("apply_patch", Arc::new(EditMock("apply_patch")));
        registry.register("write_file", Arc::new(EditMock("write_file")));
        registry.register("patch", Arc::new(EditMock("patch")));
        registry.register("read_file", Arc::new(ReadFileMock));
        registry.register("grep", Arc::new(GrepMock));
        registry
    }

    fn inherit_spec() -> crate::internal::ai::agent::profile::AgentExecutionSpec {
        crate::internal::ai::agent::profile::AgentExecutionSpec {
            name: "anything".to_string(),
            ..crate::internal::ai::agent::profile::AgentExecutionSpec::default()
        }
    }

    fn names_of(specs: &[ToolSpec]) -> Vec<String> {
        let mut names: Vec<String> = specs.iter().map(|s| s.function.name.clone()).collect();
        names.sort();
        names
    }

    /// Scenario: with `ToolSelection::Inherit` and an empty ruleset, the
    /// pre-filter must surface every registered tool. This is the flag-off
    /// baseline — OC-Phase 2 P2.3 must not change the model's view when no
    /// rules are configured.
    #[test]
    fn available_for_inherit_with_empty_ruleset_matches_full_registry() {
        let registry = available_for_registry();
        let spec = inherit_spec();
        let ruleset = Vec::new();

        let surviving = names_of(&registry.available_for(&spec, &ruleset));
        let baseline = names_of(&registry.tool_specs());
        assert_eq!(surviving, baseline);
    }

    /// Scenario: a `[{edit:*: deny}]` ruleset removes every member of
    /// `permission::EDIT_TOOLS` from the schema, while non-edit tools stay.
    /// All three EDIT_TOOLS aliases are registered in the fixture so this
    /// test pins the full opencode.md claim, not just the `apply_patch`
    /// member of the group.
    #[test]
    fn available_for_strips_edit_tools_under_wildcard_deny() {
        use crate::internal::ai::permission::{EDIT_TOOLS, PermissionAction, PermissionRule};

        let registry = available_for_registry();
        let spec = inherit_spec();
        let ruleset = vec![PermissionRule::new("edit", "*", PermissionAction::Deny)];

        let surviving = names_of(&registry.available_for(&spec, &ruleset));
        for tool in EDIT_TOOLS {
            assert!(
                !surviving.contains(&(*tool).to_string()),
                "edit tool `{tool}` must be stripped under `edit:*:deny`"
            );
        }
        assert!(surviving.contains(&"read_file".to_string()));
        assert!(surviving.contains(&"grep".to_string()));
    }

    /// Scenario: a partial-pattern allow (`{edit: "src/**": allow}`) layered
    /// over the wildcard deny puts the edit tools back in the schema. The
    /// pre-filter uses `findLast`, so the partial allow wins for the
    /// permission key.
    #[test]
    fn available_for_keeps_tool_when_last_rule_is_partial_pattern_allow() {
        use crate::internal::ai::permission::{PermissionAction, PermissionRule};

        let registry = available_for_registry();
        let spec = inherit_spec();
        let ruleset = vec![
            PermissionRule::new("edit", "*", PermissionAction::Deny),
            PermissionRule::new("edit", "src/**", PermissionAction::Allow),
        ];

        let surviving = names_of(&registry.available_for(&spec, &ruleset));
        assert!(surviving.contains(&"apply_patch".to_string()));
    }

    /// Scenario: `[{*:deny}, {grep:allow}]` — every tool except `grep` is
    /// stripped from the schema.
    #[test]
    fn available_for_per_tool_override_against_wildcard_deny() {
        use crate::internal::ai::permission::{PermissionAction, PermissionRule};

        let registry = available_for_registry();
        let spec = inherit_spec();
        let ruleset = vec![
            PermissionRule::new("*", "*", PermissionAction::Deny),
            PermissionRule::new("grep", "*", PermissionAction::Allow),
        ];

        let surviving = names_of(&registry.available_for(&spec, &ruleset));
        assert_eq!(surviving, vec!["grep".to_string()]);
    }

    /// Scenario: `ToolSelection::Allow(["read_file"])` narrows the schema
    /// to a single tool even before the ruleset stage runs. `apply_patch`
    /// and `grep` are not in the spec's allow list.
    #[test]
    fn available_for_spec_allow_list_narrows_candidates() {
        use crate::internal::ai::{agent::profile::ToolSelection, permission::PermissionRule};

        let registry = available_for_registry();
        let mut spec = inherit_spec();
        spec.tools = ToolSelection::Allow(vec!["read_file".to_string()]);
        let ruleset: Vec<PermissionRule> = Vec::new();

        let surviving = names_of(&registry.available_for(&spec, &ruleset));
        assert_eq!(surviving, vec!["read_file".to_string()]);
    }

    /// Scenario (OC-Phase 3 P3.1 flag-off invariant): the registry does
    /// not register the `task` tool by default. The schema lives on
    /// `ToolSpec::task()` for future P3.2 wiring, but the registry stays
    /// pre-dispatcher. This guard catches a regression where someone wires
    /// the task tool unconditionally and bypasses the
    /// `code.multi_agent.enabled` gate.
    #[test]
    fn registry_does_not_expose_task_tool_in_flag_off_default() {
        let registry = available_for_registry();
        let spec = inherit_spec();
        let ruleset = Vec::new();
        let surviving = names_of(&registry.available_for(&spec, &ruleset));
        assert!(
            !surviving.contains(&"task".to_string()),
            "OC-Phase 3 P3.1 invariant: task must not appear until the \
             dispatcher lands and is gated; got surviving = {surviving:?}"
        );
        // Also guard the unfiltered tool list — `available_for` could
        // hide it via an early gate; the registry itself must never
        // contain the handler in default builds.
        let registered = registry.tool_names();
        assert!(
            !registered.contains(&"task".to_string()),
            "registry must not register a `task` handler by default; \
             got tool_names = {registered:?}"
        );
    }

    /// Scenario: `ToolSelection::Deny(["grep"])` exposes every tool except
    /// `grep`. Spec deny is independent of the ruleset; both layers must
    /// match for a tool to be visible.
    #[test]
    fn available_for_spec_deny_list_excludes_tool() {
        use crate::internal::ai::{agent::profile::ToolSelection, permission::PermissionRule};

        let registry = available_for_registry();
        let mut spec = inherit_spec();
        spec.tools = ToolSelection::Deny(vec!["grep".to_string()]);
        let ruleset: Vec<PermissionRule> = Vec::new();

        let surviving = names_of(&registry.available_for(&spec, &ruleset));
        assert!(!surviving.contains(&"grep".to_string()));
        assert!(surviving.contains(&"read_file".to_string()));
        assert!(surviving.contains(&"apply_patch".to_string()));
    }
}
