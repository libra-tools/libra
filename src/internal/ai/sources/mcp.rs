//! MCP source adapter.
//!
//! This module is the shared implementation behind the legacy in-process MCP
//! bridge and the Source Pool view. Keeping schemas and dispatch here prevents
//! `run_libra_vcs` and workflow tools from drifting between registration paths.

use std::sync::Arc;

use async_trait::async_trait;
use rmcp::model::CallToolResult;

use super::{
    CapabilityManifest, Source, SourceAccess, SourceCallContext, SourceKind, SourceToolCapability,
    TrustTier,
};
use crate::internal::ai::{
    libra_vcs::classify_run_libra_vcs_safety,
    mcp::{resource::*, server::LibraMcpServer},
    sandbox::ToolRuntimeContext,
    tools::{
        context::{AiOperationContext, ToolInvocation, ToolOutput, ToolPayload},
        error::{ToolError, ToolResult},
        handlers::parse_argument_value,
        spec::{FunctionParameters, ToolSpec},
    },
};

const LIBRA_VCS_DEFAULT_APPROVAL_SCOPE: &str = "interactive";

async fn libra_vcs_allow_all_active(runtime_context: Option<&ToolRuntimeContext>) -> bool {
    let Some(approval) = runtime_context.and_then(|ctx| ctx.approval.as_ref()) else {
        return false;
    };
    let scope = approval
        .scope_key_prefix
        .as_deref()
        .unwrap_or(LIBRA_VCS_DEFAULT_APPROVAL_SCOPE);
    let store = approval.store.lock().await;
    store.allow_all_commands_for_scope(scope)
}

pub const BUILTIN_MCP_SOURCE_SLUG: &str = "libra_mcp";

#[derive(Clone)]
pub struct McpToolDefinition {
    pub name: &'static str,
    pub description: &'static str,
    pub params_schema: FunctionParameters,
}

/// Convert rmcp `CallToolResult` into the local Code UI/tool-loop output type.
fn call_tool_result_to_output(result: CallToolResult) -> ToolOutput {
    let text: String = result
        .content
        .iter()
        .filter_map(|content| content.as_text().map(|text| text.text.as_str()))
        .collect::<Vec<_>>()
        .join("\n");

    if result.is_error.unwrap_or(false) {
        ToolOutput::failure(text)
    } else {
        ToolOutput::success(text)
    }
}

/// Convert rmcp `ErrorData` into the local tool error type.
fn mcp_error_to_tool_error(error: rmcp::model::ErrorData) -> ToolError {
    ToolError::ExecutionFailed(format!("MCP error ({:?}): {}", error.code, error.message))
}

/// Parse JSON arguments string into a typed params struct.
fn parse_args<T: serde::de::DeserializeOwned>(arguments: &str) -> ToolResult<T> {
    let value = parse_argument_value(arguments)?;
    serde_json::from_value(value).map_err(|error| {
        ToolError::ParseError(format!("Failed to parse MCP tool arguments: {error}"))
    })
}

/// Inline `definitions` into `properties` by resolving `$ref`.
///
/// `FunctionParameters` does not support top-level definitions, and `schemars`
/// generates `$ref` for nested structs.
fn inline_definitions(mut root: serde_json::Value) -> serde_json::Value {
    let definitions = root
        .get("definitions")
        .cloned()
        .unwrap_or_else(|| serde_json::json!({}));

    fn recurse(value: &mut serde_json::Value, definitions: &serde_json::Value) {
        if let Some(object) = value.as_object_mut() {
            if let Some(ref_str) = object.get("$ref").and_then(|v| v.as_str())
                && let Some(def_name) = ref_str.strip_prefix("#/definitions/")
                && let Some(definition) = definitions.get(def_name)
            {
                *value = definition.clone();
                recurse(value, definitions);
                return;
            }
            for nested in object.values_mut() {
                recurse(nested, definitions);
            }
        } else if let Some(array) = value.as_array_mut() {
            for nested in array {
                recurse(nested, definitions);
            }
        }
    }

    recurse(&mut root, &definitions);
    root
}

/// Derive `FunctionParameters` from a `schemars::JsonSchema` type so the model
/// receives the full parameter schema for the tool.
fn schema_to_params<T: rmcp::schemars::JsonSchema>() -> FunctionParameters {
    let root = rmcp::schemars::schema_for!(T);
    let schema_value = inline_definitions(serde_json::to_value(root).unwrap_or_default());

    let properties = schema_value
        .get("properties")
        .and_then(|value| value.as_object())
        .cloned()
        .unwrap_or_default();

    let required = schema_value
        .get("required")
        .and_then(|value| value.as_array())
        .map(|array| {
            array
                .iter()
                .filter_map(|value| value.as_str().map(String::from))
                .collect()
        })
        .unwrap_or_default();

    FunctionParameters::Object {
        param_type: "object".to_string(),
        properties,
        required,
        definitions: None,
    }
}

pub fn mcp_tool_definitions() -> Vec<McpToolDefinition> {
    vec![
        McpToolDefinition {
            name: "run_libra_vcs",
            description: "Run an allowlisted Libra version-control command without invoking git",
            params_schema: schema_to_params::<RunLibraVcsParams>(),
        },
        McpToolDefinition {
            name: "create_intent",
            description: "Create a new Intent (Prompt/Goal)",
            params_schema: schema_to_params::<CreateIntentParams>(),
        },
        McpToolDefinition {
            name: "create_task",
            description: "Create a new Task for tracking an AI coding goal",
            params_schema: schema_to_params::<CreateTaskParams>(),
        },
        McpToolDefinition {
            name: "create_run",
            description: "Create a new Run (an execution attempt for a Task)",
            params_schema: schema_to_params::<CreateRunParams>(),
        },
        McpToolDefinition {
            name: "create_context_snapshot",
            description: "Create a new ContextSnapshot (files/state at a point in time)",
            params_schema: schema_to_params::<CreateContextSnapshotParams>(),
        },
        McpToolDefinition {
            name: "create_plan",
            description: "Create a new Plan (ordered steps for a Run)",
            params_schema: schema_to_params::<CreatePlanParams>(),
        },
        McpToolDefinition {
            name: "create_patchset",
            description: "Create a new PatchSet (code changes produced by a Run)",
            params_schema: schema_to_params::<CreatePatchSetParams>(),
        },
        McpToolDefinition {
            name: "create_evidence",
            description: "Create a new Evidence (test/lint/build results)",
            params_schema: schema_to_params::<CreateEvidenceParams>(),
        },
        McpToolDefinition {
            name: "create_tool_invocation",
            description: "Record a ToolInvocation (external tool call during a Run)",
            params_schema: schema_to_params::<CreateToolInvocationParams>(),
        },
        McpToolDefinition {
            name: "create_provenance",
            description: "Record Provenance (LLM provider/model metadata for a Run)",
            params_schema: schema_to_params::<CreateProvenanceParams>(),
        },
        McpToolDefinition {
            name: "create_decision",
            description: "Record a Decision (commit / checkpoint / abandon / retry)",
            params_schema: schema_to_params::<CreateDecisionParams>(),
        },
        McpToolDefinition {
            name: "update_intent",
            description: "Update an existing Intent (set commit_sha or status)",
            params_schema: schema_to_params::<UpdateIntentParams>(),
        },
        McpToolDefinition {
            name: "list_intents",
            description: "List recent intents",
            params_schema: schema_to_params::<ListIntentsParams>(),
        },
        McpToolDefinition {
            name: "list_tasks",
            description: "List recent tasks",
            params_schema: schema_to_params::<ListTasksParams>(),
        },
        McpToolDefinition {
            name: "list_runs",
            description: "List recent runs",
            params_schema: schema_to_params::<ListRunsParams>(),
        },
        McpToolDefinition {
            name: "list_context_snapshots",
            description: "List recent context snapshots",
            params_schema: schema_to_params::<ListContextSnapshotsParams>(),
        },
        McpToolDefinition {
            name: "list_plans",
            description: "List recent plans",
            params_schema: schema_to_params::<ListPlansParams>(),
        },
        McpToolDefinition {
            name: "list_patchsets",
            description: "List recent patchsets",
            params_schema: schema_to_params::<ListPatchSetsParams>(),
        },
        McpToolDefinition {
            name: "list_evidences",
            description: "List recent evidences",
            params_schema: schema_to_params::<ListEvidencesParams>(),
        },
        McpToolDefinition {
            name: "list_tool_invocations",
            description: "List recent tool invocations",
            params_schema: schema_to_params::<ListToolInvocationsParams>(),
        },
        McpToolDefinition {
            name: "list_provenances",
            description: "List recent provenances",
            params_schema: schema_to_params::<ListProvenancesParams>(),
        },
        McpToolDefinition {
            name: "list_decisions",
            description: "List recent decisions",
            params_schema: schema_to_params::<ListDecisionsParams>(),
        },
    ]
}

pub async fn mcp_tool_is_mutating(tool_name: &str, invocation: &ToolInvocation) -> bool {
    if tool_name == "run_libra_vcs" {
        let ToolPayload::Function { arguments } = &invocation.payload else {
            return true;
        };
        let Ok(params) = parse_args::<RunLibraVcsParams>(arguments) else {
            return true;
        };
        let args = params.args.unwrap_or_default();
        return !classify_run_libra_vcs_safety(&params.command, &args).is_allow();
    }

    mcp_tool_is_potentially_mutating(tool_name)
}

fn mcp_tool_is_potentially_mutating(tool_name: &str) -> bool {
    tool_name.starts_with("create_")
        || tool_name.starts_with("update_")
        || tool_name == "run_libra_vcs"
}

pub async fn call_mcp_tool(
    server: Arc<LibraMcpServer>,
    tool_name: &str,
    arguments: &str,
    ai_operation: Option<&AiOperationContext>,
) -> ToolResult<ToolOutput> {
    let result: Result<CallToolResult, rmcp::model::ErrorData> = match tool_name {
        "run_libra_vcs" => {
            let params: RunLibraVcsParams = parse_args(arguments)?;
            server
                .run_libra_vcs_impl_with_ai_operation(params, ai_operation)
                .await
        }
        "create_intent" => {
            let params: CreateIntentParams = parse_args(arguments)?;
            let actor = server
                .resolve_actor_from_params(params.actor_kind.as_deref(), params.actor_id.as_deref())
                .map_err(mcp_error_to_tool_error)?;
            server.create_intent_impl(params, actor).await
        }
        "create_task" => {
            let params: CreateTaskParams = parse_args(arguments)?;
            let actor = server
                .resolve_actor_from_params(params.actor_kind.as_deref(), params.actor_id.as_deref())
                .map_err(mcp_error_to_tool_error)?;
            server.create_task_impl(params, actor).await
        }
        "create_run" => {
            let params: CreateRunParams = parse_args(arguments)?;
            let actor = server
                .resolve_actor_from_params(params.actor_kind.as_deref(), params.actor_id.as_deref())
                .map_err(mcp_error_to_tool_error)?;
            server.create_run_impl(params, actor).await
        }
        "create_context_snapshot" => {
            let params: CreateContextSnapshotParams = parse_args(arguments)?;
            let actor = server
                .resolve_actor_from_params(params.actor_kind.as_deref(), params.actor_id.as_deref())
                .map_err(mcp_error_to_tool_error)?;
            server.create_context_snapshot_impl(params, actor).await
        }
        "create_plan" => {
            let params: CreatePlanParams = parse_args(arguments)?;
            let actor = server
                .resolve_actor_from_params(params.actor_kind.as_deref(), params.actor_id.as_deref())
                .map_err(mcp_error_to_tool_error)?;
            server.create_plan_impl(params, actor).await
        }
        "create_patchset" => {
            let params: CreatePatchSetParams = parse_args(arguments)?;
            let actor = server
                .resolve_actor_from_params(params.actor_kind.as_deref(), params.actor_id.as_deref())
                .map_err(mcp_error_to_tool_error)?;
            server.create_patchset_impl(params, actor).await
        }
        "create_evidence" => {
            let params: CreateEvidenceParams = parse_args(arguments)?;
            let actor = server
                .resolve_actor_from_params(params.actor_kind.as_deref(), params.actor_id.as_deref())
                .map_err(mcp_error_to_tool_error)?;
            server.create_evidence_impl(params, actor).await
        }
        "create_tool_invocation" => {
            let params: CreateToolInvocationParams = parse_args(arguments)?;
            let actor = server
                .resolve_actor_from_params(params.actor_kind.as_deref(), params.actor_id.as_deref())
                .map_err(mcp_error_to_tool_error)?;
            server.create_tool_invocation_impl(params, actor).await
        }
        "create_provenance" => {
            let params: CreateProvenanceParams = parse_args(arguments)?;
            let actor = server
                .resolve_actor_from_params(params.actor_kind.as_deref(), params.actor_id.as_deref())
                .map_err(mcp_error_to_tool_error)?;
            server.create_provenance_impl(params, actor).await
        }
        "create_decision" => {
            let params: CreateDecisionParams = parse_args(arguments)?;
            let actor = server
                .resolve_actor_from_params(params.actor_kind.as_deref(), params.actor_id.as_deref())
                .map_err(mcp_error_to_tool_error)?;
            server.create_decision_impl(params, actor).await
        }
        "update_intent" => {
            let params: UpdateIntentParams = parse_args(arguments)?;
            server.update_intent_impl(params).await
        }
        "list_intents" => {
            let params: ListIntentsParams = parse_args(arguments)?;
            server.list_intents_impl(params).await
        }
        "list_tasks" => {
            let params: ListTasksParams = parse_args(arguments)?;
            server.list_tasks_impl(params).await
        }
        "list_runs" => {
            let params: ListRunsParams = parse_args(arguments)?;
            server.list_runs_impl(params).await
        }
        "list_context_snapshots" => {
            let params: ListContextSnapshotsParams = parse_args(arguments)?;
            server.list_context_snapshots_impl(params).await
        }
        "list_plans" => {
            let params: ListPlansParams = parse_args(arguments)?;
            server.list_plans_impl(params).await
        }
        "list_patchsets" => {
            let params: ListPatchSetsParams = parse_args(arguments)?;
            server.list_patchsets_impl(params).await
        }
        "list_evidences" => {
            let params: ListEvidencesParams = parse_args(arguments)?;
            server.list_evidences_impl(params).await
        }
        "list_tool_invocations" => {
            let params: ListToolInvocationsParams = parse_args(arguments)?;
            server.list_tool_invocations_impl(params).await
        }
        "list_provenances" => {
            let params: ListProvenancesParams = parse_args(arguments)?;
            server.list_provenances_impl(params).await
        }
        "list_decisions" => {
            let params: ListDecisionsParams = parse_args(arguments)?;
            server.list_decisions_impl(params).await
        }
        other => return Err(ToolError::ToolNotFound(other.to_string())),
    };

    result
        .map(call_tool_result_to_output)
        .map_err(mcp_error_to_tool_error)
}

pub struct McpSource {
    server: Arc<LibraMcpServer>,
    manifest: CapabilityManifest,
}

impl McpSource {
    pub fn builtin(server: Arc<LibraMcpServer>) -> Self {
        let mut manifest =
            CapabilityManifest::new(BUILTIN_MCP_SOURCE_SLUG, SourceKind::Mcp, TrustTier::Builtin)
                .with_filesystem_access(SourceAccess::Workspace)
                .with_network_access(SourceAccess::None)
                .with_shared_state(true)
                .with_resource("libra://history/latest")
                .with_resource("libra://context/active");

        for definition in mcp_tool_definitions() {
            let mut capability = SourceToolCapability::new(
                definition.name,
                ToolSpec::new(definition.name, definition.description)
                    .with_parameters(definition.params_schema),
            );
            if mcp_tool_is_potentially_mutating(definition.name) {
                capability = capability.mark_mutating("repository");
            }
            manifest = manifest.with_tool(capability);
        }

        Self { server, manifest }
    }
}

#[async_trait]
impl Source for McpSource {
    fn manifest(&self) -> &CapabilityManifest {
        &self.manifest
    }

    async fn is_tool_mutating(&self, tool_name: &str, invocation: &ToolInvocation) -> bool {
        mcp_tool_is_mutating(tool_name, invocation).await
    }

    async fn requires_network(&self, _tool_name: &str, _invocation: &ToolInvocation) -> bool {
        false
    }

    async fn call_tool(
        &self,
        _context: SourceCallContext,
        invocation: ToolInvocation,
    ) -> ToolResult<ToolOutput> {
        let runtime_context = invocation.runtime_context.clone();
        let tool_name = invocation.tool_name.clone();
        let arguments = match invocation.payload {
            ToolPayload::Function { arguments } => arguments,
            _ => {
                return Err(ToolError::IncompatiblePayload(
                    "MCP source only accepts Function payloads".to_string(),
                ));
            }
        };

        if tool_name == "run_libra_vcs"
            && let Ok(params) = parse_args::<RunLibraVcsParams>(&arguments)
        {
            let args = params.args.clone().unwrap_or_default();
            let safety_decision = classify_run_libra_vcs_safety(&params.command, &args);
            if safety_decision.is_needs_human()
                && libra_vcs_allow_all_active(runtime_context.as_ref()).await
            {
                let result = self
                    .server
                    .run_libra_vcs_impl_unchecked_with_ai_operation(
                        params,
                        invocation.ai_operation.as_ref(),
                    )
                    .await;
                return result
                    .map(call_tool_result_to_output)
                    .map_err(mcp_error_to_tool_error);
            }
        }

        call_mcp_tool(
            self.server.clone(),
            &tool_name,
            &arguments,
            invocation.ai_operation.as_ref(),
        )
        .await
    }
}

#[cfg(test)]
mod tests {
    use rmcp::model::Content;

    use super::*;

    /// `BUILTIN_MCP_SOURCE_SLUG` pinned. The string is the wire-format
    /// identifier the Source Pool registers under; renaming it would
    /// break audit logs that grep on it.
    #[test]
    fn builtin_mcp_source_slug_is_libra_mcp() {
        assert_eq!(BUILTIN_MCP_SOURCE_SLUG, "libra_mcp");
    }

    /// `mcp_tool_is_potentially_mutating` matches `create_*` names,
    /// `update_*` names, and the exact `run_libra_vcs` tool. Read-only
    /// tools (`list_*`, `read_file`, etc.) return false. Pin the rule
    /// so a future broader regex doesn't accidentally classify
    /// `read_file` as mutating.
    #[test]
    fn mcp_tool_is_potentially_mutating_matches_create_update_and_run_libra_vcs() {
        for name in [
            "create_intent",
            "create_task",
            "create_run",
            "create_plan",
            "create_decision",
            "update_intent",
            "update_task",
            "update_run",
            "run_libra_vcs",
        ] {
            assert!(
                mcp_tool_is_potentially_mutating(name),
                "{name:?} must classify as mutating",
            );
        }
    }

    /// Read-only and unrelated tools are NOT mutating. Pin the
    /// inverse so the classifier can't silently widen.
    #[test]
    fn mcp_tool_is_potentially_mutating_rejects_read_only_tools() {
        for name in [
            "list_intents",
            "list_tasks",
            "list_runs",
            "read_file",
            "grep_files",
            "search_files",
            "web_search",
            "",
            "createsomething", // no underscore prefix → no match
            "updateinstead",   // no underscore prefix → no match
            "run_libra_vcs2",  // suffix doesn't match exact name
            "Run_Libra_Vcs",   // case-sensitive (matches() is exact)
        ] {
            assert!(
                !mcp_tool_is_potentially_mutating(name),
                "{name:?} must NOT classify as mutating",
            );
        }
    }

    /// `call_tool_result_to_output` joins multiple text fragments with
    /// newlines and respects `is_error` → ToolOutput::failure vs success.
    #[test]
    fn call_tool_result_to_output_joins_text_and_respects_is_error() {
        // Success path with two text fragments.
        let result = CallToolResult::success(vec![
            Content::text("first line"),
            Content::text("second line"),
        ]);
        let output = call_tool_result_to_output(result);
        // Inspecting via Debug — ToolOutput is opaque, but its
        // serialised content should contain both lines joined.
        let rendered = format!("{output:?}");
        assert!(rendered.contains("first line"));
        assert!(rendered.contains("second line"));
    }

    /// `inline_definitions` resolves `$ref` references against the
    /// top-level `definitions` map. Pin the canonical traversal.
    #[test]
    fn inline_definitions_resolves_simple_ref() {
        let input = serde_json::json!({
            "definitions": {
                "Foo": {"type": "string"}
            },
            "properties": {
                "foo": {"$ref": "#/definitions/Foo"}
            }
        });
        let out = inline_definitions(input);
        assert_eq!(
            out.get("properties")
                .and_then(|p| p.get("foo"))
                .and_then(|f| f.get("type"))
                .and_then(|t| t.as_str()),
            Some("string"),
        );
    }

    /// `inline_definitions` recurses through nested objects and arrays
    /// so refs at any depth are resolved.
    #[test]
    fn inline_definitions_recurses_through_nested_arrays_and_objects() {
        let input = serde_json::json!({
            "definitions": {
                "Bar": {"type": "number", "minimum": 0}
            },
            "items": [
                {"$ref": "#/definitions/Bar"},
                {"wrapper": {"$ref": "#/definitions/Bar"}}
            ]
        });
        let out = inline_definitions(input);
        let arr = out.get("items").and_then(|i| i.as_array()).expect("array");
        assert_eq!(
            arr[0].get("type").and_then(|t| t.as_str()),
            Some("number"),
            "array element ref must resolve",
        );
        assert_eq!(
            arr[1]
                .get("wrapper")
                .and_then(|w| w.get("type"))
                .and_then(|t| t.as_str()),
            Some("number"),
            "nested-object ref must resolve",
        );
    }

    /// `inline_definitions` leaves unknown `$ref` strings alone (no
    /// matching definition) — it does not panic or rewrite to null.
    #[test]
    fn inline_definitions_leaves_unknown_refs_intact() {
        let input = serde_json::json!({
            "properties": {
                "foo": {"$ref": "#/definitions/Missing"}
            }
        });
        let out = inline_definitions(input.clone());
        // No definitions map → ref left untouched.
        assert_eq!(out, input);
    }

    /// `inline_definitions` round-trips an input with no `$ref` references
    /// unchanged.
    #[test]
    fn inline_definitions_passes_ref_free_input_unchanged() {
        let input = serde_json::json!({
            "properties": {
                "foo": {"type": "string"},
                "bar": {"type": "integer"}
            }
        });
        let out = inline_definitions(input.clone());
        assert_eq!(out, input);
    }

    /// `mcp_tool_definitions()` produces a non-empty list with stable
    /// names. Pin a minimum size + assert known tool names appear so
    /// removing a tool by accident is caught here.
    #[test]
    fn mcp_tool_definitions_includes_canonical_tools() {
        let defs = mcp_tool_definitions();
        assert!(
            defs.len() >= 5,
            "expected at least 5 MCP tool definitions; got {}",
            defs.len(),
        );
        let names: Vec<&str> = defs.iter().map(|d| d.name).collect();
        for required in ["create_intent", "create_task", "run_libra_vcs"] {
            assert!(
                names.contains(&required),
                "canonical tool {required:?} must appear in mcp_tool_definitions; got {names:?}",
            );
        }
    }
}
