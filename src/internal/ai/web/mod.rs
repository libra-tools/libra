//! # Embedded Web Server for `libra code`
//!
//! This module serves the static Next.js bundle and the provider-agnostic
//! `/api/code/*` protocol used by the browser UI.

pub mod agent_runtime_adapter;
pub mod code_ui;
pub mod code_ui_projection;
pub mod headless;
pub mod sse_wire;
pub mod web_admission;
pub mod write_guards;

use std::{convert::Infallible, net::SocketAddr, path::PathBuf, sync::Arc, time::Duration};

pub use agent_runtime_adapter::AgentRuntimeCodeUiAdapter;
use anyhow::Context;
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
pub use web_admission::{CODE_UI_WEB_TURN_KIND, WebTurnMode, should_route_plain_message_to_plan};

use self::{
    code_ui::{
        CodeUiApiError, CodeUiControllerDetachRequest, CodeUiControllerKind,
        CodeUiGoalCancelRequest, CodeUiGoalStartRequest, CodeUiInteractionResponse,
        CodeUiMessageRequest, CodeUiRuntimeHandle, CodeUiSessionResumeRequest, CodeUiSessionStatus,
        CodeUiSkillActivateRequest, CodeUiTaskDispatchRequest,
        browser_controller_token_from_headers, ensure_session_updated_event,
    },
    write_guards::{
        SessionWriteRateLimiter, ensure_trusted_browser_origin, trusted_loopback_origins,
    },
};
use crate::{
    command::code::{
        ResumeCodeUiSessionError, resolve_storage_root, resume_code_ui_session_to_thread,
    },
    internal::{
        ai::{
            agent::runtime::RuntimeUsageService,
            observed_agents::{AgentKind, discover_skills},
            projection::ThreadProjection,
            runtime::{
                RuntimeWorkerError,
                hardening::{
                    AuditEvent, AuditSink, SecretRedactor, TracingAuditSink, project_json_for_wire,
                },
                runtime_worker_adapter_message,
            },
            usage::{UsageQueryFilter, UsageRecorder},
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
    /// Session-bound browser attach secret (W4-01). When `Some`, `kind:
    /// "browser"` attach requires matching `X-Libra-Browser-Bootstrap`.
    browser_bootstrap_token: Option<Arc<str>>,
    audit_sink: Arc<dyn AuditSink>,
    control_trace_id: Uuid,
    /// Bound listen address used to mint trusted loopback Origins (W3-05).
    bound_addr: SocketAddr,
    /// Per Code UI session write rate limiter (browser + automation, W3-05).
    write_rate_limiter: Arc<SessionWriteRateLimiter>,
    /// Wire-projection redactor (W3-12). Defaults to
    /// [`SecretRedactor::default_runtime`]; callers may attach `--env-file`
    /// forbidden values via [`WebServerOptions::secret_redactor`].
    secret_redactor: Arc<SecretRedactor>,
    /// Durable workflow fan-out for SSE wire v2 (W3-06). When `None`,
    /// `?wire=2` fail-closes with `WIRE_V2_REQUIRES_DURABLE_SESSION`.
    workflow_hub: Option<Arc<sse_wire::CodeUiWorkflowHub>>,
}

#[derive(Clone, Default)]
pub struct WebServerOptions {
    pub code_ui: Option<Arc<CodeUiRuntimeHandle>>,
    pub automation_control_token: Option<Arc<str>>,
    /// Optional session-bound browser bootstrap secret. Production Web
    /// launches with `--browser-control loopback` mint one so forgeable
    /// Origin alone cannot attach.
    pub browser_bootstrap_token: Option<Arc<str>>,
    pub audit_sink: Option<Arc<dyn AuditSink>>,
    /// Optional projection redactor. When `None`, the server uses
    /// [`SecretRedactor::default_runtime`].
    pub secret_redactor: Option<Arc<SecretRedactor>>,
    /// Optional durable workflow hub for SSE wire v2. When `None`, the server
    /// tries [`CodeUiRuntimeHandle::workflow_hub`].
    pub workflow_hub: Option<Arc<sse_wire::CodeUiWorkflowHub>>,
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
///
/// Bind failures are returned as-is (no automatic port scanning). Callers
/// should map [`std::io::ErrorKind::AddrInUse`] through
/// [`describe_web_bind_error`] so operators get an actionable `--port` hint.
pub async fn start(
    host: &str,
    port: u16,
    working_dir: PathBuf,
    options: WebServerOptions,
) -> anyhow::Result<WebServerHandle> {
    let addr = parse_listen_addr(host, port)?;
    let listener = tokio::net::TcpListener::bind(addr).await?;
    let bound_addr = listener.local_addr()?;

    let workflow_hub = options.workflow_hub.or_else(|| {
        options
            .code_ui
            .as_ref()
            .and_then(|runtime| runtime.workflow_hub())
    });
    let app = build_router(WebAppState {
        working_dir: Arc::new(working_dir),
        code_ui: options.code_ui,
        automation_control_token: options.automation_control_token,
        browser_bootstrap_token: options.browser_bootstrap_token,
        audit_sink: options
            .audit_sink
            .unwrap_or_else(|| Arc::new(TracingAuditSink)),
        control_trace_id: Uuid::new_v4(),
        bound_addr,
        write_rate_limiter: SessionWriteRateLimiter::from_env_or_default(),
        secret_redactor: options
            .secret_redactor
            .unwrap_or_else(|| Arc::new(SecretRedactor::default_runtime())),
        workflow_hub,
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

/// Operator-facing bind failure text for Code UI (W3-11).
///
/// Occupied ports fail closed with an explicit `--port` hint; Libra never
/// auto-increments away from the requested port.
pub fn describe_web_bind_error(host: &str, port: u16, err: &anyhow::Error) -> String {
    if web_bind_error_is_addr_in_use(err) {
        format!(
            "failed to bind Code UI on {host}:{port}: address already in use. \
             Pass an explicit --port <free-port>; Libra does not auto-scan ports."
        )
    } else {
        format!("failed to start web server on {host}:{port}: {err}")
    }
}

/// Parse a CLI `--host`/`--port` pair into a concrete [`SocketAddr`].
///
/// Accepts dotted IPv4, bracketed or bare IPv6 (`::1`), and hostnames such as
/// `localhost` (resolved via `ToSocketAddrs`). Plain `format!("{host}:{port}")`
/// parsing rejects `localhost` and bare `::1`.
pub fn parse_listen_addr(host: &str, port: u16) -> anyhow::Result<SocketAddr> {
    use std::net::ToSocketAddrs;

    let host = host.trim();
    if host.is_empty() {
        anyhow::bail!("bind host must not be empty");
    }

    if let Ok(addr) = format!("{host}:{port}").parse::<SocketAddr>() {
        return Ok(addr);
    }

    // Bare IPv6 without brackets (e.g. `--host ::1`).
    if host.contains(':')
        && !host.starts_with('[')
        && let Ok(addr) = format!("[{host}]:{port}").parse::<SocketAddr>()
    {
        return Ok(addr);
    }

    let mut addrs = (host, port)
        .to_socket_addrs()
        .with_context(|| format!("failed to resolve bind host '{host}'"))?;
    addrs
        .next()
        .ok_or_else(|| anyhow::anyhow!("bind host '{host}' resolved to no addresses"))
}

fn web_bind_error_is_addr_in_use(err: &anyhow::Error) -> bool {
    for cause in err.chain() {
        if let Some(io) = cause.downcast_ref::<std::io::Error>()
            && io.kind() == std::io::ErrorKind::AddrInUse
        {
            return true;
        }
    }
    let text = err.to_string().to_ascii_lowercase();
    text.contains("address already in use") || text.contains("addrinuse")
}

/// W3-08 slow-consumer contract for the SSE matrix filter `sse_slow_consumer`.
///
/// A subscribed wire-v2 client that lags past the transport broadcast budget
/// must receive `event: resync` / `WIRE_V2_RESYNC_REQUIRED` (same capacity
/// policy as over-budget bootstrap), then tip-cursor reconnect continues
/// without replaying the over-budget prefix.
///
/// Compiled only with `--features test-provider` so release binaries never
/// carry this assertion helper.
#[cfg(feature = "test-provider")]
pub async fn assert_sse_slow_consumer_contract() -> anyhow::Result<()> {
    use axum::{
        body::Body,
        extract::connect_info::MockConnectInfo,
        http::{Method, Request, StatusCode},
    };
    use futures_util::StreamExt;
    use tempfile::tempdir;
    use tower::ServiceExt;

    use crate::internal::ai::{
        session::{CodeWorkflowEventKind, SessionJsonlStore},
        web::{
            code_ui::{
                CodeUiCapabilities, CodeUiInitialController, CodeUiProviderInfo,
                CodeUiRuntimeHandle, CodeUiSession, ReadOnlyCodeUiAdapter, initial_snapshot,
            },
            sse_wire::{
                CodeUiWorkflowHub, MAX_CODE_UI_TRANSPORT_BACKLOG_EVENTS, WIRE_V2_RESYNC_REQUIRED,
            },
        },
    };

    let session = CodeUiSession::new(initial_snapshot(
        "/tmp/libra-w308-slow",
        CodeUiProviderInfo {
            provider: "test".to_string(),
            model: Some("test-model".to_string()),
            mode: None,
            managed: false,
        },
        CodeUiCapabilities::default(),
    ));
    let runtime = CodeUiRuntimeHandle::build(
        ReadOnlyCodeUiAdapter::new(session, CodeUiCapabilities::default()),
        true,
        CodeUiInitialController::Unclaimed,
    )
    .await;

    let dir = tempdir()?;
    let mut store = SessionJsonlStore::new(dir.path().to_path_buf());
    let hub = Arc::new(CodeUiWorkflowHub::attach(&mut store)?);
    let app = code_router()
        .with_state(WebAppState {
            working_dir: Arc::new(PathBuf::from("/tmp/libra-w308-slow")),
            code_ui: Some(runtime),
            automation_control_token: None,
            browser_bootstrap_token: None,
            audit_sink: Arc::new(TracingAuditSink),
            control_trace_id: Uuid::new_v4(),
            bound_addr: SocketAddr::from(([127, 0, 0, 1], 4321)),
            write_rate_limiter: SessionWriteRateLimiter::from_env_or_default(),
            secret_redactor: Arc::new(SecretRedactor::default_runtime()),
            workflow_hub: Some(hub.clone()),
        })
        .layer(MockConnectInfo(SocketAddr::from(([127, 0, 0, 1], 1))));

    let request = Request::builder()
        .method(Method::GET)
        .uri("/events?wire=2")
        .body(Body::empty())?;
    let response = app.clone().oneshot(request).await?;
    anyhow::ensure!(
        response.status() == StatusCode::OK,
        "SSE status {}",
        response.status()
    );
    let mut stream = response.into_body().into_data_stream();

    let over = MAX_CODE_UI_TRANSPORT_BACKLOG_EVENTS + 1;
    let kinds: Vec<_> = (0..over)
        .map(|i| CodeWorkflowEventKind::CodeUiProjectionDelta {
            projection: "status".to_string(),
            summary: format!("slow-{i}"),
            payload: serde_json::json!({}),
        })
        .collect();
    store.append_code_workflow_batch(&kinds)?;

    let mut collected = Vec::new();
    let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
    while tokio::time::Instant::now() < deadline {
        match timeout(Duration::from_millis(200), stream.next()).await {
            Ok(Some(Ok(chunk))) => {
                collected.extend_from_slice(&chunk);
                let text = String::from_utf8_lossy(&collected);
                if text.contains("event: resync") && text.contains(WIRE_V2_RESYNC_REQUIRED) {
                    break;
                }
            }
            Ok(Some(Err(error))) => anyhow::bail!("SSE body error: {error}"),
            Ok(None) => break,
            Err(_) => continue,
        }
    }
    let body = String::from_utf8(collected)?;
    anyhow::ensure!(
        body.contains("event: resync") && body.contains(WIRE_V2_RESYNC_REQUIRED),
        "slow consumer must get resync, not silent drop: {body}"
    );
    anyhow::ensure!(
        body.contains("lagged_catchup_exceeded")
            || body.contains("live_catchup_exceeded")
            || body.contains("bootstrap_window_exceeded"),
        "resync reason must name the capacity exit: {body}"
    );

    let durable_tail = hub.durable_tail_sequence();
    let reconnect = Request::builder()
        .method(Method::GET)
        .uri(format!("/events?wire=2&cursor={durable_tail}"))
        .body(Body::empty())?;
    let reconnect_response = app.oneshot(reconnect).await?;
    anyhow::ensure!(
        reconnect_response.status() == StatusCode::OK,
        "reconnect status {}",
        reconnect_response.status()
    );
    let mut reconnect_stream = reconnect_response.into_body().into_data_stream();

    store.append_code_workflow(CodeWorkflowEventKind::CodeUiProjectionDelta {
        projection: "status".to_string(),
        summary: "after-slow-resync".to_string(),
        payload: serde_json::json!({ "ok": true }),
    })?;

    let mut collected = Vec::new();
    let deadline = tokio::time::Instant::now() + Duration::from_secs(3);
    let expected_cursor = durable_tail + 1;
    while tokio::time::Instant::now() < deadline {
        match timeout(Duration::from_millis(200), reconnect_stream.next()).await {
            Ok(Some(Ok(chunk))) => {
                collected.extend_from_slice(&chunk);
                let text = String::from_utf8_lossy(&collected);
                if text.contains("after-slow-resync")
                    && text.contains(&format!("\"cursor\":{expected_cursor}"))
                {
                    break;
                }
            }
            Ok(Some(Err(error))) => anyhow::bail!("reconnect SSE error: {error}"),
            Ok(None) => break,
            Err(_) => continue,
        }
    }
    let body = String::from_utf8(collected)?;
    anyhow::ensure!(
        body.contains("after-slow-resync")
            && body.contains(&format!("\"cursor\":{expected_cursor}")),
        "tip-cursor reconnect must deliver the next event once: {body}"
    );
    Ok(())
}

/// W3-11 host-posture contract for the security matrix filter `host_posture`.
///
/// Non-loopback peers only receive the static remote notice (HTML, no session
/// data / tokens / control metadata). Snapshot / transcript / SSE / approval /
/// write API surfaces stay loopback-gated with `LOOPBACK_REQUIRED`.
///
/// Compiled only with `--features test-provider` so release binaries never
/// carry this assertion helper or its panic paths.
#[cfg(feature = "test-provider")]
pub async fn assert_host_posture_non_loopback_contract() -> anyhow::Result<()> {
    use axum::{
        body::{Body, to_bytes},
        extract::connect_info::MockConnectInfo,
        http::{Method, Request, StatusCode, Uri, header},
    };
    use tower::ServiceExt;

    let remote = SocketAddr::from((std::net::Ipv4Addr::new(192, 0, 2, 10), 4000));
    let state = WebAppState {
        working_dir: Arc::new(PathBuf::from("/tmp/libra-host-posture")),
        code_ui: None,
        automation_control_token: Some(Arc::from("must-not-leak")),
        browser_bootstrap_token: None,
        audit_sink: Arc::new(TracingAuditSink),
        control_trace_id: Uuid::new_v4(),
        bound_addr: SocketAddr::from(([0, 0, 0, 0], 3020)),
        write_rate_limiter: SessionWriteRateLimiter::from_env_or_default(),
        secret_redactor: Arc::new(SecretRedactor::default_runtime()),
        workflow_hub: None,
    };

    let api = code_router()
        .with_state(state.clone())
        .layer(MockConnectInfo(remote));
    for (method, uri) in [
        (Method::GET, "/session"),
        (Method::GET, "/thread-graph"),
        (Method::GET, "/events"),
        (Method::GET, "/diagnostics"),
        (Method::POST, "/controller/attach"),
        (Method::POST, "/messages"),
        (Method::POST, "/interactions/demo"),
        (Method::POST, "/control/cancel"),
    ] {
        let request = Request::builder()
            .method(method.clone())
            .uri(uri)
            .header(header::CONTENT_TYPE, "application/json")
            .body(Body::from("{}"))
            .map_err(|error| anyhow::anyhow!("host posture request build failed: {error}"))?;
        let response = api
            .clone()
            .oneshot(request)
            .await
            .map_err(|error| anyhow::anyhow!("host posture oneshot failed: {error}"))?;
        if response.status() != StatusCode::FORBIDDEN {
            anyhow::bail!(
                "non-loopback {method} {uri} must be 403, got {}",
                response.status()
            );
        }
        let body = to_bytes(response.into_body(), usize::MAX)
            .await
            .map_err(|error| anyhow::anyhow!("host posture body read failed: {error}"))?;
        let value: serde_json::Value = serde_json::from_slice(&body)
            .map_err(|error| anyhow::anyhow!("host posture JSON parse failed: {error}"))?;
        if value["error"]["code"] != "LOOPBACK_REQUIRED" {
            anyhow::bail!("non-loopback {method} {uri} must stay LOOPBACK_REQUIRED: {value}");
        }
        let text = String::from_utf8_lossy(&body);
        if text.contains("must-not-leak") {
            anyhow::bail!("non-loopback API must not echo control tokens: {text}");
        }
    }

    let mut headers = HeaderMap::new();
    headers.insert(
        header::ACCEPT,
        "text/html"
            .parse()
            .map_err(|error| anyhow::anyhow!("invalid Accept header: {error}"))?,
    );
    headers.insert(
        header::HOST,
        "0.0.0.0:3020"
            .parse()
            .map_err(|error| anyhow::anyhow!("invalid Host header: {error}"))?,
    );
    let notice = static_handler(ConnectInfo(remote), headers, Uri::from_static("/"))
        .await
        .into_response();
    if notice.status() != StatusCode::OK {
        anyhow::bail!(
            "non-loopback HTML notice must be 200, got {}",
            notice.status()
        );
    }
    let notice_body = to_bytes(notice.into_body(), usize::MAX)
        .await
        .map_err(|error| anyhow::anyhow!("host posture notice body read failed: {error}"))?;
    let html = String::from_utf8(notice_body.to_vec())
        .map_err(|error| anyhow::anyhow!("host posture notice UTF-8 failed: {error}"))?;
    if !(html.contains("loopback") || html.contains("本机")) {
        anyhow::bail!("remote notice missing loopback guidance: {html}");
    }
    if html.contains("<script") {
        anyhow::bail!("remote notice must be zero JS");
    }
    if html.contains("must-not-leak") {
        anyhow::bail!("notice must not leak tokens");
    }
    if html.contains("controllerToken") || html.contains("control-token") {
        anyhow::bail!("notice must not expose control metadata");
    }
    Ok(())
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
    //   /thread-graph     -> loopback only (observe; indexed Intent/Plan/Task/Run graph)
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
    let router = Router::new()
        .route("/session", get(code_session_handler))
        .route("/thread-graph", get(code_thread_graph_handler))
        .route("/events", get(code_events_handler))
        .route("/diagnostics", get(code_diagnostics_handler))
        .route("/threads", get(code_threads_handler))
        .route("/usage", get(code_usage_handler))
        .route("/skills", get(code_skills_handler))
        .route("/goal/status", get(code_goal_status_handler))
        .route("/controller/attach", post(code_controller_attach_handler))
        .route("/controller/detach", post(code_controller_detach_handler));
    #[cfg(feature = "test-provider")]
    let router = router.route(
        "/test/expire-controller-lease",
        post(code_expire_controller_lease_for_test_handler),
    );
    router
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
        .route("/skills/activate", post(code_skill_activate_handler))
        .route("/session/resume", post(code_session_resume_handler))
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
            retry_after_secs: None,
        })
}

async fn code_session_handler(
    ConnectInfo(remote_addr): ConnectInfo<SocketAddr>,
    State(state): State<WebAppState>,
) -> Result<Json<serde_json::Value>, WebApiError> {
    ensure_loopback_api_request(remote_addr)?;
    let runtime = code_ui_runtime(&state)?;
    let projected =
        project_json_for_wire(&runtime.snapshot().await, state.secret_redactor.as_ref()).map_err(
            |error| WebApiError {
                status: StatusCode::INTERNAL_SERVER_ERROR,
                code: "REDACTION_FAILED".to_string(),
                message: format!("failed to redact session snapshot for wire projection: {error}"),
                retry_after_secs: None,
            },
        )?;
    Ok(Json(projected))
}

#[derive(Debug, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
struct ThreadGraphRawQuery {
    thread_id: String,
}

async fn code_thread_graph_handler(
    ConnectInfo(remote_addr): ConnectInfo<SocketAddr>,
    State(state): State<WebAppState>,
    Query(query): Query<ThreadGraphRawQuery>,
) -> Result<Json<serde_json::Value>, WebApiError> {
    ensure_loopback_api_request(remote_addr)?;
    let thread_id = Uuid::parse_str(query.thread_id.trim()).map_err(|error| WebApiError {
        status: StatusCode::BAD_REQUEST,
        code: "THREAD_GRAPH_INVALID_ID".to_string(),
        message: format!("thread-graph expects a canonical thread UUID: {error}"),
        retry_after_secs: None,
    })?;
    let storage_root =
        resolve_storage_root(state.working_dir.as_path()).ok_or_else(|| WebApiError {
            status: StatusCode::INTERNAL_SERVER_ERROR,
            code: "THREAD_GRAPH_STORAGE_UNAVAILABLE".to_string(),
            message: "cannot resolve the repository storage root for the thread graph".to_string(),
            retry_after_secs: None,
        })?;
    let graph =
        match crate::command::graph::load_thread_graph_summary(&storage_root, thread_id).await {
            Ok(graph) => graph,
            Err(error)
                if error
                    .downcast_ref::<crate::command::graph::ThreadGraphNotFound>()
                    .is_some() =>
            {
                return Err(WebApiError {
                    status: StatusCode::NOT_FOUND,
                    code: "THREAD_GRAPH_NOT_FOUND".to_string(),
                    message: error.to_string(),
                    retry_after_secs: None,
                });
            }
            Err(error) => {
                return Err(WebApiError {
                    status: StatusCode::INTERNAL_SERVER_ERROR,
                    code: "THREAD_GRAPH_UNAVAILABLE".to_string(),
                    message: format!("indexed thread graph could not be loaded: {error}"),
                    retry_after_secs: None,
                });
            }
        };
    let projected = project_json_for_wire(
        &graph.to_code_ui_thread_graph(),
        state.secret_redactor.as_ref(),
    )
    .map_err(|error| WebApiError {
        status: StatusCode::INTERNAL_SERVER_ERROR,
        code: "REDACTION_FAILED".to_string(),
        message: format!("failed to redact thread graph for wire projection: {error}"),
        retry_after_secs: None,
    })?;
    Ok(Json(projected))
}

#[derive(Debug, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
struct UsageRawQuery {
    session_id: Option<String>,
    thread_id: Option<String>,
    turn_id: Option<String>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct UsageReadModelResponse {
    #[serde(skip_serializing_if = "Option::is_none")]
    turn_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    session_id: Option<String>,
    cumulative: crate::internal::ai::agent::runtime::RuntimeUsageTotals,
    #[serde(skip_serializing_if = "Option::is_none")]
    turn_delta: Option<crate::internal::ai::agent::runtime::RuntimeUsageTotals>,
    /// Absent when durable per-sub-agent enumeration is unavailable. An empty
    /// array would falsely mean "known zero sub-agents".
    #[serde(skip_serializing_if = "Option::is_none")]
    sub_agents: Option<Vec<serde_json::Value>>,
    /// `unavailable` until RuntimeUsageService can enumerate child agents.
    sub_agents_status: &'static str,
}

/// `GET /api/code/usage` exposes persisted runtime usage, never synthetic
/// zeroes.  A database failure is surfaced as an API error so callers can
/// retain the usage read model's unknown/partial semantics.
async fn code_usage_handler(
    ConnectInfo(remote_addr): ConnectInfo<SocketAddr>,
    State(state): State<WebAppState>,
    Query(query): Query<UsageRawQuery>,
) -> Result<Json<UsageReadModelResponse>, WebApiError> {
    ensure_loopback_api_request(remote_addr)?;
    let snapshot = code_ui_runtime(&state)?.snapshot().await;
    let storage_root =
        resolve_storage_root(state.working_dir.as_path()).ok_or_else(|| WebApiError {
            status: StatusCode::INTERNAL_SERVER_ERROR,
            code: "STORAGE_ROOT_UNRESOLVED".to_string(),
            message: "cannot resolve repository storage for usage query".to_string(),
            retry_after_secs: None,
        })?;
    let db_path = storage_root.join("libra.db");
    let db_path = db_path.to_str().ok_or_else(|| WebApiError {
        status: StatusCode::INTERNAL_SERVER_ERROR,
        code: "STORAGE_PATH_INVALID".to_string(),
        message: "libra database path is not valid UTF-8".to_string(),
        retry_after_secs: None,
    })?;
    let db = establish_connection(db_path)
        .await
        .map_err(|error| WebApiError {
            status: StatusCode::INTERNAL_SERVER_ERROR,
            code: "DB_UNAVAILABLE".to_string(),
            message: format!("failed to open usage storage: {error}"),
            retry_after_secs: None,
        })?;
    let usage = RuntimeUsageService::new(UsageRecorder::new(db));
    // Do not mix a caller-supplied scope with the live snapshot's other
    // dimension: `?sessionId=historical` must not AND against the live
    // threadId (and vice versa). Only fall back to the live snapshot when
    // the caller omitted both filters.
    let (response_session_id, requested_thread) = match (
        query
            .session_id
            .as_deref()
            .map(str::trim)
            .filter(|s| !s.is_empty()),
        query
            .thread_id
            .as_deref()
            .map(str::trim)
            .filter(|s| !s.is_empty()),
    ) {
        (None, None) => (
            Some(snapshot.session_id.clone()),
            snapshot.thread_id.clone(),
        ),
        (session, thread) => (session.map(str::to_string), thread.map(str::to_string)),
    };
    // Generated headless sessions often mirror session_id into
    // snapshot.threadId while durable usage rows store NULL thread_id.
    // Drop only that synthetic mirror; preserve any other nonempty ID.
    let thread_id = requested_thread.and_then(|tid| {
        if response_session_id
            .as_deref()
            .is_some_and(|session_id| session_id == tid)
        {
            return None;
        }
        Some(tid)
    });
    // Prefer a single durable scope for the SQL filter. When the SPA sends
    // both snapshot IDs, AND-ing them rejects legacy projections that historically
    // mirrored the thread UUID into sessionId while usage rows store
    // SessionState.id. Thread id is the stable join key once present; session
    // id remains the response echo / session-only fallback.
    let filter = UsageQueryFilter {
        session_id: if thread_id.is_some() {
            None
        } else {
            response_session_id.clone()
        },
        thread_id: thread_id.clone(),
        ..UsageQueryFilter::default()
    };
    let cumulative = usage
        .cumulative(filter.clone())
        .await
        .map_err(|error| WebApiError {
            status: StatusCode::INTERNAL_SERVER_ERROR,
            code: "USAGE_UNAVAILABLE".to_string(),
            message: format!("failed to query runtime usage: {error}"),
            retry_after_secs: None,
        })?;
    let turn_delta = match query.turn_id.clone() {
        Some(turn_id) => Some(usage.current_turn(turn_id.clone(), filter).await.map_err(
            |error| WebApiError {
                status: StatusCode::INTERNAL_SERVER_ERROR,
                code: "USAGE_UNAVAILABLE".to_string(),
                message: format!("failed to query runtime turn usage: {error}"),
                retry_after_secs: None,
            },
        )?),
        None => None,
    };
    Ok(Json(UsageReadModelResponse {
        turn_id: query.turn_id,
        session_id: response_session_id,
        cumulative,
        turn_delta,
        // Do not emit an empty array: that would look like "known zero
        // sub-agents" while durable enumeration is still unavailable.
        sub_agents: None,
        sub_agents_status: "unavailable",
    }))
}

async fn code_events_handler(
    ConnectInfo(remote_addr): ConnectInfo<SocketAddr>,
    State(state): State<WebAppState>,
    Query(query): Query<sse_wire::CodeEventsQuery>,
    headers: HeaderMap,
) -> Result<Response, WebApiError> {
    ensure_loopback_api_request(remote_addr)?;
    let wire = sse_wire::parse_code_events_wire_version(&query, &headers).map_err(|message| {
        WebApiError {
            status: StatusCode::BAD_REQUEST,
            code: "INVALID_WIRE_VERSION".to_string(),
            message,
            retry_after_secs: None,
        }
    })?;

    match wire {
        sse_wire::CodeUiSseWireVersion::V1 => {
            let runtime = code_ui_runtime(&state)?;
            let redactor = state.secret_redactor.clone();
            let current_snapshot = runtime.snapshot().await;
            let initial_event = ensure_session_updated_event(&current_snapshot)?;
            let receiver = runtime.subscribe();

            let initial_redactor = redactor.clone();
            let initial_stream = stream::once(async move {
                Ok::<Event, Infallible>(code_ui_event_to_sse(
                    initial_event,
                    initial_redactor.as_ref(),
                ))
            });
            let updates = BroadcastStream::new(receiver).filter_map(move |message| {
                let runtime = runtime.clone();
                let redactor = redactor.clone();
                async move {
                    code_ui_broadcast_event_or_recovery(&runtime, message)
                        .await
                        .map(|event| {
                            Ok::<Event, Infallible>(code_ui_event_to_sse(event, redactor.as_ref()))
                        })
                }
            });
            Ok(Sse::new(initial_stream.chain(updates))
                .keep_alive(KeepAlive::new())
                .into_response())
        }
        sse_wire::CodeUiSseWireVersion::V2 => {
            // `cursor` is v2-only; ignore it on v1 so legacy clients with stray
            // query params keep working.
            let cursor =
                sse_wire::parse_code_events_cursor(&query).map_err(|message| WebApiError {
                    status: StatusCode::BAD_REQUEST,
                    code: "INVALID_QUERY_PARAM".to_string(),
                    message,
                    retry_after_secs: None,
                })?;
            let _runtime = code_ui_runtime(&state)?;
            let hub = state.workflow_hub.clone().ok_or_else(|| WebApiError {
                status: StatusCode::SERVICE_UNAVAILABLE,
                code: "WIRE_V2_REQUIRES_DURABLE_SESSION".to_string(),
                message: "SSE wire v2 requires a durable Code UI session store (use the default Web launch with session persistence)".to_string(),
                retry_after_secs: None,
            })?;
            let redactor = state.secret_redactor.clone();
            let receiver = hub.subscribe();
            let durable_tail = hub.durable_tail_sequence();
            if cursor > durable_tail {
                return Err(WebApiError {
                    status: StatusCode::CONFLICT,
                    code: "WIRE_V2_CURSOR_AHEAD".to_string(),
                    message: format!(
                        "SSE wire v2 cursor {cursor} is ahead of durable workflow tail {durable_tail}; drop the cursor and resync from 0 or the last acknowledged sequence"
                    ),
                    retry_after_secs: None,
                });
            }
            let replayed = match hub.replay_after(cursor) {
                Ok(events) => events,
                Err(error) if sse_wire::transport_backlog_exceeded(&error) => {
                    // W3-08: over-budget bootstrap is a recoverable resync exit
                    // (same capacity policy as slow-consumer disconnect).
                    let resync = code_ui_wire_v2_resync_sse(
                        "bootstrap_window_exceeded",
                        cursor,
                        hub.durable_tail_sequence(),
                    );
                    let stream = stream::iter(vec![
                        Ok::<Event, std::io::Error>(resync),
                        Err(std::io::Error::other(format!(
                            "{}: bootstrap replay exceeded transport backlog",
                            sse_wire::WIRE_V2_RESYNC_REQUIRED
                        ))),
                    ]);
                    return Ok(Sse::new(stream)
                        .keep_alive(KeepAlive::new())
                        .into_response());
                }
                Err(error) => {
                    return Err(WebApiError {
                        status: StatusCode::INTERNAL_SERVER_ERROR,
                        code: "WIRE_V2_REPLAY_FAILED".to_string(),
                        message: format!(
                            "failed to replay Code UI workflow events after cursor {cursor}: {error}"
                        ),
                        retry_after_secs: None,
                    });
                }
            };
            let last_replayed = replayed
                .last()
                .map(|event| event.sequence)
                .unwrap_or(cursor);
            let last_delivered =
                std::sync::Arc::new(std::sync::atomic::AtomicU64::new(last_replayed));
            let initial_events: Vec<Result<Event, std::io::Error>> = {
                let mut out = Vec::with_capacity(replayed.len());
                for event in replayed {
                    match try_code_ui_wire_v2_to_sse(
                        &sse_wire::CodeUiWireV2Event::from_workflow_event(&event),
                        redactor.as_ref(),
                    ) {
                        Ok(frame) => out.push(Ok(frame)),
                        Err(error_event) => {
                            out.push(Ok(error_event));
                            out.push(Err(std::io::Error::other(
                                "WIRE_V2_REPLAY_FAILED: secret redaction failed during durable replay",
                            )));
                            break;
                        }
                    }
                }
                out
            };
            let initial_stream = stream::iter(initial_events);
            let live = BroadcastStream::new(receiver).flat_map(move |message| {
                let hub = hub.clone();
                let redactor = redactor.clone();
                let last_delivered = last_delivered.clone();
                let prev = last_delivered.load(std::sync::atomic::Ordering::Relaxed);
                let events: Vec<Result<Event, std::io::Error>> = match message {
                    // Fast path: contiguous in-budget full event — no disk replay.
                    Ok(sse_wire::CodeUiWorkflowLiveNotify::Event(event))
                        if event.sequence == prev.saturating_add(1) =>
                    {
                        match try_code_ui_wire_v2_to_sse(
                            &sse_wire::CodeUiWireV2Event::from_workflow_event(event.as_ref()),
                            redactor.as_ref(),
                        ) {
                            Ok(frame) => {
                                last_delivered
                                    .store(event.sequence, std::sync::atomic::Ordering::Relaxed);
                                vec![Ok(frame)]
                            }
                            Err(error_event) => vec![
                                Ok(error_event),
                                Err(std::io::Error::other(
                                    "WIRE_V2_REPLAY_FAILED: secret redaction failed on live event",
                                )),
                            ],
                        }
                    }
                    // Tip-only / out-of-order full event / lag: durable catch-up
                    // under the transport byte+count window (may resync).
                    Ok(sse_wire::CodeUiWorkflowLiveNotify::Event(event))
                        if event.sequence > prev =>
                    {
                        wire_v2_durable_catch_up_frames(
                            &hub,
                            redactor.as_ref(),
                            &last_delivered,
                            prev,
                            "live_catchup_exceeded",
                        )
                    }
                    Ok(sse_wire::CodeUiWorkflowLiveNotify::Tip { sequence }) if sequence > prev => {
                        wire_v2_durable_catch_up_frames(
                            &hub,
                            redactor.as_ref(),
                            &last_delivered,
                            prev,
                            "live_catchup_exceeded",
                        )
                    }
                    Ok(_) => Vec::new(),
                    Err(BroadcastStreamRecvError::Lagged(_)) => wire_v2_durable_catch_up_frames(
                        &hub,
                        redactor.as_ref(),
                        &last_delivered,
                        prev,
                        "lagged_catchup_exceeded",
                    ),
                };
                stream::iter(events)
            });
            Ok(Sse::new(initial_stream.chain(live))
                .keep_alive(KeepAlive::new())
                .into_response())
        }
    }
}

/// Transport-bounded durable catch-up shared by tip notifies and lag recovery.
fn wire_v2_durable_catch_up_frames(
    hub: &sse_wire::CodeUiWorkflowHub,
    redactor: &SecretRedactor,
    last_delivered: &std::sync::atomic::AtomicU64,
    prev: u64,
    capacity_reason: &str,
) -> Vec<Result<Event, std::io::Error>> {
    match hub.replay_after(prev) {
        Ok(catch_up) => {
            let mut frames = Vec::with_capacity(catch_up.len());
            for event in catch_up {
                match try_code_ui_wire_v2_to_sse(
                    &sse_wire::CodeUiWireV2Event::from_workflow_event(&event),
                    redactor,
                ) {
                    Ok(frame) => {
                        last_delivered.store(event.sequence, std::sync::atomic::Ordering::Relaxed);
                        frames.push(Ok(frame));
                    }
                    Err(error_event) => {
                        frames.push(Ok(error_event));
                        frames.push(Err(std::io::Error::other(
                            "WIRE_V2_REPLAY_FAILED: secret redaction failed during durable catch-up",
                        )));
                        break;
                    }
                }
            }
            frames
        }
        Err(error) if sse_wire::transport_backlog_exceeded(&error) => {
            vec![
                Ok(code_ui_wire_v2_resync_sse(
                    capacity_reason,
                    prev,
                    hub.durable_tail_sequence(),
                )),
                Err(std::io::Error::other(format!(
                    "{}: {capacity_reason}: {error}",
                    sse_wire::WIRE_V2_RESYNC_REQUIRED
                ))),
            ]
        }
        Err(error) => {
            vec![
                Ok(code_ui_wire_v2_error_sse(
                    "WIRE_V2_REPLAY_FAILED",
                    "SSE consumer could not catch up from the durable workflow log; reconnect from the last acknowledged cursor",
                )),
                Err(std::io::Error::other(format!(
                    "WIRE_V2_REPLAY_FAILED: durable catch-up failed: {error}"
                ))),
            ]
        }
    }
}

fn try_code_ui_wire_v2_to_sse(
    event: &sse_wire::CodeUiWireV2Event,
    redactor: &SecretRedactor,
) -> Result<Event, Event> {
    match project_json_for_wire(event, redactor) {
        Ok(projected) => Event::default()
            .event("code_workflow")
            .id(event.cursor.to_string())
            .json_data(projected)
            .map_err(|_| {
                code_ui_wire_v2_error_sse(
                    "REDACTION_FAILED",
                    "workflow event omitted because secret redaction failed",
                )
            }),
        Err(_) => Err(code_ui_wire_v2_error_sse(
            "REDACTION_FAILED",
            "workflow event omitted because secret redaction failed",
        )),
    }
}

fn code_ui_wire_v2_error_sse(code: &str, message: &str) -> Event {
    Event::default()
        .event("error")
        .json_data(serde_json::json!({
            "error": {
                "code": code,
                "message": message
            }
        }))
        .unwrap_or_else(|_| Event::default().event("error"))
}

/// W3-08 recoverable transport-capacity exit (`event: resync` then stream end).
fn code_ui_wire_v2_resync_sse(reason: &str, last_cursor: u64, durable_tail: u64) -> Event {
    let payload =
        sse_wire::CodeUiWireV2ResyncEvent::transport_backlog(reason, last_cursor, durable_tail);
    Event::default()
        .event("resync")
        .json_data(payload)
        .unwrap_or_else(|_| {
            Event::default().event("resync").data(format!(
                "{{\"code\":\"{}\",\"reason\":\"{reason}\",\"lastCursor\":{last_cursor},\"durableTail\":{durable_tail},\"action\":\"fetch_snapshot\"}}",
                sse_wire::WIRE_V2_RESYNC_REQUIRED
            ))
        })
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
    // Wave 7 / PR 7 + W3-12 — diagnostics go through the shared
    // `SecretRedactor` (optionally enriched with `--env-file` forbidden
    // values). Fail closed if projection/redaction cannot run.
    let projected = project_json_for_wire(
        &runtime
            .diagnostics()
            .await
            .redact(state.secret_redactor.as_ref()),
        state.secret_redactor.as_ref(),
    )
    .map_err(|error| WebApiError {
        status: StatusCode::INTERNAL_SERVER_ERROR,
        code: "REDACTION_FAILED".to_string(),
        message: format!("failed to redact diagnostics for wire projection: {error}"),
        retry_after_secs: None,
    })?;
    Ok(Json(projected))
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
        retry_after_secs: None,
    })
}

const DEFAULT_THREAD_LIST_LIMIT: u64 = 50;
const MAX_THREAD_LIST_LIMIT: u64 = 200;

#[derive(Debug, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ThreadListItem {
    pub id: String,
    pub title: Option<String>,
    pub archived: bool,
    pub current_intent_id: Option<String>,
    /// Per-thread cwd is not persisted on ThreadProjection yet. Omit rather
    /// than stamp the server cwd onto repository-shared (linked-worktree)
    /// threads — that would falsely mark foreign threads as resume-compatible.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub working_dir: Option<String>,
    pub created_at: chrono::DateTime<chrono::Utc>,
    pub updated_at: chrono::DateTime<chrono::Utc>,
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
            retry_after_secs: None,
        })?;
    let db_path = storage_root.join("libra.db");
    let db_path_str = db_path.to_str().ok_or_else(|| WebApiError {
        status: StatusCode::INTERNAL_SERVER_ERROR,
        code: "STORAGE_PATH_INVALID".to_string(),
        message: "libra database path is not valid UTF-8".to_string(),
        retry_after_secs: None,
    })?;

    let db = establish_connection(db_path_str)
        .await
        .map_err(|err| WebApiError {
            status: StatusCode::INTERNAL_SERVER_ERROR,
            code: "DB_UNAVAILABLE".to_string(),
            message: format!("failed to open libra database: {err}"),
            retry_after_secs: None,
        })?;

    let projections = ThreadProjection::list_active(&db, limit, offset)
        .await
        .map_err(|err| WebApiError {
            status: StatusCode::INTERNAL_SERVER_ERROR,
            code: "THREAD_LIST_FAILED".to_string(),
            message: format!("failed to list active threads: {err}"),
            retry_after_secs: None,
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
            working_dir: None,
            created_at: p.created_at,
            updated_at: p.updated_at,
        })
        .collect();

    Ok(Json(ThreadListResponse { items, next_offset }))
}

#[derive(Debug, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
struct SkillsRawQuery {
    provider: Option<String>,
    skill: Option<String>,
}

async fn code_skills_handler(
    ConnectInfo(remote_addr): ConnectInfo<SocketAddr>,
    Query(query): Query<SkillsRawQuery>,
) -> Result<Json<serde_json::Value>, WebApiError> {
    ensure_loopback_api_request(remote_addr)?;
    let provider_filter = match query
        .provider
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
    {
        None => None,
        Some(provider) => Some(
            AgentKind::from_cli_slug(provider).ok_or_else(|| WebApiError {
                status: StatusCode::BAD_REQUEST,
                code: "INVALID_SKILL_PROVIDER".to_string(),
                message: format!("unknown skill provider '{provider}'; use an A0-07 agent slug"),
                retry_after_secs: None,
            })?,
        ),
    };
    let kinds = match provider_filter {
        Some(kind) => vec![kind],
        None => vec![AgentKind::ClaudeCode, AgentKind::Codex, AgentKind::OpenCode],
    };
    let mut rows = kinds
        .into_iter()
        .flat_map(discover_skills)
        .filter(|skill| {
            query
                .skill
                .as_deref()
                .is_none_or(|name| name.trim() == skill.name)
        })
        .collect::<Vec<_>>();
    rows.sort_by(|left, right| {
        left.provider
            .cmp(&right.provider)
            .then_with(|| left.name.cmp(&right.name))
    });
    Ok(Json(serde_json::json!({ "items": rows })))
}

async fn code_skill_activate_handler(
    ConnectInfo(remote_addr): ConnectInfo<SocketAddr>,
    State(state): State<WebAppState>,
    headers: HeaderMap,
    Json(body): Json<CodeUiSkillActivateRequest>,
) -> Result<Json<serde_json::Value>, WebApiError> {
    ensure_loopback_api_request(remote_addr)?;
    let runtime = code_ui_runtime(&state)?;
    let token = browser_controller_token_from_headers(&headers);
    let mut audit_kind = CodeUiControllerKind::None;
    let mut audit_client_id = "unknown".to_string();
    let result = async {
        let lease = runtime
            .ensure_controller_write_access(token.as_deref())
            .await
            .map_err(WebApiError::from)?;
        audit_kind = lease.kind;
        audit_client_id = lease.client_id.clone();
        if lease.kind == CodeUiControllerKind::Automation {
            ensure_automation_control_token(&headers, state.automation_control_token.as_ref())?;
        }
        enforce_code_write_identity_gates(&state, &runtime, &headers, lease.kind).await?;
        let provider = body.provider.trim();
        let name = body.name.trim();
        let kind = AgentKind::from_cli_slug(provider).ok_or_else(|| WebApiError {
            status: StatusCode::BAD_REQUEST,
            code: "INVALID_SKILL_PROVIDER".to_string(),
            message: format!("unknown skill provider '{provider}'; use an A0-07 agent slug"),
            retry_after_secs: None,
        })?;
        if !discover_skills(kind).iter().any(|skill| skill.name == name) {
            return Err(WebApiError {
                status: StatusCode::BAD_REQUEST,
                code: "SKILL_NOT_DISCOVERABLE".to_string(),
                message: format!("skill '{name}' is not discoverable for provider '{provider}'"),
            retry_after_secs: None,
        });
        }
        // Discoverability is confirmed, but there is still no in-process
        // activation path that persists a selection or notifies the live
        // provider. Fail closed instead of returning accepted:true with no
        // observable effect — providers emit SkillEvent when they consume a
        // skill on a later turn.
        Err(WebApiError {
            status: StatusCode::UNPROCESSABLE_ENTITY,
            code: "SKILL_ACTIVATION_UNSUPPORTED".to_string(),
            message: format!(
                "skill '{name}' is discoverable for '{provider}', but in-process skill activation is not available yet; the provider emits SkillEvent when it consumes the skill on a later turn"
            ),
            retry_after_secs: None,
        })
    }
    .await;
    append_control_audit(
        &state,
        &runtime,
        "skills.activate",
        audit_kind,
        &audit_client_id,
        control_audit_outcome(&result),
    )
    .await;
    Ok(Json(result?))
}

async fn code_session_resume_handler(
    ConnectInfo(remote_addr): ConnectInfo<SocketAddr>,
    State(state): State<WebAppState>,
    headers: HeaderMap,
    Json(body): Json<CodeUiSessionResumeRequest>,
) -> Result<Json<serde_json::Value>, WebApiError> {
    ensure_loopback_api_request(remote_addr)?;
    let runtime = code_ui_runtime(&state)?;
    let token = browser_controller_token_from_headers(&headers);
    let mut audit_kind = CodeUiControllerKind::None;
    let mut audit_client_id = "unknown".to_string();
    let result = async {
        let lease = runtime
            .ensure_controller_write_access(token.as_deref())
            .await
            .map_err(WebApiError::from)?;
        audit_kind = lease.kind;
        audit_client_id = lease.client_id.clone();
        if lease.kind == CodeUiControllerKind::Automation {
            ensure_automation_control_token(&headers, state.automation_control_token.as_ref())?;
        }
        enforce_code_write_identity_gates(&state, &runtime, &headers, lease.kind).await?;
        let live = runtime.snapshot().await;
        if matches!(
            live.status,
            CodeUiSessionStatus::Thinking | CodeUiSessionStatus::ExecutingTool
        ) {
            return Err(WebApiError::from(CodeUiApiError::conflict(
                "SESSION_RESUME_BUSY",
                "the active Code session is still running; wait for it to settle before selecting another thread",
            )));
        }
        if live.status == CodeUiSessionStatus::IndeterminateSideEffect {
            return Err(WebApiError::from(CodeUiApiError::reconciliation_required(
                "the active Code session has an indeterminate side effect; reconcile it before resuming another thread",
            )));
        }
        // Prove the target thread is loadable under this cwd, but do not swap
        // only the browser projection: the live AgentRuntime still owns the
        // original history/worker session. In-process runtime swap is not
        // available yet — fail closed with a restart hint.
        let snapshot = resume_code_ui_session_to_thread(
            state.working_dir.as_path(),
            &body.thread_id,
            live.provider.clone(),
            live.capabilities.clone(),
        )
        .await
        .map_err(|error| match error {
            ResumeCodeUiSessionError::NotFound { .. } => WebApiError {
                status: StatusCode::NOT_FOUND,
                code: "SESSION_RESUME_NOT_FOUND".to_string(),
                message: error.to_string(),
            retry_after_secs: None,
        },
            ResumeCodeUiSessionError::LoadFailed { .. } => WebApiError {
                status: StatusCode::INTERNAL_SERVER_ERROR,
                code: "SESSION_RESUME_LOAD_FAILED".to_string(),
                message: error.to_string(),
            retry_after_secs: None,
        },
        })?;
        Err(WebApiError {
            status: StatusCode::UNPROCESSABLE_ENTITY,
            code: "SESSION_RESUME_REQUIRES_RESTART".to_string(),
            message: format!(
                "thread '{}' is resumable under '{}' (projected status {}); restart with `libra code --resume {}` from that working directory. In-process AgentRuntime swap is not yet available",
                body.thread_id,
                snapshot.working_dir,
                match snapshot.status {
                    CodeUiSessionStatus::Idle => "idle",
                    CodeUiSessionStatus::Thinking => "thinking",
                    CodeUiSessionStatus::ExecutingTool => "executing_tool",
                    CodeUiSessionStatus::AwaitingInteraction => "awaiting_interaction",
                    CodeUiSessionStatus::Completed => "completed",
                    CodeUiSessionStatus::Error => "error",
                    CodeUiSessionStatus::IndeterminateSideEffect => "indeterminate_side_effect",
                },
                body.thread_id
            ),
            retry_after_secs: None,
        })
    }
    .await;
    append_control_audit(
        &state,
        &runtime,
        "session.resume",
        audit_kind,
        &audit_client_id,
        control_audit_outcome(&result),
    )
    .await;
    Ok(Json(result?))
}

async fn code_controller_attach_handler(
    ConnectInfo(remote_addr): ConnectInfo<SocketAddr>,
    State(state): State<WebAppState>,
    headers: HeaderMap,
    Json(body): Json<code_ui::CodeUiControllerAttachRequest>,
) -> Result<Json<serde_json::Value>, WebApiError> {
    ensure_loopback_api_request(remote_addr)?;
    let runtime = code_ui_runtime(&state)?;
    let kind = resolve_controller_attach_kind(body.kind, &headers);
    let result = async {
        if !matches!(
            kind,
            CodeUiControllerKind::Browser | CodeUiControllerKind::Automation
        ) {
            return Err(WebApiError::from(CodeUiApiError::bad_request(
                "INVALID_CONTROLLER_KIND",
                format!("Controller kind '{}' cannot attach", kind.as_str()),
            )));
        }
        if kind == CodeUiControllerKind::Browser {
            ensure_browser_origin_for_write(&state, &headers)?;
            ensure_browser_bootstrap_token(&headers, state.browser_bootstrap_token.as_ref())?;
        }
        if kind == CodeUiControllerKind::Automation {
            ensure_automation_control_token(&headers, state.automation_control_token.as_ref())?;
        }
        ensure_session_write_rate_limit(&state, &runtime).await?;
        runtime
            .attach_controller(kind, &body.client_id)
            .await
            .map_err(WebApiError::from)
    }
    .await;
    append_control_audit(
        &state,
        &runtime,
        "controller.attach",
        kind,
        &body.client_id,
        control_audit_outcome(&result),
    )
    .await;
    let response = result?;
    Ok(Json(serde_json::to_value(response)?))
}

/// Test-provider-only subprocess seam. Authentication is identical to a
/// regular browser/automation write; the environment opt-in prevents an
/// all-features development binary from exposing the seam accidentally.
#[cfg(feature = "test-provider")]
async fn code_expire_controller_lease_for_test_handler(
    ConnectInfo(remote_addr): ConnectInfo<SocketAddr>,
    State(state): State<WebAppState>,
    headers: HeaderMap,
) -> Result<Json<serde_json::Value>, WebApiError> {
    ensure_loopback_api_request(remote_addr)?;
    if std::env::var("LIBRA_ENABLE_TEST_PROVIDER").as_deref() != Ok("1") {
        return Err(WebApiError::forbidden(
            "CONTROL_DISABLED",
            "controller lease test seam requires LIBRA_ENABLE_TEST_PROVIDER=1",
        ));
    }
    let runtime = code_ui_runtime(&state)?;
    let token = browser_controller_token_from_headers(&headers);
    let lease = runtime
        .ensure_controller_write_access(token.as_deref())
        .await?;
    if lease.kind == CodeUiControllerKind::Browser {
        ensure_browser_origin_for_write(&state, &headers)?;
    } else if lease.kind == CodeUiControllerKind::Automation {
        ensure_automation_control_token(&headers, state.automation_control_token.as_ref())?;
    }
    if !runtime.expire_active_controller_lease_for_test().await {
        return Err(WebApiError::from(CodeUiApiError::conflict(
            "CONTROLLER_CONFLICT",
            "no active controller lease exists to expire",
        )));
    }
    Ok(Json(serde_json::json!({ "expired": true })))
}

/// Resolve attach `kind` when callers omit it.
///
/// Automation clients historically post `{ clientId }` with
/// `X-Libra-Control-Token` and no `kind`. Prefer automation in that case so
/// the Origin gate does not break the control client. Browser SPAs either send
/// `kind: "browser"` or omit both `kind` and the control token.
fn resolve_controller_attach_kind(
    kind: Option<CodeUiControllerKind>,
    headers: &HeaderMap,
) -> CodeUiControllerKind {
    if let Some(kind) = kind {
        return kind;
    }
    let has_control_token = headers
        .get("x-libra-control-token")
        .and_then(|value| value.to_str().ok())
        .map(str::trim)
        .is_some_and(|value| !value.is_empty());
    if has_control_token {
        CodeUiControllerKind::Automation
    } else {
        CodeUiControllerKind::Browser
    }
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
        enforce_code_write_identity_gates(&state, &runtime, &headers, lease.kind).await?;
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
        enforce_code_write_identity_gates(&state, &runtime, &headers, lease.kind).await?;
        runtime
            .submit_message(token.as_deref(), body.text, body.command_id)
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
        enforce_code_write_identity_gates(&state, &runtime, &headers, lease.kind).await?;
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
                // Browser controllers reach parity with the historical `Esc` cancel
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
        enforce_code_write_identity_gates(&state, &runtime, &headers, lease.kind).await?;
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
        enforce_code_write_identity_gates(&state, &runtime, &headers, lease.kind).await?;
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
        enforce_code_write_identity_gates(&state, &runtime, &headers, lease.kind).await?;
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
        enforce_code_write_identity_gates(&state, &runtime, &headers, lease.kind).await?;
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
        retry_after_secs: None,
    }
    .into_response()
}

fn code_ui_runtime(state: &WebAppState) -> Result<Arc<CodeUiRuntimeHandle>, WebApiError> {
    state
        .code_ui
        .clone()
        .ok_or_else(|| WebApiError::from(CodeUiApiError::unavailable()))
}

fn ensure_browser_origin_for_write(
    state: &WebAppState,
    headers: &HeaderMap,
) -> Result<(), WebApiError> {
    let trusted = trusted_loopback_origins(state.bound_addr);
    ensure_trusted_browser_origin(headers, &trusted)
        .map_err(|error| WebApiError::forbidden(error.code(), error.message()))
}

async fn ensure_session_write_rate_limit(
    state: &WebAppState,
    runtime: &CodeUiRuntimeHandle,
) -> Result<(), WebApiError> {
    let session_id = runtime.snapshot().await.session_id;
    state
        .write_rate_limiter
        .check_and_record(&session_id)
        .map_err(WebApiError::rate_limited)
}

/// Shared post-auth write gates for browser Origin (when applicable) and
/// per-session rate limiting (W3-05).
async fn enforce_code_write_identity_gates(
    state: &WebAppState,
    runtime: &CodeUiRuntimeHandle,
    headers: &HeaderMap,
    controller_kind: CodeUiControllerKind,
) -> Result<(), WebApiError> {
    if controller_kind == CodeUiControllerKind::Browser {
        ensure_browser_origin_for_write(state, headers)?;
    }
    ensure_session_write_rate_limit(state, runtime).await
}

fn code_ui_event_to_sse(event: code_ui::CodeUiEventEnvelope, redactor: &SecretRedactor) -> Event {
    // W3-12: never emit an unredacted snapshot on the SSE wire. If
    // redaction/serialization fails, drop payload data (fail closed).
    match project_json_for_wire(&event, redactor) {
        Ok(projected) => Event::default()
            .event(event.event_type.as_str())
            .json_data(projected)
            .unwrap_or_else(|_| code_ui_redaction_failed_sse(event.event_type)),
        Err(_) => code_ui_redaction_failed_sse(event.event_type),
    }
}

fn code_ui_redaction_failed_sse(event_type: code_ui::CodeUiEventType) -> Event {
    Event::default()
        .event(event_type.as_str())
        .json_data(serde_json::json!({
            "error": {
                "code": "REDACTION_FAILED",
                "message": "session event omitted because secret redaction failed"
            }
        }))
        .unwrap_or_else(|_| Event::default().event(event_type.as_str()))
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
            "Automation write control is not enabled; start with --control write",
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

/// Require the session-bound browser bootstrap secret when the server minted
/// one. `None` keeps the historical Origin-only gate (in-process tests).
fn ensure_browser_bootstrap_token(
    headers: &HeaderMap,
    expected: Option<&Arc<str>>,
) -> Result<(), WebApiError> {
    let Some(expected) = expected else {
        return Ok(());
    };

    let Some(actual) = headers
        .get("x-libra-browser-bootstrap")
        .and_then(|value| value.to_str().ok())
        .map(str::trim)
        .filter(|value| !value.is_empty())
    else {
        return Err(WebApiError::forbidden(
            "MISSING_BROWSER_BOOTSTRAP",
            "X-Libra-Browser-Bootstrap is required for browser controller attach",
        ));
    };

    if actual != expected.as_ref() {
        return Err(WebApiError::forbidden(
            "INVALID_BROWSER_BOOTSTRAP",
            "X-Libra-Browser-Bootstrap does not match this Libra Code session",
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
    let redactor = state.secret_redactor.as_ref();
    let client_id = sanitized_audit_client_id(redactor, client_id);
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
            tracing::warn!(error = %error, "failed to serialize local control audit summary");
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
        tracing::warn!(error = %error, action, "failed to append local control audit event");
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
    /// Optional `Retry-After` delay in whole seconds (ceil of remaining window).
    retry_after_secs: Option<u64>,
}

impl WebApiError {
    fn forbidden(code: impl Into<String>, message: impl Into<String>) -> Self {
        Self {
            status: StatusCode::FORBIDDEN,
            code: code.into(),
            message: message.into(),
            retry_after_secs: None,
        }
    }

    fn rate_limited(retry_after: Duration) -> Self {
        let secs = retry_after_secs_ceil(retry_after);
        Self {
            status: StatusCode::TOO_MANY_REQUESTS,
            code: "RATE_LIMITED".to_string(),
            message: format!("Code UI session write rate limit exceeded; retry after {secs}s"),
            retry_after_secs: Some(secs),
        }
    }
}

/// Ceil a retry delay to whole seconds so clients waiting the advertised
/// value are not immediately re-throttled by truncated `as_secs()`.
fn retry_after_secs_ceil(retry_after: Duration) -> u64 {
    retry_after.as_millis().div_ceil(1000).max(1) as u64
}

impl From<CodeUiApiError> for WebApiError {
    fn from(value: CodeUiApiError) -> Self {
        Self {
            status: StatusCode::from_u16(value.status).unwrap_or(StatusCode::INTERNAL_SERVER_ERROR),
            code: value.code,
            message: value.message,
            retry_after_secs: None,
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
                retry_after_secs: None,
            };
        }
        Self {
            status: StatusCode::INTERNAL_SERVER_ERROR,
            code: "INTERNAL_ERROR".to_string(),
            message: value.to_string(),
            retry_after_secs: None,
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
        let mut response = (
            self.status,
            Json(serde_json::json!({
                "error": {
                    "code": self.code,
                    "message": self.message,
                }
            })),
        )
            .into_response();
        if let Some(secs) = self.retry_after_secs
            && let Ok(value) = header::HeaderValue::from_str(&secs.to_string())
        {
            response.headers_mut().insert(header::RETRY_AFTER, value);
        }
        response
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
            CodeUiTranscriptEntry, CodeUiTranscriptEntryKind, ReadOnlyCodeUiAdapter,
            initial_snapshot,
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
            CodeUiInitialController::Unclaimed,
        )
        .await
    }

    /// W3-12: `/session` and `/diagnostics` must scrub configured env-file
    /// literals via `WebAppState.secret_redactor`, not only the marker-only
    /// default. Pins the startup wiring that unit-level
    /// `project_json_for_wire` tests cannot see.
    #[tokio::test]
    async fn code_session_and_diagnostics_scrub_configured_env_file_literals() {
        use axum::extract::connect_info::MockConnectInfo;

        let secret = "sk-w312-live-wire-envfile-literal";
        let session = CodeUiSession::new(initial_snapshot(
            "/tmp/libra-w312",
            CodeUiProviderInfo {
                provider: "test".to_string(),
                model: Some("test-model".to_string()),
                mode: None,
                managed: false,
            },
            CodeUiCapabilities::default(),
        ));
        session
            .upsert_transcript_entry(CodeUiTranscriptEntry {
                id: "leak".to_string(),
                kind: CodeUiTranscriptEntryKind::InfoNote,
                title: Some("bootstrap".to_string()),
                content: Some(format!("provider key {secret}")),
                status: None,
                streaming: false,
                metadata: serde_json::json!({}),
                created_at: chrono::Utc::now(),
                updated_at: chrono::Utc::now(),
            })
            .await;
        let runtime = CodeUiRuntimeHandle::build_with_control(
            ReadOnlyCodeUiAdapter::new(session, CodeUiCapabilities::default()),
            false,
            true,
            CodeUiInitialController::Unclaimed,
        )
        .await;
        let redactor = Arc::new(
            SecretRedactor::default_runtime()
                .with_forbidden_env_values([("OPENAI_API_KEY", secret)]),
        );
        let app = code_router()
            .with_state(WebAppState {
                working_dir: Arc::new(PathBuf::from("/tmp/libra-w312")),
                code_ui: Some(runtime),
                automation_control_token: None,
                browser_bootstrap_token: None,
                audit_sink: Arc::new(TracingAuditSink),
                control_trace_id: Uuid::new_v4(),
                bound_addr: std::net::SocketAddr::from(([127, 0, 0, 1], 4317)),
                write_rate_limiter: SessionWriteRateLimiter::from_env_or_default(),
                secret_redactor: redactor,
                workflow_hub: None,
            })
            .layer(MockConnectInfo(SocketAddr::from(([127, 0, 0, 1], 1))));

        for uri in ["/session", "/diagnostics"] {
            let request = Request::builder()
                .method(Method::GET)
                .uri(uri)
                .body(Body::empty())
                .unwrap();
            let response = app.clone().oneshot(request).await.unwrap();
            assert_eq!(
                response.status(),
                StatusCode::OK,
                "{uri} should succeed, got {}",
                response.status()
            );
            let body = to_bytes(response.into_body(), usize::MAX).await.unwrap();
            let text = String::from_utf8_lossy(&body);
            assert!(
                !text.contains(secret),
                "{uri} leaked env-file secret: {text}"
            );
            if uri == "/session" {
                assert!(
                    text.contains("[REDACTED]"),
                    "{uri} should replace the secret with [REDACTED]: {text}"
                );
            }
        }
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
    fn parse_listen_addr_accepts_loopback_aliases() {
        let v4 = parse_listen_addr("127.0.0.1", 4317).expect("ipv4");
        assert_eq!(v4, SocketAddr::from(([127, 0, 0, 1], 4317)));

        let localhost = parse_listen_addr("localhost", 0).expect("localhost");
        assert!(localhost.ip().is_loopback());
        assert_eq!(localhost.port(), 0);

        let bare_v6 = parse_listen_addr("::1", 4318).expect("bare ipv6");
        assert_eq!(bare_v6, SocketAddr::from((Ipv6Addr::LOCALHOST, 4318)));

        let bracket_v6 = parse_listen_addr("[::1]", 4319).expect("bracket ipv6");
        assert_eq!(bracket_v6, SocketAddr::from((Ipv6Addr::LOCALHOST, 4319)));
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

    #[test]
    fn browser_bootstrap_auth_skips_when_unset() {
        let headers = HeaderMap::new();
        assert!(ensure_browser_bootstrap_token(&headers, None).is_ok());
    }

    #[test]
    fn browser_bootstrap_auth_requires_header_when_minted() {
        let headers = HeaderMap::new();
        let expected: Arc<str> = Arc::from("bootstrap-secret");

        let error = ensure_browser_bootstrap_token(&headers, Some(&expected)).unwrap_err();

        assert_eq!(error.status, StatusCode::FORBIDDEN);
        assert_eq!(error.code, "MISSING_BROWSER_BOOTSTRAP");
    }

    #[test]
    fn browser_bootstrap_auth_rejects_mismatched_token() {
        let mut headers = HeaderMap::new();
        headers.insert("x-libra-browser-bootstrap", "wrong".parse().unwrap());
        let expected: Arc<str> = Arc::from("bootstrap-secret");

        let error = ensure_browser_bootstrap_token(&headers, Some(&expected)).unwrap_err();

        assert_eq!(error.status, StatusCode::FORBIDDEN);
        assert_eq!(error.code, "INVALID_BROWSER_BOOTSTRAP");
    }

    #[test]
    fn browser_bootstrap_auth_accepts_matching_token() {
        let mut headers = HeaderMap::new();
        headers.insert(
            "x-libra-browser-bootstrap",
            "bootstrap-secret".parse().unwrap(),
        );
        let expected: Arc<str> = Arc::from("bootstrap-secret");

        assert!(ensure_browser_bootstrap_token(&headers, Some(&expected)).is_ok());
    }

    /// Wave 2 / PR 2 — route-level loopback gate ordering for read
    /// routes. `docs/development/commands/_general.md` §5.3 / §6.3 inline test:
    /// `GET /api/code/session` from a non-loopback `ConnectInfo`
    /// MUST short-circuit with `403 LOOPBACK_REQUIRED` BEFORE the
    /// runtime is touched. This guards the documented loopback ↦
    /// body ↦ token error-code ordering — a regression that hands
    /// remote callers the runtime-unavailable error first would
    /// leak whether the session is up.
    fn thread_graph_test_app(working_dir: PathBuf) -> axum::Router {
        use axum::extract::connect_info::MockConnectInfo;
        code_router()
            .with_state(WebAppState {
                working_dir: Arc::new(working_dir),
                code_ui: None,
                automation_control_token: None,
                browser_bootstrap_token: None,
                audit_sink: Arc::new(TracingAuditSink),
                control_trace_id: Uuid::new_v4(),
                bound_addr: std::net::SocketAddr::from(([127, 0, 0, 1], 4317)),
                write_rate_limiter: SessionWriteRateLimiter::from_env_or_default(),
                secret_redactor: Arc::new(SecretRedactor::default_runtime()),
                workflow_hub: None,
            })
            .layer(MockConnectInfo(SocketAddr::from(([127, 0, 0, 1], 1))))
    }

    async fn thread_graph_error_code(app: axum::Router, uri: &str) -> (StatusCode, String) {
        let request = Request::builder()
            .method(Method::GET)
            .uri(uri)
            .body(Body::empty())
            .unwrap();
        let response = app.oneshot(request).await.unwrap();
        let status = response.status();
        let body = to_bytes(response.into_body(), usize::MAX).await.unwrap();
        let value: serde_json::Value = serde_json::from_slice(&body).unwrap();
        let code = value["error"]["code"]
            .as_str()
            .unwrap_or_default()
            .to_string();
        (status, code)
    }

    #[tokio::test]
    async fn code_thread_graph_route_rejects_invalid_id_and_unresolved_storage() {
        let app = thread_graph_test_app(PathBuf::from("/tmp/libra-thread-graph-missing-repo"));
        let (status, code) =
            thread_graph_error_code(app.clone(), "/thread-graph?threadId=not-a-uuid").await;
        assert_eq!(status, StatusCode::BAD_REQUEST);
        assert_eq!(code, "THREAD_GRAPH_INVALID_ID");

        let (status, code) = thread_graph_error_code(
            app,
            "/thread-graph?threadId=11111111-1111-4111-8111-111111111111",
        )
        .await;
        assert_eq!(status, StatusCode::INTERNAL_SERVER_ERROR);
        assert_eq!(code, "THREAD_GRAPH_STORAGE_UNAVAILABLE");
    }

    #[tokio::test]
    async fn code_thread_graph_route_rejects_non_loopback() {
        use axum::extract::connect_info::MockConnectInfo;
        let app = code_router()
            .with_state(WebAppState {
                working_dir: Arc::new(PathBuf::from("/tmp/libra")),
                code_ui: None,
                automation_control_token: None,
                browser_bootstrap_token: None,
                audit_sink: Arc::new(TracingAuditSink),
                control_trace_id: Uuid::new_v4(),
                bound_addr: std::net::SocketAddr::from(([127, 0, 0, 1], 4317)),
                write_rate_limiter: SessionWriteRateLimiter::from_env_or_default(),
                secret_redactor: Arc::new(SecretRedactor::default_runtime()),
                workflow_hub: None,
            })
            .layer(MockConnectInfo(SocketAddr::from((
                Ipv4Addr::new(192, 0, 2, 10),
                34567,
            ))));
        let (status, code) = thread_graph_error_code(
            app,
            "/thread-graph?threadId=11111111-1111-4111-8111-111111111111",
        )
        .await;
        assert_eq!(status, StatusCode::FORBIDDEN);
        assert_eq!(code, "LOOPBACK_REQUIRED");
    }

    #[tokio::test]
    #[serial_test::serial]
    async fn code_thread_graph_route_returns_not_found_for_missing_thread() {
        let temp = tempfile::tempdir().expect("temp repo");
        crate::utils::test::setup_with_new_libra_in(temp.path()).await;
        let app = thread_graph_test_app(temp.path().to_path_buf());
        let (status, code) = thread_graph_error_code(
            app,
            "/thread-graph?threadId=11111111-1111-4111-8111-111111111111",
        )
        .await;
        assert_eq!(status, StatusCode::NOT_FOUND);
        assert_eq!(code, "THREAD_GRAPH_NOT_FOUND");
    }

    #[tokio::test]
    #[serial_test::serial]
    async fn code_thread_graph_route_returns_redacted_indexed_graph() {
        use axum::extract::connect_info::MockConnectInfo;
        use chrono::{TimeZone, Utc};
        use git_internal::internal::object::types::ActorRef;

        use crate::{
            internal::{
                ai::projection::{
                    PlanHeadRef, SchedulerState, SchedulerStateRepository, ThreadIntentLinkReason,
                    ThreadIntentRef, ThreadParticipant, ThreadParticipantRole, ThreadProjection,
                },
                db::establish_connection,
            },
            utils::util::DATABASE,
        };

        let secret = "sk-w404-thread-graph-envfile-literal";
        let temp = tempfile::tempdir().expect("temp repo");
        crate::utils::test::setup_with_new_libra_in(temp.path()).await;
        let db_path = temp.path().join(".libra").join(DATABASE);
        let db = establish_connection(db_path.to_str().expect("utf-8 db path"))
            .await
            .expect("open test db");
        let thread_id = Uuid::parse_str("11111111-1111-4111-8111-111111111111").expect("uuid");
        let intent_id = Uuid::parse_str("22222222-2222-4222-8222-222222222222").expect("uuid");
        let plan_id = Uuid::parse_str("33333333-3333-4333-8333-333333333333").expect("uuid");
        let owner = ActorRef::human("thread-graph-route").expect("actor");
        let now = Utc.timestamp_opt(1_700_000_000, 0).single().expect("ts");
        ThreadProjection {
            thread_id,
            title: Some(format!("Planner thread {secret}")),
            owner: owner.clone(),
            participants: vec![ThreadParticipant {
                actor: owner,
                role: ThreadParticipantRole::Owner,
                joined_at: now,
            }],
            current_intent_id: Some(intent_id),
            latest_intent_id: Some(intent_id),
            intents: vec![ThreadIntentRef {
                intent_id,
                ordinal: 0,
                is_head: true,
                linked_at: now,
                link_reason: ThreadIntentLinkReason::Seed,
            }],
            metadata: None,
            archived: false,
            created_at: now,
            updated_at: now,
            version: 1,
        }
        .create(&db)
        .await
        .expect("create thread projection");
        SchedulerStateRepository::new(db)
            .insert_initial(&SchedulerState {
                thread_id,
                selected_plan_id: Some(plan_id),
                selected_plan_ids: vec![PlanHeadRef {
                    plan_id,
                    ordinal: 0,
                }],
                current_plan_heads: vec![PlanHeadRef {
                    plan_id,
                    ordinal: 0,
                }],
                active_task_id: None,
                active_run_id: None,
                live_context_window: Vec::new(),
                metadata: None,
                updated_at: now,
                version: 1,
            })
            .await
            .expect("insert scheduler state");

        let redactor = Arc::new(
            SecretRedactor::default_runtime()
                .with_forbidden_env_values([("OPENAI_API_KEY", secret)]),
        );
        let app = code_router()
            .with_state(WebAppState {
                working_dir: Arc::new(temp.path().to_path_buf()),
                code_ui: None,
                automation_control_token: None,
                browser_bootstrap_token: None,
                audit_sink: Arc::new(TracingAuditSink),
                control_trace_id: Uuid::new_v4(),
                bound_addr: std::net::SocketAddr::from(([127, 0, 0, 1], 4317)),
                write_rate_limiter: SessionWriteRateLimiter::from_env_or_default(),
                secret_redactor: redactor,
                workflow_hub: None,
            })
            .layer(MockConnectInfo(SocketAddr::from(([127, 0, 0, 1], 1))));
        let request = Request::builder()
            .method(Method::GET)
            .uri(format!("/thread-graph?threadId={thread_id}"))
            .body(Body::empty())
            .unwrap();
        let response = app.oneshot(request).await.unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let body = to_bytes(response.into_body(), usize::MAX).await.unwrap();
        let text = String::from_utf8_lossy(&body);
        assert!(
            !text.contains(secret),
            "thread-graph leaked env-file secret: {text}"
        );
        assert!(
            text.contains("[REDACTED]"),
            "thread-graph should redact the title secret: {text}"
        );
        let value: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(value["threadId"], thread_id.to_string());
        assert!(
            value["nodes"]
                .as_array()
                .is_some_and(|nodes| nodes.iter().any(|node| node["kind"] == "intent")),
            "indexed graph must include the intent node: {value}"
        );
    }

    #[tokio::test]
    async fn code_session_route_rejects_non_loopback_with_loopback_required() {
        use axum::extract::connect_info::MockConnectInfo;
        let app = code_router()
            .with_state(WebAppState {
                working_dir: Arc::new(PathBuf::from("/tmp/libra")),
                code_ui: None,
                automation_control_token: None,
                browser_bootstrap_token: None,
                audit_sink: Arc::new(TracingAuditSink),
                control_trace_id: Uuid::new_v4(),
                bound_addr: std::net::SocketAddr::from(([127, 0, 0, 1], 4317)),
                write_rate_limiter: SessionWriteRateLimiter::from_env_or_default(),
                secret_redactor: Arc::new(SecretRedactor::default_runtime()),
                workflow_hub: None,
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
                browser_bootstrap_token: None,
                audit_sink: Arc::new(TracingAuditSink),
                control_trace_id: Uuid::new_v4(),
                bound_addr: std::net::SocketAddr::from(([127, 0, 0, 1], 4317)),
                write_rate_limiter: SessionWriteRateLimiter::from_env_or_default(),
                secret_redactor: Arc::new(SecretRedactor::default_runtime()),
                workflow_hub: None,
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
                browser_bootstrap_token: None,
                audit_sink: Arc::new(TracingAuditSink),
                control_trace_id: Uuid::new_v4(),
                bound_addr: std::net::SocketAddr::from(([127, 0, 0, 1], 4317)),
                write_rate_limiter: SessionWriteRateLimiter::from_env_or_default(),
                secret_redactor: Arc::new(SecretRedactor::default_runtime()),
                workflow_hub: None,
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
    async fn browser_attach_requires_bootstrap_when_minted() {
        use axum::extract::connect_info::MockConnectInfo;

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
        let runtime = CodeUiRuntimeHandle::build(
            ReadOnlyCodeUiAdapter::new(session, CodeUiCapabilities::default()),
            true,
            CodeUiInitialController::Unclaimed,
        )
        .await;
        let bootstrap: Arc<str> = Arc::from("session-bootstrap-secret");
        let app = code_router()
            .with_state(WebAppState {
                working_dir: Arc::new(PathBuf::from("/tmp/libra")),
                code_ui: Some(runtime),
                automation_control_token: None,
                browser_bootstrap_token: Some(bootstrap.clone()),
                audit_sink: Arc::new(TracingAuditSink),
                control_trace_id: Uuid::new_v4(),
                bound_addr: std::net::SocketAddr::from(([127, 0, 0, 1], 4317)),
                write_rate_limiter: SessionWriteRateLimiter::from_env_or_default(),
                secret_redactor: Arc::new(SecretRedactor::default_runtime()),
                workflow_hub: None,
            })
            .layer(MockConnectInfo(SocketAddr::from(([127, 0, 0, 1], 1))));

        let missing = Request::builder()
            .method(Method::POST)
            .uri("/controller/attach")
            .header(header::CONTENT_TYPE, "application/json")
            .header(header::ORIGIN, "http://127.0.0.1:4317")
            .body(Body::from(
                r#"{"clientId":"browser-missing","kind":"browser"}"#,
            ))
            .unwrap();
        let response = app.clone().oneshot(missing).await.unwrap();
        assert_eq!(response.status(), StatusCode::FORBIDDEN);
        let body = to_bytes(response.into_body(), usize::MAX).await.unwrap();
        let value: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(value["error"]["code"], "MISSING_BROWSER_BOOTSTRAP");

        let wrong = Request::builder()
            .method(Method::POST)
            .uri("/controller/attach")
            .header(header::CONTENT_TYPE, "application/json")
            .header(header::ORIGIN, "http://127.0.0.1:4317")
            .header("X-Libra-Browser-Bootstrap", "wrong-secret")
            .body(Body::from(
                r#"{"clientId":"browser-wrong","kind":"browser"}"#,
            ))
            .unwrap();
        let response = app.clone().oneshot(wrong).await.unwrap();
        assert_eq!(response.status(), StatusCode::FORBIDDEN);
        let body = to_bytes(response.into_body(), usize::MAX).await.unwrap();
        let value: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(value["error"]["code"], "INVALID_BROWSER_BOOTSTRAP");

        let ok = Request::builder()
            .method(Method::POST)
            .uri("/controller/attach")
            .header(header::CONTENT_TYPE, "application/json")
            .header(header::ORIGIN, "http://127.0.0.1:4317")
            .header("X-Libra-Browser-Bootstrap", bootstrap.as_ref())
            .body(Body::from(r#"{"clientId":"browser-ok","kind":"browser"}"#))
            .unwrap();
        let response = app.oneshot(ok).await.unwrap();
        assert_eq!(
            response.status(),
            StatusCode::OK,
            "matching bootstrap must allow browser attach"
        );
    }

    #[tokio::test]
    async fn code_controller_detach_route_rejects_non_loopback_before_body_parse() {
        use axum::extract::connect_info::MockConnectInfo;
        let app = code_router()
            .with_state(WebAppState {
                working_dir: Arc::new(PathBuf::from("/tmp/libra")),
                code_ui: None,
                automation_control_token: None,
                browser_bootstrap_token: None,
                audit_sink: Arc::new(TracingAuditSink),
                control_trace_id: Uuid::new_v4(),
                bound_addr: std::net::SocketAddr::from(([127, 0, 0, 1], 4317)),
                write_rate_limiter: SessionWriteRateLimiter::from_env_or_default(),
                secret_redactor: Arc::new(SecretRedactor::default_runtime()),
                workflow_hub: None,
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
            browser_bootstrap_token: None,
            audit_sink: Arc::new(TracingAuditSink),
            control_trace_id: Uuid::new_v4(),
            bound_addr: std::net::SocketAddr::from(([127, 0, 0, 1], 4317)),
            write_rate_limiter: SessionWriteRateLimiter::from_env_or_default(),
            secret_redactor: Arc::new(SecretRedactor::default_runtime()),
            workflow_hub: None,
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
    async fn code_messages_route_forwards_command_id_and_rejects_invalid_ids() {
        use std::sync::Mutex;

        use axum::extract::connect_info::MockConnectInfo;

        type SubmittedCommandIds = Vec<(String, Option<String>)>;

        #[derive(Clone)]
        struct CommandIdAdapter {
            session: Arc<CodeUiSession>,
            submitted: Arc<Mutex<SubmittedCommandIds>>,
        }

        #[async_trait::async_trait]
        impl crate::internal::ai::web::code_ui::CodeUiReadModel for CommandIdAdapter {
            fn session(&self) -> Arc<CodeUiSession> {
                self.session.clone()
            }
        }

        #[async_trait::async_trait]
        impl crate::internal::ai::web::code_ui::CodeUiCommandAdapter for CommandIdAdapter {
            fn capabilities(&self) -> CodeUiCapabilities {
                CodeUiCapabilities {
                    message_input: true,
                    command_idempotency: true,
                    ..CodeUiCapabilities::default()
                }
            }

            async fn submit_message(&self, text: String) -> anyhow::Result<()> {
                self.submitted.lock().unwrap().push((text, None));
                Ok(())
            }

            async fn submit_message_with_command_id(
                &self,
                text: String,
                command_id: Option<String>,
            ) -> anyhow::Result<()> {
                self.submitted.lock().unwrap().push((text, command_id));
                Ok(())
            }

            async fn respond_interaction(
                &self,
                _interaction_id: &str,
                _response: crate::internal::ai::web::code_ui::CodeUiInteractionResponse,
            ) -> anyhow::Result<()> {
                Ok(())
            }
        }

        let session = CodeUiSession::new(initial_snapshot(
            "/tmp/libra",
            CodeUiProviderInfo {
                provider: "test".to_string(),
                model: Some("test-model".to_string()),
                mode: None,
                managed: false,
            },
            CodeUiCapabilities {
                message_input: true,
                command_idempotency: true,
                ..CodeUiCapabilities::default()
            },
        ));
        let adapter = Arc::new(CommandIdAdapter {
            session: session.clone(),
            submitted: Arc::new(Mutex::new(Vec::new())),
        });
        let runtime =
            CodeUiRuntimeHandle::build(adapter.clone(), true, CodeUiInitialController::Unclaimed)
                .await;
        let attach = runtime
            .attach_browser_controller("browser-a")
            .await
            .expect("browser controller should attach");

        let app = code_router()
            .with_state(WebAppState {
                working_dir: Arc::new(PathBuf::from("/tmp/libra")),
                code_ui: Some(runtime.clone()),
                automation_control_token: None,
                browser_bootstrap_token: None,
                audit_sink: Arc::new(TracingAuditSink),
                control_trace_id: Uuid::new_v4(),
                bound_addr: std::net::SocketAddr::from(([127, 0, 0, 1], 4317)),
                write_rate_limiter: SessionWriteRateLimiter::from_env_or_default(),
                secret_redactor: Arc::new(SecretRedactor::default_runtime()),
                workflow_hub: None,
            })
            .layer(MockConnectInfo(SocketAddr::from(([127, 0, 0, 1], 1))));

        let body = r#"{"text":"hello","commandId":"cmd-route-1"}"#;
        let request = Request::builder()
            .method(Method::POST)
            .uri("/messages")
            .header(header::CONTENT_TYPE, "application/json")
            .header(header::ORIGIN, "http://127.0.0.1:4317")
            .header("X-Code-Controller-Token", attach.controller_token.clone())
            .body(Body::from(body))
            .unwrap();
        let response = app.clone().oneshot(request).await.unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let submitted = adapter.submitted.lock().unwrap().clone();
        assert_eq!(submitted.len(), 1);
        assert_eq!(submitted[0].0, "hello");
        let composed = submitted[0]
            .1
            .as_deref()
            .expect("route must forward a composed commandId");
        assert!(
            composed.contains(":cmd-route-1"),
            "composed commandId should preserve the caller id, got {composed}"
        );
        assert_eq!(
            composed.len(),
            64 + 1 + "cmd-route-1".len(),
            "composed commandId should be sha256(client)+':'+caller id, got {composed}"
        );

        let invalid = Request::builder()
            .method(Method::POST)
            .uri("/messages")
            .header(header::CONTENT_TYPE, "application/json")
            .header(header::ORIGIN, "http://127.0.0.1:4317")
            .header("X-Code-Controller-Token", attach.controller_token)
            .body(Body::from(r#"{"text":"hello","commandId":" padded "}"#))
            .unwrap();
        let response = app.oneshot(invalid).await.unwrap();
        assert_eq!(response.status(), StatusCode::BAD_REQUEST);
        let body = to_bytes(response.into_body(), usize::MAX).await.unwrap();
        let value: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(value["error"]["code"], "INVALID_COMMAND_ID");
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
            CodeUiInitialController::Unclaimed,
        )
        .await;
        let audit_sink = Arc::new(InMemoryAuditSink::default());
        let app = code_router().with_state(WebAppState {
            working_dir: Arc::new(PathBuf::from("/tmp/libra")),
            code_ui: Some(runtime),
            automation_control_token: Some(Arc::from("control-token-secret")),
            browser_bootstrap_token: None,
            audit_sink: audit_sink.clone(),
            control_trace_id: Uuid::new_v4(),
            bound_addr: std::net::SocketAddr::from(([127, 0, 0, 1], 4317)),
            write_rate_limiter: SessionWriteRateLimiter::from_env_or_default(),
            secret_redactor: Arc::new(SecretRedactor::default_runtime()),
            workflow_hub: None,
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

    #[test]
    fn sanitized_audit_client_id_scrubs_configured_env_file_literals() {
        let redactor = SecretRedactor::default_runtime()
            .with_forbidden_env_values([("OPENAI_API_KEY", "sk-audit-envfile-literal")]);
        let sanitized =
            sanitized_audit_client_id(&redactor, "browser-sk-audit-envfile-literal-client");
        assert!(
            !sanitized.contains("sk-audit-envfile-literal"),
            "audit client id must use the configured env-file redactor: {sanitized}"
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

    /// W3-01: skill activate / session resume live on the write router
    /// (256 KiB body cap) and must reject oversized bodies before the
    /// handler runs.
    #[tokio::test]
    async fn code_skill_activate_and_resume_routes_enforce_write_body_limit() {
        let app = code_write_router().with_state(WebAppState {
            working_dir: Arc::new(PathBuf::from("/tmp/libra")),
            code_ui: None,
            automation_control_token: None,
            browser_bootstrap_token: None,
            audit_sink: Arc::new(TracingAuditSink),
            control_trace_id: Uuid::new_v4(),
            bound_addr: std::net::SocketAddr::from(([127, 0, 0, 1], 4317)),
            write_rate_limiter: SessionWriteRateLimiter::from_env_or_default(),
            secret_redactor: Arc::new(SecretRedactor::default_runtime()),
            workflow_hub: None,
        });
        let oversized = "x".repeat(CODE_CONTROL_BODY_LIMIT_BYTES + 1);
        for uri in ["/skills/activate", "/session/resume"] {
            let body = format!(
                r#"{{"threadId":"{oversized}","provider":"claude-code","name":"/review"}}"#
            );
            let request = Request::builder()
                .method(Method::POST)
                .uri(uri)
                .header(header::CONTENT_TYPE, "application/json")
                .header(header::CONTENT_LENGTH, body.len().to_string())
                .body(Body::from(body))
                .unwrap();
            let response = app.clone().oneshot(request).await.unwrap();
            assert_eq!(
                response.status(),
                StatusCode::PAYLOAD_TOO_LARGE,
                "{uri} must honor the write body limit"
            );
            let body = to_bytes(response.into_body(), usize::MAX).await.unwrap();
            let value: serde_json::Value = serde_json::from_slice(&body).unwrap();
            assert_eq!(value["error"]["code"], "PAYLOAD_TOO_LARGE");
        }
    }

    /// W3-01: automation leases on resume / skill-activate require the
    /// secondary control token, matching other write handlers.
    #[tokio::test]
    async fn code_session_resume_requires_automation_control_token() {
        use axum::extract::connect_info::MockConnectInfo;

        // The resume handler resolves the repository storage root before
        // looking up the thread, so point the fixture at a minimal but
        // resolvable repository: a missing thread must reach the NotFound
        // branch rather than fail closed on storage-root resolution.
        let temp = tempfile::tempdir().unwrap();
        let repo = temp.path().join("repo");
        std::fs::create_dir_all(repo.join(".libra")).unwrap();
        std::fs::write(repo.join(".libra").join("libra.db"), b"").unwrap();
        let working_dir = repo.to_string_lossy().to_string();

        let session = CodeUiSession::new(initial_snapshot(
            working_dir.clone(),
            CodeUiProviderInfo {
                provider: "test".to_string(),
                model: Some("test-model".to_string()),
                mode: None,
                managed: false,
            },
            CodeUiCapabilities {
                provider_session_resume: true,
                ..CodeUiCapabilities::default()
            },
        ));
        let runtime = CodeUiRuntimeHandle::build_with_control(
            ReadOnlyCodeUiAdapter::new(
                session,
                CodeUiCapabilities {
                    provider_session_resume: true,
                    ..CodeUiCapabilities::default()
                },
            ),
            false,
            true,
            CodeUiInitialController::Unclaimed,
        )
        .await;
        let attach = runtime
            .attach_controller(CodeUiControllerKind::Automation, "automation-a")
            .await
            .expect("automation controller should attach");

        let app = code_router()
            .with_state(WebAppState {
                working_dir: Arc::new(PathBuf::from(&working_dir)),
                code_ui: Some(runtime),
                automation_control_token: Some(Arc::from("control-token-secret")),
                browser_bootstrap_token: None,
                audit_sink: Arc::new(TracingAuditSink),
                control_trace_id: Uuid::new_v4(),
                bound_addr: std::net::SocketAddr::from(([127, 0, 0, 1], 4317)),
                write_rate_limiter: SessionWriteRateLimiter::from_env_or_default(),
                secret_redactor: Arc::new(SecretRedactor::default_runtime()),
                workflow_hub: None,
            })
            .layer(MockConnectInfo(SocketAddr::from(([127, 0, 0, 1], 1))));

        let missing = Request::builder()
            .method(Method::POST)
            .uri("/session/resume")
            .header(header::CONTENT_TYPE, "application/json")
            .header(header::ORIGIN, "http://127.0.0.1:4317")
            .header("X-Code-Controller-Token", attach.controller_token.clone())
            .body(Body::from(r#"{"threadId":"thread-missing"}"#))
            .unwrap();
        let response = app.clone().oneshot(missing).await.unwrap();
        assert_eq!(response.status(), StatusCode::FORBIDDEN);
        let body = to_bytes(response.into_body(), usize::MAX).await.unwrap();
        let value: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(value["error"]["code"], "MISSING_CONTROL_TOKEN");

        let with_token = Request::builder()
            .method(Method::POST)
            .uri("/session/resume")
            .header(header::CONTENT_TYPE, "application/json")
            .header(header::ORIGIN, "http://127.0.0.1:4317")
            .header("X-Code-Controller-Token", attach.controller_token)
            .header("X-Libra-Control-Token", "control-token-secret")
            .body(Body::from(r#"{"threadId":"thread-missing"}"#))
            .unwrap();
        let response = app.oneshot(with_token).await.unwrap();
        // After the control token check, resume either cannot find the
        // thread or fail-closes on in-process swap — never a silent 200.
        assert_ne!(response.status(), StatusCode::OK);
        let body = to_bytes(response.into_body(), usize::MAX).await.unwrap();
        let value: serde_json::Value = serde_json::from_slice(&body).unwrap();
        let code = value["error"]["code"].as_str().unwrap_or_default();
        assert!(
            code == "SESSION_RESUME_NOT_FOUND" || code == "SESSION_RESUME_REQUIRES_RESTART",
            "expected resume failure after auth, got {value}"
        );
    }

    #[tokio::test]
    async fn code_skills_rejects_unknown_provider_query() {
        use axum::extract::connect_info::MockConnectInfo;

        let app = code_router()
            .with_state(WebAppState {
                working_dir: Arc::new(PathBuf::from("/tmp/libra")),
                code_ui: None,
                automation_control_token: None,
                browser_bootstrap_token: None,
                audit_sink: Arc::new(TracingAuditSink),
                control_trace_id: Uuid::new_v4(),
                bound_addr: std::net::SocketAddr::from(([127, 0, 0, 1], 4317)),
                write_rate_limiter: SessionWriteRateLimiter::from_env_or_default(),
                secret_redactor: Arc::new(SecretRedactor::default_runtime()),
                workflow_hub: None,
            })
            .layer(MockConnectInfo(SocketAddr::from(([127, 0, 0, 1], 1))));

        let request = Request::builder()
            .method(Method::GET)
            .uri("/skills?provider=not-an-agent")
            .body(Body::empty())
            .unwrap();
        let response = app.oneshot(request).await.unwrap();
        assert_eq!(response.status(), StatusCode::BAD_REQUEST);
        let body = to_bytes(response.into_body(), usize::MAX).await.unwrap();
        let value: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(value["error"]["code"], "INVALID_SKILL_PROVIDER");
    }

    #[tokio::test]
    async fn code_skill_activate_rejects_discoverable_skill_without_activation_effect() {
        use axum::extract::connect_info::MockConnectInfo;

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
        let runtime = CodeUiRuntimeHandle::build(
            ReadOnlyCodeUiAdapter::new(session, CodeUiCapabilities::default()),
            true,
            CodeUiInitialController::Unclaimed,
        )
        .await;
        let attach = runtime
            .attach_browser_controller("browser-skill")
            .await
            .expect("browser controller should attach");
        let audit_sink = Arc::new(InMemoryAuditSink::default());
        let app = code_router()
            .with_state(WebAppState {
                working_dir: Arc::new(PathBuf::from("/tmp/libra")),
                code_ui: Some(runtime),
                automation_control_token: None,
                browser_bootstrap_token: None,
                audit_sink: audit_sink.clone(),
                control_trace_id: Uuid::new_v4(),
                bound_addr: std::net::SocketAddr::from(([127, 0, 0, 1], 4317)),
                write_rate_limiter: SessionWriteRateLimiter::from_env_or_default(),
                secret_redactor: Arc::new(SecretRedactor::default_runtime()),
                workflow_hub: None,
            })
            .layer(MockConnectInfo(SocketAddr::from(([127, 0, 0, 1], 1))));

        let request = Request::builder()
            .method(Method::POST)
            .uri("/skills/activate")
            .header(header::CONTENT_TYPE, "application/json")
            .header(header::ORIGIN, "http://127.0.0.1:4317")
            .header("X-Code-Controller-Token", attach.controller_token)
            .body(Body::from(r#"{"provider":"claude-code","name":"/review"}"#))
            .unwrap();
        let response = app.oneshot(request).await.unwrap();
        assert_eq!(response.status(), StatusCode::UNPROCESSABLE_ENTITY);
        let body = to_bytes(response.into_body(), usize::MAX).await.unwrap();
        let value: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(value["error"]["code"], "SKILL_ACTIVATION_UNSUPPORTED");

        let events = audit_sink.events().await;
        assert!(
            events.iter().any(|event| event.action == "skills.activate"),
            "leased skill activate must append audit"
        );
    }

    #[test]
    fn usage_thread_filter_ignores_synthetic_session_mirrored_ids() {
        let session_id = Some("session-not-a-uuid".to_string());
        let mirrored = Some("session-not-a-uuid".to_string());
        let canonical = Some("canonical-thread-from-session-meta".to_string());

        let ignore_mirrored = mirrored.clone().and_then(|tid| {
            let trimmed = tid.trim();
            if trimmed.is_empty() {
                return None;
            }
            if session_id
                .as_deref()
                .is_some_and(|session| session == trimmed)
            {
                return None;
            }
            Some(trimmed.to_string())
        });
        assert_eq!(ignore_mirrored, None);

        let keep_canonical = canonical.clone().and_then(|tid| {
            let trimmed = tid.trim();
            if trimmed.is_empty() {
                return None;
            }
            if session_id
                .as_deref()
                .is_some_and(|session| session == trimmed)
            {
                return None;
            }
            Some(trimmed.to_string())
        });
        assert_eq!(
            keep_canonical.as_deref(),
            Some("canonical-thread-from-session-meta")
        );
    }

    #[tokio::test]
    async fn code_usage_returns_persisted_totals_for_session_filter() {
        use axum::extract::connect_info::MockConnectInfo;

        use crate::{
            internal::{
                ai::{
                    completion::CompletionUsageSummary,
                    usage::{UsageContext, UsageRecorder},
                },
                db::establish_connection,
            },
            utils::test::setup_with_new_libra_in,
        };

        let tmp = tempfile::tempdir().expect("tempdir");
        setup_with_new_libra_in(tmp.path()).await;
        let storage_root =
            resolve_storage_root(tmp.path()).expect("initialized temp repo has a storage root");
        let db_path = storage_root.join("libra.db");
        let db = establish_connection(db_path.to_str().expect("utf-8 db path"))
            .await
            .expect("open libra.db");
        let recorder = UsageRecorder::new(db);
        recorder
            .record_summary(
                &UsageContext {
                    repo_id: Some("repo-usage-route".to_string()),
                    session_id: Some("session-usage-route".to_string()),
                    thread_id: Some("thread-usage-route".to_string()),
                    agent_run_id: None,
                    run_id: Some("run-usage-route".to_string()),
                    turn_id: Some("turn-usage-route".to_string()),
                    event_id: None,
                    provider: "test".to_string(),
                    model: "test-model".to_string(),
                    request_kind: "completion".to_string(),
                    intent: None,
                    agent_name: None,
                },
                &CompletionUsageSummary {
                    input_tokens: 11,
                    output_tokens: 7,
                    cached_tokens: None,
                    reasoning_tokens: None,
                    total_tokens: Some(18),
                    cost_usd: Some(0.02),
                },
                Some(100),
            )
            .await
            .expect("persist usage row");

        let session = CodeUiSession::new(initial_snapshot(
            tmp.path().to_string_lossy(),
            CodeUiProviderInfo {
                provider: "test".to_string(),
                model: Some("test-model".to_string()),
                mode: None,
                managed: false,
            },
            CodeUiCapabilities::default(),
        ));
        session
            .replace_snapshot(code_ui::CodeUiEventType::SessionUpdated, {
                let mut snapshot = session.snapshot().await;
                snapshot.session_id = "session-usage-route".to_string();
                snapshot.thread_id = Some("thread-usage-route".to_string());
                snapshot
            })
            .await;
        let runtime = CodeUiRuntimeHandle::build(
            ReadOnlyCodeUiAdapter::new(session, CodeUiCapabilities::default()),
            true,
            CodeUiInitialController::Unclaimed,
        )
        .await;

        let build_app = |runtime: Arc<CodeUiRuntimeHandle>| {
            code_router()
                .with_state(WebAppState {
                    working_dir: Arc::new(tmp.path().to_path_buf()),
                    code_ui: Some(runtime),
                    automation_control_token: None,
                    browser_bootstrap_token: None,
                    audit_sink: Arc::new(TracingAuditSink),
                    control_trace_id: Uuid::new_v4(),
                    bound_addr: std::net::SocketAddr::from(([127, 0, 0, 1], 4317)),
                    write_rate_limiter: SessionWriteRateLimiter::from_env_or_default(),
                    secret_redactor: Arc::new(SecretRedactor::default_runtime()),
                    workflow_hub: None,
                })
                .layer(MockConnectInfo(SocketAddr::from(([127, 0, 0, 1], 1))))
        };

        let request = Request::builder()
            .method(Method::GET)
            .uri("/usage?sessionId=session-usage-route&threadId=thread-usage-route")
            .body(Body::empty())
            .unwrap();
        let response = build_app(runtime.clone()).oneshot(request).await.unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let body = to_bytes(response.into_body(), usize::MAX).await.unwrap();
        let value: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(value["sessionId"], "session-usage-route");
        assert_eq!(value["cumulative"]["requestCount"], 1);
        assert_eq!(value["cumulative"]["totalTokens"], 18);
        assert!(value.get("subAgents").is_none());
        assert_eq!(value["subAgentsStatus"], "unavailable");

        // Prefer thread scope when both IDs are present: a stale/projected
        // sessionId must not zero out totals that match the durable thread.
        let mismatched = Request::builder()
            .method(Method::GET)
            .uri("/usage?sessionId=not-the-durable-session&threadId=thread-usage-route")
            .body(Body::empty())
            .unwrap();
        let mismatched_response = build_app(runtime).oneshot(mismatched).await.unwrap();
        assert_eq!(mismatched_response.status(), StatusCode::OK);
        let mismatched_body = to_bytes(mismatched_response.into_body(), usize::MAX)
            .await
            .unwrap();
        let mismatched_value: serde_json::Value = serde_json::from_slice(&mismatched_body).unwrap();
        assert_eq!(mismatched_value["sessionId"], "not-the-durable-session");
        assert_eq!(mismatched_value["cumulative"]["requestCount"], 1);
        assert_eq!(mismatched_value["cumulative"]["totalTokens"], 18);
    }

    /// W3-06: `/events?wire=2` streams durable workflow envelopes; illegal wire
    /// and missing hub fail closed; cursor reconnect skips already-seen rows.
    #[tokio::test]
    async fn code_events_wire_v2_http_contract() {
        use axum::extract::connect_info::MockConnectInfo;
        use tempfile::tempdir;

        use crate::internal::ai::{
            session::{CodeWorkflowEventKind, SessionJsonlStore},
            web::sse_wire::CodeUiWorkflowHub,
        };

        let runtime = test_code_ui_runtime().await;
        let dir = tempdir().expect("tempdir");
        let mut store = SessionJsonlStore::new(dir.path().to_path_buf());
        let hub = Arc::new(CodeUiWorkflowHub::attach(&mut store).expect("attach workflow hub"));
        let secret = "sk-w306-wire-v2-secret-literal";
        for (idx, projection) in ["status", "transcript_upsert"].into_iter().enumerate() {
            store
                .append_code_workflow(CodeWorkflowEventKind::CodeUiProjectionDelta {
                    projection: projection.to_string(),
                    summary: format!("delta-{idx}"),
                    payload: serde_json::json!({
                        "n": idx,
                        "note": format!("provider key {secret}"),
                    }),
                })
                .expect("append");
        }
        let redactor = Arc::new(
            SecretRedactor::default_runtime()
                .with_forbidden_env_values([("OPENAI_API_KEY", secret)]),
        );

        let app = code_router()
            .with_state(WebAppState {
                working_dir: Arc::new(PathBuf::from("/tmp/libra")),
                code_ui: Some(runtime.clone()),
                automation_control_token: None,
                browser_bootstrap_token: None,
                audit_sink: Arc::new(TracingAuditSink),
                control_trace_id: Uuid::new_v4(),
                bound_addr: SocketAddr::from(([127, 0, 0, 1], 4318)),
                write_rate_limiter: SessionWriteRateLimiter::from_env_or_default(),
                secret_redactor: redactor,
                workflow_hub: Some(hub.clone()),
            })
            .layer(MockConnectInfo(SocketAddr::from(([127, 0, 0, 1], 1))));

        let illegal = Request::builder()
            .method(Method::GET)
            .uri("/events?wire=9")
            .body(Body::empty())
            .unwrap();
        let illegal_response = app.clone().oneshot(illegal).await.unwrap();
        assert_eq!(illegal_response.status(), StatusCode::BAD_REQUEST);
        let illegal_body = to_bytes(illegal_response.into_body(), usize::MAX)
            .await
            .unwrap();
        let illegal_value: serde_json::Value = serde_json::from_slice(&illegal_body).unwrap();
        assert_eq!(illegal_value["error"]["code"], "INVALID_WIRE_VERSION");

        // v1 must ignore stray/non-numeric cursor query params (v2-only field).
        let v1_stray_cursor = Request::builder()
            .method(Method::GET)
            .uri("/events?wire=1&cursor=not-a-number")
            .body(Body::empty())
            .unwrap();
        let v1_response = app.clone().oneshot(v1_stray_cursor).await.unwrap();
        assert_eq!(
            v1_response.status(),
            StatusCode::OK,
            "v1 must ignore invalid cursor query values"
        );

        let no_hub = code_router()
            .with_state(WebAppState {
                working_dir: Arc::new(PathBuf::from("/tmp/libra")),
                code_ui: Some(runtime.clone()),
                automation_control_token: None,
                browser_bootstrap_token: None,
                audit_sink: Arc::new(TracingAuditSink),
                control_trace_id: Uuid::new_v4(),
                bound_addr: SocketAddr::from(([127, 0, 0, 1], 4318)),
                write_rate_limiter: SessionWriteRateLimiter::from_env_or_default(),
                secret_redactor: Arc::new(SecretRedactor::default_runtime()),
                workflow_hub: None,
            })
            .layer(MockConnectInfo(SocketAddr::from(([127, 0, 0, 1], 1))));
        let missing = Request::builder()
            .method(Method::GET)
            .uri("/events?wire=2")
            .body(Body::empty())
            .unwrap();
        let missing_response = no_hub.oneshot(missing).await.unwrap();
        assert_eq!(missing_response.status(), StatusCode::SERVICE_UNAVAILABLE);
        let missing_body = to_bytes(missing_response.into_body(), usize::MAX)
            .await
            .unwrap();
        let missing_value: serde_json::Value = serde_json::from_slice(&missing_body).unwrap();
        assert_eq!(
            missing_value["error"]["code"],
            "WIRE_V2_REQUIRES_DURABLE_SESSION"
        );

        let request = Request::builder()
            .method(Method::GET)
            .uri("/events?wire=2")
            .body(Body::empty())
            .unwrap();
        let response = app.clone().oneshot(request).await.unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        // The SSE response stays open for live updates; take frames until the
        // durable replay prefix is observed (or the budget expires).
        let body = {
            use futures_util::StreamExt;
            let mut stream = response.into_body().into_data_stream();
            let mut collected = Vec::new();
            let deadline = tokio::time::Instant::now() + Duration::from_secs(2);
            while tokio::time::Instant::now() < deadline {
                match timeout(Duration::from_millis(200), stream.next()).await {
                    Ok(Some(Ok(chunk))) => {
                        collected.extend_from_slice(&chunk);
                        let text = String::from_utf8_lossy(&collected);
                        if text.contains("\"cursor\":2") && text.contains("id: 2") {
                            break;
                        }
                    }
                    Ok(Some(Err(error))) => panic!("SSE body error: {error}"),
                    Ok(None) => break,
                    Err(_) => continue,
                }
            }
            String::from_utf8(collected).expect("utf8 SSE body")
        };
        assert!(
            body.contains("event: code_workflow"),
            "v2 stream must use code_workflow events: {body}"
        );
        assert!(
            body.contains("\"cursor\":1") && body.contains("\"cursor\":2"),
            "v2 connect must replay durable cursors: {body}"
        );
        assert!(
            body.contains("id: 1") && body.contains("id: 2"),
            "v2 SSE id must mirror durable cursor: {body}"
        );
        assert!(
            !body.contains(secret),
            "v2 durable replay must redact forbidden secrets: {body}"
        );
        assert!(
            body.contains("[REDACTED]"),
            "v2 durable replay must retain the redaction marker: {body}"
        );

        let reconnect = Request::builder()
            .method(Method::GET)
            .uri("/events?wire=2&cursor=1")
            .body(Body::empty())
            .unwrap();
        let reconnect_response = app.clone().oneshot(reconnect).await.unwrap();
        assert_eq!(reconnect_response.status(), StatusCode::OK);
        let reconnect_body = {
            use futures_util::StreamExt;
            let mut stream = reconnect_response.into_body().into_data_stream();
            let mut collected = Vec::new();
            let deadline = tokio::time::Instant::now() + Duration::from_secs(2);
            while tokio::time::Instant::now() < deadline {
                match timeout(Duration::from_millis(200), stream.next()).await {
                    Ok(Some(Ok(chunk))) => {
                        collected.extend_from_slice(&chunk);
                        let text = String::from_utf8_lossy(&collected);
                        if text.contains("\"cursor\":2") {
                            break;
                        }
                    }
                    Ok(Some(Err(error))) => panic!("SSE reconnect body error: {error}"),
                    Ok(None) => break,
                    Err(_) => continue,
                }
            }
            String::from_utf8(collected).expect("utf8 reconnect body")
        };
        assert!(
            reconnect_body.contains("\"cursor\":2"),
            "cursor reconnect must include later events: {reconnect_body}"
        );
        assert!(
            !reconnect_body.contains("\"cursor\":1"),
            "cursor reconnect must not duplicate cursor=1: {reconnect_body}"
        );

        let ahead = Request::builder()
            .method(Method::GET)
            .uri("/events?wire=2&cursor=99")
            .body(Body::empty())
            .unwrap();
        let ahead_response = app.oneshot(ahead).await.unwrap();
        assert_eq!(ahead_response.status(), StatusCode::CONFLICT);
        let ahead_body = to_bytes(ahead_response.into_body(), usize::MAX)
            .await
            .unwrap();
        let ahead_value: serde_json::Value = serde_json::from_slice(&ahead_body).unwrap();
        assert_eq!(ahead_value["error"]["code"], "WIRE_V2_CURSOR_AHEAD");
    }

    /// W3-06: production `start()` resolves the hub from the lifecycle host
    /// when `WebServerOptions.workflow_hub` is unset — pin that fallback so a
    /// headless runtime regression cannot silently 503 wire v2.
    #[tokio::test]
    async fn code_events_wire_v2_resolves_hub_from_lifecycle() {
        use axum::extract::connect_info::MockConnectInfo;
        use futures_util::future::BoxFuture;
        use tempfile::tempdir;

        use crate::internal::ai::{
            session::{CodeWorkflowEventKind, SessionJsonlStore},
            web::{
                agent_runtime_adapter::CodeUiLifecycleShutdown, code_ui::CodeUiRuntimeOptions,
                sse_wire::CodeUiWorkflowHub,
            },
        };

        struct LifecycleHub(Arc<CodeUiWorkflowHub>);
        impl CodeUiLifecycleShutdown for LifecycleHub {
            fn shutdown(&self) -> BoxFuture<'_, anyhow::Result<()>> {
                Box::pin(async { Ok(()) })
            }

            fn workflow_hub(&self) -> Option<Arc<CodeUiWorkflowHub>> {
                Some(self.0.clone())
            }
        }

        let dir = tempdir().expect("tempdir");
        let mut store = SessionJsonlStore::new(dir.path().to_path_buf());
        let hub = Arc::new(CodeUiWorkflowHub::attach(&mut store).expect("attach workflow hub"));
        store
            .append_code_workflow(CodeWorkflowEventKind::CodeUiProjectionDelta {
                projection: "status".to_string(),
                summary: "lifecycle-hub".to_string(),
                payload: serde_json::json!({ "ok": true }),
            })
            .expect("append");

        let session = CodeUiSession::new(initial_snapshot(
            "/tmp/libra-w306-lifecycle",
            CodeUiProviderInfo {
                provider: "test".to_string(),
                model: Some("test-model".to_string()),
                mode: None,
                managed: false,
            },
            CodeUiCapabilities::default(),
        ));
        let runtime = CodeUiRuntimeHandle::build_with_options_and_lifecycle(
            ReadOnlyCodeUiAdapter::new(session, CodeUiCapabilities::default()),
            CodeUiRuntimeOptions::new(true, false, CodeUiInitialController::Unclaimed),
            Some(Arc::new(LifecycleHub(hub))),
        )
        .await;

        // Mirror `start()`: options.workflow_hub is None, fall back to runtime.
        let workflow_hub = None.or_else(|| runtime.workflow_hub());
        assert!(
            workflow_hub.is_some(),
            "lifecycle host must expose the durable workflow hub"
        );

        let app = code_router()
            .with_state(WebAppState {
                working_dir: Arc::new(PathBuf::from("/tmp/libra-w306-lifecycle")),
                code_ui: Some(runtime),
                automation_control_token: None,
                browser_bootstrap_token: None,
                audit_sink: Arc::new(TracingAuditSink),
                control_trace_id: Uuid::new_v4(),
                bound_addr: SocketAddr::from(([127, 0, 0, 1], 4319)),
                write_rate_limiter: SessionWriteRateLimiter::from_env_or_default(),
                secret_redactor: Arc::new(SecretRedactor::default_runtime()),
                workflow_hub,
            })
            .layer(MockConnectInfo(SocketAddr::from(([127, 0, 0, 1], 1))));

        let request = Request::builder()
            .method(Method::GET)
            .uri("/events?wire=2")
            .body(Body::empty())
            .unwrap();
        let response = app.oneshot(request).await.unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let body = {
            use futures_util::StreamExt;
            let mut stream = response.into_body().into_data_stream();
            let mut collected = Vec::new();
            let deadline = tokio::time::Instant::now() + Duration::from_secs(2);
            while tokio::time::Instant::now() < deadline {
                match timeout(Duration::from_millis(200), stream.next()).await {
                    Ok(Some(Ok(chunk))) => {
                        collected.extend_from_slice(&chunk);
                        let text = String::from_utf8_lossy(&collected);
                        if text.contains("\"cursor\":1") {
                            break;
                        }
                    }
                    Ok(Some(Err(error))) => panic!("SSE body error: {error}"),
                    Ok(None) => break,
                    Err(_) => continue,
                }
            }
            String::from_utf8(collected).expect("utf8 SSE body")
        };
        assert!(
            body.contains("event: code_workflow") && body.contains("\"cursor\":1"),
            "lifecycle-derived hub must serve wire v2 replay: {body}"
        );
    }

    /// W3-06: live fan-out must reach an already-subscribed v2 client after a
    /// post-connect durable append (not only pre-connect replay).
    #[tokio::test]
    async fn code_events_wire_v2_live_fanout_after_subscribe() {
        use axum::extract::connect_info::MockConnectInfo;
        use futures_util::StreamExt;
        use tempfile::tempdir;

        use crate::internal::ai::{
            session::{CodeWorkflowEventKind, SessionJsonlStore},
            web::sse_wire::CodeUiWorkflowHub,
        };

        let runtime = test_code_ui_runtime().await;
        let dir = tempdir().expect("tempdir");
        let mut store = SessionJsonlStore::new(dir.path().to_path_buf());
        let hub = Arc::new(CodeUiWorkflowHub::attach(&mut store).expect("attach workflow hub"));
        let app = code_router()
            .with_state(WebAppState {
                working_dir: Arc::new(PathBuf::from("/tmp/libra-w306-live")),
                code_ui: Some(runtime),
                automation_control_token: None,
                browser_bootstrap_token: None,
                audit_sink: Arc::new(TracingAuditSink),
                control_trace_id: Uuid::new_v4(),
                bound_addr: SocketAddr::from(([127, 0, 0, 1], 4320)),
                write_rate_limiter: SessionWriteRateLimiter::from_env_or_default(),
                secret_redactor: Arc::new(SecretRedactor::default_runtime()),
                workflow_hub: Some(hub),
            })
            .layer(MockConnectInfo(SocketAddr::from(([127, 0, 0, 1], 1))));

        let request = Request::builder()
            .method(Method::GET)
            .uri("/events?wire=2")
            .body(Body::empty())
            .unwrap();
        let response = app.oneshot(request).await.unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let mut stream = response.into_body().into_data_stream();

        // Subscribe first, then append so the frame must come from live fan-out.
        store
            .append_code_workflow(CodeWorkflowEventKind::CodeUiProjectionDelta {
                projection: "status".to_string(),
                summary: "live-after-subscribe".to_string(),
                payload: serde_json::json!({ "live": true }),
            })
            .expect("post-subscribe append");

        let mut collected = Vec::new();
        let deadline = tokio::time::Instant::now() + Duration::from_secs(2);
        while tokio::time::Instant::now() < deadline {
            match timeout(Duration::from_millis(200), stream.next()).await {
                Ok(Some(Ok(chunk))) => {
                    collected.extend_from_slice(&chunk);
                    let text = String::from_utf8_lossy(&collected);
                    if text.contains("live-after-subscribe") && text.contains("\"cursor\":1") {
                        break;
                    }
                }
                Ok(Some(Err(error))) => panic!("SSE body error: {error}"),
                Ok(None) => break,
                Err(_) => continue,
            }
        }
        let body = String::from_utf8(collected).expect("utf8 SSE body");
        assert!(
            body.contains("event: code_workflow")
                && body.contains("live-after-subscribe")
                && body.contains("\"cursor\":1"),
            "subscribed v2 client must receive live durable appends: {body}"
        );
    }

    /// W3-08: slow consumer past the transport broadcast budget gets the same
    /// recoverable resync exit as over-budget bootstrap (no silent drop).
    #[cfg(feature = "test-provider")]
    #[tokio::test]
    async fn code_events_wire_v2_slow_consumer_resync() {
        assert_sse_slow_consumer_contract()
            .await
            .expect("sse_slow_consumer contract");
    }

    /// W3-08: bootstrap past the transport window emits resync (not 500).
    #[tokio::test]
    async fn code_events_wire_v2_bootstrap_resync_required() {
        use axum::extract::connect_info::MockConnectInfo;
        use futures_util::StreamExt;
        use tempfile::tempdir;

        use crate::internal::ai::{
            session::{CodeWorkflowEventKind, SessionJsonlStore},
            web::sse_wire::{
                CodeUiWorkflowHub, MAX_CODE_UI_TRANSPORT_BACKLOG_EVENTS, WIRE_V2_RESYNC_REQUIRED,
            },
        };

        let runtime = test_code_ui_runtime().await;
        let dir = tempdir().expect("tempdir");
        let mut store = SessionJsonlStore::new(dir.path().to_path_buf());
        let hub = Arc::new(CodeUiWorkflowHub::attach(&mut store).expect("attach workflow hub"));
        let over = MAX_CODE_UI_TRANSPORT_BACKLOG_EVENTS + 1;
        let kinds: Vec<_> = (0..over)
            .map(|i| CodeWorkflowEventKind::CodeUiProjectionDelta {
                projection: "status".to_string(),
                summary: format!("boot-{i}"),
                payload: serde_json::json!({}),
            })
            .collect();
        store
            .append_code_workflow_batch(&kinds)
            .expect("seed over-budget log");

        let app = code_router()
            .with_state(WebAppState {
                working_dir: Arc::new(PathBuf::from("/tmp/libra-w308-boot")),
                code_ui: Some(runtime),
                automation_control_token: None,
                browser_bootstrap_token: None,
                audit_sink: Arc::new(TracingAuditSink),
                control_trace_id: Uuid::new_v4(),
                bound_addr: SocketAddr::from(([127, 0, 0, 1], 4322)),
                write_rate_limiter: SessionWriteRateLimiter::from_env_or_default(),
                secret_redactor: Arc::new(SecretRedactor::default_runtime()),
                workflow_hub: Some(hub),
            })
            .layer(MockConnectInfo(SocketAddr::from(([127, 0, 0, 1], 1))));

        let request = Request::builder()
            .method(Method::GET)
            .uri("/events?wire=2")
            .body(Body::empty())
            .unwrap();
        let response = app.oneshot(request).await.unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let mut stream = response.into_body().into_data_stream();
        let mut collected = Vec::new();
        let deadline = tokio::time::Instant::now() + Duration::from_secs(3);
        while tokio::time::Instant::now() < deadline {
            match timeout(Duration::from_millis(200), stream.next()).await {
                Ok(Some(Ok(chunk))) => {
                    collected.extend_from_slice(&chunk);
                    let text = String::from_utf8_lossy(&collected);
                    if text.contains("event: resync") {
                        break;
                    }
                }
                Ok(Some(Err(_))) => break,
                Ok(None) => break,
                Err(_) => continue,
            }
        }
        let body = String::from_utf8(collected).expect("utf8 SSE body");
        assert!(
            body.contains("event: resync")
                && body.contains(WIRE_V2_RESYNC_REQUIRED)
                && body.contains("bootstrap_window_exceeded"),
            "over-budget bootstrap must emit resync: {body}"
        );
        assert!(
            !body.contains("event: code_workflow"),
            "over-budget bootstrap must not silently stream a truncated prefix: {body}"
        );
    }
}
