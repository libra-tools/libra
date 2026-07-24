//! `libra service` — headless local service with notification v1
//! (lore.md §1.11, a Libra extension; Git has no equivalent).
//!
//! The service is LOCAL-ONLY by construction: `--host` must be a literal
//! loopback IP (validated at parse time AND re-checked at bind time), so no
//! outward TCP port can ever be opened — the lore row's explicit "不要做
//! hosted server" red line. Every endpoint additionally enforces a loopback
//! peer address, and every data-carrying endpoint (the SSE event stream, the
//! dirty-mark and notify publishers) requires the 0600 bearer token — the
//! lore-mandated "最小本机访问控制" for a bus carrying dirty-set/automation
//! payloads (other local uids are NOT trusted).
//!
//! Notification v1 is an in-memory broadcast bus with AT-MOST-ONCE delivery:
//! events are lost on consumer lag (a `resync` event tells the consumer to
//! re-read authoritative state) and the sequence restarts on service restart.
//! That is deliberate (§7.9): the only durable facts are the advisory dirty
//! marks, which live in SQLite via the validated `DirtyCache::mark_paths`
//! owner API (over-report-only, batch-refused on any repo-escaping path) and
//! therefore survive `kill -9` (§7.10); everything on the bus is derivable.
//!
//! Deferred with rationale (see docs/development/commands/service.md): UDS
//! transport (the lore row's OR is satisfied by the loopback branch), a
//! filesystem watcher feeding marks (accelerator only — needs a new heavy
//! dependency; marks flow through the token-gated endpoint), repo/status
//! read passthroughs, MCP (already served by `libra code`), daemonization
//! (foreground + external supervision), and §7.7 automatic replay.

use std::{
    net::{IpAddr, SocketAddr},
    path::PathBuf,
    sync::{
        Arc,
        atomic::{AtomicU64, Ordering},
    },
};

use axum::{
    Json, Router,
    extract::{ConnectInfo, DefaultBodyLimit, State},
    http::{HeaderMap, StatusCode},
    response::sse::{Event, KeepAlive, Sse},
    routing::{get, post},
};
use clap::{Parser, Subcommand};
use serde::{Deserialize, Serialize};
use tokio::sync::broadcast;

use crate::{
    command::code_control_files::{
        ControlInfo, ControlPaths, acquire_control_lock, cleanup_control_files,
        ensure_control_token_file, pid_is_live, validate_token_file_perms, write_control_info,
    },
    internal::dirty::{DirtyCache, MarkError},
    utils::{
        error::{CliError, CliResult, StableErrorCode},
        output::{OutputConfig, emit_json_data},
        util,
    },
};

pub const SERVICE_EXAMPLES: &str = "\
EXAMPLES:
    libra service run                      Run the headless service (loopback, OS-assigned port)
    libra service run --port 7311          Pin the port (still loopback-only)
    libra service status                   Show the running instance (pid, URL, health)
    libra service events                   Tail the notification stream
    libra --json service events            NDJSON event stream for tooling
    curl -H \"X-Libra-Service-Token: $(cat .libra/service/service-token)\" \\
         -X POST http://127.0.0.1:PORT/api/service/dirty/mark \\
         -d '{\"paths\":[\"src/main.rs\"]}' -H 'content-type: application/json'

NOTES:
    Local-only by construction: --host must be a loopback IP; every endpoint
    checks the peer address, and data-carrying endpoints require the 0600
    token file (.libra/service/service-token). Notifications are at-most-once
    (durable facts live in SQLite; re-read state after a `resync` event).";

/// Run a headless local service: notification bus + dirty-mark ingestion (Libra extension).
#[derive(Parser, Debug)]
#[command(after_help = SERVICE_EXAMPLES)]
pub struct ServiceArgs {
    #[command(subcommand)]
    pub command: ServiceCommand,
}

#[derive(Subcommand, Debug)]
pub enum ServiceCommand {
    /// Run the service in the foreground (Ctrl-C / SIGTERM to stop).
    Run {
        /// Loopback IP to bind (127.0.0.0/8 or ::1). Hostnames and
        /// non-loopback IPs are refused — the service never opens an
        /// outward TCP port.
        #[arg(long, default_value = "127.0.0.1")]
        host: String,
        /// Port to bind (0 = OS-assigned; the real port is published in
        /// .libra/service/service.json).
        #[arg(long, default_value_t = 0)]
        port: u16,
    },
    /// Report the running instance (pid, base URL, liveness).
    Status,
    /// Tail the notification stream (human lines; NDJSON under --json).
    Events,
}

/// One notification: monotonically increasing `seq` within a service run,
/// `kind` + JSON payload. At-most-once; `seq` restarts with the service.
#[derive(Debug, Clone, Serialize)]
struct NotificationEnvelope {
    seq: u64,
    #[serde(rename = "type")]
    kind: String,
    at: String,
    data: serde_json::Value,
}

struct ServiceBus {
    sender: broadcast::Sender<NotificationEnvelope>,
    seq: AtomicU64,
}

impl ServiceBus {
    fn new() -> Self {
        let (sender, _) = broadcast::channel(256);
        Self {
            sender,
            seq: AtomicU64::new(0),
        }
    }

    fn publish(&self, kind: &str, data: serde_json::Value) {
        let envelope = NotificationEnvelope {
            seq: self.seq.fetch_add(1, Ordering::SeqCst),
            kind: kind.to_string(),
            at: crate::internal::dirty::now_timestamp(),
            data,
        };
        // No receivers is fine — events are advisory.
        let _ = self.sender.send(envelope);
    }
}

#[derive(Clone)]
struct ServiceState {
    bus: Arc<ServiceBus>,
    token: Arc<String>,
}

const SERVICE_TOKEN_HEADER: &str = "x-libra-service-token";
/// Request-body cap for the mutating endpoints (mirrors the code router's
/// deliberate limit — an unbounded paths array is a trivial local DoS).
const SERVICE_BODY_LIMIT: usize = 256 * 1024;

/// Loopback peer enforcement — defense-in-depth behind the bind-time guard.
fn require_loopback(peer: &SocketAddr) -> Result<(), (StatusCode, String)> {
    if peer.ip().is_loopback() {
        Ok(())
    } else {
        Err((
            StatusCode::FORBIDDEN,
            "service endpoints accept loopback connections only".to_string(),
        ))
    }
}

/// Token enforcement for every data-carrying endpoint (SSE included): the
/// bus carries dirty paths and automation payloads, and other local uids are
/// not trusted (0600 token file is the access control).
fn require_token(state: &ServiceState, headers: &HeaderMap) -> Result<(), (StatusCode, String)> {
    let presented = headers
        .get(SERVICE_TOKEN_HEADER)
        .and_then(|value| value.to_str().ok());
    match presented {
        Some(token) if token == state.token.as_str() => Ok(()),
        Some(_) => Err((StatusCode::FORBIDDEN, "invalid service token".to_string())),
        None => Err((
            StatusCode::UNAUTHORIZED,
            format!("missing {SERVICE_TOKEN_HEADER} header"),
        )),
    }
}

async fn health_handler(
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
) -> Result<Json<serde_json::Value>, (StatusCode, String)> {
    require_loopback(&peer)?;
    Ok(Json(serde_json::json!({ "status": "ok" })))
}

async fn events_handler(
    State(state): State<ServiceState>,
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    headers: HeaderMap,
) -> Result<
    Sse<impl futures_core::Stream<Item = Result<Event, std::convert::Infallible>>>,
    (StatusCode, String),
> {
    require_loopback(&peer)?;
    require_token(&state, &headers)?;
    let mut receiver = state.bus.sender.subscribe();
    let stream = async_stream::stream! {
        loop {
            match receiver.recv().await {
                Ok(envelope) => {
                    let payload = serde_json::to_string(&envelope)
                        .unwrap_or_else(|_| "{}".to_string());
                    yield Ok(Event::default().data(payload));
                }
                Err(broadcast::error::RecvError::Lagged(missed)) => {
                    // At-most-once: tell the consumer to re-read state.
                    let notice = serde_json::json!({
                        "seq": null,
                        "type": "resync",
                        "data": { "missed": missed },
                    });
                    yield Ok(Event::default().data(notice.to_string()));
                }
                Err(broadcast::error::RecvError::Closed) => break,
            }
        }
    };
    Ok(Sse::new(stream).keep_alive(KeepAlive::default()))
}

#[derive(Debug, Deserialize)]
struct MarkRequest {
    paths: Vec<String>,
}

async fn dirty_mark_handler(
    State(state): State<ServiceState>,
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    headers: HeaderMap,
    Json(request): Json<MarkRequest>,
) -> Result<Json<serde_json::Value>, (StatusCode, String)> {
    require_loopback(&peer)?;
    require_token(&state, &headers)?;
    if request.paths.is_empty() {
        return Err((StatusCode::BAD_REQUEST, "paths must be non-empty".into()));
    }
    // §C.4.1.1 service contract: a legacy request carries only `paths` — in
    // a multi-worktree repository it is REJECTED rather than defaulted to
    // the service process's own scope (the dirty cache is per-worktree and
    // the caller's worktree is unknown). Scope-carrying requests arrive with
    // the composite-dispatcher work (W4-adjacent).
    let registry = crate::utils::util::storage_path().join("worktrees.json");
    match std::fs::read_to_string(&registry) {
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            // No registry file: single (main-only) worktree repository.
        }
        Ok(raw) => {
            // Parse the registry with the REAL persisted schema (the same
            // `WorktreeState` type `libra worktree` writes) and only accept
            // a validated single-main state. Everything else fails closed:
            // deserialization failures (syntactically invalid JSON, `[null]`,
            // entries missing required fields like `is_main`/`locked`) AND
            // deserializable-but-corrupt shapes (an empty worktree list or a
            // sole non-main entry — the real loader treats those as needing
            // main-entry repair). Never guess the scope.
            let single_main = crate::command::worktree::WorktreeState::parse(raw.as_bytes())
                .is_ok_and(|state| state.is_single_main());
            if !single_main {
                return Err((
                    StatusCode::CONFLICT,
                    "dirty-mark requests without a worktree scope are rejected in a \
                     multi-worktree repository; run `libra dirty <paths>` in the \
                     target worktree instead"
                        .into(),
                ));
            }
        }
        Err(_) => {
            // Unreadable registry: fail closed for the same reason.
            return Err((
                StatusCode::CONFLICT,
                "the worktree registry is unreadable; scope-less dirty-mark \
                 requests are rejected — run `libra dirty <paths>` in the \
                 target worktree instead"
                    .into(),
            ));
        }
    }
    let workdir_relative: Vec<PathBuf> = request.paths.iter().map(util::to_workdir_path).collect();
    // The owner API enforces the repo-escape gate (whole batch refused).
    match DirtyCache::mark_paths(&workdir_relative).await {
        Ok(stored) => {
            state
                .bus
                .publish("dirty_marked", serde_json::json!({ "paths": stored }));
            Ok(Json(serde_json::json!({ "marked": stored })))
        }
        Err(error @ MarkError::Escaping(_)) => Err((StatusCode::BAD_REQUEST, error.to_string())),
        Err(error) => Err((
            StatusCode::INTERNAL_SERVER_ERROR,
            format!("failed to write the dirty cache: {error}"),
        )),
    }
}

#[derive(Debug, Deserialize)]
struct NotifyRequest {
    #[serde(rename = "type")]
    kind: String,
    #[serde(default)]
    data: serde_json::Value,
}

async fn notify_handler(
    State(state): State<ServiceState>,
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    headers: HeaderMap,
    Json(request): Json<NotifyRequest>,
) -> Result<Json<serde_json::Value>, (StatusCode, String)> {
    require_loopback(&peer)?;
    require_token(&state, &headers)?;
    if request.kind.trim().is_empty() || request.kind.len() > 128 {
        return Err((
            StatusCode::BAD_REQUEST,
            "type must be 1..=128 characters".into(),
        ));
    }
    state.bus.publish(&request.kind, request.data);
    Ok(Json(serde_json::json!({ "published": true })))
}

/// Resolve the service control-file paths under `.libra/service/`.
fn service_paths() -> CliResult<ControlPaths> {
    util::require_repo().map_err(|_| CliError::repo_not_found())?;
    let dir = util::storage_path().join("service");
    Ok(ControlPaths {
        token: dir.join("service-token"),
        info: dir.join("service.json"),
        lock: dir.join("service.lock"),
    })
}

/// Parse and enforce the loopback-only host contract (exit 129 on violation).
fn parse_loopback_host(host: &str) -> CliResult<IpAddr> {
    let ip: IpAddr = host.parse().map_err(|_| {
        CliError::command_usage(format!(
            "--host must be a literal loopback IP (got '{host}'); hostnames are not accepted"
        ))
    })?;
    if !ip.is_loopback() {
        return Err(CliError::command_usage(format!(
            "--host {host} is not a loopback address; the service never opens an outward TCP port"
        )));
    }
    Ok(ip)
}

pub async fn execute(args: ServiceArgs) {
    if let Err(err) = execute_safe(args, &OutputConfig::default()).await {
        err.print_stderr();
    }
}

pub async fn execute_safe(args: ServiceArgs, output: &OutputConfig) -> CliResult<()> {
    util::require_repo().map_err(|_| CliError::repo_not_found())?;
    match args.command {
        ServiceCommand::Run { host, port } => run_service(&host, port, output).await,
        ServiceCommand::Status => service_status(output).await,
        ServiceCommand::Events => service_events(output).await,
    }
}

async fn run_service(host: &str, port: u16, output: &OutputConfig) -> CliResult<()> {
    let ip = parse_loopback_host(host)?;
    let paths = service_paths()?;
    if let Some(parent) = paths.lock.parent() {
        std::fs::create_dir_all(parent).map_err(|e| {
            CliError::fatal(format!("failed to create the service directory: {e}"))
                .with_stable_code(StableErrorCode::IoWriteFailed)
        })?;
    }
    // Single instance per repo; stale locks from dead pids are reclaimed by
    // the existing advisory-lock machinery.
    let lock = acquire_control_lock(&paths.lock).map_err(|e| {
        CliError::failure(format!("cannot start the service: {e}"))
            .with_stable_code(StableErrorCode::ConflictOperationBlocked)
            .with_hint("another instance may be running; see `libra service status`")
    })?;
    let token = ensure_control_token_file(&paths.token).await.map_err(|e| {
        CliError::fatal(format!("failed to prepare the service token: {e}"))
            .with_stable_code(StableErrorCode::IoWriteFailed)
    })?;

    // Report (never auto-fix) a stale dirty-cache scan lock: it self-heals on
    // the next scanner; grabbing it here could race a genuinely long scan.
    if let Ok(Some(meta)) = DirtyCache::meta().await
        && let Some(pid) = meta.scan_lock_pid
        && !pid_is_live(pid as u32)
    {
        crate::utils::error::emit_warning(format!(
            "stale dirty-cache scan lock from dead pid {pid} (the next `status --scan` reclaims it)"
        ));
    }

    // IPv6-safe: construct the SocketAddr directly (never format!-parse).
    let addr = SocketAddr::new(ip, port);
    let listener = tokio::net::TcpListener::bind(addr).await.map_err(|e| {
        CliError::fatal(format!("failed to bind {addr}: {e}"))
            .with_stable_code(StableErrorCode::IoWriteFailed)
            .with_hint("pass a different --port, or 0 for an OS-assigned one")
    })?;
    let bound = listener.local_addr().map_err(|e| {
        CliError::fatal(format!("failed to read the bound address: {e}"))
            .with_stable_code(StableErrorCode::IoReadFailed)
    })?;
    let base_url = format!("http://{bound}");

    let state = ServiceState {
        bus: Arc::new(ServiceBus::new()),
        token: Arc::new(token),
    };
    let app = Router::new()
        .route("/api/health", get(health_handler))
        .route("/api/service/events", get(events_handler))
        .route("/api/service/dirty/mark", post(dirty_mark_handler))
        .route("/api/service/notify", post(notify_handler))
        .layer(DefaultBodyLimit::max(SERVICE_BODY_LIMIT))
        .with_state(state.clone());

    let info = ControlInfo {
        version: 1,
        mode: "service".to_string(),
        pid: std::process::id(),
        base_url: base_url.clone(),
        mcp_url: None,
        working_dir: util::working_dir(),
        thread_id: None,
        started_at: chrono::Utc::now(),
    };
    write_control_info(&paths.info, &info).map_err(|e| {
        CliError::fatal(format!("failed to write service.json: {e}"))
            .with_stable_code(StableErrorCode::IoWriteFailed)
    })?;

    state.bus.publish(
        "service_lifecycle",
        serde_json::json!({ "event": "started", "base_url": base_url }),
    );
    if !output.quiet {
        println!("libra service listening on {base_url} (loopback-only; Ctrl-C to stop)");
    }

    let server = axum::serve(
        listener,
        app.into_make_service_with_connect_info::<SocketAddr>(),
    )
    .with_graceful_shutdown(shutdown_signal());
    let result = server.await;

    state.bus.publish(
        "service_lifecycle",
        serde_json::json!({ "event": "stopping" }),
    );
    cleanup_control_files(&paths, false, true);
    drop(lock);
    result.map_err(|e| {
        CliError::fatal(format!("service terminated abnormally: {e}"))
            .with_stable_code(StableErrorCode::IoWriteFailed)
    })?;
    if !output.quiet {
        println!("service stopped");
    }
    Ok(())
}

async fn shutdown_signal() {
    #[cfg(unix)]
    {
        let mut sigterm =
            match tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate()) {
                Ok(signal) => signal,
                Err(_) => {
                    let _ = tokio::signal::ctrl_c().await;
                    return;
                }
            };
        tokio::select! {
            _ = tokio::signal::ctrl_c() => {}
            _ = sigterm.recv() => {}
        }
    }
    #[cfg(not(unix))]
    {
        let _ = tokio::signal::ctrl_c().await;
    }
}

#[derive(Debug, Serialize)]
struct ServiceStatusReport {
    running: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pid: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    base_url: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    started_at: Option<String>,
    /// `ok`, `unreachable`, or `stale` (dead pid / missing file).
    health: String,
}

async fn service_status(output: &OutputConfig) -> CliResult<()> {
    let paths = service_paths()?;
    let report = match std::fs::read_to_string(&paths.info) {
        Err(_) => ServiceStatusReport {
            running: false,
            pid: None,
            base_url: None,
            started_at: None,
            health: "stale".to_string(),
        },
        Ok(text) => match serde_json::from_str::<ControlInfo>(&text) {
            Err(_) => ServiceStatusReport {
                running: false,
                pid: None,
                base_url: None,
                started_at: None,
                health: "stale".to_string(),
            },
            Ok(info) => {
                let live = pid_is_live(info.pid);
                let health = if !live {
                    "stale".to_string()
                } else {
                    let url = format!("{}/api/health", info.base_url);
                    match reqwest::Client::new()
                        .get(&url)
                        .timeout(std::time::Duration::from_secs(2))
                        .send()
                        .await
                    {
                        Ok(response) if response.status().is_success() => "ok".to_string(),
                        _ => "unreachable".to_string(),
                    }
                };
                ServiceStatusReport {
                    running: live,
                    pid: Some(info.pid),
                    base_url: Some(info.base_url),
                    started_at: Some(info.started_at.to_rfc3339()),
                    health,
                }
            }
        },
    };
    if output.is_json() {
        return emit_json_data("service", &report, output);
    }
    if !output.quiet {
        if report.running {
            println!(
                "service running (pid {}, {}) health: {}",
                report.pid.unwrap_or_default(),
                report.base_url.as_deref().unwrap_or("?"),
                report.health
            );
        } else {
            println!("service not running");
        }
    }
    if !report.running {
        return Err(CliError::silent_exit(1));
    }
    Ok(())
}

async fn service_events(output: &OutputConfig) -> CliResult<()> {
    let paths = service_paths()?;
    let info: ControlInfo = std::fs::read_to_string(&paths.info)
        .ok()
        .and_then(|text| serde_json::from_str(&text).ok())
        .ok_or_else(|| {
            CliError::failure("no running service found")
                .with_stable_code(StableErrorCode::CliInvalidTarget)
                .with_hint("start one with `libra service run`")
        })?;
    validate_token_file_perms(&paths.token).map_err(|e| {
        CliError::fatal(format!("service token file rejected: {e}"))
            .with_stable_code(StableErrorCode::IoReadFailed)
    })?;
    let token = std::fs::read_to_string(&paths.token).map_err(|e| {
        CliError::fatal(format!("failed to read the service token: {e}"))
            .with_stable_code(StableErrorCode::IoReadFailed)
    })?;
    let url = format!("{}/api/service/events", info.base_url);
    let response = reqwest::Client::new()
        .get(&url)
        .header(SERVICE_TOKEN_HEADER, token.trim())
        .send()
        .await
        .map_err(|e| {
            CliError::failure(format!("cannot reach the service at {url}: {e}"))
                .with_stable_code(StableErrorCode::CliInvalidTarget)
                .with_hint("check `libra service status`")
        })?;
    if !response.status().is_success() {
        return Err(
            CliError::failure(format!("event stream refused: HTTP {}", response.status()))
                .with_stable_code(StableErrorCode::CliInvalidTarget),
        );
    }
    let mut stream = response.bytes_stream();
    use futures_util::StreamExt;
    let mut buffer = String::new();
    while let Some(chunk) = stream.next().await {
        let chunk = match chunk {
            Ok(chunk) => chunk,
            Err(_) => break, // server went away: clean exit
        };
        buffer.push_str(&String::from_utf8_lossy(&chunk));
        while let Some(pos) = buffer.find('\n') {
            let line = buffer[..pos].trim().to_string();
            buffer.drain(..=pos);
            let Some(payload) = line.strip_prefix("data:") else {
                continue;
            };
            let payload = payload.trim();
            if payload.is_empty() {
                continue;
            }
            if output.is_json() {
                println!("{payload}");
            } else if let Ok(value) = serde_json::from_str::<serde_json::Value>(payload) {
                println!(
                    "[{}] {} {}",
                    value.get("seq").and_then(|v| v.as_u64()).unwrap_or(0),
                    value.get("type").and_then(|v| v.as_str()).unwrap_or("?"),
                    value.get("data").map(|v| v.to_string()).unwrap_or_default()
                );
            }
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn loopback_host_validation_matrix() {
        for ok in ["127.0.0.1", "127.8.8.8", "::1"] {
            assert!(parse_loopback_host(ok).is_ok(), "{ok}");
        }
        for bad in ["0.0.0.0", "192.168.1.10", "8.8.8.8", "localhost", "::"] {
            assert!(parse_loopback_host(bad).is_err(), "{bad}");
        }
    }

    #[test]
    fn envelope_serializes_with_stable_keys() {
        let envelope = NotificationEnvelope {
            seq: 7,
            kind: "notify".to_string(),
            at: "2026-07-02T00:00:00.000000Z".to_string(),
            data: serde_json::json!({"k": "v"}),
        };
        let value = serde_json::to_value(&envelope).unwrap();
        assert_eq!(value["seq"], 7);
        assert_eq!(value["type"], "notify");
        assert_eq!(value["data"]["k"], "v");
    }
}
