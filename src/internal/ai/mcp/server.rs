//! MCP `ServerHandler` implementation: resources (URI) and tool routing.
//!
//! - `LibraMcpServer` declares MCP capabilities (resources/tools) and implements resource reads.
//! - Tool implementations live in `crate::internal::ai::mcp::resource` and are registered via
//!   `rmcp`'s `#[tool_router]`.
//!
//! # Resource behavior (summary)
//!
//! - `libra://object/{object_id}`: resolve id -> hash in the AI history branch, then read JSON blob from storage.
//! - `libra://objects/{object_type}`: list objects by type (one line: `{object_id} {object_hash}`).
//!   All AI object types (intent, task, run, plan, etc.) are stored on a single branch (`refs/libra/intent`).
//! - `libra://history/latest`: returns the current AI orphan-branch HEAD commit hash.
//! - `libra://context/active`: returns the latest active Run/Task/ContextSnapshot as JSON.
//!
//! If `HistoryManager` or `Storage` is missing, related calls return `ErrorData`.
use std::{
    path::PathBuf,
    sync::{Arc, Mutex},
};

use rmcp::{
    RoleServer, ServerHandler, handler::server::router::tool::ToolRouter, model::*,
    service::RequestContext, tool_handler,
};

use crate::{
    internal::ai::{
        history::HistoryManager,
        mcp::authz::{AuthzDecision, McpAuthorizer, McpOperation},
        runtime::hardening::PrincipalContext,
        web::code_ui::CodeUiSession,
    },
    utils::{storage::Storage, storage_ext::StorageExt},
};

#[derive(Clone)]
pub struct LibraMcpServer {
    pub intent_history_manager: Option<Arc<HistoryManager>>,
    pub storage: Option<Arc<dyn Storage + Send + Sync>>,
    pub working_dir: Option<PathBuf>,
    code_ui_session: Arc<Mutex<Option<Arc<CodeUiSession>>>>,
    /// Optional Phase 5 authorization gate. When `None` (the default), every
    /// MCP operation runs unauthenticated as it always has; when `Some`, the
    /// configured [`McpAuthorizer`] gates the operation via
    /// [`authorize_or_error`](Self::authorize_or_error) before the impl
    /// proceeds.
    authz: Arc<Mutex<Option<Arc<dyn McpAuthorizer>>>>,
    // pub repo_id: Uuid,
    tool_router: ToolRouter<LibraMcpServer>,
}

impl LibraMcpServer {
    pub fn new(
        intent_history_manager: Option<Arc<HistoryManager>>,
        storage: Option<Arc<dyn Storage + Send + Sync>>,
    ) -> Self {
        let repair_history = intent_history_manager.clone();
        let server = Self {
            intent_history_manager,
            storage,
            working_dir: None,
            code_ui_session: Arc::new(Mutex::new(None)),
            authz: Arc::new(Mutex::new(None)),
            tool_router: Self::build_tool_router(),
        };
        crate::internal::ai::memory::schedule_observer_repair(repair_history);
        server
    }

    pub fn new_with_working_dir(
        intent_history_manager: Option<Arc<HistoryManager>>,
        storage: Option<Arc<dyn Storage + Send + Sync>>,
        working_dir: PathBuf,
    ) -> Self {
        let repair_history = intent_history_manager.clone();
        let server = Self {
            intent_history_manager,
            storage,
            working_dir: Some(working_dir),
            code_ui_session: Arc::new(Mutex::new(None)),
            authz: Arc::new(Mutex::new(None)),
            tool_router: Self::build_tool_router(),
        };
        crate::internal::ai::memory::schedule_observer_repair(repair_history);
        server
    }

    /// Install an authorization gate. Call before serving any requests; once
    /// set, every authorized impl method (currently only
    /// [`list_resources_impl`](Self::list_resources_impl) — Phase 5 will
    /// extend this) calls [`Self::authorize_or_error`] before proceeding.
    ///
    /// Replacing the authz handler is allowed (mirrors the lock-and-swap
    /// behavior of [`set_code_ui_session`](Self::set_code_ui_session)). The
    /// mutex is poison-tolerant — a poisoned lock just gets recovered into
    /// the new state rather than propagating the panic.
    pub fn set_authz(&self, authz: Arc<dyn McpAuthorizer>) {
        match self.authz.lock() {
            Ok(mut guard) => *guard = Some(authz),
            Err(poisoned) => {
                let mut guard = poisoned.into_inner();
                *guard = Some(authz);
            }
        }
    }

    /// Snapshot of the currently-installed authz handler, if any.
    fn current_authz(&self) -> Option<Arc<dyn McpAuthorizer>> {
        match self.authz.lock() {
            Ok(guard) => guard.clone(),
            Err(poisoned) => poisoned.into_inner().clone(),
        }
    }

    /// Phase 5 gate: returns `Ok(())` when no authz handler is installed
    /// (preserves pre-Phase-5 unconditional-allow semantics), or when the
    /// installed handler returns [`AuthzDecision::Allow`]. Otherwise
    /// converts `Deny` / `NeedsHuman` decisions and any `AuthzError` from
    /// the backend into an [`ErrorData`] so the MCP transport layer can
    /// return a structured error to the client.
    ///
    /// All authz checks run as the system principal today
    /// ([`PrincipalContext::system()`]); per-request principal threading
    /// (caller token → principal) is queued for a follow-up patch.
    pub(crate) async fn authorize_or_error(&self, op: McpOperation<'_>) -> Result<(), ErrorData> {
        self.authorize_with_principal_or_error(op, PrincipalContext::system())
            .await
    }

    /// Per-request authz variant: derive a [`PrincipalContext`] from the
    /// caller's [`git_internal::internal::object::types::ActorRef`] (via
    /// [`PrincipalContext::from_actor`]) and route through the same
    /// decision plumbing as [`authorize_or_error`]. Wired into the
    /// `create_*_impl` family in `mcp/resource.rs`, which threads an
    /// `actor: ActorRef` parameter from the MCP transport.
    pub(crate) async fn authorize_or_error_with_actor(
        &self,
        op: McpOperation<'_>,
        actor: &git_internal::internal::object::types::ActorRef,
    ) -> Result<(), ErrorData> {
        self.authorize_with_principal_or_error(op, PrincipalContext::from_actor(actor))
            .await
    }

    async fn authorize_with_principal_or_error(
        &self,
        op: McpOperation<'_>,
        principal: PrincipalContext,
    ) -> Result<(), ErrorData> {
        let Some(authz) = self.current_authz() else {
            return Ok(());
        };
        match authz.authorize(&principal, op).await {
            Ok(AuthzDecision::Allow) => Ok(()),
            Ok(AuthzDecision::Deny { reason }) => Err(ErrorData::invalid_request(
                format!("MCP authorization denied: {reason}"),
                None,
            )),
            Ok(AuthzDecision::NeedsHuman { reason }) => Err(ErrorData::invalid_request(
                format!("MCP authorization requires human approval: {reason}"),
                None,
            )),
            Err(error) => Err(ErrorData::internal_error(
                format!("MCP authorization backend error: {error}"),
                None,
            )),
        }
    }

    pub fn set_code_ui_session(&self, session: Arc<CodeUiSession>) {
        match self.code_ui_session.lock() {
            Ok(mut guard) => {
                *guard = Some(session);
            }
            Err(poisoned) => {
                let mut guard = poisoned.into_inner();
                *guard = Some(session);
            }
        }
    }

    pub(crate) fn code_ui_session(&self) -> Option<Arc<CodeUiSession>> {
        match self.code_ui_session.lock() {
            Ok(guard) => guard.clone(),
            Err(poisoned) => poisoned.into_inner().clone(),
        }
    }
}

impl LibraMcpServer {
    pub async fn list_resources_impl(&self) -> Result<Vec<Annotated<RawResource>>, ErrorData> {
        self.authorize_or_error(McpOperation::ListResources).await?;
        Ok(vec![
            RawResource::new("libra://history/latest", "Latest History Head").no_annotation(),
            RawResource::new("libra://context/active", "Active Context").no_annotation(),
        ])
    }

    pub async fn read_resource_impl(&self, uri: &str) -> Result<Vec<ResourceContents>, ErrorData> {
        self.authorize_or_error(McpOperation::ReadResource { uri })
            .await?;
        if uri == "libra://history/latest" {
            let history = self
                .intent_history_manager
                .as_ref()
                .ok_or_else(|| ErrorData::internal_error("History not available", None))?;
            let head = history
                .resolve_history_head()
                .await
                .map_err(|e| ErrorData::internal_error(e.to_string(), None))?;
            let text = match head {
                Some(hash) => hash.to_string(),
                None => "no history".to_string(),
            };
            return Ok(vec![ResourceContents::text(text, uri)]);
        }

        if uri == "libra://context/active" {
            return self.read_active_context().await;
        }

        if let Some(object_type) = uri.strip_prefix("libra://objects/") {
            let object_type = match object_type {
                "context_snapshot" => "snapshot",
                "tool_invocation" => "invocation",
                other => other,
            };
            let history = self
                .intent_history_manager
                .as_ref()
                .ok_or_else(|| ErrorData::internal_error("History not available", None))?;
            let objects = history
                .list_objects(object_type)
                .await
                .map_err(|e| ErrorData::internal_error(e.to_string(), None))?;
            let body = objects
                .into_iter()
                .map(|(id, hash)| format!("{} {}", id, hash))
                .collect::<Vec<_>>()
                .join("\n");
            return Ok(vec![ResourceContents::text(body, uri)]);
        }

        if let Some(object_id_str) = uri.strip_prefix("libra://object/") {
            let history = self
                .intent_history_manager
                .as_ref()
                .ok_or_else(|| ErrorData::internal_error("History not available", None))?;
            let storage = self
                .storage
                .as_ref()
                .ok_or_else(|| ErrorData::internal_error("Storage not available", None))?;

            let result = history
                .find_object_hash(object_id_str)
                .await
                .map_err(|e| ErrorData::internal_error(e.to_string(), None))?;

            match result {
                Some((hash, _type)) => {
                    let (data, _) = storage
                        .get(&hash)
                        .await
                        .map_err(|e| ErrorData::internal_error(e.to_string(), None))?;
                    let json_str = String::from_utf8_lossy(&data).to_string();
                    return Ok(vec![ResourceContents::text(json_str, uri)]);
                }
                None => {
                    return Err(ErrorData::resource_not_found(
                        format!("Object not found: {}", object_id_str),
                        None,
                    ));
                }
            }
        }

        Err(ErrorData::resource_not_found("Resource not found", None))
    }

    /// Build the `libra://context/active` resource by finding the latest
    /// non-terminal Run, then loading its parent Task and linked ContextSnapshot.
    ///
    /// Returns a JSON object with `task`, `run`, and optionally `context_snapshot` fields.
    /// If no active run is found, falls back to the latest non-terminal Task.
    /// If nothing is active, returns `{"active": false}`.
    async fn read_active_context(&self) -> Result<Vec<ResourceContents>, ErrorData> {
        use git_internal::internal::object::{
            context::ContextSnapshot, run::Run, run_event::RunEventKind, task::Task,
            task_event::TaskEventKind,
        };

        use super::resource::{run_status_label, task_status_label};

        let history = self
            .intent_history_manager
            .as_ref()
            .ok_or_else(|| ErrorData::internal_error("History not available", None))?;
        let storage = self
            .storage
            .as_ref()
            .ok_or_else(|| ErrorData::internal_error("Storage not available", None))?;

        let uri = "libra://context/active";

        let latest_run_events = self.latest_run_events().await?;
        let latest_task_events = self.latest_task_events().await?;

        // 1. Find the latest active Run (UUID v7 is lexicographically time-ordered)
        let runs = history
            .list_objects("run")
            .await
            .map_err(|e| ErrorData::internal_error(e.to_string(), None))?;

        let mut active_run: Option<Run> = None;
        // Iterate in reverse so the latest (by UUID sort) is checked first
        for (_id, hash) in runs.into_iter().rev() {
            if let Ok(run) = storage.get_json::<Run>(&hash).await {
                let status_kind = latest_run_events
                    .get(&run.header().object_id())
                    .cloned()
                    .unwrap_or(RunEventKind::Created);
                if matches!(status_kind, RunEventKind::Completed | RunEventKind::Failed) {
                    continue;
                }
                active_run = Some(run);
                break;
            }
        }

        let mut result = serde_json::Map::new();

        if let Some(run) = &active_run {
            // Serialize run info
            let run_status = latest_run_events
                .get(&run.header().object_id())
                .map(|kind| run_status_label(kind))
                .unwrap_or("created");
            let run_obj = serde_json::json!({
                "id": run.header().object_id().to_string(),
                "status": run_status,
                "task_id": run.task().to_string(),
                "base_commit_sha": run.commit().to_string(),
            });
            result.insert("run".to_string(), run_obj);

            // Load parent Task
            let task_hash = history
                .get_object_hash("task", &run.task().to_string())
                .await
                .map_err(|e| ErrorData::internal_error(e.to_string(), None))?;
            if let Some(hash) = task_hash
                && let Ok(task) = storage.get_json::<Task>(&hash).await
            {
                let task_status = latest_task_events
                    .get(&task.header().object_id())
                    .map(|kind| task_status_label(kind))
                    .unwrap_or("draft");
                let task_obj = serde_json::json!({
                    "id": task.header().object_id().to_string(),
                    "title": task.title(),
                    "status": task_status,
                    "goal_type": task.goal().map(|g| g.to_string()),
                    "constraints": task.constraints(),
                    "acceptance_criteria": task.acceptance_criteria(),
                });
                result.insert("task".to_string(), task_obj);
            }

            // Load linked ContextSnapshot if present
            if let Some(snapshot_id) = run.snapshot() {
                let snap_hash = history
                    .get_object_hash("snapshot", &snapshot_id.to_string())
                    .await
                    .map_err(|e| ErrorData::internal_error(e.to_string(), None))?;
                if let Some(hash) = snap_hash
                    && let Ok(snapshot) = storage.get_json::<ContextSnapshot>(&hash).await
                {
                    let items: Vec<serde_json::Value> = snapshot
                        .items()
                        .iter()
                        .map(|item| {
                            serde_json::json!({
                                "kind": format!("{:?}", item.kind),
                                "path": item.path,
                                "content_id": item.blob.as_ref().map(|b| b.to_string()).unwrap_or_default(),
                            })
                        })
                        .collect();
                    let snap_obj = serde_json::json!({
                        "id": snapshot.header().object_id().to_string(),
                        "selection_strategy": format!("{:?}", snapshot.selection_strategy()),
                        "items": items,
                        "summary": snapshot.summary(),
                    });
                    result.insert("context_snapshot".to_string(), snap_obj);
                }
            }

            result.insert("active".to_string(), serde_json::Value::Bool(true));
        } else {
            // No active run — try to find the latest non-terminal Task as fallback
            let tasks = history
                .list_objects("task")
                .await
                .map_err(|e| ErrorData::internal_error(e.to_string(), None))?;

            let mut found_task = false;
            for (_id, hash) in tasks.into_iter().rev() {
                if let Ok(task) = storage.get_json::<Task>(&hash).await {
                    let status_kind = latest_task_events
                        .get(&task.header().object_id())
                        .cloned()
                        .unwrap_or(TaskEventKind::Created);
                    if matches!(
                        status_kind,
                        TaskEventKind::Done | TaskEventKind::Failed | TaskEventKind::Cancelled
                    ) {
                        continue;
                    }
                    let task_obj = serde_json::json!({
                        "id": task.header().object_id().to_string(),
                        "title": task.title(),
                        "status": task_status_label(&status_kind),
                        "goal_type": task.goal().map(|g| g.to_string()),
                        "constraints": task.constraints(),
                        "acceptance_criteria": task.acceptance_criteria(),
                    });
                    result.insert("task".to_string(), task_obj);
                    result.insert("active".to_string(), serde_json::Value::Bool(true));
                    found_task = true;
                    break;
                }
            }

            if !found_task {
                result.insert("active".to_string(), serde_json::Value::Bool(false));
            }
        }

        let json = serde_json::to_string(&result)
            .map_err(|e| ErrorData::internal_error(e.to_string(), None))?;
        Ok(vec![ResourceContents::text(json, uri)])
    }
}

#[tool_handler(router = self.tool_router)]
impl ServerHandler for LibraMcpServer {
    fn get_info(&self) -> ServerInfo {
        ServerInfo::new(
            ServerCapabilities::builder()
                .enable_resources()
                .enable_tools()
                .build(),
        )
        .with_protocol_version(ProtocolVersion::V_2024_11_05)
        .with_server_info(Implementation::new("libra", env!("CARGO_PKG_VERSION")))
        .with_instructions(
            "Libra MCP Server exposes AI workflow objects and event logs (intent/task/run lifecycle events) backed by git-internal.",
        )
    }

    async fn list_resources(
        &self,
        _request: Option<PaginatedRequestParams>,
        _: RequestContext<RoleServer>,
    ) -> Result<ListResourcesResult, ErrorData> {
        let resources = self.list_resources_impl().await?;
        Ok(ListResourcesResult::with_all_items(resources))
    }

    async fn read_resource(
        &self,
        request: ReadResourceRequestParams,
        _: RequestContext<RoleServer>,
    ) -> Result<ReadResourceResult, ErrorData> {
        let contents = self.read_resource_impl(&request.uri).await?;
        Ok(ReadResourceResult::new(contents))
    }

    async fn list_resource_templates(
        &self,
        _request: Option<PaginatedRequestParams>,
        _: RequestContext<RoleServer>,
    ) -> Result<ListResourceTemplatesResult, ErrorData> {
        self.authorize_or_error(McpOperation::ListResourceTemplates)
            .await?;
        Ok(ListResourceTemplatesResult::with_all_items(vec![
            ResourceTemplate::new(
                RawResourceTemplate {
                    uri_template: "libra://object/{object_id}".to_string(),
                    name: "Get AI Object by ID".to_string(),
                    description: None,
                    mime_type: None,
                    title: None,
                    icons: None,
                },
                None,
            ),
            ResourceTemplate::new(
                RawResourceTemplate {
                    uri_template: "libra://objects/{object_type}".to_string(),
                    name: "List AI Objects by Type".to_string(),
                    description: None,
                    mime_type: None,
                    title: None,
                    icons: None,
                },
                None,
            ),
        ]))
    }
}
