//! MCP tools: create and list AI workflow process objects (Task/Run/Plan/...).
//!
//! This file uses `rmcp`'s `#[tool]` macro to expose `LibraMcpServer` methods as MCP tools.
//! Each tool's input schema is derived via `schemars::JsonSchema` for client discovery and validation.
//!
//! # Tool naming
//!
//! Tool names match the Rust method names (e.g. `create_task`, `list_runs`). Results are returned
//! as text via `CallToolResult`.
//!
//! # How tools and resources work together
//!
//! - `create_*` returns an object UUID (e.g. `"Task created with ID: ..."`).
//!   All `create_*` tools accept optional `actor_kind` (`"human"`, `"agent"`, `"system"`,
//!   `"mcp_client"`) and `actor_id` parameters to identify the creator. When omitted, the
//!   actor is derived from the MCP client handshake or defaults to `mcp_client("mcp-user")`.
//! - Status is event-sourced in git-internal (`intent_event`, `task_event`, `run_event`).
//!   `list_intents`/`list_tasks`/`list_runs` reconstruct status from latest events.
//! - To fetch the full JSON payload, read the resource: `libra://object/{object_id}`.
//!
//! # object_type (history directory name)
//!
//! List tools call `HistoryManager::list_objects(object_type)` using the following types:
//! `task`, `task_event`, `run`, `run_event`, `snapshot`, `plan`, `patchset`, `evidence`,
//! `invocation`, `provenance`, `decision`, `intent`, `intent_event`, `context_frame`,
//! `plan_step_event`, `run_usage`.
use std::{collections::HashMap, path::PathBuf, process::Stdio};

use chrono::Utc;
use git_internal::internal::object::{
    context::{ContextItem, ContextItemKind, ContextSnapshot, SelectionStrategy},
    context_frame::{ContextFrame, FrameKind},
    decision::{Decision, DecisionType},
    evidence::{Evidence, EvidenceKind},
    intent::{Intent, IntentSpec},
    intent_event::{IntentEvent, IntentEventKind},
    patchset::{ChangeType, DiffFormat, PatchSet, TouchedFile},
    plan::{Plan, PlanStep},
    plan_step_event::{PlanStepEvent, PlanStepStatus},
    provenance::Provenance,
    run::Run,
    run_event::{RunEvent, RunEventKind},
    run_usage::RunUsage,
    task::{GoalType, Task},
    task_event::{TaskEvent, TaskEventKind},
    tool::{IoFootprint, ToolInvocation, ToolStatus},
    types::{ActorKind, ActorRef, ArtifactRef},
};
use rmcp::{
    RoleServer,
    handler::server::{router::tool::ToolRouter, wrapper::Parameters},
    model::*,
    schemars,
    service::RequestContext,
    tool, tool_router,
};
use serde::{
    Deserialize, Deserializer, Serialize,
    de::{DeserializeOwned, Error as _},
};
use serde_json::{Map, Value, json};
use tokio::{
    process::Command,
    time::{Duration, timeout},
};
use uuid::Uuid;

use crate::{
    internal::{
        ai::{
            libra_vcs::{
                ALLOWED_COMMANDS, classify_run_libra_vcs_safety,
                format_run_libra_vcs_safety_message, normalize_tool_args,
                unsupported_command_message,
            },
            mcp::{authz::McpOperation, server::LibraMcpServer},
            util::normalize_commit_anchor,
            web::code_ui::{CodeUiTaskSnapshot, CodeUiTranscriptEntry, CodeUiTranscriptEntryKind},
        },
        head::Head,
    },
    utils::storage_ext::{Identifiable, StorageExt},
};

const ZERO_COMMIT_SHA: &str = "0000000000000000000000000000000000000000";
const LIBRA_VCS_TIMEOUT_SECONDS: u64 = 120;

impl LibraMcpServer {
    /// Default actor for MCP tool calls. Extracted for testability.
    pub fn default_actor(&self) -> Result<ActorRef, ErrorData> {
        ActorRef::mcp_client("mcp-user").map_err(|e| ErrorData::internal_error(e.to_string(), None))
    }

    fn libra_vcs_working_dir(&self) -> Result<PathBuf, ErrorData> {
        if let Some(working_dir) = &self.working_dir {
            return Ok(working_dir.clone());
        }
        std::env::current_dir().map_err(|e| {
            ErrorData::internal_error(
                format!("failed to resolve Libra VCS working directory: {e}"),
                None,
            )
        })
    }

    /// Resolve actor identity from explicit tool parameters only, without requiring
    /// a `RequestContext`. Falls back to `default_actor()` when no explicit params
    /// are provided.
    ///
    /// This is used by the legacy bridge path where no MCP session exists.
    pub fn resolve_actor_from_params(
        &self,
        actor_kind: Option<&str>,
        actor_id: Option<&str>,
    ) -> Result<ActorRef, ErrorData> {
        if let Some(kind_str) = actor_kind {
            let id = actor_id.unwrap_or("unknown");
            let kind: ActorKind = kind_str.into();
            return ActorRef::new(kind, id).map_err(|e| ErrorData::invalid_params(e, None));
        }
        self.default_actor()
    }

    /// Resolve actor identity for a tool call.
    ///
    /// Priority:
    /// 1. Explicit `actor_kind` + `actor_id` from tool parameters (lets callers specify
    ///    human / agent / system / mcp_client).
    /// 2. MCP peer info from the initialization handshake (`McpClient` kind).
    /// 3. Fallback default `McpClient("mcp-user")`.
    fn resolve_actor(
        &self,
        ctx: &RequestContext<RoleServer>,
        actor_kind: Option<&str>,
        actor_id: Option<&str>,
    ) -> Result<ActorRef, ErrorData> {
        if let Some(kind_str) = actor_kind {
            let id = actor_id.unwrap_or("unknown");
            let kind: ActorKind = kind_str.into();
            return ActorRef::new(kind, id).map_err(|e| ErrorData::invalid_params(e, None));
        }
        // No explicit actor — derive from MCP peer info.
        if let Some(client_info) = ctx.peer.peer_info() {
            let client_name = &client_info.client_info.name;
            return ActorRef::mcp_client(client_name)
                .map_err(|e| ErrorData::internal_error(e.to_string(), None));
        }
        self.default_actor()
    }

    async fn store_object<T>(&self, object: &T) -> Result<(), ErrorData>
    where
        T: Serialize + Send + Sync + Identifiable,
    {
        let object_type = object.object_type();
        let object_id = object.object_id();
        let tracked = self.intent_history_manager.is_some();
        let started_at = std::time::Instant::now();

        tracing::debug!(
            target: "libra::ai::mcp",
            object_type = %object_type,
            object_id = %object_id,
            tracked,
            "mcp object write started"
        );

        let storage = self.storage.as_ref().ok_or_else(|| {
            tracing::warn!(
                target: "libra::ai::mcp",
                object_type = %object_type,
                object_id = %object_id,
                tracked,
                "mcp object write failed: storage not available"
            );
            ErrorData::internal_error("Storage not available", None)
        })?;

        let write_result = if let Some(history) = &self.intent_history_manager {
            storage.put_tracked(object, history).await
        } else {
            storage.put_json(object).await
        };

        match write_result {
            Ok(hash) => {
                tracing::info!(
                    target: "libra::ai::mcp",
                    object_type = %object_type,
                    object_id = %object_id,
                    object_hash = %hash,
                    tracked,
                    elapsed_ms = started_at.elapsed().as_millis(),
                    "mcp object write succeeded"
                );
                Ok(())
            }
            Err(error) => {
                let message = error.to_string();
                tracing::warn!(
                    target: "libra::ai::mcp",
                    object_type = %object_type,
                    object_id = %object_id,
                    tracked,
                    elapsed_ms = started_at.elapsed().as_millis(),
                    error = %message,
                    "mcp object write failed"
                );
                Err(ErrorData::internal_error(message, None))
            }
        }
    }

    /// Validate object references when history is available.
    ///
    /// In memory-only tests the server may be constructed without a history
    /// manager; in that case we skip foreign-key style checks.
    async fn ensure_object_exists(
        &self,
        object_type: &str,
        object_id: Uuid,
        field: &str,
    ) -> Result<(), ErrorData> {
        let Some(history) = &self.intent_history_manager else {
            return Ok(());
        };

        let exists = history
            .get_object_hash(object_type, &object_id.to_string())
            .await
            .map_err(|e| ErrorData::internal_error(e.to_string(), None))?
            .is_some();

        if exists {
            Ok(())
        } else {
            Err(ErrorData::invalid_params(
                format!("{field} not found: {object_id}"),
                None,
            ))
        }
    }

    /// Load one tracked object by type/id when history is enabled.
    ///
    /// Returns `Ok(None)` if history is disabled on this server instance.
    async fn load_tracked_object<T>(
        &self,
        object_type: &str,
        object_id: Uuid,
        field: &str,
    ) -> Result<Option<T>, ErrorData>
    where
        T: DeserializeOwned + Send + Sync,
    {
        let Some(history) = &self.intent_history_manager else {
            return Ok(None);
        };
        let storage = self
            .storage
            .as_ref()
            .ok_or_else(|| ErrorData::internal_error("Storage not available", None))?;

        let hash = history
            .get_object_hash(object_type, &object_id.to_string())
            .await
            .map_err(|e| ErrorData::internal_error(e.to_string(), None))?
            .ok_or_else(|| {
                ErrorData::invalid_params(format!("{field} not found: {object_id}"), None)
            })?;

        let object = storage
            .get_json::<T>(&hash)
            .await
            .map_err(|e| ErrorData::internal_error(e.to_string(), None))?;
        Ok(Some(object))
    }

    async fn resolve_base_commit_anchor(&self, base_commit_sha: &str) -> Result<String, ErrorData> {
        let input = base_commit_sha.trim();
        if input.eq_ignore_ascii_case("HEAD") {
            return self.resolve_head_commit_anchor().await;
        }

        normalize_commit_anchor(input).map_err(|e| ErrorData::invalid_params(e, None))
    }

    async fn resolve_head_commit_anchor(&self) -> Result<String, ErrorData> {
        let history = self.intent_history_manager.as_ref().ok_or_else(|| {
            ErrorData::invalid_params(
                "base_commit_sha=HEAD requires repository history to be available",
                None,
            )
        })?;
        let db = history.database_connection();
        let commit = Head::current_commit_result_with_conn(&db)
            .await
            .map_err(|e| {
                ErrorData::invalid_params(
                    format!("failed to resolve base_commit_sha=HEAD: {e}"),
                    None,
                )
            })?;
        let commit_sha = commit
            .map(|commit| commit.to_string())
            .unwrap_or_else(|| ZERO_COMMIT_SHA.to_string());
        normalize_commit_anchor(&commit_sha).map_err(|e| {
            ErrorData::internal_error(
                format!("resolved HEAD produced an invalid commit hash: {e}"),
                None,
            )
        })
    }

    pub(super) async fn latest_task_events(
        &self,
    ) -> Result<HashMap<Uuid, TaskEventKind>, ErrorData> {
        let history = self
            .intent_history_manager
            .as_ref()
            .ok_or_else(|| ErrorData::internal_error("History not available", None))?;
        let storage = self
            .storage
            .as_ref()
            .ok_or_else(|| ErrorData::internal_error("Storage not available", None))?;

        let objects = history
            .list_objects("task_event")
            .await
            .map_err(|e| ErrorData::internal_error(e.to_string(), None))?;

        let mut latest = HashMap::<Uuid, TaskEvent>::new();
        for (_id, hash) in objects {
            if let Ok(event) = storage.get_json::<TaskEvent>(&hash).await {
                latest
                    .entry(event.task_id())
                    .and_modify(|current| {
                        if event.header().created_at() > current.header().created_at() {
                            *current = event.clone();
                        }
                    })
                    .or_insert(event);
            }
        }

        Ok(latest
            .into_iter()
            .map(|(task_id, event)| (task_id, event.kind().clone()))
            .collect())
    }

    pub(super) async fn latest_run_events(&self) -> Result<HashMap<Uuid, RunEventKind>, ErrorData> {
        let history = self
            .intent_history_manager
            .as_ref()
            .ok_or_else(|| ErrorData::internal_error("History not available", None))?;
        let storage = self
            .storage
            .as_ref()
            .ok_or_else(|| ErrorData::internal_error("Storage not available", None))?;

        let objects = history
            .list_objects("run_event")
            .await
            .map_err(|e| ErrorData::internal_error(e.to_string(), None))?;

        let mut latest = HashMap::<Uuid, RunEvent>::new();
        for (_id, hash) in objects {
            if let Ok(event) = storage.get_json::<RunEvent>(&hash).await {
                latest
                    .entry(event.run_id())
                    .and_modify(|current| {
                        if event.header().created_at() > current.header().created_at() {
                            *current = event.clone();
                        }
                    })
                    .or_insert(event);
            }
        }

        Ok(latest
            .into_iter()
            .map(|(run_id, event)| (run_id, event.kind().clone()))
            .collect())
    }
}

/// Helper to convert local ArtifactParams to git_internal::ArtifactRef
fn convert_artifact(p: ArtifactParams) -> Result<ArtifactRef, ErrorData> {
    ArtifactRef::new(p.store, p.key).map_err(|e| ErrorData::invalid_params(e, None))
}

fn parse_uuid(value: &str, field: &str) -> Result<Uuid, ErrorData> {
    let normalized = value.trim().trim_start_matches("uuid:");
    normalized
        .parse::<Uuid>()
        .map_err(|e| ErrorData::invalid_params(format!("invalid {field}: {e}"), None))
}

fn parse_optional_uuid(value: Option<String>, field: &str) -> Result<Option<Uuid>, ErrorData> {
    value.map(|v| parse_uuid(&v, field)).transpose()
}

fn parse_uuid_vec(values: Option<Vec<String>>, field: &str) -> Result<Vec<Uuid>, ErrorData> {
    values
        .unwrap_or_default()
        .into_iter()
        .map(|v| parse_uuid(&v, field))
        .collect()
}

fn parse_intent_spec(spec: String) -> IntentSpec {
    match serde_json::from_str::<serde_json::Value>(&spec) {
        Ok(value) => IntentSpec(value),
        Err(_) => IntentSpec::from(spec),
    }
}

fn parse_run_libra_vcs_params_value(value: Value) -> Result<RunLibraVcsParams, String> {
    match value {
        Value::Object(mut map) => {
            let command_value = take_first_value(&mut map, &["command", "cmd", "subcommand"]);
            let args_value = take_first_value(&mut map, &["args", "argv", "arguments"]);

            let mut command = command_value
                .as_ref()
                .and_then(value_as_trimmed_string)
                .unwrap_or_default();
            let mut args = match args_value {
                Some(args_value) => parse_libra_vcs_args_value(args_value)?,
                None if !map.is_empty() => libra_vcs_arg_map_to_args(map)?,
                None => Vec::new(),
            };

            if command.is_empty()
                && let Some(first) = args.first().cloned()
            {
                command = first;
                args.remove(0);
            }

            if command.is_empty() {
                return Err("run_libra_vcs requires a `command` string".to_string());
            }

            strip_redundant_libra_vcs_prefix(&command, &mut args);
            Ok(RunLibraVcsParams {
                command,
                args: (!args.is_empty()).then_some(args),
            })
        }
        Value::Array(items) => {
            let mut args = scalar_array_to_strings(items)?;
            if args.is_empty() {
                return Err("run_libra_vcs array form requires at least a command".to_string());
            }
            let command = args.remove(0);
            strip_redundant_libra_vcs_prefix(&command, &mut args);
            Ok(RunLibraVcsParams {
                command,
                args: (!args.is_empty()).then_some(args),
            })
        }
        Value::String(command_line) => {
            let mut args = split_libra_vcs_arg_string(&command_line)?;
            if args.is_empty() {
                return Err("run_libra_vcs string form requires a command".to_string());
            }
            let command = args.remove(0);
            strip_redundant_libra_vcs_prefix(&command, &mut args);
            Ok(RunLibraVcsParams {
                command,
                args: (!args.is_empty()).then_some(args),
            })
        }
        other => Err(format!(
            "run_libra_vcs arguments must be an object, array, or command string, got {}",
            json_type_name(&other)
        )),
    }
}

fn take_first_value(map: &mut Map<String, Value>, keys: &[&str]) -> Option<Value> {
    keys.iter().find_map(|key| map.remove(*key))
}

fn parse_libra_vcs_args_value(value: Value) -> Result<Vec<String>, String> {
    match value {
        Value::Null => Ok(Vec::new()),
        Value::Array(items) => scalar_array_to_strings(items),
        Value::String(raw) => split_libra_vcs_arg_string(&raw),
        Value::Object(map) => libra_vcs_arg_map_to_args(map),
        other => value_as_trimmed_string(&other)
            .map(|value| vec![value])
            .ok_or_else(|| {
                format!(
                    "run_libra_vcs args must be an array, string, or object, got {}",
                    json_type_name(&other)
                )
            }),
    }
}

fn scalar_array_to_strings(items: Vec<Value>) -> Result<Vec<String>, String> {
    items
        .iter()
        .map(|item| {
            value_as_trimmed_string(item).ok_or_else(|| {
                format!(
                    "run_libra_vcs args entries must be strings, numbers, or booleans, got {}",
                    json_type_name(item)
                )
            })
        })
        .collect()
}

fn split_libra_vcs_arg_string(raw: &str) -> Result<Vec<String>, String> {
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return Ok(Vec::new());
    }

    if matches!(trimmed.as_bytes().first(), Some(b'[')) {
        let value = serde_json::from_str::<Value>(trimmed).map_err(|err| {
            format!("failed to parse run_libra_vcs args JSON array string: {err}")
        })?;
        return parse_libra_vcs_args_value(value);
    }

    shlex::split(trimmed)
        .ok_or_else(|| "failed to parse run_libra_vcs args string as shell words".to_string())
}

fn libra_vcs_arg_map_to_args(map: Map<String, Value>) -> Result<Vec<String>, String> {
    let mut args = Vec::new();
    for (key, value) in map {
        let normalized_key = key.trim();
        if normalized_key.is_empty() || matches!(normalized_key, "command" | "cmd" | "subcommand") {
            continue;
        }

        if matches!(normalized_key, "path" | "file") {
            if let Some(path) = value_as_trimmed_string(&value) {
                args.push(path);
            }
            continue;
        }

        if matches!(normalized_key, "paths" | "files") {
            match value {
                Value::Array(items) => args.extend(scalar_array_to_strings(items)?),
                other => {
                    if let Some(path) = value_as_trimmed_string(&other) {
                        args.push(path);
                    }
                }
            }
            continue;
        }

        if is_libra_vcs_positional_arg_key(normalized_key) {
            match value {
                Value::Array(items) => args.extend(scalar_array_to_strings(items)?),
                other => {
                    if let Some(path) = value_as_trimmed_string(&other) {
                        args.push(path);
                    }
                }
            }
            continue;
        }

        let Some(flag) = libra_vcs_map_flag(normalized_key) else {
            continue;
        };
        match value {
            Value::Bool(true) => args.push(flag),
            Value::Bool(false) | Value::Null => {}
            Value::Array(items) => {
                for item in scalar_array_to_strings(items)? {
                    args.push(flag.clone());
                    args.push(item);
                }
            }
            other => {
                if let Some(text) = value_as_trimmed_string(&other) {
                    args.push(flag);
                    args.push(text);
                }
            }
        }
    }
    Ok(args)
}

fn is_libra_vcs_positional_arg_key(key: &str) -> bool {
    let normalized = key
        .trim()
        .trim_start_matches('-')
        .chars()
        .filter(|ch| !matches!(ch, '-' | '_' | ' '))
        .flat_map(char::to_lowercase)
        .collect::<String>();
    matches!(
        normalized.as_str(),
        "pathspec"
            | "pathspecs"
            | "positional"
            | "positionals"
            | "object"
            | "objects"
            | "revision"
            | "revisions"
            | "rev"
            | "revs"
            | "commit"
            | "commits"
            | "branch"
            | "branches"
            | "branchname"
            | "ref"
            | "refs"
    )
}

fn libra_vcs_map_flag(key: &str) -> Option<String> {
    let normalized = key.trim().trim_start_matches('-').replace('_', "-");
    let normalized = normalized.trim_matches('-');
    (!normalized.is_empty()).then(|| format!("--{normalized}"))
}

fn strip_redundant_libra_vcs_prefix(command: &str, args: &mut Vec<String>) {
    if args.first().is_some_and(|arg| arg == "libra") {
        args.remove(0);
    }
    if args.first().is_some_and(|arg| arg == command) {
        args.remove(0);
    }
}

fn value_as_trimmed_string(value: &Value) -> Option<String> {
    let text = match value {
        Value::String(text) => text.clone(),
        Value::Number(number) => number.to_string(),
        Value::Bool(flag) => flag.to_string(),
        Value::Null | Value::Array(_) | Value::Object(_) => return None,
    };
    let text = text.trim();
    (!text.is_empty()).then(|| text.to_string())
}

fn json_type_name(value: &Value) -> &'static str {
    match value {
        Value::Null => "null",
        Value::Bool(_) => "boolean",
        Value::Number(_) => "number",
        Value::String(_) => "string",
        Value::Array(_) => "array",
        Value::Object(_) => "object",
    }
}

fn normalize_libra_vcs_command(command: &str) -> Result<&'static str, ErrorData> {
    let command = command.trim();
    if command.is_empty() {
        return Err(ErrorData::invalid_params(
            "run_libra_vcs requires a Libra command",
            None,
        ));
    }
    if command.chars().any(char::is_whitespace) {
        return Err(ErrorData::invalid_params(
            "run_libra_vcs command must be a single allowlisted Libra command; pass flags and paths in args",
            None,
        ));
    }

    ALLOWED_COMMANDS
        .iter()
        .copied()
        .find(|allowed| *allowed == command)
        .ok_or_else(|| {
            ErrorData::invalid_params(unsupported_command_message("Libra VCS", command), None)
        })
}

fn validate_libra_vcs_args(args: &[String]) -> Result<(), ErrorData> {
    for arg in args {
        if arg.contains('\0') {
            return Err(ErrorData::invalid_params(
                "run_libra_vcs args must not contain NUL bytes",
                None,
            ));
        }

        let normalized = arg.trim();
        if normalized == "git"
            || normalized.ends_with("/git")
            || normalized.ends_with("\\git")
            || normalized.eq_ignore_ascii_case("git.exe")
        {
            return Err(ErrorData::invalid_params(
                "git is not allowed for Libra-managed agent execution; use Libra VCS commands only",
                None,
            ));
        }
    }

    Ok(())
}

fn format_libra_vcs_invocation(command: &str, args: &[String]) -> String {
    let mut parts = vec!["libra".to_string(), command.to_string()];
    parts.extend(args.iter().cloned());
    parts.join(" ")
}

fn libra_vcs_process_args(command: &str, args: &[String]) -> Vec<String> {
    let mut process_args = vec!["--json=compact".to_string(), command.to_string()];
    process_args.extend(strip_libra_vcs_output_args(args));
    process_args
}

fn strip_libra_vcs_output_args(args: &[String]) -> Vec<String> {
    args.iter()
        .filter(|arg| {
            !matches!(arg.as_str(), "--json" | "-J" | "--machine")
                && !arg.starts_with("--json=")
                && !arg.starts_with("-J=")
        })
        .cloned()
        .collect()
}

fn format_libra_vcs_output(
    command: &str,
    args: &[String],
    output: std::process::Output,
) -> CallToolResult {
    let exit_code = output.status.code();
    let stdout = String::from_utf8_lossy(&output.stdout)
        .trim_end()
        .to_string();
    let stderr = String::from_utf8_lossy(&output.stderr)
        .trim_end()
        .to_string();
    let argv = {
        let mut argv = vec!["libra".to_string()];
        argv.extend(libra_vcs_process_args(command, args));
        argv
    };
    let body = json!({
        "command": format_libra_vcs_invocation(command, args),
        "argv": argv,
        "exit_code": exit_code,
        "success": output.status.success(),
        "stdout": stdout,
        "stderr": stderr,
        "stdout_json": parse_json_text(&stdout),
        "stderr_json": parse_json_text(&stderr),
    });
    let body = match serde_json::to_string(&body) {
        Ok(body) => body,
        Err(err) => format!(
            "{{\"success\":false,\"command\":\"{}\",\"serialization_error\":\"{}\"}}",
            escape_json_string(&format_libra_vcs_invocation(command, args)),
            escape_json_string(&err.to_string())
        ),
    };

    if output.status.success() {
        CallToolResult::success(vec![Content::text(body)])
    } else {
        CallToolResult::error(vec![Content::text(body)])
    }
}

fn parse_json_text(text: &str) -> Option<Value> {
    let trimmed = text.trim();
    if trimmed.is_empty() {
        return None;
    }

    serde_json::from_str(trimmed).ok().or_else(|| {
        trimmed
            .lines()
            .rev()
            .map(str::trim)
            .find(|line| !line.is_empty())
            .and_then(|line| serde_json::from_str(line).ok())
    })
}

fn escape_json_string(value: &str) -> String {
    value
        .chars()
        .flat_map(|ch| match ch {
            '"' => "\\\"".chars().collect::<Vec<_>>(),
            '\\' => "\\\\".chars().collect::<Vec<_>>(),
            '\n' => "\\n".chars().collect::<Vec<_>>(),
            '\r' => "\\r".chars().collect::<Vec<_>>(),
            '\t' => "\\t".chars().collect::<Vec<_>>(),
            other => vec![other],
        })
        .collect()
}

fn parse_intent_event_kind(status: &str) -> Result<Option<IntentEventKind>, ErrorData> {
    match status {
        "draft" => Ok(None),
        "active" | "analyzed" => Ok(Some(IntentEventKind::Analyzed)),
        "completed" => Ok(Some(IntentEventKind::Completed)),
        "cancelled" | "discarded" => Ok(Some(IntentEventKind::Cancelled)),
        _ => Err(ErrorData::invalid_params("invalid intent status", None)),
    }
}

fn intent_status_label(kind: Option<&IntentEventKind>) -> &'static str {
    match kind {
        Some(IntentEventKind::Analyzed) => "active",
        Some(IntentEventKind::Completed) => "completed",
        Some(IntentEventKind::Cancelled) => "cancelled",
        Some(IntentEventKind::Other(_)) => "other",
        None => "draft",
    }
}

fn parse_task_event_kind(status: &str) -> Result<TaskEventKind, ErrorData> {
    match status {
        "draft" | "created" => Ok(TaskEventKind::Created),
        "running" => Ok(TaskEventKind::Running),
        "blocked" => Ok(TaskEventKind::Blocked),
        "done" | "completed" => Ok(TaskEventKind::Done),
        "failed" => Ok(TaskEventKind::Failed),
        "cancelled" => Ok(TaskEventKind::Cancelled),
        _ => Err(ErrorData::invalid_params("invalid task status", None)),
    }
}

fn parse_created_uuid(result: &CallToolResult) -> Option<Uuid> {
    result.content.iter().find_map(|content| {
        let text = content.as_text().map(|text| text.text.as_str())?;
        let id = text.split("ID:").nth(1)?.trim();
        Uuid::parse_str(id).ok()
    })
}

fn parse_run_event_kind(status: &str) -> Result<RunEventKind, ErrorData> {
    match status {
        "created" => Ok(RunEventKind::Created),
        "patching" => Ok(RunEventKind::Patching),
        "validating" => Ok(RunEventKind::Validating),
        "completed" => Ok(RunEventKind::Completed),
        "failed" => Ok(RunEventKind::Failed),
        "checkpointed" => Ok(RunEventKind::Checkpointed),
        _ => Err(ErrorData::invalid_params("invalid run status", None)),
    }
}

pub(super) fn task_status_label(kind: &TaskEventKind) -> &'static str {
    match kind {
        TaskEventKind::Created => "draft",
        TaskEventKind::Running => "running",
        TaskEventKind::Blocked => "blocked",
        TaskEventKind::Done => "done",
        TaskEventKind::Failed => "failed",
        TaskEventKind::Cancelled => "cancelled",
    }
}

pub(super) fn run_status_label(kind: &RunEventKind) -> &'static str {
    match kind {
        RunEventKind::Created => "created",
        RunEventKind::Patching => "patching",
        RunEventKind::Validating => "validating",
        RunEventKind::Completed => "completed",
        RunEventKind::Failed => "failed",
        RunEventKind::Checkpointed => "checkpointed",
    }
}

fn parse_frame_kind(kind: &str) -> FrameKind {
    match kind.trim() {
        "intent_analysis" => FrameKind::IntentAnalysis,
        "step_summary" => FrameKind::StepSummary,
        "code_change" => FrameKind::CodeChange,
        "system_state" => FrameKind::SystemState,
        "error_recovery" => FrameKind::ErrorRecovery,
        "checkpoint" => FrameKind::Checkpoint,
        "tool_call" => FrameKind::ToolCall,
        other => FrameKind::Other(other.to_string()),
    }
}

fn parse_plan_step_status(status: &str) -> Result<PlanStepStatus, ErrorData> {
    match status {
        "pending" => Ok(PlanStepStatus::Pending),
        "progressing" => Ok(PlanStepStatus::Progressing),
        "completed" => Ok(PlanStepStatus::Completed),
        "failed" => Ok(PlanStepStatus::Failed),
        "skipped" => Ok(PlanStepStatus::Skipped),
        _ => Err(ErrorData::invalid_params("invalid plan step status", None)),
    }
}

fn parse_context_item_kind(kind: Option<&str>) -> ContextItemKind {
    match kind.unwrap_or("file").trim() {
        "file" => ContextItemKind::File,
        "url" => ContextItemKind::Url,
        "snippet" => ContextItemKind::Snippet,
        "command" => ContextItemKind::Command,
        "image" => ContextItemKind::Image,
        other => ContextItemKind::Other(other.to_string()),
    }
}

#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
pub struct ArtifactParams {
    pub store: String,
    pub key: String,
    pub content_type: Option<String>,
    pub size_bytes: Option<u64>,
    pub hash: Option<String>,
}

#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
pub struct CreateIntentParams {
    /// The prompt or goal content (raw user input / natural language description).
    pub content: String,
    /// AI-analyzed structured content (e.g. canonical IntentSpec JSON).
    /// Stored in the Intent object's `spec` field. When `None`, the Intent
    /// is created without structured content (Draft state).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub structured_content: Option<String>,
    /// ID of the parent intent, forming the history chain.
    pub parent_id: Option<String>,
    /// IDs of parent intents (merge-style intent revision).
    pub parent_ids: Option<Vec<String>>,
    /// Context frames used while deriving the structured intent spec.
    pub analysis_context_frame_ids: Option<Vec<String>>,
    /// Initial lifecycle status: "draft", "active"/"analyzed", "completed", "cancelled"/"discarded".
    pub status: Option<String>,
    /// SHA of the code commit this intent resulted in (cross-reference to the code branch).
    pub commit_sha: Option<String>,
    /// Optional human-readable lifecycle reason for emitted intent event.
    pub reason: Option<String>,
    /// Optional follow-up intent id for completed lifecycle transitions.
    pub next_intent_id: Option<String>,
    /// Actor kind: "human", "agent", "system", "mcp_client". Omit to auto-detect.
    pub actor_kind: Option<String>,
    /// Actor identifier (e.g. username, agent name). Required when `actor_kind` is set.
    pub actor_id: Option<String>,
}

#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
pub struct UpdateIntentParams {
    /// ID of the intent to update.
    pub intent_id: String,
    /// Lifecycle status transition ("active"/"analyzed", "completed", "cancelled").
    pub status: Option<String>,
    /// Resulting commit SHA for lifecycle events that produced a commit.
    pub commit_sha: Option<String>,
    /// Optional human-readable lifecycle reason.
    pub reason: Option<String>,
    /// Optional follow-up intent id.
    pub next_intent_id: Option<String>,
}

#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
pub struct ListIntentsParams {
    pub limit: Option<usize>,
}

#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
pub struct CreateTaskParams {
    pub title: String,
    pub description: Option<String>,
    pub goal_type: Option<String>,
    pub constraints: Option<Vec<String>>,
    pub acceptance_criteria: Option<Vec<String>>,
    /// Actor who requested this task (kind: "human", "agent", etc.).
    pub requested_by_kind: Option<String>,
    /// Actor ID for the requester.
    pub requested_by_id: Option<String>,
    /// UUIDs of tasks this task depends on.
    pub dependencies: Option<Vec<String>>,
    /// ID of the intent this task belongs to.
    pub intent_id: Option<String>,
    /// Optional parent task id when decomposing larger work.
    pub parent_task_id: Option<String>,
    /// Optional originating plan step id that spawned this task.
    pub origin_step_id: Option<String>,
    /// Task status: "draft", "running", "done", "failed", "cancelled". Defaults to "draft".
    pub status: Option<String>,
    /// Optional reason for the initial task lifecycle event.
    pub reason: Option<String>,
    /// Search tags (key-value pairs)
    pub tags: Option<HashMap<String, String>>,
    /// External ID mapping
    pub external_ids: Option<HashMap<String, String>>,
    /// Actor kind: "human", "agent", "system", "mcp_client". Omit to auto-detect.
    pub actor_kind: Option<String>,
    /// Actor identifier (e.g. username, agent name). Required when `actor_kind` is set.
    pub actor_id: Option<String>,
}

#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
pub struct ListTasksParams {
    pub limit: Option<usize>,
    pub status: Option<String>,
}

#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
pub struct CreateRunParams {
    pub task_id: String,
    pub base_commit_sha: String,
    /// Optional selected plan revision.
    pub plan_id: Option<String>,
    pub status: Option<String>,
    pub context_snapshot_id: Option<String>,
    pub error: Option<String>,
    /// Agent instances participating in this run.
    /// Accepted for forward compatibility; not yet persisted by the Run object model.
    pub agent_instances: Option<Vec<AgentInstanceParams>>,
    /// Arbitrary metrics JSON (e.g. token counts, timings).
    pub metrics_json: Option<String>,
    /// Optional lifecycle reason for the initial run event.
    pub reason: Option<String>,
    /// Scheduler version (legacy field name: orchestrator_version).
    /// Accepted for forward compatibility; not yet persisted by the Run object model.
    pub orchestrator_version: Option<String>,
    /// Search tags (key-value pairs)
    pub tags: Option<HashMap<String, String>>,
    /// External ID mapping
    pub external_ids: Option<HashMap<String, String>>,
    /// Actor kind: "human", "agent", "system", "mcp_client". Omit to auto-detect.
    pub actor_kind: Option<String>,
    /// Actor identifier (e.g. username, agent name). Required when `actor_kind` is set.
    pub actor_id: Option<String>,
}

#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
pub struct AgentInstanceParams {
    pub role: String,
    pub provider_route: Option<String>,
}

#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
pub struct ListRunsParams {
    pub limit: Option<usize>,
    pub status: Option<String>,
}

#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
pub struct CreateContextSnapshotParams {
    pub selection_strategy: String,
    pub items: Option<Vec<ContextItemParams>>,
    pub summary: Option<String>,
    /// Search tags (key-value pairs)
    pub tags: Option<HashMap<String, String>>,
    /// External ID mapping
    pub external_ids: Option<HashMap<String, String>>,
    /// Actor kind: "human", "agent", "system", "mcp_client". Omit to auto-detect.
    pub actor_kind: Option<String>,
    /// Actor identifier (e.g. username, agent name). Required when `actor_kind` is set.
    pub actor_id: Option<String>,
}

#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
pub struct ContextItemParams {
    pub kind: Option<String>,
    pub path: String,
    pub preview: Option<String>,
    pub content_hash: Option<String>,
    pub blob_hash: Option<String>,
}

#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
pub struct ListContextSnapshotsParams {
    pub limit: Option<usize>,
}

#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
pub struct CreatePlanParams {
    /// Owning intent id for this plan revision.
    pub intent_id: String,
    /// Parent plan revisions for replanning/merge workflows.
    pub parent_plan_ids: Option<Vec<String>>,
    /// Planning-time context frame ids.
    pub context_frame_ids: Option<Vec<String>>,
    pub steps: Option<Vec<PlanStepParams>>,
    /// Search tags (key-value pairs)
    pub tags: Option<HashMap<String, String>>,
    /// External ID mapping
    pub external_ids: Option<HashMap<String, String>>,
    /// Actor kind: "human", "agent", "system", "mcp_client". Omit to auto-detect.
    pub actor_kind: Option<String>,
    /// Actor identifier (e.g. username, agent name). Required when `actor_kind` is set.
    pub actor_id: Option<String>,
}

#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
pub struct PlanStepParams {
    pub description: String,
    pub inputs: Option<serde_json::Value>,
    pub checks: Option<serde_json::Value>,
}

#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
pub struct ListPlansParams {
    pub limit: Option<usize>,
}

#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
pub struct CreatePatchSetParams {
    pub run_id: String,
    pub generation: u32,
    pub sequence: Option<u32>,
    pub base_commit_sha: String,
    pub touched_files: Option<Vec<TouchedFileParams>>,
    pub rationale: Option<String>,
    pub diff_format: Option<String>,
    pub diff_artifact: Option<ArtifactParams>,
    /// Search tags (key-value pairs)
    pub tags: Option<HashMap<String, String>>,
    /// External ID mapping
    pub external_ids: Option<HashMap<String, String>>,
    /// Actor kind: "human", "agent", "system", "mcp_client". Omit to auto-detect.
    pub actor_kind: Option<String>,
    /// Actor identifier (e.g. username, agent name). Required when `actor_kind` is set.
    pub actor_id: Option<String>,
}

#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
pub struct TouchedFileParams {
    pub path: String,
    pub change_type: String,
    pub lines_added: u32,
    pub lines_deleted: u32,
}

#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
pub struct ListPatchSetsParams {
    pub limit: Option<usize>,
}

#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
pub struct CreateEvidenceParams {
    pub run_id: String,
    pub patchset_id: Option<String>,
    pub kind: String,
    pub tool: String,
    pub command: Option<String>,
    pub exit_code: Option<i32>,
    pub summary: Option<String>,
    pub report_artifacts: Option<Vec<ArtifactParams>>,
    /// Search tags (key-value pairs)
    pub tags: Option<HashMap<String, String>>,
    /// External ID mapping
    pub external_ids: Option<HashMap<String, String>>,
    /// Actor kind: "human", "agent", "system", "mcp_client". Omit to auto-detect.
    pub actor_kind: Option<String>,
    /// Actor identifier (e.g. username, agent name). Required when `actor_kind` is set.
    pub actor_id: Option<String>,
}

#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
pub struct ListEvidencesParams {
    pub limit: Option<usize>,
}

#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
pub struct CreateToolInvocationParams {
    pub run_id: String,
    pub tool_name: String,
    pub status: Option<String>,
    pub args_json: Option<String>,
    pub io_footprint: Option<IoFootprintParams>,
    pub result_summary: Option<String>,
    pub artifacts: Option<Vec<ArtifactParams>>,
    /// Search tags (key-value pairs)
    pub tags: Option<HashMap<String, String>>,
    /// External ID mapping
    pub external_ids: Option<HashMap<String, String>>,
    /// Actor kind: "human", "agent", "system", "mcp_client". Omit to auto-detect.
    pub actor_kind: Option<String>,
    /// Actor identifier (e.g. username, agent name). Required when `actor_kind` is set.
    pub actor_id: Option<String>,
}

#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
pub struct ListToolInvocationsParams {
    pub limit: Option<usize>,
}

#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
pub struct IoFootprintParams {
    pub paths_read: Option<Vec<String>>,
    pub paths_written: Option<Vec<String>>,
}

#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
pub struct CreateProvenanceParams {
    pub run_id: String,
    pub provider: String,
    pub model: String,
    pub parameters_json: Option<String>,
    pub temperature: Option<f64>,
    pub max_tokens: Option<u64>,
    /// Search tags (key-value pairs)
    pub tags: Option<HashMap<String, String>>,
    /// External ID mapping
    pub external_ids: Option<HashMap<String, String>>,
    /// Actor kind: "human", "agent", "system", "mcp_client". Omit to auto-detect.
    pub actor_kind: Option<String>,
    /// Actor identifier (e.g. username, agent name). Required when `actor_kind` is set.
    pub actor_id: Option<String>,
}

#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
pub struct ListProvenancesParams {
    pub limit: Option<usize>,
}

#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
pub struct CreateDecisionParams {
    pub run_id: String,
    pub decision_type: String,
    pub chosen_patchset_id: Option<String>,
    /// The commit SHA produced by this decision (64-hex or 40-hex SHA-1).
    pub result_commit_sha: Option<String>,
    pub checkpoint_id: Option<String>,
    pub rationale: Option<String>,
    /// Search tags (key-value pairs)
    pub tags: Option<HashMap<String, String>>,
    /// External ID mapping
    pub external_ids: Option<HashMap<String, String>>,
    /// Actor kind: "human", "agent", "system", "mcp_client". Omit to auto-detect.
    pub actor_kind: Option<String>,
    /// Actor identifier (e.g. username, agent name). Required when `actor_kind` is set.
    pub actor_id: Option<String>,
}

#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
pub struct ListDecisionsParams {
    pub limit: Option<usize>,
}

#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
pub struct CreateContextFrameParams {
    /// Semantic frame kind: "intent_analysis", "step_summary", "code_change",
    /// "system_state", "error_recovery", "checkpoint", "tool_call", or custom string.
    pub kind: String,
    /// Short human-readable description of the context increment.
    pub summary: String,
    /// Optional associated intent id.
    pub intent_id: Option<String>,
    /// Optional associated run id.
    pub run_id: Option<String>,
    /// Optional associated plan id.
    pub plan_id: Option<String>,
    /// Optional associated plan-step id.
    pub step_id: Option<String>,
    /// Optional structured payload (arbitrary JSON).
    pub data: Option<serde_json::Value>,
    /// Optional approximate token footprint for budgeting.
    pub token_estimate: Option<u64>,
    /// Actor kind: "human", "agent", "system", "mcp_client". Omit to auto-detect.
    pub actor_kind: Option<String>,
    /// Actor identifier (e.g. username, agent name). Required when `actor_kind` is set.
    pub actor_id: Option<String>,
}

#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
pub struct ListContextFramesParams {
    pub limit: Option<usize>,
}

#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
pub struct CreatePlanStepEventParams {
    /// Plan revision that owns the step.
    pub plan_id: String,
    /// Stable logical step id inside the plan.
    pub step_id: String,
    /// Run attempt that produced this step event.
    pub run_id: String,
    /// Step execution status: "pending", "progressing", "completed", "failed", "skipped".
    pub status: String,
    /// Optional human-readable reason for this status transition.
    pub reason: Option<String>,
    /// Context frame ids consumed while executing the step.
    pub consumed_frames: Option<Vec<String>>,
    /// Context frame ids produced while executing the step.
    pub produced_frames: Option<Vec<String>>,
    /// Optional durable task spawned from this step.
    pub spawned_task_id: Option<String>,
    /// Optional structured runtime outputs.
    pub outputs: Option<serde_json::Value>,
    /// Actor kind: "human", "agent", "system", "mcp_client". Omit to auto-detect.
    pub actor_kind: Option<String>,
    /// Actor identifier (e.g. username, agent name). Required when `actor_kind` is set.
    pub actor_id: Option<String>,
}

#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
pub struct ListPlanStepEventsParams {
    pub limit: Option<usize>,
}

#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
pub struct CreateRunUsageParams {
    /// Run that produced this usage summary.
    pub run_id: String,
    /// Input tokens consumed.
    pub input_tokens: u64,
    /// Output tokens produced.
    pub output_tokens: u64,
    /// Optional billing estimate in USD.
    pub cost_usd: Option<f64>,
    /// Actor kind: "human", "agent", "system", "mcp_client". Omit to auto-detect.
    pub actor_kind: Option<String>,
    /// Actor identifier (e.g. username, agent name). Required when `actor_kind` is set.
    pub actor_id: Option<String>,
}

#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
pub struct ListRunUsagesParams {
    pub limit: Option<usize>,
}

#[derive(Debug, schemars::JsonSchema)]
pub struct RunLibraVcsParams {
    /// Allowlisted Libra subcommand: status, diff, branch, log, show, show-ref, ls-files, add,
    /// commit, or switch. Pass Git-like flags in args only when they map cleanly to Libra.
    pub command: String,
    /// Command arguments as argv entries. Shell syntax is not evaluated. Prefer
    /// `status --json` or `status --porcelain v2 --untracked-files=all` for repository state.
    /// `status -uall` is normalized to `--untracked-files=all` for compatibility.
    pub args: Option<Vec<String>>,
}

impl<'de> Deserialize<'de> for RunLibraVcsParams {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let value = Value::deserialize(deserializer)?;
        parse_run_libra_vcs_params_value(value).map_err(D::Error::custom)
    }
}

#[tool_router]
impl LibraMcpServer {
    #[tool(
        description = "Run an allowlisted Libra version-control command without invoking git. Allowed commands: status, diff, branch, log, show, show-ref, ls-files, add, commit, switch. Pass flags in args. Use ls-files for tracked/untracked repository path inspection."
    )]
    pub async fn run_libra_vcs(
        &self,
        Parameters(params): Parameters<RunLibraVcsParams>,
    ) -> Result<CallToolResult, ErrorData> {
        self.run_libra_vcs_impl(params).await
    }

    pub async fn run_libra_vcs_impl(
        &self,
        params: RunLibraVcsParams,
    ) -> Result<CallToolResult, ErrorData> {
        let args = params.args.unwrap_or_default();
        let safety_decision = classify_run_libra_vcs_safety(&params.command, &args);
        if !safety_decision.is_allow() {
            return Ok(CallToolResult::error(vec![Content::text(
                format_run_libra_vcs_safety_message(&params.command, &args, &safety_decision),
            )]));
        }
        self.dispatch_libra_vcs(&params.command, args).await
    }

    /// Variant of [`Self::run_libra_vcs_impl`] that runs `add`/`commit`/`switch` and
    /// other `needs_human` commands without prompting, intended for callers
    /// that have already confirmed the user granted a session-level
    /// allow-all-commands decision. `deny` decisions are still rejected so
    /// destructive commands (`reset`, `rm`, `push`, etc.) cannot leak through.
    pub async fn run_libra_vcs_impl_unchecked(
        &self,
        params: RunLibraVcsParams,
    ) -> Result<CallToolResult, ErrorData> {
        let args = params.args.unwrap_or_default();
        let safety_decision = classify_run_libra_vcs_safety(&params.command, &args);
        if safety_decision.is_deny() {
            return Ok(CallToolResult::error(vec![Content::text(
                format_run_libra_vcs_safety_message(&params.command, &args, &safety_decision),
            )]));
        }
        self.dispatch_libra_vcs(&params.command, args).await
    }

    async fn dispatch_libra_vcs(
        &self,
        command: &str,
        args: Vec<String>,
    ) -> Result<CallToolResult, ErrorData> {
        let command = normalize_libra_vcs_command(command)?;
        validate_libra_vcs_args(&args)?;
        let args = normalize_tool_args(command, &args)
            .map_err(|message| ErrorData::invalid_params(message, None))?;
        let working_dir = self.libra_vcs_working_dir()?;
        let executable = std::env::current_exe().map_err(|e| {
            ErrorData::internal_error(format!("failed to locate Libra executable: {e}"), None)
        })?;
        let process_args = libra_vcs_process_args(command, &args);

        let child = Command::new(executable)
            .args(&process_args)
            .current_dir(&working_dir)
            .stdin(Stdio::null())
            .output();

        match timeout(Duration::from_secs(LIBRA_VCS_TIMEOUT_SECONDS), child).await {
            Ok(Ok(output)) => Ok(format_libra_vcs_output(command, &args, output)),
            Ok(Err(err)) => Err(ErrorData::internal_error(
                format!("failed to run Libra VCS command '{command}': {err}"),
                None,
            )),
            Err(_) => Ok(CallToolResult::error(vec![Content::text(format!(
                "Libra VCS command timed out after {}s: {}",
                LIBRA_VCS_TIMEOUT_SECONDS,
                format_libra_vcs_invocation(command, &args)
            ))])),
        }
    }

    #[tool(description = "Create a new Intent (Prompt/Goal)")]
    pub async fn create_intent(
        &self,
        ctx: RequestContext<RoleServer>,
        Parameters(params): Parameters<CreateIntentParams>,
    ) -> Result<CallToolResult, ErrorData> {
        let actor = self.resolve_actor(
            &ctx,
            params.actor_kind.as_deref(),
            params.actor_id.as_deref(),
        )?;
        self.create_intent_impl(params, actor).await
    }

    pub async fn create_intent_impl(
        &self,
        params: CreateIntentParams,
        actor: ActorRef,
    ) -> Result<CallToolResult, ErrorData> {
        self.authorize_or_error_with_actor(
            McpOperation::CallTool {
                tool_name: "create_intent",
            },
            &actor,
        )
        .await?;
        let mut parent_ids = parse_uuid_vec(params.parent_ids, "parent_ids")?;
        if let Some(parent_id) = parse_optional_uuid(params.parent_id, "parent_id")? {
            parent_ids.push(parent_id);
        }
        parent_ids.sort_unstable();
        parent_ids.dedup();
        for parent_id in &parent_ids {
            self.ensure_object_exists("intent", *parent_id, "parent_id")
                .await?;
        }

        let mut intent = if parent_ids.is_empty() {
            Intent::new(actor.clone(), params.content)
                .map_err(|e| ErrorData::internal_error(e, None))?
        } else {
            Intent::new_revision_chain(actor.clone(), params.content, &parent_ids)
                .map_err(|e| ErrorData::invalid_params(e, None))?
        };

        if let Some(spec) = params.structured_content {
            intent.set_spec(Some(parse_intent_spec(spec)));
        }

        let analysis_context_frames = parse_uuid_vec(
            params.analysis_context_frame_ids,
            "analysis_context_frame_ids",
        )?;
        if !analysis_context_frames.is_empty() {
            intent.set_analysis_context_frames(analysis_context_frames);
        }

        // 0.7 stores intent lifecycle/commit state in IntentEvent.
        let mut lifecycle_kind = match params.status.as_deref() {
            Some(status) => parse_intent_event_kind(status)?,
            None => None,
        };
        if lifecycle_kind.is_none() && params.commit_sha.is_some() {
            lifecycle_kind = Some(IntentEventKind::Completed);
        }

        self.store_object(&intent).await?;

        if let Some(kind) = lifecycle_kind {
            let mut event = IntentEvent::new(actor, intent.header().object_id(), kind)
                .map_err(|e| ErrorData::internal_error(e, None))?;
            if let Some(sha) = params.commit_sha {
                let normalized = normalize_commit_anchor(&sha)
                    .map_err(|e| ErrorData::invalid_params(e.to_string(), None))?;
                let ih = normalized
                    .parse()
                    .map_err(|e: String| ErrorData::invalid_params(e, None))?;
                event.set_result_commit(Some(ih));
            }
            event.set_reason(params.reason);
            if let Some(next_intent_id) =
                parse_optional_uuid(params.next_intent_id, "next_intent_id")?
            {
                event.set_next_intent_id(Some(next_intent_id));
            }
            self.store_object(&event).await?;
        }

        Ok(CallToolResult::success(vec![Content::text(format!(
            "Intent created with ID: {}",
            intent.header().object_id()
        ))]))
    }

    #[tool(description = "List recent intents")]
    pub async fn list_intents(
        &self,
        Parameters(params): Parameters<ListIntentsParams>,
    ) -> Result<CallToolResult, ErrorData> {
        self.list_intents_impl(params).await
    }

    pub async fn list_intents_impl(
        &self,
        params: ListIntentsParams,
    ) -> Result<CallToolResult, ErrorData> {
        self.authorize_or_error(McpOperation::CallTool {
            tool_name: "list_intents",
        })
        .await?;
        let history = self
            .intent_history_manager
            .as_ref()
            .ok_or_else(|| ErrorData::internal_error("History not available", None))?;
        let storage = self
            .storage
            .as_ref()
            .ok_or_else(|| ErrorData::internal_error("Storage not available", None))?;

        let objects = history
            .list_objects("intent")
            .await
            .map_err(|e| ErrorData::internal_error(e.to_string(), None))?;

        let mut intents = Vec::new();
        for (_id, hash) in objects {
            if let Ok(intent) = storage.get_json::<Intent>(&hash).await {
                intents.push(intent);
            }
        }

        let mut latest_events = HashMap::<Uuid, IntentEvent>::new();
        let event_objects = history
            .list_objects("intent_event")
            .await
            .map_err(|e| ErrorData::internal_error(e.to_string(), None))?;
        for (_id, hash) in event_objects {
            if let Ok(event) = storage.get_json::<IntentEvent>(&hash).await {
                latest_events
                    .entry(event.intent_id())
                    .and_modify(|current| {
                        if event.header().created_at() > current.header().created_at() {
                            *current = event.clone();
                        }
                    })
                    .or_insert(event);
            }
        }

        // Sort by created_at descending
        intents.sort_by_key(|b| std::cmp::Reverse(b.header().created_at()));

        let limit = params.limit.unwrap_or(10);
        let out: Vec<String> = intents
            .into_iter()
            .take(limit)
            .map(|i| {
                let lifecycle = latest_events
                    .get(&i.header().object_id())
                    .map(|event| event.kind());
                let spec_preview = i
                    .spec()
                    .map(|spec| spec.0.to_string().replace('\n', " "))
                    .unwrap_or_else(|| "-".to_string());
                format!(
                    "ID: {} | Status: {} | Prompt: {:.50} | Parents: {} | Spec: {:.50}",
                    i.header().object_id(),
                    intent_status_label(lifecycle),
                    i.prompt().replace('\n', " "),
                    i.parents().len(),
                    spec_preview
                )
            })
            .collect();

        if out.is_empty() {
            Ok(CallToolResult::success(vec![Content::text(
                "No intents found.",
            )]))
        } else {
            Ok(CallToolResult::success(vec![Content::text(out.join("\n"))]))
        }
    }

    #[tool(description = "Update an existing Intent (set commit_sha or status)")]
    pub async fn update_intent(
        &self,
        Parameters(params): Parameters<UpdateIntentParams>,
    ) -> Result<CallToolResult, ErrorData> {
        self.update_intent_impl(params).await
    }

    pub async fn update_intent_impl(
        &self,
        params: UpdateIntentParams,
    ) -> Result<CallToolResult, ErrorData> {
        let intent_id = parse_uuid(&params.intent_id, "intent_id")?;
        let history = self
            .intent_history_manager
            .as_ref()
            .ok_or_else(|| ErrorData::internal_error("History not available", None))?;

        // Ensure the target intent exists.
        history
            .get_object_hash("intent", &intent_id.to_string())
            .await
            .map_err(|e| ErrorData::internal_error(e.to_string(), None))?
            .ok_or_else(|| {
                ErrorData::invalid_params(format!("Intent not found: {intent_id}"), None)
            })?;

        let mut event_kind = match params.status.as_deref() {
            Some(status) => parse_intent_event_kind(status)?,
            None => None,
        };
        if event_kind.is_none() && params.commit_sha.is_some() {
            event_kind = Some(IntentEventKind::Completed);
        }

        let Some(event_kind) = event_kind else {
            return Err(ErrorData::invalid_params(
                "No lifecycle transition to record. Provide 'status' and/or 'commit_sha'.",
                None,
            ));
        };

        let actor = self.default_actor()?;
        let mut event = IntentEvent::new(actor, intent_id, event_kind)
            .map_err(|e| ErrorData::internal_error(e, None))?;

        if let Some(sha) = params.commit_sha {
            let normalized = normalize_commit_anchor(&sha)
                .map_err(|e| ErrorData::invalid_params(e.to_string(), None))?;
            let ih = normalized
                .parse()
                .map_err(|e: String| ErrorData::invalid_params(e, None))?;
            event.set_result_commit(Some(ih));
        }
        event.set_reason(params.reason);
        if let Some(next_intent_id) = parse_optional_uuid(params.next_intent_id, "next_intent_id")?
        {
            event.set_next_intent_id(Some(next_intent_id));
        }

        self.store_object(&event).await?;

        Ok(CallToolResult::success(vec![Content::text(format!(
            "Intent {} updated successfully",
            params.intent_id
        ))]))
    }

    #[tool(description = "Create a new Task")]
    pub async fn create_task(
        &self,
        ctx: RequestContext<RoleServer>,
        Parameters(params): Parameters<CreateTaskParams>,
    ) -> Result<CallToolResult, ErrorData> {
        let title = params.title.clone();
        let status = params
            .status
            .as_deref()
            .map(parse_task_event_kind)
            .transpose()?
            .unwrap_or(TaskEventKind::Created);
        let actor = self.resolve_actor(
            &ctx,
            params.actor_kind.as_deref(),
            params.actor_id.as_deref(),
        )?;
        let result = self.create_task_impl(params, actor).await?;
        if !result.is_error.unwrap_or(false)
            && let Some(task_id) = parse_created_uuid(&result)
        {
            self.emit_code_ui_mcp_task_created(task_id, &title, &status)
                .await;
        }
        Ok(result)
    }

    /// Core implementation of create_task, callable without RequestContext for testing.
    pub async fn create_task_impl(
        &self,
        params: CreateTaskParams,
        actor: ActorRef,
    ) -> Result<CallToolResult, ErrorData> {
        self.authorize_or_error_with_actor(
            McpOperation::CallTool {
                tool_name: "create_task",
            },
            &actor,
        )
        .await?;
        let goal_type = if let Some(gt) = params.goal_type {
            use std::str::FromStr;
            Some(GoalType::from_str(&gt).map_err(|e| ErrorData::invalid_params(e, None))?)
        } else {
            None
        };

        let mut task = Task::new(actor.clone(), params.title, goal_type)
            .map_err(|e| ErrorData::internal_error(e.to_string(), None))?;

        if let Some(desc) = params.description {
            task.set_description(Some(desc));
        }

        if let Some(constraints) = params.constraints {
            for c in constraints {
                task.add_constraint(c);
            }
        }

        if let Some(criteria) = params.acceptance_criteria {
            for c in criteria {
                task.add_acceptance_criterion(c);
            }
        }

        // Set optional requested_by actor
        if let Some(rb_kind) = params.requested_by_kind {
            let rb_id = params.requested_by_id.as_deref().unwrap_or("unknown");
            let rb_actor_kind: ActorKind = rb_kind.as_str().into();
            let rb_actor = ActorRef::new(rb_actor_kind, rb_id)
                .map_err(|e| ErrorData::invalid_params(e, None))?;
            task.set_requester(Some(rb_actor));
        }

        // Add task dependencies
        if let Some(deps) = params.dependencies {
            for dep in deps {
                let dep_id = parse_uuid(&dep, "dependencies")?;
                self.ensure_object_exists("task", dep_id, "dependencies")
                    .await?;
                task.add_dependency(dep_id);
            }
        }

        // Set intent if provided
        if let Some(intent_id_str) = params.intent_id {
            let intent_id = parse_uuid(&intent_id_str, "intent_id")?;
            self.ensure_object_exists("intent", intent_id, "intent_id")
                .await?;
            task.set_intent(Some(intent_id));
        }

        if let Some(parent_task_id) = params.parent_task_id {
            let parent_task_id = parse_uuid(&parent_task_id, "parent_task_id")?;
            self.ensure_object_exists("task", parent_task_id, "parent_task_id")
                .await?;
            task.set_parent(Some(parent_task_id));
        }

        if let Some(origin_step_id) = params.origin_step_id {
            task.set_origin_step_id(Some(parse_uuid(&origin_step_id, "origin_step_id")?));
        }

        self.store_object(&task).await?;

        let initial_status = params
            .status
            .as_deref()
            .map(parse_task_event_kind)
            .transpose()?
            .unwrap_or(TaskEventKind::Created);
        let mut task_event = TaskEvent::new(actor, task.header().object_id(), initial_status)
            .map_err(|e| ErrorData::internal_error(e, None))?;
        task_event.set_reason(params.reason);
        self.store_object(&task_event).await?;

        Ok(CallToolResult::success(vec![Content::text(format!(
            "Task created with ID: {}",
            task.header().object_id()
        ))]))
    }

    async fn emit_code_ui_mcp_task_created(
        &self,
        task_id: Uuid,
        title: &str,
        status: &TaskEventKind,
    ) {
        let Some(session) = self.code_ui_session() else {
            return;
        };
        let now = Utc::now();
        let status_label = task_status_label(status).to_string();
        let task_id = task_id.to_string();
        session
            .upsert_task(CodeUiTaskSnapshot {
                id: task_id.clone(),
                title: Some(title.to_string()),
                status: status_label.clone(),
                details: Some("Created through MCP create_task".to_string()),
                updated_at: now,
            })
            .await;
        session
            .upsert_transcript_entry(CodeUiTranscriptEntry {
                id: format!("mcp-task-created-{task_id}"),
                kind: CodeUiTranscriptEntryKind::InfoNote,
                title: Some("MCP task created".to_string()),
                content: Some(format!("MCP create_task: {title} ({status_label})")),
                status: Some("completed".to_string()),
                streaming: false,
                metadata: json!({
                    "source": "mcp",
                    "tool": "create_task",
                    "taskId": task_id,
                }),
                created_at: now,
                updated_at: now,
            })
            .await;
    }

    #[tool(description = "List recent tasks")]
    pub async fn list_tasks(
        &self,
        Parameters(params): Parameters<ListTasksParams>,
    ) -> Result<CallToolResult, ErrorData> {
        self.list_tasks_impl(params).await
    }

    /// Core implementation of list_tasks, callable without rmcp Parameters wrapper.
    pub async fn list_tasks_impl(
        &self,
        params: ListTasksParams,
    ) -> Result<CallToolResult, ErrorData> {
        self.authorize_or_error(McpOperation::CallTool {
            tool_name: "list_tasks",
        })
        .await?;
        let history = self
            .intent_history_manager
            .as_ref()
            .ok_or_else(|| ErrorData::internal_error("History not available", None))?;
        let storage = self
            .storage
            .as_ref()
            .ok_or_else(|| ErrorData::internal_error("Storage not available", None))?;

        let objects = history
            .list_objects("task")
            .await
            .map_err(|e| ErrorData::internal_error(e.to_string(), None))?;
        let latest_status = self.latest_task_events().await?;

        let mut tasks_info = Vec::new();
        let limit = params.limit.unwrap_or(10);

        for (_id, hash) in objects.into_iter() {
            if tasks_info.len() >= limit {
                break;
            }
            // Read task from storage to get title/status
            if let Ok(task) = storage.get_json::<Task>(&hash).await {
                let status_kind = latest_status
                    .get(&task.header().object_id())
                    .cloned()
                    .unwrap_or(TaskEventKind::Created);
                let status = task_status_label(&status_kind);
                // Filter by status if requested
                if let Some(status_filter) = &params.status
                    && status != status_filter
                {
                    continue;
                }

                tasks_info.push(format!(
                    "ID: {} | Title: {} | Status: {}",
                    task.header().object_id(),
                    task.title(),
                    status
                ));
            }
        }

        if tasks_info.is_empty() {
            Ok(CallToolResult::success(vec![Content::text(
                "No tasks found.",
            )]))
        } else {
            Ok(CallToolResult::success(vec![Content::text(
                tasks_info.join("\n"),
            )]))
        }
    }

    #[tool(description = "Create a new Run")]
    pub async fn create_run(
        &self,
        ctx: RequestContext<RoleServer>,
        Parameters(params): Parameters<CreateRunParams>,
    ) -> Result<CallToolResult, ErrorData> {
        let actor = self.resolve_actor(
            &ctx,
            params.actor_kind.as_deref(),
            params.actor_id.as_deref(),
        )?;
        self.create_run_impl(params, actor).await
    }

    /// Core implementation of create_run, callable without RequestContext.
    pub async fn create_run_impl(
        &self,
        params: CreateRunParams,
        actor: ActorRef,
    ) -> Result<CallToolResult, ErrorData> {
        self.authorize_or_error_with_actor(
            McpOperation::CallTool {
                tool_name: "create_run",
            },
            &actor,
        )
        .await?;
        let task_id = parse_uuid(&params.task_id, "task_id")?;
        let task_for_checks = self
            .load_tracked_object::<Task>("task", task_id, "task_id")
            .await?;

        let base_commit_sha = self
            .resolve_base_commit_anchor(&params.base_commit_sha)
            .await?;

        let mut run = Run::new(actor.clone(), task_id, &base_commit_sha)
            .map_err(|e| ErrorData::invalid_params(e, None))?;

        if let Some(plan_id) = params.plan_id {
            let plan_id = parse_uuid(&plan_id, "plan_id")?;
            let plan_for_checks = self
                .load_tracked_object::<Plan>("plan", plan_id, "plan_id")
                .await?;
            if let (Some(task), Some(plan)) = (task_for_checks.as_ref(), plan_for_checks.as_ref())
                && let Some(task_intent) = task.intent()
                && plan.intent() != task_intent
            {
                return Err(ErrorData::invalid_params(
                    format!(
                        "plan_id intent {} does not match task intent {}",
                        plan.intent(),
                        task_intent
                    ),
                    None,
                ));
            }
            run.set_plan(Some(plan_id));
        }
        if let Some(id) = params.context_snapshot_id {
            let snapshot_id = parse_uuid(&id, "context_snapshot_id")?;
            self.ensure_object_exists("snapshot", snapshot_id, "context_snapshot_id")
                .await?;
            run.set_snapshot(Some(snapshot_id));
        }
        self.store_object(&run).await?;

        // 0.7 moved run lifecycle/error/metrics into RunEvent.
        let initial_status = params
            .status
            .as_deref()
            .map(parse_run_event_kind)
            .transpose()?
            .unwrap_or(RunEventKind::Created);
        let mut run_event = RunEvent::new(actor, run.header().object_id(), initial_status)
            .map_err(|e| ErrorData::internal_error(e, None))?;
        run_event.set_reason(params.reason);
        run_event.set_error(params.error);
        if let Some(metrics_json) = params.metrics_json {
            let metrics = serde_json::from_str(&metrics_json)
                .map_err(|e| ErrorData::invalid_params(e.to_string(), None))?;
            run_event.set_metrics(Some(metrics));
        }
        self.store_object(&run_event).await?;

        Ok(CallToolResult::success(vec![Content::text(format!(
            "Run created with ID: {}",
            run.header().object_id()
        ))]))
    }

    #[tool(description = "List recent runs")]
    pub async fn list_runs(
        &self,
        Parameters(params): Parameters<ListRunsParams>,
    ) -> Result<CallToolResult, ErrorData> {
        self.list_runs_impl(params).await
    }

    /// Core implementation of list_runs, callable without rmcp Parameters wrapper.
    pub async fn list_runs_impl(
        &self,
        params: ListRunsParams,
    ) -> Result<CallToolResult, ErrorData> {
        self.authorize_or_error(McpOperation::CallTool {
            tool_name: "list_runs",
        })
        .await?;
        let history = self
            .intent_history_manager
            .as_ref()
            .ok_or_else(|| ErrorData::internal_error("History not available", None))?;
        let storage = self
            .storage
            .as_ref()
            .ok_or_else(|| ErrorData::internal_error("Storage not available", None))?;

        let objects = history
            .list_objects("run")
            .await
            .map_err(|e| ErrorData::internal_error(e.to_string(), None))?;
        let latest_status = self.latest_run_events().await?;

        let mut out = Vec::new();
        let limit = params.limit.unwrap_or(10);
        for (_id, hash) in objects.into_iter() {
            if out.len() >= limit {
                break;
            }
            if let Ok(run) = storage.get_json::<Run>(&hash).await {
                let status_kind = latest_status
                    .get(&run.header().object_id())
                    .cloned()
                    .unwrap_or(RunEventKind::Created);
                let status = run_status_label(&status_kind);
                if let Some(status_filter) = &params.status
                    && status != status_filter
                {
                    continue;
                }
                out.push(format!(
                    "ID: {} | Task: {} | Status: {}",
                    run.header().object_id(),
                    run.task(),
                    status
                ));
            }
        }

        if out.is_empty() {
            Ok(CallToolResult::success(vec![Content::text(
                "No runs found.",
            )]))
        } else {
            Ok(CallToolResult::success(vec![Content::text(out.join("\n"))]))
        }
    }

    #[tool(description = "Create a new ContextSnapshot")]
    pub async fn create_context_snapshot(
        &self,
        ctx: RequestContext<RoleServer>,
        Parameters(params): Parameters<CreateContextSnapshotParams>,
    ) -> Result<CallToolResult, ErrorData> {
        let actor = self.resolve_actor(
            &ctx,
            params.actor_kind.as_deref(),
            params.actor_id.as_deref(),
        )?;
        self.create_context_snapshot_impl(params, actor).await
    }

    /// Core implementation of create_context_snapshot, callable without RequestContext.
    pub async fn create_context_snapshot_impl(
        &self,
        params: CreateContextSnapshotParams,
        actor: ActorRef,
    ) -> Result<CallToolResult, ErrorData> {
        self.authorize_or_error_with_actor(
            McpOperation::CallTool {
                tool_name: "create_context_snapshot",
            },
            &actor,
        )
        .await?;
        let strategy = match params.selection_strategy.as_str() {
            "explicit" => SelectionStrategy::Explicit,
            "heuristic" => SelectionStrategy::Heuristic,
            _ => {
                return Err(ErrorData::invalid_params(
                    "invalid selection_strategy",
                    None,
                ));
            }
        };

        let mut snapshot = ContextSnapshot::new(actor, strategy)
            .map_err(|e| ErrorData::invalid_params(e, None))?;

        if let Some(items) = params.items {
            for item in items {
                use git_internal::hash::ObjectHash;
                let mut ctx_item =
                    ContextItem::new(parse_context_item_kind(item.kind.as_deref()), item.path)
                        .map_err(|e| ErrorData::invalid_params(e, None))?;
                ctx_item.preview = item.preview;

                if let Some(blob_hash) = item.blob_hash.or(item.content_hash) {
                    let blob_hash = blob_hash
                        .parse::<ObjectHash>()
                        .map_err(|e| ErrorData::invalid_params(e.to_string(), None))?;
                    ctx_item.set_blob(Some(blob_hash));
                }

                snapshot.add_item(ctx_item);
            }
        }
        if let Some(summary) = params.summary {
            snapshot.set_summary(Some(summary));
        }

        self.store_object(&snapshot).await?;

        Ok(CallToolResult::success(vec![Content::text(format!(
            "ContextSnapshot created with ID: {}",
            snapshot.header().object_id()
        ))]))
    }

    #[tool(description = "List recent context snapshots")]
    pub async fn list_context_snapshots(
        &self,
        Parameters(params): Parameters<ListContextSnapshotsParams>,
    ) -> Result<CallToolResult, ErrorData> {
        self.list_context_snapshots_impl(params).await
    }

    /// Core implementation of list_context_snapshots, callable without rmcp Parameters wrapper.
    pub async fn list_context_snapshots_impl(
        &self,
        params: ListContextSnapshotsParams,
    ) -> Result<CallToolResult, ErrorData> {
        self.authorize_or_error(McpOperation::CallTool {
            tool_name: "list_context_snapshots",
        })
        .await?;
        let history = self
            .intent_history_manager
            .as_ref()
            .ok_or_else(|| ErrorData::internal_error("History not available", None))?;
        let storage = self
            .storage
            .as_ref()
            .ok_or_else(|| ErrorData::internal_error("Storage not available", None))?;

        let objects = history
            .list_objects("snapshot")
            .await
            .map_err(|e| ErrorData::internal_error(e.to_string(), None))?;

        let limit = params.limit.unwrap_or(10);
        let mut out = Vec::new();
        for (_id, hash) in objects.into_iter() {
            if out.len() >= limit {
                break;
            }
            if let Ok(snap) = storage.get_json::<ContextSnapshot>(&hash).await {
                out.push(format!(
                    "ID: {} | Strategy: {:?} | Items: {} | Summary: {}",
                    snap.header().object_id(),
                    snap.selection_strategy(),
                    snap.items().len(),
                    snap.summary().unwrap_or("-"),
                ));
            }
        }

        if out.is_empty() {
            Ok(CallToolResult::success(vec![Content::text(
                "No context snapshots found.",
            )]))
        } else {
            Ok(CallToolResult::success(vec![Content::text(out.join("\n"))]))
        }
    }

    #[tool(description = "Create a new Plan")]
    pub async fn create_plan(
        &self,
        ctx: RequestContext<RoleServer>,
        Parameters(params): Parameters<CreatePlanParams>,
    ) -> Result<CallToolResult, ErrorData> {
        let actor = self.resolve_actor(
            &ctx,
            params.actor_kind.as_deref(),
            params.actor_id.as_deref(),
        )?;
        self.create_plan_impl(params, actor).await
    }

    /// Core implementation of create_plan, callable without RequestContext.
    pub async fn create_plan_impl(
        &self,
        params: CreatePlanParams,
        actor: ActorRef,
    ) -> Result<CallToolResult, ErrorData> {
        self.authorize_or_error_with_actor(
            McpOperation::CallTool {
                tool_name: "create_plan",
            },
            &actor,
        )
        .await?;
        let intent_id = parse_uuid(&params.intent_id, "intent_id")?;
        self.ensure_object_exists("intent", intent_id, "intent_id")
            .await?;
        let mut plan =
            Plan::new(actor, intent_id).map_err(|e| ErrorData::internal_error(e, None))?;

        let parent_plan_ids = parse_uuid_vec(params.parent_plan_ids, "parent_plan_ids")?;
        if !parent_plan_ids.is_empty() {
            for parent_plan_id in &parent_plan_ids {
                let parent_plan = self
                    .load_tracked_object::<Plan>("plan", *parent_plan_id, "parent_plan_ids")
                    .await?;
                if let Some(parent_plan) = parent_plan
                    && parent_plan.intent() != intent_id
                {
                    return Err(ErrorData::invalid_params(
                        format!(
                            "parent_plan_ids must belong to intent {}: {} belongs to {}",
                            intent_id,
                            parent_plan.header().object_id(),
                            parent_plan.intent()
                        ),
                        None,
                    ));
                }
            }
            plan.set_parents(parent_plan_ids);
        }
        let context_frame_ids = parse_uuid_vec(params.context_frame_ids, "context_frame_ids")?;
        if !context_frame_ids.is_empty() {
            plan.set_context_frames(context_frame_ids);
        }

        if let Some(steps) = params.steps {
            for step in steps {
                let mut ps = PlanStep::new(step.description);
                ps.set_inputs(step.inputs);
                ps.set_checks(step.checks);
                plan.add_step(ps);
            }
        }
        self.store_object(&plan).await?;

        Ok(CallToolResult::success(vec![Content::text(format!(
            "Plan created with ID: {}",
            plan.header().object_id()
        ))]))
    }

    #[tool(description = "List recent plans")]
    pub async fn list_plans(
        &self,
        Parameters(params): Parameters<ListPlansParams>,
    ) -> Result<CallToolResult, ErrorData> {
        self.list_plans_impl(params).await
    }

    /// Core implementation of list_plans, callable without rmcp Parameters wrapper.
    pub async fn list_plans_impl(
        &self,
        params: ListPlansParams,
    ) -> Result<CallToolResult, ErrorData> {
        self.authorize_or_error(McpOperation::CallTool {
            tool_name: "list_plans",
        })
        .await?;
        let history = self
            .intent_history_manager
            .as_ref()
            .ok_or_else(|| ErrorData::internal_error("History not available", None))?;
        let storage = self
            .storage
            .as_ref()
            .ok_or_else(|| ErrorData::internal_error("Storage not available", None))?;

        let objects = history
            .list_objects("plan")
            .await
            .map_err(|e| ErrorData::internal_error(e.to_string(), None))?;

        let limit = params.limit.unwrap_or(10);
        let mut out = Vec::new();
        for (_id, hash) in objects.into_iter() {
            if out.len() >= limit {
                break;
            }
            if let Ok(plan) = storage.get_json::<Plan>(&hash).await {
                out.push(format!(
                    "ID: {} | Steps: {}",
                    plan.header().object_id(),
                    plan.steps().len(),
                ));
            }
        }

        if out.is_empty() {
            Ok(CallToolResult::success(vec![Content::text(
                "No plans found.",
            )]))
        } else {
            Ok(CallToolResult::success(vec![Content::text(out.join("\n"))]))
        }
    }

    #[tool(description = "Create a new PatchSet")]
    pub async fn create_patchset(
        &self,
        ctx: RequestContext<RoleServer>,
        Parameters(params): Parameters<CreatePatchSetParams>,
    ) -> Result<CallToolResult, ErrorData> {
        let actor = self.resolve_actor(
            &ctx,
            params.actor_kind.as_deref(),
            params.actor_id.as_deref(),
        )?;
        self.create_patchset_impl(params, actor).await
    }

    /// Core implementation of create_patchset, callable without RequestContext.
    pub async fn create_patchset_impl(
        &self,
        params: CreatePatchSetParams,
        actor: ActorRef,
    ) -> Result<CallToolResult, ErrorData> {
        let run_id = parse_uuid(&params.run_id, "run_id")?;
        self.ensure_object_exists("run", run_id, "run_id").await?;

        let base_commit_sha = self
            .resolve_base_commit_anchor(&params.base_commit_sha)
            .await?;

        let mut patchset = PatchSet::new(actor, run_id, &base_commit_sha)
            .map_err(|e| ErrorData::invalid_params(e, None))?;
        patchset.set_sequence(params.sequence.unwrap_or(params.generation));

        if let Some(files) = params.touched_files {
            for f in files {
                let ct = match f.change_type.as_str() {
                    "add" => ChangeType::Add,
                    "modify" => ChangeType::Modify,
                    "delete" => ChangeType::Delete,
                    "rename" => ChangeType::Rename,
                    "copy" => ChangeType::Copy,
                    _ => return Err(ErrorData::invalid_params("invalid change_type", None)),
                };
                let touched = TouchedFile::new(f.path, ct, f.lines_added, f.lines_deleted)
                    .map_err(|e| ErrorData::invalid_params(e, None))?;
                patchset.add_touched(touched);
            }
        }
        patchset.set_rationale(params.rationale);
        if let Some(artifact_params) = params.diff_artifact {
            let artifact = convert_artifact(artifact_params)?;
            patchset.set_artifact(Some(artifact));
        }

        if let Some(format) = params.diff_format {
            match format.as_str() {
                "unified_diff" => {}
                "git_diff" => {
                    if patchset.format() != &DiffFormat::GitDiff {
                        return Err(ErrorData::invalid_params(
                            "git_diff format is not writable in git-internal yet",
                            None,
                        ));
                    }
                }
                _ => return Err(ErrorData::invalid_params("invalid diff_format", None)),
            }
        }

        self.store_object(&patchset).await?;

        Ok(CallToolResult::success(vec![Content::text(format!(
            "PatchSet created with ID: {}",
            patchset.header().object_id()
        ))]))
    }

    #[tool(description = "List recent patchsets")]
    pub async fn list_patchsets(
        &self,
        Parameters(params): Parameters<ListPatchSetsParams>,
    ) -> Result<CallToolResult, ErrorData> {
        self.list_patchsets_impl(params).await
    }

    /// Core implementation of list_patchsets, callable without rmcp Parameters wrapper.
    pub async fn list_patchsets_impl(
        &self,
        params: ListPatchSetsParams,
    ) -> Result<CallToolResult, ErrorData> {
        let history = self
            .intent_history_manager
            .as_ref()
            .ok_or_else(|| ErrorData::internal_error("History not available", None))?;
        let storage = self
            .storage
            .as_ref()
            .ok_or_else(|| ErrorData::internal_error("Storage not available", None))?;

        let objects = history
            .list_objects("patchset")
            .await
            .map_err(|e| ErrorData::internal_error(e.to_string(), None))?;

        let limit = params.limit.unwrap_or(10);
        let mut out = Vec::new();
        for (_id, hash) in objects.into_iter() {
            if out.len() >= limit {
                break;
            }
            if let Ok(ps) = storage.get_json::<PatchSet>(&hash).await {
                out.push(format!(
                    "ID: {} | Run: {} | Seq: {} | Files: {} | Format: {:?}",
                    ps.header().object_id(),
                    ps.run(),
                    ps.sequence(),
                    ps.touched().len(),
                    ps.format(),
                ));
            }
        }

        if out.is_empty() {
            Ok(CallToolResult::success(vec![Content::text(
                "No patchsets found.",
            )]))
        } else {
            Ok(CallToolResult::success(vec![Content::text(out.join("\n"))]))
        }
    }

    #[tool(description = "Create a new Evidence")]
    pub async fn create_evidence(
        &self,
        ctx: RequestContext<RoleServer>,
        Parameters(params): Parameters<CreateEvidenceParams>,
    ) -> Result<CallToolResult, ErrorData> {
        let actor = self.resolve_actor(
            &ctx,
            params.actor_kind.as_deref(),
            params.actor_id.as_deref(),
        )?;
        self.create_evidence_impl(params, actor).await
    }

    /// Core implementation of create_evidence, callable without RequestContext.
    pub async fn create_evidence_impl(
        &self,
        params: CreateEvidenceParams,
        actor: ActorRef,
    ) -> Result<CallToolResult, ErrorData> {
        let run_id = parse_uuid(&params.run_id, "run_id")?;
        self.ensure_object_exists("run", run_id, "run_id").await?;

        let kind = match params.kind.as_str() {
            "test" => EvidenceKind::Test,
            "lint" => EvidenceKind::Lint,
            "build" => EvidenceKind::Build,
            other => EvidenceKind::Other(other.to_string()),
        };

        let mut evidence = Evidence::new(actor, run_id, kind, params.tool)
            .map_err(|e| ErrorData::internal_error(e, None))?;

        if let Some(id) = params.patchset_id {
            let parsed = parse_uuid(&id, "patchset_id")?;
            let patchset = self
                .load_tracked_object::<PatchSet>("patchset", parsed, "patchset_id")
                .await?;
            if let Some(patchset) = patchset
                && patchset.run() != run_id
            {
                return Err(ErrorData::invalid_params(
                    format!(
                        "patchset_id {} belongs to run {}, not {}",
                        parsed,
                        patchset.run(),
                        run_id
                    ),
                    None,
                ));
            }
            evidence.set_patchset_id(Some(parsed));
        }
        evidence.set_command(params.command);
        evidence.set_exit_code(params.exit_code);
        evidence.set_summary(params.summary);
        if let Some(artifacts) = params.report_artifacts {
            for ap in artifacts {
                let artifact = convert_artifact(ap)?;
                evidence.add_report_artifact(artifact);
            }
        }

        self.store_object(&evidence).await?;

        Ok(CallToolResult::success(vec![Content::text(format!(
            "Evidence created with ID: {}",
            evidence.header().object_id()
        ))]))
    }

    #[tool(description = "List recent evidences")]
    pub async fn list_evidences(
        &self,
        Parameters(params): Parameters<ListEvidencesParams>,
    ) -> Result<CallToolResult, ErrorData> {
        self.list_evidences_impl(params).await
    }

    /// Core implementation of list_evidences, callable without rmcp Parameters wrapper.
    pub async fn list_evidences_impl(
        &self,
        params: ListEvidencesParams,
    ) -> Result<CallToolResult, ErrorData> {
        let history = self
            .intent_history_manager
            .as_ref()
            .ok_or_else(|| ErrorData::internal_error("History not available", None))?;
        let storage = self
            .storage
            .as_ref()
            .ok_or_else(|| ErrorData::internal_error("Storage not available", None))?;

        let objects = history
            .list_objects("evidence")
            .await
            .map_err(|e| ErrorData::internal_error(e.to_string(), None))?;

        let limit = params.limit.unwrap_or(10);
        let mut out = Vec::new();
        for (_id, hash) in objects.into_iter() {
            if out.len() >= limit {
                break;
            }
            if let Ok(ev) = storage.get_json::<Evidence>(&hash).await {
                out.push(format!(
                    "ID: {} | Kind: {:?} | Tool: {} | Exit: {} | Summary: {}",
                    ev.header().object_id(),
                    ev.kind(),
                    ev.tool(),
                    ev.exit_code().map_or("-".to_string(), |c| c.to_string()),
                    ev.summary().unwrap_or("-"),
                ));
            }
        }

        if out.is_empty() {
            Ok(CallToolResult::success(vec![Content::text(
                "No evidences found.",
            )]))
        } else {
            Ok(CallToolResult::success(vec![Content::text(out.join("\n"))]))
        }
    }

    #[tool(description = "Create a new ToolInvocation")]
    pub async fn create_tool_invocation(
        &self,
        ctx: RequestContext<RoleServer>,
        Parameters(params): Parameters<CreateToolInvocationParams>,
    ) -> Result<CallToolResult, ErrorData> {
        let actor = self.resolve_actor(
            &ctx,
            params.actor_kind.as_deref(),
            params.actor_id.as_deref(),
        )?;
        self.create_tool_invocation_impl(params, actor).await
    }

    /// Core implementation of create_tool_invocation, callable without RequestContext.
    pub async fn create_tool_invocation_impl(
        &self,
        params: CreateToolInvocationParams,
        actor: ActorRef,
    ) -> Result<CallToolResult, ErrorData> {
        let run_id = parse_uuid(&params.run_id, "run_id")?;
        self.ensure_object_exists("run", run_id, "run_id").await?;

        let mut inv = ToolInvocation::new(actor, run_id, params.tool_name)
            .map_err(|e| ErrorData::internal_error(e, None))?;
        if let Some(status) = params.status {
            inv.set_status(match status.as_str() {
                "ok" => ToolStatus::Ok,
                "error" => ToolStatus::Error,
                _ => return Err(ErrorData::invalid_params("invalid tool status", None)),
            });
        }
        if let Some(args_json) = params.args_json {
            let args = serde_json::from_str(&args_json)
                .map_err(|e| ErrorData::invalid_params(e.to_string(), None))?;
            inv.set_args(args);
        }
        inv.set_io_footprint(params.io_footprint.map(|p| IoFootprint {
            paths_read: p.paths_read.unwrap_or_default(),
            paths_written: p.paths_written.unwrap_or_default(),
        }));
        inv.set_result_summary(params.result_summary);
        if let Some(artifacts) = params.artifacts {
            for ap in artifacts {
                let artifact = convert_artifact(ap)?;
                inv.add_artifact(artifact);
            }
        }

        self.store_object(&inv).await?;

        Ok(CallToolResult::success(vec![Content::text(format!(
            "ToolInvocation created with ID: {}",
            inv.header().object_id()
        ))]))
    }

    #[tool(description = "List recent tool invocations")]
    pub async fn list_tool_invocations(
        &self,
        Parameters(params): Parameters<ListToolInvocationsParams>,
    ) -> Result<CallToolResult, ErrorData> {
        self.list_tool_invocations_impl(params).await
    }

    /// Core implementation of list_tool_invocations, callable without rmcp Parameters wrapper.
    pub async fn list_tool_invocations_impl(
        &self,
        params: ListToolInvocationsParams,
    ) -> Result<CallToolResult, ErrorData> {
        let history = self
            .intent_history_manager
            .as_ref()
            .ok_or_else(|| ErrorData::internal_error("History not available", None))?;
        let storage = self
            .storage
            .as_ref()
            .ok_or_else(|| ErrorData::internal_error("Storage not available", None))?;

        let objects = history
            .list_objects("invocation")
            .await
            .map_err(|e| ErrorData::internal_error(e.to_string(), None))?;

        let limit = params.limit.unwrap_or(10);
        let mut out = Vec::new();
        for (_id, hash) in objects.into_iter() {
            if out.len() >= limit {
                break;
            }
            if let Ok(inv) = storage.get_json::<ToolInvocation>(&hash).await {
                out.push(format!(
                    "ID: {} | Tool: {} | Status: {:?} | Summary: {}",
                    inv.header().object_id(),
                    inv.tool_name(),
                    inv.status(),
                    inv.result_summary().unwrap_or("-"),
                ));
            }
        }

        if out.is_empty() {
            Ok(CallToolResult::success(vec![Content::text(
                "No tool invocations found.",
            )]))
        } else {
            Ok(CallToolResult::success(vec![Content::text(out.join("\n"))]))
        }
    }

    #[tool(description = "Create a new Provenance")]
    pub async fn create_provenance(
        &self,
        ctx: RequestContext<RoleServer>,
        Parameters(params): Parameters<CreateProvenanceParams>,
    ) -> Result<CallToolResult, ErrorData> {
        let actor = self.resolve_actor(
            &ctx,
            params.actor_kind.as_deref(),
            params.actor_id.as_deref(),
        )?;
        self.create_provenance_impl(params, actor).await
    }

    /// Core implementation of create_provenance, callable without RequestContext.
    pub async fn create_provenance_impl(
        &self,
        params: CreateProvenanceParams,
        actor: ActorRef,
    ) -> Result<CallToolResult, ErrorData> {
        let run_id = parse_uuid(&params.run_id, "run_id")?;
        self.ensure_object_exists("run", run_id, "run_id").await?;

        let mut prov = Provenance::new(actor, run_id, params.provider, params.model)
            .map_err(|e| ErrorData::internal_error(e, None))?;
        let parameters = if let Some(parameters_json) = params.parameters_json {
            Some(
                serde_json::from_str(&parameters_json)
                    .map_err(|e| ErrorData::invalid_params(e.to_string(), None))?,
            )
        } else {
            None
        };
        prov.set_parameters(parameters);
        prov.set_temperature(params.temperature);
        prov.set_max_tokens(params.max_tokens);

        self.store_object(&prov).await?;

        Ok(CallToolResult::success(vec![Content::text(format!(
            "Provenance created with ID: {}",
            prov.header().object_id()
        ))]))
    }

    #[tool(description = "List recent provenances")]
    pub async fn list_provenances(
        &self,
        Parameters(params): Parameters<ListProvenancesParams>,
    ) -> Result<CallToolResult, ErrorData> {
        self.list_provenances_impl(params).await
    }

    /// Core implementation of list_provenances, callable without rmcp Parameters wrapper.
    pub async fn list_provenances_impl(
        &self,
        params: ListProvenancesParams,
    ) -> Result<CallToolResult, ErrorData> {
        let history = self
            .intent_history_manager
            .as_ref()
            .ok_or_else(|| ErrorData::internal_error("History not available", None))?;
        let storage = self
            .storage
            .as_ref()
            .ok_or_else(|| ErrorData::internal_error("Storage not available", None))?;

        let objects = history
            .list_objects("provenance")
            .await
            .map_err(|e| ErrorData::internal_error(e.to_string(), None))?;

        let limit = params.limit.unwrap_or(10);
        let mut out = Vec::new();
        for (_id, hash) in objects.into_iter() {
            if out.len() >= limit {
                break;
            }
            if let Ok(prov) = storage.get_json::<Provenance>(&hash).await {
                out.push(format!(
                    "ID: {} | Provider: {} | Model: {}",
                    prov.header().object_id(),
                    prov.provider(),
                    prov.model(),
                ));
            }
        }

        if out.is_empty() {
            Ok(CallToolResult::success(vec![Content::text(
                "No provenances found.",
            )]))
        } else {
            Ok(CallToolResult::success(vec![Content::text(out.join("\n"))]))
        }
    }

    #[tool(description = "Create a new Decision")]
    pub async fn create_decision(
        &self,
        ctx: RequestContext<RoleServer>,
        Parameters(params): Parameters<CreateDecisionParams>,
    ) -> Result<CallToolResult, ErrorData> {
        let actor = self.resolve_actor(
            &ctx,
            params.actor_kind.as_deref(),
            params.actor_id.as_deref(),
        )?;
        self.create_decision_impl(params, actor).await
    }

    /// Core implementation of create_decision, callable without RequestContext.
    pub async fn create_decision_impl(
        &self,
        params: CreateDecisionParams,
        actor: ActorRef,
    ) -> Result<CallToolResult, ErrorData> {
        let run_id = parse_uuid(&params.run_id, "run_id")?;
        self.ensure_object_exists("run", run_id, "run_id").await?;

        let decision_type = match params.decision_type.as_str() {
            "commit" => DecisionType::Commit,
            "checkpoint" => DecisionType::Checkpoint,
            "abandon" => DecisionType::Abandon,
            "retry" => DecisionType::Retry,
            "rollback" => DecisionType::Rollback,
            other => DecisionType::Other(other.to_string()),
        };

        let mut decision = Decision::new(actor, run_id, decision_type)
            .map_err(|e| ErrorData::internal_error(e, None))?;

        if let Some(id) = params.chosen_patchset_id {
            let parsed = parse_uuid(&id, "chosen_patchset_id")?;
            let patchset = self
                .load_tracked_object::<PatchSet>("patchset", parsed, "chosen_patchset_id")
                .await?;
            if let Some(patchset) = patchset
                && patchset.run() != run_id
            {
                return Err(ErrorData::invalid_params(
                    format!(
                        "chosen_patchset_id {} belongs to run {}, not {}",
                        parsed,
                        patchset.run(),
                        run_id
                    ),
                    None,
                ));
            }
            decision.set_chosen_patchset_id(Some(parsed));
        }
        decision.set_checkpoint_id(params.checkpoint_id);
        decision.set_rationale(params.rationale);

        // Set result commit SHA if provided
        if let Some(sha) = params.result_commit_sha {
            let normalized =
                normalize_commit_anchor(&sha).map_err(|e| ErrorData::invalid_params(e, None))?;
            let hash_val = normalized
                .parse()
                .map_err(|e: String| ErrorData::invalid_params(e, None))?;
            decision.set_result_commit_sha(Some(hash_val));
        }

        self.store_object(&decision).await?;

        Ok(CallToolResult::success(vec![Content::text(format!(
            "Decision created with ID: {}",
            decision.header().object_id()
        ))]))
    }

    #[tool(description = "List recent decisions")]
    pub async fn list_decisions(
        &self,
        Parameters(params): Parameters<ListDecisionsParams>,
    ) -> Result<CallToolResult, ErrorData> {
        self.list_decisions_impl(params).await
    }

    /// Core implementation of list_decisions, callable without rmcp Parameters wrapper.
    pub async fn list_decisions_impl(
        &self,
        params: ListDecisionsParams,
    ) -> Result<CallToolResult, ErrorData> {
        let history = self
            .intent_history_manager
            .as_ref()
            .ok_or_else(|| ErrorData::internal_error("History not available", None))?;
        let storage = self
            .storage
            .as_ref()
            .ok_or_else(|| ErrorData::internal_error("Storage not available", None))?;

        let objects = history
            .list_objects("decision")
            .await
            .map_err(|e| ErrorData::internal_error(e.to_string(), None))?;

        let limit = params.limit.unwrap_or(10);
        let mut out = Vec::new();
        for (_id, hash) in objects.into_iter() {
            if out.len() >= limit {
                break;
            }
            if let Ok(dec) = storage.get_json::<Decision>(&hash).await {
                out.push(format!(
                    "ID: {} | Type: {:?} | Rationale: {}",
                    dec.header().object_id(),
                    dec.decision_type(),
                    dec.rationale().unwrap_or("-"),
                ));
            }
        }

        if out.is_empty() {
            Ok(CallToolResult::success(vec![Content::text(
                "No decisions found.",
            )]))
        } else {
            Ok(CallToolResult::success(vec![Content::text(out.join("\n"))]))
        }
    }

    // ── ContextFrame tools ──────────────────────────────────────────

    #[tool(description = "Create a new ContextFrame (incremental context window entry)")]
    pub async fn create_context_frame(
        &self,
        ctx: RequestContext<RoleServer>,
        Parameters(params): Parameters<CreateContextFrameParams>,
    ) -> Result<CallToolResult, ErrorData> {
        let actor = self.resolve_actor(
            &ctx,
            params.actor_kind.as_deref(),
            params.actor_id.as_deref(),
        )?;
        self.create_context_frame_impl(params, actor).await
    }

    /// Core implementation of create_context_frame, callable without RequestContext.
    pub async fn create_context_frame_impl(
        &self,
        params: CreateContextFrameParams,
        actor: ActorRef,
    ) -> Result<CallToolResult, ErrorData> {
        let kind = parse_frame_kind(&params.kind);

        let mut frame = ContextFrame::new(actor, kind, params.summary)
            .map_err(|e| ErrorData::internal_error(e, None))?;

        if let Some(id) = params.intent_id {
            let parsed = parse_uuid(&id, "intent_id")?;
            self.ensure_object_exists("intent", parsed, "intent_id")
                .await?;
            frame.set_intent_id(Some(parsed));
        }
        if let Some(id) = params.run_id {
            let parsed = parse_uuid(&id, "run_id")?;
            self.ensure_object_exists("run", parsed, "run_id").await?;
            frame.set_run_id(Some(parsed));
        }
        if let Some(id) = params.plan_id {
            let parsed = parse_uuid(&id, "plan_id")?;
            self.ensure_object_exists("plan", parsed, "plan_id").await?;
            frame.set_plan_id(Some(parsed));
        }
        if let Some(id) = params.step_id {
            let parsed = parse_uuid(&id, "step_id")?;
            frame.set_step_id(Some(parsed));
        }
        if let Some(data) = params.data {
            frame.set_data(Some(data));
        }
        if let Some(est) = params.token_estimate {
            frame.set_token_estimate(Some(est));
        }

        self.store_object(&frame).await?;

        Ok(CallToolResult::success(vec![Content::text(format!(
            "ContextFrame created with ID: {}",
            frame.header().object_id()
        ))]))
    }

    #[tool(description = "List recent context frames")]
    pub async fn list_context_frames(
        &self,
        Parameters(params): Parameters<ListContextFramesParams>,
    ) -> Result<CallToolResult, ErrorData> {
        self.list_context_frames_impl(params).await
    }

    /// Core implementation of list_context_frames, callable without rmcp Parameters wrapper.
    pub async fn list_context_frames_impl(
        &self,
        params: ListContextFramesParams,
    ) -> Result<CallToolResult, ErrorData> {
        let history = self
            .intent_history_manager
            .as_ref()
            .ok_or_else(|| ErrorData::internal_error("History not available", None))?;
        let storage = self
            .storage
            .as_ref()
            .ok_or_else(|| ErrorData::internal_error("Storage not available", None))?;

        let objects = history
            .list_objects("context_frame")
            .await
            .map_err(|e| ErrorData::internal_error(e.to_string(), None))?;

        let limit = params.limit.unwrap_or(10);
        let mut out = Vec::new();
        for (_id, hash) in objects.into_iter() {
            if out.len() >= limit {
                break;
            }
            if let Ok(frame) = storage.get_json::<ContextFrame>(&hash).await {
                out.push(format!(
                    "ID: {} | Kind: {:?} | Summary: {} | Tokens: {}",
                    frame.header().object_id(),
                    frame.kind(),
                    frame.summary(),
                    frame
                        .token_estimate()
                        .map_or("-".to_string(), |t| t.to_string()),
                ));
            }
        }

        if out.is_empty() {
            Ok(CallToolResult::success(vec![Content::text(
                "No context frames found.",
            )]))
        } else {
            Ok(CallToolResult::success(vec![Content::text(out.join("\n"))]))
        }
    }

    // ── PlanStepEvent tools ─────────────────────────────────────────

    #[tool(description = "Create a new PlanStepEvent (step execution lifecycle event)")]
    pub async fn create_plan_step_event(
        &self,
        ctx: RequestContext<RoleServer>,
        Parameters(params): Parameters<CreatePlanStepEventParams>,
    ) -> Result<CallToolResult, ErrorData> {
        let actor = self.resolve_actor(
            &ctx,
            params.actor_kind.as_deref(),
            params.actor_id.as_deref(),
        )?;
        self.create_plan_step_event_impl(params, actor).await
    }

    /// Core implementation of create_plan_step_event, callable without RequestContext.
    pub async fn create_plan_step_event_impl(
        &self,
        params: CreatePlanStepEventParams,
        actor: ActorRef,
    ) -> Result<CallToolResult, ErrorData> {
        let plan_id = parse_uuid(&params.plan_id, "plan_id")?;
        let step_id = parse_uuid(&params.step_id, "step_id")?;
        let run_id = parse_uuid(&params.run_id, "run_id")?;
        let status = parse_plan_step_status(&params.status)?;

        self.ensure_object_exists("plan", plan_id, "plan_id")
            .await?;
        self.ensure_object_exists("run", run_id, "run_id").await?;

        let mut event = PlanStepEvent::new(actor, plan_id, step_id, run_id, status)
            .map_err(|e| ErrorData::internal_error(e, None))?;

        if let Some(reason) = params.reason {
            event.set_reason(Some(reason));
        }
        if let Some(ids) = params.consumed_frames {
            let uuids: Vec<Uuid> = ids
                .iter()
                .map(|s| parse_uuid(s, "consumed_frames"))
                .collect::<Result<_, _>>()?;
            event.set_consumed_frames(uuids);
        }
        if let Some(ids) = params.produced_frames {
            let uuids: Vec<Uuid> = ids
                .iter()
                .map(|s| parse_uuid(s, "produced_frames"))
                .collect::<Result<_, _>>()?;
            event.set_produced_frames(uuids);
        }
        if let Some(id) = params.spawned_task_id {
            let parsed = parse_uuid(&id, "spawned_task_id")?;
            event.set_spawned_task_id(Some(parsed));
        }
        if let Some(outputs) = params.outputs {
            event.set_outputs(Some(outputs));
        }

        self.store_object(&event).await?;

        Ok(CallToolResult::success(vec![Content::text(format!(
            "PlanStepEvent created with ID: {}",
            event.header().object_id()
        ))]))
    }

    #[tool(description = "List recent plan step events")]
    pub async fn list_plan_step_events(
        &self,
        Parameters(params): Parameters<ListPlanStepEventsParams>,
    ) -> Result<CallToolResult, ErrorData> {
        self.list_plan_step_events_impl(params).await
    }

    /// Core implementation of list_plan_step_events, callable without rmcp Parameters wrapper.
    pub async fn list_plan_step_events_impl(
        &self,
        params: ListPlanStepEventsParams,
    ) -> Result<CallToolResult, ErrorData> {
        let history = self
            .intent_history_manager
            .as_ref()
            .ok_or_else(|| ErrorData::internal_error("History not available", None))?;
        let storage = self
            .storage
            .as_ref()
            .ok_or_else(|| ErrorData::internal_error("Storage not available", None))?;

        let objects = history
            .list_objects("plan_step_event")
            .await
            .map_err(|e| ErrorData::internal_error(e.to_string(), None))?;

        let limit = params.limit.unwrap_or(10);
        let mut out = Vec::new();
        for (_id, hash) in objects.into_iter() {
            if out.len() >= limit {
                break;
            }
            if let Ok(evt) = storage.get_json::<PlanStepEvent>(&hash).await {
                out.push(format!(
                    "ID: {} | Plan: {} | Step: {} | Status: {:?} | Reason: {}",
                    evt.header().object_id(),
                    evt.plan_id(),
                    evt.step_id(),
                    evt.status(),
                    evt.reason().unwrap_or("-"),
                ));
            }
        }

        if out.is_empty() {
            Ok(CallToolResult::success(vec![Content::text(
                "No plan step events found.",
            )]))
        } else {
            Ok(CallToolResult::success(vec![Content::text(out.join("\n"))]))
        }
    }

    // ── RunUsage tools ──────────────────────────────────────────────

    #[tool(description = "Record token/cost usage for a run")]
    pub async fn create_run_usage(
        &self,
        ctx: RequestContext<RoleServer>,
        Parameters(params): Parameters<CreateRunUsageParams>,
    ) -> Result<CallToolResult, ErrorData> {
        let actor = self.resolve_actor(
            &ctx,
            params.actor_kind.as_deref(),
            params.actor_id.as_deref(),
        )?;
        self.create_run_usage_impl(params, actor).await
    }

    /// Core implementation of create_run_usage, callable without RequestContext.
    pub async fn create_run_usage_impl(
        &self,
        params: CreateRunUsageParams,
        actor: ActorRef,
    ) -> Result<CallToolResult, ErrorData> {
        let run_id = parse_uuid(&params.run_id, "run_id")?;
        self.ensure_object_exists("run", run_id, "run_id").await?;

        let usage = RunUsage::new(
            actor,
            run_id,
            params.input_tokens,
            params.output_tokens,
            params.cost_usd,
        )
        .map_err(|e| ErrorData::internal_error(e, None))?;

        self.store_object(&usage).await?;

        Ok(CallToolResult::success(vec![Content::text(format!(
            "RunUsage created with ID: {} | Input: {} | Output: {} | Cost: {}",
            usage.header().object_id(),
            usage.input_tokens(),
            usage.output_tokens(),
            usage
                .cost_usd()
                .map_or("-".to_string(), |c| format!("${:.4}", c)),
        ))]))
    }

    #[tool(description = "List recent run usage records")]
    pub async fn list_run_usages(
        &self,
        Parameters(params): Parameters<ListRunUsagesParams>,
    ) -> Result<CallToolResult, ErrorData> {
        self.list_run_usages_impl(params).await
    }

    /// Core implementation of list_run_usages, callable without rmcp Parameters wrapper.
    pub async fn list_run_usages_impl(
        &self,
        params: ListRunUsagesParams,
    ) -> Result<CallToolResult, ErrorData> {
        let history = self
            .intent_history_manager
            .as_ref()
            .ok_or_else(|| ErrorData::internal_error("History not available", None))?;
        let storage = self
            .storage
            .as_ref()
            .ok_or_else(|| ErrorData::internal_error("Storage not available", None))?;

        let objects = history
            .list_objects("run_usage")
            .await
            .map_err(|e| ErrorData::internal_error(e.to_string(), None))?;

        let limit = params.limit.unwrap_or(10);
        let mut out = Vec::new();
        for (_id, hash) in objects.into_iter() {
            if out.len() >= limit {
                break;
            }
            if let Ok(u) = storage.get_json::<RunUsage>(&hash).await {
                out.push(format!(
                    "ID: {} | Run: {} | In: {} | Out: {} | Cost: {}",
                    u.header().object_id(),
                    u.run_id(),
                    u.input_tokens(),
                    u.output_tokens(),
                    u.cost_usd()
                        .map_or("-".to_string(), |c| format!("${:.4}", c)),
                ));
            }
        }

        if out.is_empty() {
            Ok(CallToolResult::success(vec![Content::text(
                "No run usage records found.",
            )]))
        } else {
            Ok(CallToolResult::success(vec![Content::text(out.join("\n"))]))
        }
    }
}

impl LibraMcpServer {
    /// Public accessor for the tool router generated by `#[tool_router]`.
    ///
    /// The `#[tool_router]` macro generates a private `tool_router()` method on
    /// the impl block where the `#[tool]` methods live (this file). This wrapper
    /// re-exports it so `server.rs` (`new()`) can call it.
    pub(crate) fn build_tool_router() -> ToolRouter<Self> {
        Self::tool_router()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn normalize_libra_vcs_command_allows_status() {
        assert_eq!(normalize_libra_vcs_command("status").unwrap(), "status");
    }

    #[test]
    fn normalize_libra_vcs_command_rejects_git() {
        let err = normalize_libra_vcs_command("git").unwrap_err();
        assert!(err.message.contains("unsupported Libra VCS command"));
        assert!(
            err.message
                .contains(crate::internal::ai::libra_vcs::ALLOWED_COMMANDS_DISPLAY)
        );
    }

    #[test]
    fn normalize_libra_vcs_command_rejects_shell_like_input() {
        let err = normalize_libra_vcs_command("status --short").unwrap_err();
        assert!(err.message.contains("single allowlisted Libra command"));
    }

    #[test]
    fn validate_libra_vcs_args_rejects_git_binary() {
        let err = validate_libra_vcs_args(&["/usr/bin/git".to_string()]).unwrap_err();
        assert!(err.message.contains("git is not allowed"));
    }

    #[test]
    fn run_libra_vcs_status_compat_rewrites_git_untracked_shorthand() {
        let args = normalize_tool_args("status", &["-uall".to_string()]).unwrap();

        assert_eq!(args, vec!["--untracked-files=all"]);
    }

    #[test]
    fn run_libra_vcs_status_compat_rejects_status_a_with_hint() {
        let err = normalize_tool_args("status", &["-a".to_string()]).unwrap_err();

        assert!(err.contains("--untracked-files=all"));
    }

    #[test]
    fn run_libra_vcs_params_accepts_stringified_args_array() {
        let params: RunLibraVcsParams = serde_json::from_value(serde_json::json!({
            "command": "add",
            "args": "[\"add\", \".\"]"
        }))
        .unwrap();

        assert_eq!(params.command, "add");
        assert_eq!(params.args, Some(vec![".".to_string()]));
    }

    #[test]
    fn run_libra_vcs_params_accepts_args_object_flags() {
        let params: RunLibraVcsParams = serde_json::from_value(serde_json::json!({
            "command": "status",
            "args": {"short": true, "ignored": false}
        }))
        .unwrap();

        assert_eq!(params.command, "status");
        assert_eq!(params.args, Some(vec!["--short".to_string()]));
    }

    #[test]
    fn run_libra_vcs_params_accepts_dashed_arg_object_flags() {
        let params: RunLibraVcsParams = serde_json::from_value(serde_json::json!({
            "command": "status",
            "args": {"--porcelain": "v2", "--untracked-files": "all"}
        }))
        .unwrap();

        assert_eq!(params.command, "status");
        assert_eq!(
            params.args,
            Some(vec![
                "--porcelain".to_string(),
                "v2".to_string(),
                "--untracked-files".to_string(),
                "all".to_string()
            ])
        );
    }

    #[test]
    fn run_libra_vcs_params_treats_pathspec_keys_as_positionals() {
        let params: RunLibraVcsParams = serde_json::from_value(serde_json::json!({
            "command": "add",
            "args": {"pathspecs": ["."]}
        }))
        .unwrap();

        assert_eq!(params.command, "add");
        assert_eq!(params.args, Some(vec![".".to_string()]));
    }

    #[test]
    fn run_libra_vcs_params_accepts_top_level_flag_fields() {
        let params: RunLibraVcsParams = serde_json::from_value(serde_json::json!({
            "command": "status",
            "short": true
        }))
        .unwrap();

        assert_eq!(params.command, "status");
        assert_eq!(params.args, Some(vec!["--short".to_string()]));
    }

    #[test]
    fn run_libra_vcs_process_args_default_to_json() {
        let args = libra_vcs_process_args("status", &["--porcelain".to_string(), "v2".to_string()]);

        assert_eq!(
            args,
            vec![
                "--json=compact".to_string(),
                "status".to_string(),
                "--porcelain".to_string(),
                "v2".to_string()
            ]
        );
    }
}
