//! # Embedded Web Server for `libra code`
//!
//! This module serves the static Next.js bundle and the provider-agnostic
//! `/api/code/*` protocol used by the browser UI.

pub mod code_ui;
pub mod code_ui_projection;
pub mod headless;

use std::{convert::Infallible, net::SocketAddr, path::PathBuf, sync::Arc, time::Duration};

use axum::{
    Json, Router,
    body::{Body, to_bytes},
    extract::{ConnectInfo, Path, Query, Request, State},
    http::{HeaderMap, StatusCode, header},
    middleware::{self, Next},
    response::{
        IntoResponse, Response,
        sse::{Event, KeepAlive, Sse},
    },
    routing::{get, post},
};
use futures_util::stream::{self, StreamExt};
use serde::Serialize;
use tokio::{sync::oneshot, time::timeout};
use tokio_stream::wrappers::{BroadcastStream, errors::BroadcastStreamRecvError};
use uuid::Uuid;

use self::code_ui::{
    CodeUiApiError, CodeUiControllerDetachRequest, CodeUiControllerKind, CodeUiGoalCancelRequest,
    CodeUiGoalStartRequest, CodeUiInteractionResponse, CodeUiMessageRequest, CodeUiRuntimeHandle,
    CodeUiTaskDispatchRequest, browser_controller_token_from_headers, ensure_session_updated_event,
};
use crate::{
    command::code::resolve_storage_root,
    internal::{
        ai::{
            projection::ThreadProjection,
            runtime::{
                RuntimeWorkerError,
                hardening::{AuditEvent, AuditSink, SecretRedactor, TracingAuditSink},
                runtime_worker_adapter_message,
            },
        },
        db::establish_connection,
    },
    utils::util::get_repo_name_from_url,
};

const CODE_CONTROL_BODY_LIMIT_BYTES: usize = 256 * 1024;
const CODE_CONTROL_BODY_REJECT_DRAIN_BYTES: usize = 1024 * 1024;
const WEB_SERVER_SHUTDOWN_TIMEOUT: Duration = Duration::from_secs(5);

#[derive(Clone)]
struct WebAppState {
    working_dir: Arc<PathBuf>,
    code_ui: Option<Arc<CodeUiRuntimeHandle>>,
    automation_control_token: Option<Arc<str>>,
    audit_sink: Arc<dyn AuditSink>,
    control_trace_id: Uuid,
}

#[derive(Clone, Default)]
pub struct WebServerOptions {
    pub code_ui: Option<Arc<CodeUiRuntimeHandle>>,
    pub automation_control_token: Option<Arc<str>>,
    pub audit_sink: Option<Arc<dyn AuditSink>>,
}

/// Handle to a running web server, providing its bound address and a
/// mechanism for graceful shutdown via the oneshot channel.
pub struct WebServerHandle {
    pub addr: SocketAddr,
    shutdown_tx: oneshot::Sender<()>,
    join: tokio::task::JoinHandle<anyhow::Result<()>>,
}

/// Bounded shutdown outcome for the embedded Code web listener.
#[derive(Clone, Debug, thiserror::Error, PartialEq, Eq)]
pub enum WebServerShutdownError {
    #[error("web_server did not stop before the shutdown deadline")]
    TimedOut,
    #[error("web_server task exited unexpectedly during shutdown: {reason}")]
    TaskFailed { reason: String },
}

impl WebServerHandle {
    pub async fn shutdown(self) -> Result<(), WebServerShutdownError> {
        self.shutdown_with_timeout(WEB_SERVER_SHUTDOWN_TIMEOUT)
            .await
    }

    async fn shutdown_with_timeout(
        self,
        shutdown_timeout: Duration,
    ) -> Result<(), WebServerShutdownError> {
        let _ = self.shutdown_tx.send(());
        // Abort the listener if this future is cancelled by an outer lifecycle
        // deadline before the local timeout can call `join.abort()`.
        let mut join = AbortJoinOnDrop::new(self.join);
        match timeout(shutdown_timeout, join.as_mut()).await {
            Ok(Ok(Ok(()))) => {
                join.disarm();
                Ok(())
            }
            Ok(Ok(Err(error))) => {
                join.disarm();
                Err(WebServerShutdownError::TaskFailed {
                    reason: error.to_string(),
                })
            }
            Ok(Err(error)) => {
                join.disarm();
                Err(WebServerShutdownError::TaskFailed {
                    reason: error.to_string(),
                })
            }
            Err(_) => {
                join.abort_now().await;
                Err(WebServerShutdownError::TimedOut)
            }
        }
    }
}

/// Ensures a Tokio task is aborted if its owning shutdown future is dropped
/// mid-wait (for example when a process lifecycle deadline cancels the step).
struct AbortJoinOnDrop<T> {
    handle: Option<tokio::task::JoinHandle<T>>,
}

impl<T> AbortJoinOnDrop<T> {
    fn new(handle: tokio::task::JoinHandle<T>) -> Self {
        Self {
            handle: Some(handle),
        }
    }

    fn as_mut(&mut self) -> &mut tokio::task::JoinHandle<T> {
        // INVARIANT: handle is present until `disarm` / `abort_now`.
        self.handle
            .as_mut()
            .expect("AbortJoinOnDrop used after disarm")
    }

    fn disarm(&mut self) {
        self.handle.take();
    }

    async fn abort_now(&mut self) {
        if let Some(handle) = self.handle.take() {
            handle.abort();
            let _ = handle.await;
        }
    }
}

impl<T> Drop for AbortJoinOnDrop<T> {
    fn drop(&mut self) {
        if let Some(handle) = self.handle.take() {
            handle.abort();
        }
    }
}

/// Binds an `axum` HTTP server to `host:port` and spawns it as a background
/// tokio task. Returns a [`WebServerHandle`] for later graceful shutdown.
pub async fn start(
    host: &str,
    port: u16,
    working_dir: PathBuf,
    options: WebServerOptions,
) -> anyhow::Result<WebServerHandle> {
    let addr: SocketAddr = format!("{host}:{port}").parse()?;
    let listener = tokio::net::TcpListener::bind(addr).await?;
    let bound_addr = listener.local_addr()?;

    let app = build_router(WebAppState {
        working_dir: Arc::new(working_dir),
        code_ui: options.code_ui,
        automation_control_token: options.automation_control_token,
        audit_sink: options
            .audit_sink
            .unwrap_or_else(|| Arc::new(TracingAuditSink)),
        control_trace_id: Uuid::new_v4(),
    });
    let (shutdown_tx, shutdown_rx) = oneshot::channel::<()>();

    let join = tokio::spawn(async move {
        axum::serve(
            listener,
            app.into_make_service_with_connect_info::<SocketAddr>(),
        )
        .with_graceful_shutdown(async move {
            let _ = shutdown_rx.await;
        })
        .await
        .map_err(|e| anyhow::anyhow!(e))
    });

    Ok(WebServerHandle {
        addr: bound_addr,
        shutdown_tx,
        join,
    })
}

fn build_router(state: WebAppState) -> Router {
    Router::new()
        .nest("/api", api_router())
        .with_state(state)
        .fallback(static_handler)
}

fn api_router() -> Router<WebAppState> {
    Router::new()
        .route("/health", get(|| async { "ok" }))
        .route("/repo", get(repo_info_handler))
        .route("/repo/status", get(repo_status_handler))
        .nest("/code", code_router())
}

fn code_router() -> Router<WebAppState> {
    // Auth layer matrix (matches docs/commands/code.md):
    //   /session          -> loopback only (observe)
    //   /events           -> loopback only (observe)
    //   /diagnostics      -> loopback only (observe)
    //   /threads          -> loopback only (observe; lists active thread projections)
    //   /goal/status      -> loopback only (observe; mirrors /session)
    //   /controller/attach  -> loopback; automation also needs X-Libra-Control-Token
    //   /controller/detach  -> loopback + controller-token; automation also needs control-token
    //   /messages         -> loopback + controller-token; automation also needs control-token
    //   /interactions/{id} -> loopback + controller-token; automation also needs control-token
    //   /control/cancel   -> loopback + controller-token (browser); also requires X-Libra-Control-Token for automation leases
    //   /task/dispatch    -> loopback + controller-token; OC-Phase 3 P3.6 user-initiated sub-agent dispatch
    //   /goal/start       -> loopback + controller-token; OC-Phase 6 P6.6
    //   /goal/cancel      -> loopback + controller-token; OC-Phase 6 P6.6
    // Codex pass-1 P1: the loopback middleware is the OUTERMOST
    // layer on EVERY code route, including `/controller/attach`
    // and `/controller/detach`. Without it, those POST routes
    // would let axum's `Json<...>` extractor reject malformed/
    // oversized bodies BEFORE the per-handler loopback check ran,
    // leaking that the runtime is up to a remote caller.
    Router::new()
        .route("/session", get(code_session_handler))
        .route("/events", get(code_events_handler))
        .route("/diagnostics", get(code_diagnostics_handler))
        .route("/threads", get(code_threads_handler))
        .route("/goal/status", get(code_goal_status_handler))
        .route("/controller/attach", post(code_controller_attach_handler))
        .route("/controller/detach", post(code_controller_detach_handler))
        .merge(code_write_router())
        .layer(middleware::from_fn(enforce_code_route_loopback))
}

fn code_write_router() -> Router<WebAppState> {
    // Layer order on `Router::layer`: each subsequent `.layer()`
    // wraps the previous (tower service-builder semantics), so
    // the LAST `.layer()` is the OUTERMOST and runs first on
    // each request. Body limit goes here; the loopback gate is
    // applied at the `code_router()` level above so it covers
    // every code route uniformly.
    Router::new()
        .route("/messages", post(code_message_handler))
        .route("/interactions/{id}", post(code_interaction_handler))
        .route("/control/cancel", post(code_cancel_handler))
        .route("/task/dispatch", post(code_task_dispatch_handler))
        .route("/goal/start", post(code_goal_start_handler))
        .route("/goal/cancel", post(code_goal_cancel_handler))
        .layer(middleware::from_fn(enforce_code_write_body_limit))
}

async fn static_handler(
    ConnectInfo(remote_addr): ConnectInfo<SocketAddr>,
    headers: HeaderMap,
    uri: axum::http::Uri,
) -> impl IntoResponse {
    use crate::command::web_assets::WebAssets;

    let path = uri.path().trim_start_matches('/');
    if path.contains("..") {
        return (StatusCode::NOT_FOUND, "404 Not Found").into_response();
    }
    if !remote_addr.ip().is_loopback() {
        if should_show_remote_notice(path, &headers) {
            return remote_notice_response(remote_addr, &headers);
        }
        return (StatusCode::NOT_FOUND, "404 Not Found").into_response();
    }

    let resolved = if WebAssets::get(path).is_some() {
        Some(path.to_string())
    } else {
        let index_path = format!("{}/index.html", path.trim_end_matches('/'));
        if WebAssets::get(&index_path).is_some() {
            Some(index_path)
        } else if WebAssets::get("index.html").is_some() {
            Some("index.html".to_string())
        } else {
            None
        }
    };

    match resolved {
        Some(resolved_path) => match WebAssets::get(&resolved_path) {
            Some(content) => {
                let mime = mime_guess::from_path(&resolved_path).first_or_octet_stream();
                (
                    StatusCode::OK,
                    [(header::CONTENT_TYPE, mime.as_ref().to_string())],
                    content.data.to_vec(),
                )
                    .into_response()
            }
            None => (
                StatusCode::INTERNAL_SERVER_ERROR,
                "embedded asset lookup became inconsistent",
            )
                .into_response(),
        },
        None => (StatusCode::NOT_FOUND, "404 Not Found").into_response(),
    }
}

fn should_show_remote_notice(path: &str, headers: &HeaderMap) -> bool {
    if path.starts_with("api/") {
        return false;
    }
    let accepts_html = headers
        .get(header::ACCEPT)
        .and_then(|value| value.to_str().ok())
        .is_none_or(|value| value.contains("text/html") || value.contains("*/*"));
    if !accepts_html {
        return false;
    }
    path.is_empty()
        || path.ends_with('/')
        || path.ends_with(".html")
        || std::path::Path::new(path).extension().is_none()
}

fn remote_notice_response(remote_addr: SocketAddr, headers: &HeaderMap) -> Response {
    use crate::command::web_assets::WebAssets;

    let asset_path = remote_notice_asset_path(headers);
    let Some(content) = WebAssets::get(asset_path) else {
        return (
            StatusCode::INTERNAL_SERVER_ERROR,
            "embedded remote access notice is missing",
        )
            .into_response();
    };
    let Ok(template) = std::str::from_utf8(&content.data) else {
        return (
            StatusCode::INTERNAL_SERVER_ERROR,
            "embedded remote access notice is not valid UTF-8",
        )
            .into_response();
    };
    let bind = headers
        .get(header::HOST)
        .and_then(|value| value.to_str().ok())
        .unwrap_or("(unknown)");
    let body = template
        .replace("__LIBRA_BIND__", &escape_html(bind))
        .replace(
            "__LIBRA_REMOTE__",
            &escape_html(&remote_addr.ip().to_string()),
        )
        .replace("__LIBRA_VERSION__", env!("CARGO_PKG_VERSION"))
        .replace("__LIBRA_COMMIT__", "unknown");
    (
        StatusCode::OK,
        [(header::CONTENT_TYPE, "text/html; charset=utf-8")],
        body,
    )
        .into_response()
}

fn remote_notice_asset_path(headers: &HeaderMap) -> &'static str {
    headers
        .get(header::ACCEPT_LANGUAGE)
        .and_then(|value| value.to_str().ok())
        .filter(|value| {
            value
                .split(',')
                .next()
                .is_some_and(|language| language.trim().to_ascii_lowercase().starts_with("zh"))
        })
        .map(|_| "remote-notice/zh-CN/index.html")
        .unwrap_or("remote-notice/index.html")
}

fn escape_html(value: &str) -> String {
    let mut escaped = String::with_capacity(value.len());
    for character in value.chars() {
        match character {
            '&' => escaped.push_str("&amp;"),
            '<' => escaped.push_str("&lt;"),
            '>' => escaped.push_str("&gt;"),
            '"' => escaped.push_str("&quot;"),
            '\'' => escaped.push_str("&#39;"),
            _ => escaped.push(character),
        }
    }
    escaped
}

async fn repo_info_handler(
    ConnectInfo(remote_addr): ConnectInfo<SocketAddr>,
    State(state): State<WebAppState>,
) -> Result<Json<serde_json::Value>, WebApiError> {
    use serde_json::json;

    use crate::internal::config::ConfigKv;

    ensure_loopback_api_request(remote_addr)?;

    let id = ConfigKv::get("libra.repoid")
        .await
        .ok()
        .flatten()
        .map(|entry| entry.value)
        .unwrap_or_default();

    let name = match ConfigKv::get_current_remote_url().await {
        Ok(Some(url)) => get_repo_name_from_url(&url)
            .map(|value| value.to_string())
            .unwrap_or_default(),
        _ => state
            .working_dir
            .file_name()
            .map(|name| name.to_string_lossy().to_string())
            .unwrap_or_default(),
    };

    // §C.4.1: no storage root → no description, rather than reading one out
    // of a directory this process would have had to invent.
    let description = resolve_storage_root(&state.working_dir)
        .map(|root| std::fs::read_to_string(root.join("description")).unwrap_or_default())
        .unwrap_or_default();

    Ok(Json(json!({
        "id": id,
        "name": name,
        "description": description.trim(),
    })))
}

async fn repo_status_handler(
    ConnectInfo(remote_addr): ConnectInfo<SocketAddr>,
    State(state): State<WebAppState>,
) -> Result<Json<serde_json::Value>, WebApiError> {
    ensure_loopback_api_request(remote_addr)?;
    crate::command::status::collect_status_json_envelope_for_api(state.working_dir.as_path())
        .await
        .map(Json)
        .map_err(|err| WebApiError {
            status: StatusCode::INTERNAL_SERVER_ERROR,
            code: "STATUS_UNAVAILABLE".to_string(),
            message: format!("failed to collect repository status: {err}"),
        })
}

async fn code_session_handler(
    ConnectInfo(remote_addr): ConnectInfo<SocketAddr>,
    State(state): State<WebAppState>,
) -> Result<Json<serde_json::Value>, WebApiError> {
    ensure_loopback_api_request(remote_addr)?;
    let runtime = code_ui_runtime(&state)?;
    Ok(Json(serde_json::to_value(runtime.snapshot().await)?))
}

async fn code_events_handler(
    ConnectInfo(remote_addr): ConnectInfo<SocketAddr>,
    State(state): State<WebAppState>,
) -> Result<Sse<impl futures_util::Stream<Item = Result<Event, Infallible>>>, WebApiError> {
    ensure_loopback_api_request(remote_addr)?;
    let runtime = code_ui_runtime(&state)?;
    let current_snapshot = runtime.snapshot().await;
    let initial_event = ensure_session_updated_event(&current_snapshot)?;
    let receiver = runtime.subscribe();

    let initial_stream = stream::once(async move { Ok(code_ui_event_to_sse(initial_event)) });
    let updates = BroadcastStream::new(receiver).filter_map(move |message| {
        let runtime = runtime.clone();
        async move {
            code_ui_broadcast_event_or_recovery(&runtime, message)
                .await
                .map(|event| Ok(code_ui_event_to_sse(event)))
        }
    });

    Ok(Sse::new(initial_stream.chain(updates)).keep_alive(KeepAlive::new()))
}

async fn code_ui_broadcast_event_or_recovery(
    runtime: &Arc<CodeUiRuntimeHandle>,
    message: Result<code_ui::CodeUiEventEnvelope, BroadcastStreamRecvError>,
) -> Option<code_ui::CodeUiEventEnvelope> {
    match message {
        Ok(event) => Some(event),
        Err(BroadcastStreamRecvError::Lagged(_)) => {
            ensure_session_updated_event(&runtime.snapshot().await).ok()
        }
    }
}

async fn code_diagnostics_handler(
    ConnectInfo(remote_addr): ConnectInfo<SocketAddr>,
    State(state): State<WebAppState>,
) -> Result<Json<serde_json::Value>, WebApiError> {
    ensure_loopback_api_request(remote_addr)?;
    let runtime = code_ui_runtime(&state)?;
    // Wave 7 / PR 7 — pass diagnostics through `SecretRedactor` so
    // automation clients never observe the harness control token,
    // controller token, or secret-like path components from
    // `LIBRA_LOG_FILE`. The redactor's marker set is the source of
    // truth (see `SecretRedactor::default_runtime()`); this handler
    // is only responsible for applying it before serialisation.
    let redactor = SecretRedactor::default_runtime();
    Ok(Json(serde_json::to_value(
        runtime.diagnostics().await.redact(&redactor),
    )?))
}

#[derive(Debug, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
struct ThreadsRawQuery {
    /// Page size; clamped to `[1, MAX_THREAD_LIST_LIMIT]`. Defaults to 50.
    /// Parsed manually from string so invalid values surface as a Code UI
    /// error envelope instead of axum's default 400 plaintext.
    #[serde(default)]
    limit: Option<String>,
    /// Page offset; defaults to 0.
    #[serde(default)]
    offset: Option<String>,
}

fn parse_optional_u64(field: &str, value: Option<&str>) -> Result<Option<u64>, WebApiError> {
    let Some(raw) = value else { return Ok(None) };
    raw.parse::<u64>().map(Some).map_err(|_| WebApiError {
        status: StatusCode::BAD_REQUEST,
        code: "INVALID_QUERY_PARAM".to_string(),
        message: format!("query parameter `{field}` must be a non-negative integer"),
    })
}

const DEFAULT_THREAD_LIST_LIMIT: u64 = 50;
const MAX_THREAD_LIST_LIMIT: u64 = 200;

#[derive(Debug, serde::Serialize)]
#[serde(rename_all = "camelCase")]
struct ThreadListItem {
    id: String,
    title: Option<String>,
    archived: bool,
    current_intent_id: Option<String>,
    created_at: chrono::DateTime<chrono::Utc>,
    updated_at: chrono::DateTime<chrono::Utc>,
}

#[derive(Debug, serde::Serialize)]
#[serde(rename_all = "camelCase")]
struct ThreadListResponse {
    items: Vec<ThreadListItem>,
    /// Offset to pass for the next page; absent when this page returned fewer
    /// items than the requested limit (the caller has reached the end).
    #[serde(skip_serializing_if = "Option::is_none")]
    next_offset: Option<u64>,
}

async fn code_threads_handler(
    ConnectInfo(remote_addr): ConnectInfo<SocketAddr>,
    State(state): State<WebAppState>,
    Query(raw_query): Query<ThreadsRawQuery>,
) -> Result<Json<ThreadListResponse>, WebApiError> {
    ensure_loopback_api_request(remote_addr)?;

    let limit = parse_optional_u64("limit", raw_query.limit.as_deref())?
        .unwrap_or(DEFAULT_THREAD_LIST_LIMIT)
        .clamp(1, MAX_THREAD_LIST_LIMIT);
    let offset = parse_optional_u64("offset", raw_query.offset.as_deref())?.unwrap_or(0);

    let storage_root =
        resolve_storage_root(state.working_dir.as_path()).ok_or_else(|| WebApiError {
            status: StatusCode::INTERNAL_SERVER_ERROR,
            code: "STORAGE_ROOT_UNRESOLVED".to_string(),
            message: "cannot resolve the repository storage root; if this is a linked worktree, \
                      run `libra worktree repair --confirm <worktree-path>`"
                .to_string(),
        })?;
    let db_path = storage_root.join("libra.db");
    let db_path_str = db_path.to_str().ok_or_else(|| WebApiError {
        status: StatusCode::INTERNAL_SERVER_ERROR,
        code: "STORAGE_PATH_INVALID".to_string(),
        message: "libra database path is not valid UTF-8".to_string(),
    })?;

    let db = establish_connection(db_path_str)
        .await
        .map_err(|err| WebApiError {
            status: StatusCode::INTERNAL_SERVER_ERROR,
            code: "DB_UNAVAILABLE".to_string(),
            message: format!("failed to open libra database: {err}"),
        })?;

    let projections = ThreadProjection::list_active(&db, limit, offset)
        .await
        .map_err(|err| WebApiError {
            status: StatusCode::INTERNAL_SERVER_ERROR,
            code: "THREAD_LIST_FAILED".to_string(),
            message: format!("failed to list active threads: {err}"),
        })?;

    let next_offset = if (projections.len() as u64) < limit {
        None
    } else {
        Some(offset + projections.len() as u64)
    };

    let items = projections
        .into_iter()
        .map(|p| ThreadListItem {
            id: p.thread_id.to_string(),
            title: p.title,
            archived: p.archived,
            current_intent_id: p.current_intent_id.map(|id| id.to_string()),
            created_at: p.created_at,
            updated_at: p.updated_at,
        })
        .collect();

    Ok(Json(ThreadListResponse { items, next_offset }))
}

async fn code_controller_attach_handler(
    ConnectInfo(remote_addr): ConnectInfo<SocketAddr>,
    State(state): State<WebAppState>,
    headers: HeaderMap,
    Json(body): Json<code_ui::CodeUiControllerAttachRequest>,
) -> Result<Json<serde_json::Value>, WebApiError> {
    ensure_loopback_api_request(remote_addr)?;
    let runtime = code_ui_runtime(&state)?;
    let result = async {
        if body.kind == CodeUiControllerKind::Automation {
            ensure_automation_control_token(&headers, state.automation_control_token.as_ref())?;
        }
        runtime
            .attach_controller(body.kind, &body.client_id)
            .await
            .map_err(WebApiError::from)
    }
    .await;
    append_control_audit(
        &state,
        &runtime,
        "controller.attach",
        body.kind,
        &body.client_id,
        control_audit_outcome(&result),
    )
    .await;
    let response = result?;
    Ok(Json(serde_json::to_value(response)?))
}

async fn code_controller_detach_handler(
    ConnectInfo(remote_addr): ConnectInfo<SocketAddr>,
    State(state): State<WebAppState>,
    headers: HeaderMap,
    Json(body): Json<CodeUiControllerDetachRequest>,
) -> Result<Json<serde_json::Value>, WebApiError> {
    ensure_loopback_api_request(remote_addr)?;
    let runtime = code_ui_runtime(&state)?;
    let token = browser_controller_token_from_headers(&headers).ok_or_else(|| {
        WebApiError::from(CodeUiApiError::forbidden(
            "MISSING_CONTROLLER_TOKEN",
            "A browser controller token is required for detach",
        ))
    })?;
    let mut audit_kind = CodeUiControllerKind::None;
    let result = async {
        let lease = runtime.ensure_controller_write_access(Some(&token)).await?;
        audit_kind = lease.kind;
        if lease.kind == CodeUiControllerKind::Automation {
            ensure_automation_control_token(&headers, state.automation_control_token.as_ref())?;
        }
        runtime
            .detach_controller(lease.kind, &body.client_id, &token)
            .await
            .map_err(WebApiError::from)
    }
    .await;
    append_control_audit(
        &state,
        &runtime,
        "controller.detach",
        audit_kind,
        &body.client_id,
        control_audit_outcome(&result),
    )
    .await;
    result?;
    Ok(Json(serde_json::json!({ "detached": true })))
}

async fn code_message_handler(
    ConnectInfo(remote_addr): ConnectInfo<SocketAddr>,
    State(state): State<WebAppState>,
    headers: HeaderMap,
    Json(body): Json<CodeUiMessageRequest>,
) -> Result<Json<serde_json::Value>, WebApiError> {
    ensure_loopback_api_request(remote_addr)?;
    let runtime = code_ui_runtime(&state)?;
    let token = browser_controller_token_from_headers(&headers);
    let mut audit_kind = CodeUiControllerKind::None;
    let mut audit_client_id = "unknown".to_string();
    let result = async {
        let lease = runtime
            .ensure_controller_write_access(token.as_deref())
            .await?;
        audit_kind = lease.kind;
        audit_client_id = lease.client_id.clone();
        if lease.kind == CodeUiControllerKind::Automation {
            ensure_automation_control_token(&headers, state.automation_control_token.as_ref())?;
        }
        runtime
            .submit_message(token.as_deref(), body.text)
            .await
            .map_err(WebApiError::from)
    }
    .await;
    append_control_audit(
        &state,
        &runtime,
        "message.submit",
        audit_kind,
        &audit_client_id,
        control_audit_outcome(&result),
    )
    .await;
    result?;
    Ok(Json(serde_json::to_value(code_ui::CodeUiAckResponse {
        accepted: true,
    })?))
}

async fn code_interaction_handler(
    ConnectInfo(remote_addr): ConnectInfo<SocketAddr>,
    State(state): State<WebAppState>,
    Path(interaction_id): Path<String>,
    headers: HeaderMap,
    Json(body): Json<CodeUiInteractionResponse>,
) -> Result<Json<serde_json::Value>, WebApiError> {
    ensure_loopback_api_request(remote_addr)?;
    let runtime = code_ui_runtime(&state)?;
    let token = browser_controller_token_from_headers(&headers);
    let mut audit_kind = CodeUiControllerKind::None;
    let mut audit_client_id = "unknown".to_string();
    let result = async {
        let lease = runtime
            .ensure_controller_write_access(token.as_deref())
            .await?;
        audit_kind = lease.kind;
        audit_client_id = lease.client_id.clone();
        if lease.kind == CodeUiControllerKind::Automation {
            ensure_automation_control_token(&headers, state.automation_control_token.as_ref())?;
        }
        runtime
            .respond_interaction(token.as_deref(), &interaction_id, body)
            .await
            .map_err(WebApiError::from)
    }
    .await;
    append_control_audit(
        &state,
        &runtime,
        "interaction.respond",
        audit_kind,
        &audit_client_id,
        control_audit_outcome(&result),
    )
    .await;
    result?;
    Ok(Json(serde_json::to_value(code_ui::CodeUiAckResponse {
        accepted: true,
    })?))
}

async fn code_cancel_handler(
    ConnectInfo(remote_addr): ConnectInfo<SocketAddr>,
    State(state): State<WebAppState>,
    headers: HeaderMap,
) -> Result<Json<serde_json::Value>, WebApiError> {
    ensure_loopback_api_request(remote_addr)?;
    let runtime = code_ui_runtime(&state)?;
    let token = browser_controller_token_from_headers(&headers);
    let mut audit_kind = CodeUiControllerKind::None;
    let mut audit_client_id = "unknown".to_string();
    let result = async {
        let lease = runtime
            .ensure_controller_write_access(token.as_deref())
            .await?;
        audit_kind = lease.kind;
        audit_client_id = lease.client_id.clone();
        match lease.kind {
            CodeUiControllerKind::Browser => {
                // Browser controllers reach parity with the TUI `Esc` cancel
                // path: the lease token alone is enough — no automation
                // control token required.
            }
            CodeUiControllerKind::Automation => {
                ensure_automation_control_token(&headers, state.automation_control_token.as_ref())?;
            }
            _ => {
                return Err(WebApiError::from(CodeUiApiError::forbidden(
                    "AUTOMATION_CONTROLLER_REQUIRED",
                    "Only a browser or automation controller can cancel through /api/code/control/cancel",
                )));
            }
        }
        runtime
            .cancel_turn(token.as_deref())
            .await
            .map_err(WebApiError::from)
    }
    .await;
    append_control_audit(
        &state,
        &runtime,
        "turn.cancel",
        audit_kind,
        &audit_client_id,
        control_audit_outcome(&result),
    )
    .await;
    result?;
    Ok(Json(serde_json::to_value(code_ui::CodeUiAckResponse {
        accepted: true,
    })?))
}

/// `POST /api/code/task/dispatch` — explicitly dispatch a
/// sub-agent from an automation or browser controller. Body:
/// `{ "agent": "<agent>", "prompt": "<prompt>" }`. Requires a
/// controller token because it mutates the transcript and may run
/// tools. OC-Phase 3 P3.6.
async fn code_task_dispatch_handler(
    ConnectInfo(remote_addr): ConnectInfo<SocketAddr>,
    State(state): State<WebAppState>,
    headers: HeaderMap,
    Json(body): Json<CodeUiTaskDispatchRequest>,
) -> Result<Json<serde_json::Value>, WebApiError> {
    ensure_loopback_api_request(remote_addr)?;
    let runtime = code_ui_runtime(&state)?;
    let token = browser_controller_token_from_headers(&headers);
    let mut audit_kind = CodeUiControllerKind::None;
    let mut audit_client_id = "unknown".to_string();
    let result = async {
        let lease = runtime
            .ensure_controller_write_access(token.as_deref())
            .await?;
        audit_kind = lease.kind;
        audit_client_id = lease.client_id.clone();
        if lease.kind == CodeUiControllerKind::Automation {
            ensure_automation_control_token(&headers, state.automation_control_token.as_ref())?;
        }
        runtime
            .task_dispatch(token.as_deref(), body.agent, body.prompt)
            .await
            .map_err(WebApiError::from)
    }
    .await;
    append_control_audit(
        &state,
        &runtime,
        "task.dispatch",
        audit_kind,
        &audit_client_id,
        control_audit_outcome(&result),
    )
    .await;
    let rendered = result?;
    Ok(Json(serde_json::json!({
        "accepted": true,
        "result": rendered,
    })))
}

/// `POST /api/code/goal/start` — open an active Goal in the
/// session. Body: `{ "objective": "<text>" }`. Requires a
/// controller token (write-access lease) just like
/// `/api/code/messages`. OC-Phase 6 P6.6.
async fn code_goal_start_handler(
    ConnectInfo(remote_addr): ConnectInfo<SocketAddr>,
    State(state): State<WebAppState>,
    headers: HeaderMap,
    Json(body): Json<CodeUiGoalStartRequest>,
) -> Result<Json<serde_json::Value>, WebApiError> {
    ensure_loopback_api_request(remote_addr)?;
    let runtime = code_ui_runtime(&state)?;
    let token = browser_controller_token_from_headers(&headers);
    let mut audit_kind = CodeUiControllerKind::None;
    let mut audit_client_id = "unknown".to_string();
    let result = async {
        let lease = runtime
            .ensure_controller_write_access(token.as_deref())
            .await?;
        audit_kind = lease.kind;
        audit_client_id = lease.client_id.clone();
        if lease.kind == CodeUiControllerKind::Automation {
            ensure_automation_control_token(&headers, state.automation_control_token.as_ref())?;
        }
        runtime
            .goal_start(token.as_deref(), body.objective)
            .await
            .map_err(WebApiError::from)
    }
    .await;
    append_control_audit(
        &state,
        &runtime,
        "goal.start",
        audit_kind,
        &audit_client_id,
        control_audit_outcome(&result),
    )
    .await;
    let rendered = result?;
    Ok(Json(serde_json::json!({
        "accepted": true,
        "status": rendered,
    })))
}

/// `GET /api/code/goal/status` — render the active Goal's
/// snapshot. Loopback-only observe (no controller token), mirroring
/// `/api/code/session`. OC-Phase 6 P6.6.
async fn code_goal_status_handler(
    ConnectInfo(remote_addr): ConnectInfo<SocketAddr>,
    State(state): State<WebAppState>,
) -> Result<Json<serde_json::Value>, WebApiError> {
    ensure_loopback_api_request(remote_addr)?;
    let runtime = code_ui_runtime(&state)?;
    let rendered = runtime.goal_status().await.map_err(WebApiError::from)?;
    Ok(Json(serde_json::json!({ "status": rendered })))
}

/// `POST /api/code/goal/cancel` — explicit cancellation of the
/// active Goal. Body: `{ "reason": "<text>" }`. Requires a
/// controller token. OC-Phase 6 P6.6.
async fn code_goal_cancel_handler(
    ConnectInfo(remote_addr): ConnectInfo<SocketAddr>,
    State(state): State<WebAppState>,
    headers: HeaderMap,
    Json(body): Json<CodeUiGoalCancelRequest>,
) -> Result<Json<serde_json::Value>, WebApiError> {
    ensure_loopback_api_request(remote_addr)?;
    let runtime = code_ui_runtime(&state)?;
    let token = browser_controller_token_from_headers(&headers);
    let mut audit_kind = CodeUiControllerKind::None;
    let mut audit_client_id = "unknown".to_string();
    let result = async {
        let lease = runtime
            .ensure_controller_write_access(token.as_deref())
            .await?;
        audit_kind = lease.kind;
        audit_client_id = lease.client_id.clone();
        if lease.kind == CodeUiControllerKind::Automation {
            ensure_automation_control_token(&headers, state.automation_control_token.as_ref())?;
        }
        runtime
            .goal_cancel(token.as_deref(), body.reason)
            .await
            .map_err(WebApiError::from)
    }
    .await;
    append_control_audit(
        &state,
        &runtime,
        "goal.cancel",
        audit_kind,
        &audit_client_id,
        control_audit_outcome(&result),
    )
    .await;
    let rendered = result?;
    Ok(Json(serde_json::json!({
        "accepted": true,
        "status": rendered,
    })))
}

/// Per-request loopback gate. Mirrors the per-handler
/// `ensure_loopback_api_request` check but runs as a middleware so
/// it fires BEFORE any body-reading middleware on the write path.
/// Without this layer a non-loopback caller sending an oversized
/// body would learn `PAYLOAD_TOO_LARGE` first, leaking that the
/// runtime is up. Wave 2 / PR 2 wires this in to make the
/// documented error-code ordering (loopback ↦ body ↦ token)
/// observable.
async fn enforce_code_route_loopback(request: Request, next: Next) -> Response {
    // Production injects `ConnectInfo<SocketAddr>` via
    // `into_make_service_with_connect_info`; tests inject the
    // mock variant `axum::extract::connect_info::MockConnectInfo<SocketAddr>`.
    // The `ConnectInfo` extractor itself falls back to the mock,
    // so we mirror that lookup here. If neither is present (a
    // raw oneshot without ConnectInfo wiring) the middleware
    // declines to enforce — the per-handler check still applies
    // for production code paths.
    let remote = request
        .extensions()
        .get::<ConnectInfo<SocketAddr>>()
        .map(|info| info.0)
        .or_else(|| {
            request
                .extensions()
                .get::<axum::extract::connect_info::MockConnectInfo<SocketAddr>>()
                .map(|info| info.0)
        });
    if let Some(addr) = remote
        && let Err(error) = ensure_loopback_api_request(addr)
    {
        return error.into_response();
    }
    next.run(request).await
}

async fn enforce_code_write_body_limit(request: Request, next: Next) -> Response {
    let (parts, body) = request.into_parts();
    match to_bytes(body, CODE_CONTROL_BODY_REJECT_DRAIN_BYTES).await {
        Ok(body) if body.len() <= CODE_CONTROL_BODY_LIMIT_BYTES => {
            next.run(Request::from_parts(parts, Body::from(body))).await
        }
        Ok(_) | Err(_) => code_control_body_too_large_response(),
    }
}

fn code_control_body_too_large_response() -> Response {
    WebApiError {
        status: StatusCode::PAYLOAD_TOO_LARGE,
        code: "PAYLOAD_TOO_LARGE".to_string(),
        message: "Code UI write request bodies are limited to 256KiB".to_string(),
    }
    .into_response()
}

fn code_ui_runtime(state: &WebAppState) -> Result<Arc<CodeUiRuntimeHandle>, WebApiError> {
    state
        .code_ui
        .clone()
        .ok_or_else(|| WebApiError::from(CodeUiApiError::unavailable()))
}

fn code_ui_event_to_sse(event: code_ui::CodeUiEventEnvelope) -> Event {
    Event::default()
        .event(event.event_type.as_str())
        .json_data(event)
        .unwrap_or_else(|_| {
            Event::default().event(code_ui::CodeUiEventType::SessionUpdated.as_str())
        })
}

fn ensure_loopback_api_request(remote_addr: SocketAddr) -> Result<(), WebApiError> {
    if remote_addr.ip().is_loopback() {
        return Ok(());
    }

    Err(WebApiError::forbidden(
        "LOOPBACK_REQUIRED",
        "Libra Code API requests must come from a loopback client",
    ))
}

fn ensure_automation_control_token(
    headers: &HeaderMap,
    expected: Option<&Arc<str>>,
) -> Result<(), WebApiError> {
    let Some(expected) = expected else {
        return Err(WebApiError::forbidden(
            "CONTROL_DISABLED",
            "Local TUI automation write control is not enabled; start with --control write",
        ));
    };

    let Some(actual) = headers
        .get("x-libra-control-token")
        .and_then(|value| value.to_str().ok())
        .map(str::trim)
        .filter(|value| !value.is_empty())
    else {
        return Err(WebApiError::forbidden(
            "MISSING_CONTROL_TOKEN",
            "X-Libra-Control-Token is required for automation control requests",
        ));
    };

    if actual != expected.as_ref() {
        return Err(WebApiError::forbidden(
            "INVALID_CONTROL_TOKEN",
            "X-Libra-Control-Token does not match this Libra Code session",
        ));
    }

    Ok(())
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ControlAuditOutcome<'a> {
    Accepted,
    Error(&'a str),
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct ControlAuditRecord<'a> {
    thread_id: Option<String>,
    controller_kind: &'static str,
    client_id: &'a str,
    result: &'static str,
    error_code: Option<&'a str>,
}

fn control_audit_outcome<T>(result: &Result<T, WebApiError>) -> ControlAuditOutcome<'_> {
    match result {
        Ok(_) => ControlAuditOutcome::Accepted,
        Err(error) => ControlAuditOutcome::Error(error.code.as_str()),
    }
}

async fn append_control_audit(
    state: &WebAppState,
    runtime: &CodeUiRuntimeHandle,
    action: &'static str,
    controller_kind: CodeUiControllerKind,
    client_id: &str,
    outcome: ControlAuditOutcome<'_>,
) {
    let snapshot = runtime.snapshot().await;
    let redactor = SecretRedactor::default_runtime();
    let client_id = sanitized_audit_client_id(&redactor, client_id);
    let (result, error_code) = match outcome {
        ControlAuditOutcome::Accepted => ("accepted", None),
        ControlAuditOutcome::Error(code) => ("error", Some(code)),
    };
    let record = ControlAuditRecord {
        thread_id: snapshot.thread_id.clone(),
        controller_kind: controller_kind.as_str(),
        client_id: &client_id,
        result,
        error_code,
    };
    let redacted_summary = match serde_json::to_string(&record) {
        Ok(summary) => redactor.redact(&summary),
        Err(error) => {
            tracing::warn!(error = %error, "failed to serialize local TUI control audit summary");
            return;
        }
    };
    let trace_id = snapshot
        .thread_id
        .as_deref()
        .and_then(|thread_id| Uuid::parse_str(thread_id).ok())
        .unwrap_or(state.control_trace_id);

    if let Err(error) = state
        .audit_sink
        .append(AuditEvent {
            trace_id,
            principal_id: format!(
                "local-tui-control:{}:{}",
                controller_kind.as_str(),
                client_id
            ),
            action: action.to_string(),
            policy_version: "local-tui-control/v1".to_string(),
            redacted_summary,
            at: chrono::Utc::now(),
        })
        .await
    {
        tracing::warn!(error = %error, action, "failed to append local TUI control audit event");
    }
}

fn sanitized_audit_client_id(redactor: &SecretRedactor, client_id: &str) -> String {
    let redacted = redactor.redact(client_id.trim());
    let mut sanitized = redacted
        .chars()
        .map(|ch| if ch.is_control() { '_' } else { ch })
        .collect::<String>();
    if sanitized.is_empty() {
        sanitized = "unknown".to_string();
    }
    if sanitized.chars().count() > 80 {
        sanitized = sanitized.chars().take(80).collect();
    }
    sanitized
}

struct WebApiError {
    status: StatusCode,
    code: String,
    message: String,
}

impl WebApiError {
    fn forbidden(code: impl Into<String>, message: impl Into<String>) -> Self {
        Self {
            status: StatusCode::FORBIDDEN,
            code: code.into(),
            message: message.into(),
        }
    }
}

impl From<CodeUiApiError> for WebApiError {
    fn from(value: CodeUiApiError) -> Self {
        Self {
            status: StatusCode::from_u16(value.status).unwrap_or(StatusCode::INTERNAL_SERVER_ERROR),
            code: value.code,
            message: value.message,
        }
    }
}

impl From<anyhow::Error> for WebApiError {
    fn from(value: anyhow::Error) -> Self {
        // Prefer typed sources over message-text matching so an ordinary
        // failure that happens to mention reconciliation cannot become a 409.
        if let Some(api_error) = value.downcast_ref::<CodeUiApiError>() {
            return api_error.clone().into();
        }
        if let Some(RuntimeWorkerError::ReconciliationRequired { session_id }) =
            value.downcast_ref::<RuntimeWorkerError>()
        {
            return Self {
                status: StatusCode::CONFLICT,
                code: "RECONCILIATION_REQUIRED".to_string(),
                message: runtime_worker_adapter_message(
                    RuntimeWorkerError::ReconciliationRequired {
                        session_id: session_id.clone(),
                    },
                ),
            };
        }
        Self {
            status: StatusCode::INTERNAL_SERVER_ERROR,
            code: "INTERNAL_ERROR".to_string(),
            message: value.to_string(),
        }
    }
}

impl From<serde_json::Error> for WebApiError {
    fn from(value: serde_json::Error) -> Self {
        anyhow::Error::new(value).into()
    }
}

impl IntoResponse for WebApiError {
    fn into_response(self) -> Response {
        (
            self.status,
            Json(serde_json::json!({
                "error": {
                    "code": self.code,
                    "message": self.message,
                }
            })),
        )
            .into_response()
    }
}

#[cfg(test)]
mod tests {
    use std::{
        future,
        net::{Ipv4Addr, Ipv6Addr, SocketAddr},
        time::Duration,
    };

    use axum::{
        body::{Body, to_bytes},
        http::{Method, Request, Uri},
    };
    use tower::ServiceExt;

    use super::*;
    use crate::internal::ai::{
        runtime::hardening::InMemoryAuditSink,
        web::code_ui::{
            CodeUiCapabilities, CodeUiInitialController, CodeUiProviderInfo, CodeUiSession,
            ReadOnlyCodeUiAdapter, initial_snapshot,
        },
    };

    async fn test_code_ui_runtime() -> Arc<CodeUiRuntimeHandle> {
        let session = CodeUiSession::new(initial_snapshot(
            "/tmp/libra",
            CodeUiProviderInfo {
                provider: "test".to_string(),
                model: Some("test-model".to_string()),
                mode: None,
                managed: false,
            },
            CodeUiCapabilities::default(),
        ));
        CodeUiRuntimeHandle::build_with_control(
            ReadOnlyCodeUiAdapter::new(session, CodeUiCapabilities::default()),
            false,
            true,
            CodeUiInitialController::LocalTui {
                owner_label: "Terminal UI".to_string(),
                reason: None,
            },
        )
        .await
    }

    #[tokio::test]
    async fn web_server_shutdown_reports_a_bounded_timeout_and_aborts_the_task() {
        let (shutdown_tx, _shutdown_rx) = oneshot::channel();
        let handle = WebServerHandle {
            addr: SocketAddr::from(([127, 0, 0, 1], 0)),
            shutdown_tx,
            join: tokio::spawn(async { future::pending::<anyhow::Result<()>>().await }),
        };

        let result = handle
            .shutdown_with_timeout(Duration::from_millis(10))
            .await;

        assert_eq!(result, Err(WebServerShutdownError::TimedOut));
    }

    #[test]
    fn loopback_api_request_allows_loopback_clients() {
        let ipv4 = SocketAddr::from((Ipv4Addr::LOCALHOST, 34567));
        let ipv6 = SocketAddr::from((Ipv6Addr::LOCALHOST, 34567));

        assert!(ensure_loopback_api_request(ipv4).is_ok());
        assert!(ensure_loopback_api_request(ipv6).is_ok());
    }

    #[test]
    fn loopback_api_request_rejects_remote_clients() {
        let remote = SocketAddr::from((Ipv4Addr::new(192, 0, 2, 10), 34567));
        let error =
            ensure_loopback_api_request(remote).expect_err("remote client must be rejected");

        assert_eq!(error.status, StatusCode::FORBIDDEN);
        assert_eq!(error.code, "LOOPBACK_REQUIRED");
    }

    #[test]
    fn code_control_auth_rejects_when_disabled() {
        let headers = HeaderMap::new();

        let error = ensure_automation_control_token(&headers, None).unwrap_err();

        assert_eq!(error.status, StatusCode::FORBIDDEN);
        assert_eq!(error.code, "CONTROL_DISABLED");
    }

    #[test]
    fn code_control_auth_requires_token_header() {
        let headers = HeaderMap::new();
        let expected: Arc<str> = Arc::from("secret");

        let error = ensure_automation_control_token(&headers, Some(&expected)).unwrap_err();

        assert_eq!(error.status, StatusCode::FORBIDDEN);
        assert_eq!(error.code, "MISSING_CONTROL_TOKEN");
    }

    #[test]
    fn code_control_auth_rejects_invalid_token() {
        let mut headers = HeaderMap::new();
        headers.insert("x-libra-control-token", "wrong".parse().unwrap());
        let expected: Arc<str> = Arc::from("secret");

        let error = ensure_automation_control_token(&headers, Some(&expected)).unwrap_err();

        assert_eq!(error.status, StatusCode::FORBIDDEN);
        assert_eq!(error.code, "INVALID_CONTROL_TOKEN");
    }

    #[test]
    fn code_control_auth_accepts_matching_token() {
        let mut headers = HeaderMap::new();
        headers.insert("x-libra-control-token", "secret".parse().unwrap());
        let expected: Arc<str> = Arc::from("secret");

        assert!(ensure_automation_control_token(&headers, Some(&expected)).is_ok());
    }

    /// Wave 2 / PR 2 — route-level loopback gate ordering for read
    /// routes. `docs/development/commands/_general.md` §5.3 / §6.3 inline test:
    /// `GET /api/code/session` from a non-loopback `ConnectInfo`
    /// MUST short-circuit with `403 LOOPBACK_REQUIRED` BEFORE the
    /// runtime is touched. This guards the documented loopback ↦
    /// body ↦ token error-code ordering — a regression that hands
    /// remote callers the runtime-unavailable error first would
    /// leak whether the session is up.
    #[tokio::test]
    async fn code_session_route_rejects_non_loopback_with_loopback_required() {
        use axum::extract::connect_info::MockConnectInfo;
        let app = code_router()
            .with_state(WebAppState {
                working_dir: Arc::new(PathBuf::from("/tmp/libra")),
                code_ui: None,
                automation_control_token: None,
                audit_sink: Arc::new(TracingAuditSink),
                control_trace_id: Uuid::new_v4(),
            })
            .layer(MockConnectInfo(SocketAddr::from((
                Ipv4Addr::new(192, 0, 2, 10),
                34567,
            ))));
        let request = Request::builder()
            .method(Method::GET)
            .uri("/session")
            .body(Body::empty())
            .unwrap();
        let response = app.oneshot(request).await.unwrap();
        assert_eq!(
            response.status(),
            StatusCode::FORBIDDEN,
            "non-loopback GET /session must be 403, got {}",
            response.status()
        );
        let body = to_bytes(response.into_body(), usize::MAX).await.unwrap();
        let value: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(value["error"]["code"], "LOOPBACK_REQUIRED");
    }

    /// Wave 2 / PR 2 — same gate for the write surface. `POST
    /// /api/code/messages` from a non-loopback caller MUST return
    /// `LOOPBACK_REQUIRED` BEFORE the body-size middleware, the
    /// content-type check, or any controller-token check fires.
    /// Without this ordering a remote caller could probe whether
    /// the runtime is up by counting which error code they get.
    ///
    /// Codex pass-1 P1: build the test app from `code_router()`
    /// (not `code_write_router()`) so the loopback middleware
    /// applies — the layer was promoted to cover attach/detach
    /// too, and now lives on the outer router.
    #[tokio::test]
    async fn code_messages_route_rejects_non_loopback_before_body_or_token_check() {
        use axum::extract::connect_info::MockConnectInfo;
        let app = code_router()
            .with_state(WebAppState {
                working_dir: Arc::new(PathBuf::from("/tmp/libra")),
                code_ui: None,
                automation_control_token: None,
                audit_sink: Arc::new(TracingAuditSink),
                control_trace_id: Uuid::new_v4(),
            })
            .layer(MockConnectInfo(SocketAddr::from((
                Ipv4Addr::new(192, 0, 2, 10),
                34567,
            ))));
        // Deliberately send a body that would otherwise fail body
        // limit / controller-token checks; if loopback is enforced
        // FIRST the error must still be LOOPBACK_REQUIRED, not
        // PAYLOAD_TOO_LARGE / MISSING_CONTROLLER_TOKEN.
        let oversized = "x".repeat(CODE_CONTROL_BODY_LIMIT_BYTES + 1);
        let body = format!(r#"{{"text":"{oversized}"}}"#);
        let request = Request::builder()
            .method(Method::POST)
            .uri("/messages")
            .header(header::CONTENT_TYPE, "application/json")
            .header(header::CONTENT_LENGTH, body.len().to_string())
            .body(Body::from(body))
            .unwrap();
        let response = app.oneshot(request).await.unwrap();
        assert_eq!(
            response.status(),
            StatusCode::FORBIDDEN,
            "non-loopback POST /messages must be 403, got {}",
            response.status()
        );
        let body = to_bytes(response.into_body(), usize::MAX).await.unwrap();
        let value: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(
            value["error"]["code"], "LOOPBACK_REQUIRED",
            "loopback gate MUST fire before body/token checks; got: {value}",
        );
    }

    /// Codex pass-1 P1 — attach/detach coverage. POST routes that
    /// use axum's `Json<...>` extractor would otherwise let
    /// malformed-body deserialisation errors fire BEFORE the
    /// per-handler loopback check, leaking liveness to a remote
    /// caller. The middleware layered on `code_router()` must
    /// short-circuit with `LOOPBACK_REQUIRED` regardless of body
    /// shape.
    #[tokio::test]
    async fn code_controller_attach_route_rejects_non_loopback_before_body_parse() {
        use axum::extract::connect_info::MockConnectInfo;
        let app = code_router()
            .with_state(WebAppState {
                working_dir: Arc::new(PathBuf::from("/tmp/libra")),
                code_ui: None,
                automation_control_token: None,
                audit_sink: Arc::new(TracingAuditSink),
                control_trace_id: Uuid::new_v4(),
            })
            .layer(MockConnectInfo(SocketAddr::from((
                Ipv4Addr::new(192, 0, 2, 10),
                34567,
            ))));
        // Send a malformed body so a Json extractor would otherwise
        // fail the request with 400/415 BEFORE reaching the
        // per-handler check. We must still get 403 LOOPBACK_REQUIRED.
        let request = Request::builder()
            .method(Method::POST)
            .uri("/controller/attach")
            .header(header::CONTENT_TYPE, "application/json")
            .body(Body::from("{not valid json"))
            .unwrap();
        let response = app.oneshot(request).await.unwrap();
        assert_eq!(response.status(), StatusCode::FORBIDDEN);
        let body = to_bytes(response.into_body(), usize::MAX).await.unwrap();
        let value: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(value["error"]["code"], "LOOPBACK_REQUIRED");
    }

    #[tokio::test]
    async fn code_controller_detach_route_rejects_non_loopback_before_body_parse() {
        use axum::extract::connect_info::MockConnectInfo;
        let app = code_router()
            .with_state(WebAppState {
                working_dir: Arc::new(PathBuf::from("/tmp/libra")),
                code_ui: None,
                automation_control_token: None,
                audit_sink: Arc::new(TracingAuditSink),
                control_trace_id: Uuid::new_v4(),
            })
            .layer(MockConnectInfo(SocketAddr::from((
                Ipv4Addr::new(192, 0, 2, 10),
                34567,
            ))));
        let request = Request::builder()
            .method(Method::POST)
            .uri("/controller/detach")
            .header(header::CONTENT_TYPE, "application/json")
            .body(Body::from("{not valid json"))
            .unwrap();
        let response = app.oneshot(request).await.unwrap();
        assert_eq!(response.status(), StatusCode::FORBIDDEN);
        let body = to_bytes(response.into_body(), usize::MAX).await.unwrap();
        let value: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(value["error"]["code"], "LOOPBACK_REQUIRED");
    }

    #[tokio::test]
    async fn code_write_body_limit_returns_json_error() {
        let app = code_write_router().with_state(WebAppState {
            working_dir: Arc::new(PathBuf::from("/tmp/libra")),
            code_ui: None,
            automation_control_token: None,
            audit_sink: Arc::new(TracingAuditSink),
            control_trace_id: Uuid::new_v4(),
        });
        let oversized_text = "x".repeat(CODE_CONTROL_BODY_LIMIT_BYTES + 1);
        let body = format!(r#"{{"text":"{oversized_text}"}}"#);
        let request = Request::builder()
            .method(Method::POST)
            .uri("/messages")
            .header(header::CONTENT_TYPE, "application/json")
            .header(header::CONTENT_LENGTH, body.len().to_string())
            .body(Body::from(body))
            .unwrap();

        let response = app.oneshot(request).await.unwrap();

        assert_eq!(response.status(), StatusCode::PAYLOAD_TOO_LARGE);
        let body = to_bytes(response.into_body(), usize::MAX).await.unwrap();
        let value: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(value["error"]["code"], "PAYLOAD_TOO_LARGE");
    }

    #[tokio::test]
    async fn automation_attach_appends_redacted_control_audit_event() {
        let session = CodeUiSession::new(initial_snapshot(
            "/tmp/libra",
            CodeUiProviderInfo {
                provider: "test".to_string(),
                model: Some("test-model".to_string()),
                mode: None,
                managed: false,
            },
            CodeUiCapabilities::default(),
        ));
        let runtime = CodeUiRuntimeHandle::build_with_control(
            ReadOnlyCodeUiAdapter::new(session, CodeUiCapabilities::default()),
            false,
            true,
            CodeUiInitialController::LocalTui {
                owner_label: "Terminal UI".to_string(),
                reason: None,
            },
        )
        .await;
        let audit_sink = Arc::new(InMemoryAuditSink::default());
        let app = code_router().with_state(WebAppState {
            working_dir: Arc::new(PathBuf::from("/tmp/libra")),
            code_ui: Some(runtime),
            automation_control_token: Some(Arc::from("control-token-secret")),
            audit_sink: audit_sink.clone(),
            control_trace_id: Uuid::new_v4(),
        });
        let request = Request::builder()
            .method(Method::POST)
            .uri("/controller/attach")
            .header(header::CONTENT_TYPE, "application/json")
            .header("x-libra-control-token", "control-token-secret")
            .extension(ConnectInfo(SocketAddr::from((Ipv4Addr::LOCALHOST, 4000))))
            .body(Body::from(
                r#"{"clientId":"local-script token:super-secret","kind":"automation"}"#,
            ))
            .unwrap();

        let response = app.oneshot(request).await.unwrap();

        assert_eq!(response.status(), StatusCode::OK);
        let events = audit_sink.events().await;
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].action, "controller.attach");
        assert_eq!(events[0].policy_version, "local-tui-control/v1");
        assert!(
            events[0]
                .redacted_summary
                .contains("\"result\":\"accepted\"")
        );
        assert!(!events[0].redacted_summary.contains("super-secret"));
        assert!(!events[0].redacted_summary.contains("control-token-secret"));
    }

    #[tokio::test]
    async fn static_handler_rejects_parent_directory_segments() {
        let response = static_handler(
            ConnectInfo(SocketAddr::from((Ipv4Addr::LOCALHOST, 4000))),
            HeaderMap::new(),
            Uri::from_static("/../index.html"),
        )
        .await
        .into_response();

        assert_eq!(response.status(), StatusCode::NOT_FOUND);
    }

    #[tokio::test]
    async fn static_handler_shows_remote_notice_for_non_loopback_html() {
        let mut headers = HeaderMap::new();
        headers.insert(header::ACCEPT, "text/html".parse().unwrap());
        headers.insert(header::HOST, "0.0.0.0:3020".parse().unwrap());
        let response = static_handler(
            ConnectInfo(SocketAddr::from((Ipv4Addr::new(192, 0, 2, 10), 4000))),
            headers,
            Uri::from_static("/"),
        )
        .await
        .into_response();

        assert_eq!(response.status(), StatusCode::OK);
        let content_type = response
            .headers()
            .get(header::CONTENT_TYPE)
            .and_then(|value| value.to_str().ok())
            .unwrap_or_default();
        assert!(content_type.starts_with("text/html"));
        let body = to_bytes(response.into_body(), usize::MAX).await.unwrap();
        let html = String::from_utf8(body.to_vec()).unwrap();
        assert!(html.contains("loopback"), "html: {html}");
        assert!(html.contains("0.0.0.0:3020"), "html: {html}");
        assert!(html.contains("192.0.2.10"), "html: {html}");
        assert!(!html.contains("<script"), "remote notice must be zero JS");
        assert!(
            !html.contains("token"),
            "remote notice must not expose tokens"
        );
    }

    #[tokio::test]
    async fn static_handler_returns_404_for_non_loopback_assets() {
        let mut headers = HeaderMap::new();
        headers.insert(header::ACCEPT, "image/svg+xml".parse().unwrap());
        let response = static_handler(
            ConnectInfo(SocketAddr::from((Ipv4Addr::new(192, 0, 2, 10), 4000))),
            headers,
            Uri::from_static("/logo.svg"),
        )
        .await
        .into_response();

        assert_eq!(response.status(), StatusCode::NOT_FOUND);
    }

    #[tokio::test]
    async fn static_handler_selects_chinese_remote_notice() {
        let mut headers = HeaderMap::new();
        headers.insert(header::ACCEPT, "text/html".parse().unwrap());
        headers.insert(header::ACCEPT_LANGUAGE, "zh-CN,zh;q=0.9".parse().unwrap());
        let response = static_handler(
            ConnectInfo(SocketAddr::from((Ipv4Addr::new(192, 0, 2, 10), 4000))),
            headers,
            Uri::from_static("/"),
        )
        .await
        .into_response();

        assert_eq!(response.status(), StatusCode::OK);
        let body = to_bytes(response.into_body(), usize::MAX).await.unwrap();
        let html = String::from_utf8(body.to_vec()).unwrap();
        assert!(html.contains("仅限本机访问"), "html: {html}");
        assert!(!html.contains("<script"), "remote notice must be zero JS");
    }

    #[tokio::test]
    async fn sse_lag_recovers_with_full_session_snapshot_event() {
        let runtime = test_code_ui_runtime().await;

        let event =
            code_ui_broadcast_event_or_recovery(&runtime, Err(BroadcastStreamRecvError::Lagged(3)))
                .await
                .expect("lagged receiver should produce recovery event");

        assert_eq!(
            event.event_type,
            crate::internal::ai::web::code_ui::CodeUiEventType::SessionUpdated
        );
        assert_eq!(event.seq, 0);
        let snapshot = crate::internal::ai::web::code_ui::snapshot_from_event(&event)
            .expect("recovery event should contain full snapshot");
        assert_eq!(snapshot.provider.provider, "test");
    }

    /// Wave 3 / PR 3 §5.6 — control-audit `client_id` field
    /// redaction. The plan calls out "client_id 80 字符上限、控制
    /// 字符替换" — `sanitized_audit_client_id` enforces both, plus
    /// a fallback "unknown" for empty input and a redactor pass for
    /// secret-like substrings. This L0 test pins each rule so a
    /// future refactor of the audit pipeline cannot quietly drop
    /// any of them.
    #[test]
    fn sanitized_audit_client_id_truncates_at_80_chars() {
        let redactor = SecretRedactor::default_runtime();
        let long = "x".repeat(200);
        let sanitized = sanitized_audit_client_id(&redactor, &long);
        assert_eq!(
            sanitized.chars().count(),
            80,
            "expected truncation to 80 chars, got '{sanitized}'",
        );
    }

    #[test]
    fn sanitized_audit_client_id_replaces_control_characters_with_underscore() {
        let redactor = SecretRedactor::default_runtime();
        // Cover the full `char::is_control()` set the implementation
        // sanitizes against:
        //   * C0 controls 0x00–0x1F (NUL, BEL, tab, newline, ESC, …)
        //   * DEL 0x7F
        //   * C1 controls 0x80–0x9F (NEL 0x85, APC 0x9F, …)
        // A sanitizer change that drops DEL or the C1 range would
        // regress this test; covering all three groups guards both.
        let raw = "c\t\nA\u{0007}\u{0000}\u{001b}B\u{007f}C\u{0085}D\u{009f}end";
        let sanitized = sanitized_audit_client_id(&redactor, raw);
        // The fixture has no leading/trailing whitespace, so
        // `trim()` inside the helper is a no-op; every embedded
        // control is replaced with `_`. Build the expected
        // string by walking the input through the same `is_control()
        // → '_'` substitution the implementation uses, so this
        // assertion stays in lock-step with any future change.
        let expected: String = "c\t\nA\u{0007}\u{0000}\u{001b}B\u{007f}C\u{0085}D\u{009f}end"
            .trim()
            .chars()
            .map(|ch| if ch.is_control() { '_' } else { ch })
            .collect();
        assert_eq!(sanitized, expected);
        // Spot-check that DEL and a representative C1 char
        // ARE represented as `_` in the output (regression
        // anchor — these are the codepoints Codex pass-1 P2 C1
        // flagged as missing from the original test).
        assert!(!sanitized.contains('\u{007f}'), "DEL leaked: {sanitized:?}");
        assert!(!sanitized.contains('\u{0085}'), "NEL leaked: {sanitized:?}");
        assert!(!sanitized.contains('\u{009f}'), "APC leaked: {sanitized:?}");
    }

    #[test]
    fn sanitized_audit_client_id_falls_back_to_unknown_when_empty() {
        let redactor = SecretRedactor::default_runtime();
        // Whitespace-only inputs trim to empty, so the fallback
        // must kick in rather than producing an empty string that
        // would be unreadable in audit logs.
        for input in ["", "   ", "\t\n  \r"] {
            let sanitized = sanitized_audit_client_id(&redactor, input);
            assert_eq!(sanitized, "unknown", "input '{input:?}' should fall back");
        }
    }

    /// Default runtime redactor only masks marker-prefixed values
    /// (`token=`, `password:`, `x-libra-control-token=`, …) — it
    /// does NOT do bare-token pattern detection. The audit
    /// pipeline still runs the redactor over the client_id, so a
    /// caller that ATTACHES a marker pattern around a secret
    /// (e.g. paste of `token=...`) gets it scrubbed before the
    /// summary is persisted. Bare secret-shaped client IDs without
    /// markers WILL pass through; that's a documented gap, not a
    /// silent failure (Codex pass-1 P2 C5).
    #[test]
    fn sanitized_audit_client_id_runs_marker_redactor_over_input() {
        let redactor = SecretRedactor::default_runtime();
        let raw = "client-id:token=top-secret-payload";
        let sanitized = sanitized_audit_client_id(&redactor, raw);
        assert!(
            !sanitized.contains("top-secret-payload"),
            "marker redactor failed to mask the value: '{sanitized}'",
        );
    }

    /// Companion regression for the documented gap above: a bare
    /// secret-shaped client_id without a marker prefix DOES survive
    /// the redactor. This is intentional given the marker-only
    /// design of `SecretRedactor::default_runtime()`. Pinning it
    /// makes any future change to the redactor surface (e.g.
    /// adopting pattern-based detection) appear as an obvious
    /// `assert!(...)` failure that needs a deliberate update.
    #[test]
    fn sanitized_audit_client_id_does_not_mask_bare_secret_shaped_input() {
        let redactor = SecretRedactor::default_runtime();
        // A bare secret-SHAPED string with no marker prefix:
        // long random-looking alnum that an attacker might paste
        // as a client_id. The marker redactor (which only looks
        // for `marker=` / `marker:` boundaries) leaves it alone.
        // We use a deliberately synthetic prefix so secret-
        // scanning push protection doesn't flag the literal as a
        // real provider token.
        //
        // Codex pass-2 P2: the assertion has to be tight enough
        // to catch ANY redaction — not just the prefix. If a
        // future change accidentally masks the FAKE… payload, an
        // assertion that only checks for `synthetic-pin-` would
        // still pass and silently invalidate the documented gap.
        // Assert full equality (the input has no leading/trailing
        // whitespace so `trim()` is a no-op).
        let raw = "synthetic-pin-FAKEFAKEFAKEFAKEFAKE-xyz";
        let sanitized = sanitized_audit_client_id(&redactor, raw);
        assert_eq!(
            sanitized, raw,
            "marker-only redactor unexpectedly altered a bare secret \
             shape; the test pin needs updating",
        );
    }

    #[test]
    fn sanitized_audit_client_id_caps_chars_not_bytes() {
        let redactor = SecretRedactor::default_runtime();
        // 120 four-byte emoji codepoints. The cap is 80 CHARS
        // (not bytes), so the result must contain exactly 80
        // chars and a byte length of 80*4 = 320 bytes. A bytes-
        // based truncation would leave us with 80 bytes (= 20
        // emoji) and a much shorter char count.
        //
        // Codex pass-1 P3 C4: the previous version asserted
        // `from_utf8` succeeded — tautological for char-based
        // truncation. The byte-length check below is what
        // actually proves the implementation counts chars rather
        // than bytes, since a bytes-based cap would yield byte_len
        // == 80, not 320.
        let raw = "📦".repeat(120);
        let sanitized = sanitized_audit_client_id(&redactor, &raw);
        assert_eq!(
            sanitized.chars().count(),
            80,
            "cap must apply per-char, got {} chars",
            sanitized.chars().count(),
        );
        assert_eq!(
            sanitized.len(),
            80 * 4,
            "cap must be char-based, not byte-based; a byte cap \
             would have yielded ~80 bytes",
        );
    }
}
