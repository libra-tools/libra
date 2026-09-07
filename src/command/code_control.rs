//! Canonical JSON-RPC 2.0 NDJSON automation control client (W4-02).
//!
//! Used by `libra code --control stdio` (control-info discovery by default).
//! It is **not** an MCP server (`libra code --stdio` is the deprecated
//! MCP-only legacy transport; see DEFER-02 for a future `libra mcp --stdio`).
//! The W4-09 `code-control` forwarding-shim entry was physically removed in
//! W5-01; this module now hosts only the canonical client.

use std::{
    io::{self, BufRead, Write},
    path::PathBuf,
};

use futures_util::StreamExt;
use reqwest::{Client, RequestBuilder, StatusCode};
use serde::{Deserialize, Serialize, de::DeserializeOwned};
use serde_json::{Value, json};
use url::Url;

use crate::utils::error::{CliError, CliResult};

/// SSE wire explicitly requested by the built-in stdio automation client.
///
/// Keep this independent from the server's omitted-wire compatibility default:
/// DF-05 migrates consumers to v2 while DF-06 owns the server default switch.
pub const BUILT_IN_CODE_EVENTS_SSE_WIRE_VERSION: u8 = 2;
const WIRE_V2_REQUIRES_DURABLE_SESSION: &str = "WIRE_V2_REQUIRES_DURABLE_SESSION";
const WIRE_V2_RESYNC_REQUIRED: &str = "WIRE_V2_RESYNC_REQUIRED";
const WIRE_V2_CURSOR_AHEAD: &str = "WIRE_V2_CURSOR_AHEAD";
const MAX_CONSECUTIVE_EVENT_STREAM_RESYNCS: usize = 3;

#[derive(Debug, Deserialize)]
struct JsonRpcRequest {
    jsonrpc: Option<String>,
    method: Option<String>,
    #[serde(default)]
    params: Option<Value>,
    #[serde(default)]
    id: Option<Value>,
}

#[derive(Debug, Serialize)]
struct JsonRpcResponse {
    jsonrpc: &'static str,
    #[serde(skip_serializing_if = "Option::is_none")]
    result: Option<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    error: Option<JsonRpcErrorObject>,
    id: Value,
}

#[derive(Debug, Clone, Serialize)]
struct JsonRpcErrorObject {
    code: i64,
    message: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    data: Option<Value>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct AttachParams {
    client_id: String,
    #[serde(default)]
    kind: Option<String>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct DetachParams {
    client_id: String,
    controller_token: String,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct SubmitParams {
    text: String,
    controller_token: String,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct RespondParams {
    interaction_id: String,
    controller_token: String,
    response: Value,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct CancelParams {
    controller_token: String,
}

#[derive(Debug, Default)]
struct EventsSubscribeParams {
    /// Last durable v2 cursor acknowledged by the automation consumer.
    /// Omission preserves the initial bootstrap replay from cursor zero.
    cursor: Option<u64>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct TaskDispatchParams {
    agent: String,
    prompt: String,
    controller_token: String,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct GoalStartParams {
    objective: String,
    controller_token: String,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct GoalCancelParams {
    reason: String,
    controller_token: String,
}

/// Canonical JSON-RPC 2.0 NDJSON control client (W4-02).
///
/// Drives an existing Code UI write-control session over stdin/stdout.
/// Called by the canonical `libra code --control stdio` entry (W5-01 removed
/// the W4-09 forwarding-shim entry that also reached this helper).
pub async fn run_control_stdio_client(url: &str, token_file: &PathBuf) -> CliResult<()> {
    let base_url = Url::parse(url).map_err(|error| {
        CliError::command_usage(format!(
            "--control-url must be a valid control endpoint base URL (got '{url}': {error})"
        ))
    })?;
    ensure_loopback_control_url(&base_url)?;
    let control_token = read_control_token(token_file)?;
    // Loopback-only: never honor HTTP(S)_PROXY (would tunnel the control token
    // off-box) and never follow redirects (a loopback 3xx could bounce the
    // token header to a remote Location).
    let client = Client::builder()
        .no_proxy()
        .redirect(reqwest::redirect::Policy::none())
        .build()
        .map_err(|error| CliError::fatal(format!("failed to build HTTP client: {error}")))?;

    let stdin = io::stdin();
    let lines = stdin.lock().lines();
    for line in lines {
        let line = line.map_err(|error| {
            CliError::fatal(format!(
                "failed to read JSON-RPC request from stdin: {error}"
            ))
        })?;
        if line.trim().is_empty() {
            continue;
        }

        let parsed = match parse_json_rpc_request(&line) {
            Ok(request) => request,
            Err(error) => {
                write_json_rpc_response(&json_rpc_error(Value::Null, error))?;
                continue;
            }
        };
        let id = parsed.id.clone().unwrap_or(Value::Null);
        let response = dispatch_json_rpc_request(&client, &base_url, &control_token, parsed).await;
        match response {
            DispatchResult::Response(response) => write_json_rpc_response(&response)?,
            DispatchResult::NotificationOnly => {}
            DispatchResult::Subscribe { response, cursor } => {
                write_json_rpc_response(&response)?;
                stream_events(&client, &base_url, cursor).await?;
                break;
            }
            DispatchResult::Error(error) => write_json_rpc_response(&json_rpc_error(id, error))?,
        }
    }

    Ok(())
}

/// Control clients forward the local process token as
/// `X-Libra-Control-Token`. Restrict the base URL to loopback HTTP(S) so a
/// mistaken or malicious `--control-url` cannot exfiltrate that
/// token (or an arbitrary readable file passed as the token path) off-box.
pub(crate) fn ensure_loopback_control_url(url: &Url) -> CliResult<()> {
    let scheme = url.scheme();
    if scheme != "http" && scheme != "https" {
        return Err(CliError::command_usage(format!(
            "--control-url must use http or https (got '{scheme}')"
        )));
    }
    // Require a literal loopback IP. Reject `localhost` / other hostnames so a
    // poisoned hosts/DNS mapping cannot send the control token off-box.
    let host = match url.host() {
        Some(url::Host::Ipv4(addr)) if addr.is_loopback() => return Ok(()),
        Some(url::Host::Ipv6(addr)) if addr.is_loopback() => return Ok(()),
        Some(url::Host::Domain(name)) => name.to_string(),
        Some(other) => other.to_string(),
        None => String::new(),
    };
    Err(CliError::command_usage(format!(
        "--control-url must use a literal loopback IP such as http://127.0.0.1:3000 or http://[::1]:3000 (got '{url}'{host_hint}); hostnames like localhost are rejected so DNS/hosts remapping cannot exfiltrate the control token",
        host_hint = if host.is_empty() {
            String::new()
        } else {
            format!("; host '{host}'")
        }
    )))
}

pub(crate) fn read_control_token(path: &PathBuf) -> CliResult<String> {
    if !path.exists() {
        return Err(CliError::fatal(format!(
            "CONTROL_TOKEN_MISSING: local control token file '{}' is missing",
            path.display()
        ))
        .with_stable_code(crate::utils::error::StableErrorCode::AuthMissingCredentials));
    }
    // Fail-closed on symlink / non-file / overly permissive mode (W3-10 / W4-10).
    crate::command::code_control_files::validate_token_file_perms(path).map_err(|error| {
        CliError::fatal(format!(
            "CONTROL_TOKEN_PERMS: control token file '{}' rejected: {error}",
            path.display()
        ))
        .with_stable_code(crate::utils::error::StableErrorCode::AuthPermissionDenied)
    })?;
    let content = std::fs::read_to_string(path).map_err(|error| {
        CliError::fatal(format!(
            "CONTROL_TOKEN_MISSING: failed to read local control token file '{}': {error}",
            path.display()
        ))
        .with_stable_code(crate::utils::error::StableErrorCode::AuthMissingCredentials)
    })?;
    let token = content.trim().to_string();
    if token.is_empty() {
        return Err(CliError::fatal(format!(
            "CONTROL_TOKEN_MISSING: local control token file '{}' is empty",
            path.display()
        ))
        .with_stable_code(crate::utils::error::StableErrorCode::AuthMissingCredentials));
    }
    Ok(token)
}

fn parse_json_rpc_request(line: &str) -> Result<JsonRpcRequest, JsonRpcErrorObject> {
    let request: JsonRpcRequest =
        serde_json::from_str(line).map_err(|error| JsonRpcErrorObject {
            code: -32700,
            message: format!("Parse error: {error}"),
            data: None,
        })?;
    if request.jsonrpc.as_deref() != Some("2.0") || request.method.is_none() {
        return Err(JsonRpcErrorObject {
            code: -32600,
            message: "Invalid Request: expected JSON-RPC 2.0 object with method".to_string(),
            data: None,
        });
    }
    Ok(request)
}

enum DispatchResult {
    Response(JsonRpcResponse),
    NotificationOnly,
    Subscribe {
        response: JsonRpcResponse,
        cursor: u64,
    },
    Error(JsonRpcErrorObject),
}

async fn dispatch_json_rpc_request(
    client: &Client,
    base_url: &Url,
    control_token: &str,
    request: JsonRpcRequest,
) -> DispatchResult {
    let id = request.id.clone().unwrap_or(Value::Null);
    let Some(method) = request.method.as_deref() else {
        return DispatchResult::Error(JsonRpcErrorObject {
            code: -32600,
            message: "Invalid Request: missing method".to_string(),
            data: None,
        });
    };
    let result = match method {
        "session.get" => send_get(client, base_url, "/api/code/session").await,
        "diagnostics.get" => send_get(client, base_url, "/api/code/diagnostics").await,
        "controller.attach" => {
            let params = match parse_params::<AttachParams>(request.params) {
                Ok(params) => params,
                Err(error) => return DispatchResult::Error(error),
            };
            let mut body = json!({ "clientId": params.client_id });
            // Default omitted kind to automation: this client always authenticates
            // with X-Libra-Control-Token and never sends a browser Origin.
            body["kind"] = Value::String(params.kind.unwrap_or_else(|| "automation".to_string()));
            send_post(
                client,
                base_url,
                "/api/code/controller/attach",
                control_token,
                None,
                body,
            )
            .await
        }
        "controller.detach" => {
            let params = match parse_params::<DetachParams>(request.params) {
                Ok(params) => params,
                Err(error) => return DispatchResult::Error(error),
            };
            send_post(
                client,
                base_url,
                "/api/code/controller/detach",
                control_token,
                Some(&params.controller_token),
                json!({ "clientId": params.client_id }),
            )
            .await
        }
        "message.submit" => {
            let params = match parse_params::<SubmitParams>(request.params) {
                Ok(params) => params,
                Err(error) => return DispatchResult::Error(error),
            };
            send_post(
                client,
                base_url,
                "/api/code/messages",
                control_token,
                Some(&params.controller_token),
                json!({ "text": params.text }),
            )
            .await
        }
        "interaction.respond" => {
            let params = match parse_params::<RespondParams>(request.params) {
                Ok(params) => params,
                Err(error) => return DispatchResult::Error(error),
            };
            let endpoint = format!("/api/code/interactions/{}", params.interaction_id);
            send_post(
                client,
                base_url,
                &endpoint,
                control_token,
                Some(&params.controller_token),
                params.response,
            )
            .await
        }
        "turn.cancel" => {
            let params = match parse_params::<CancelParams>(request.params) {
                Ok(params) => params,
                Err(error) => return DispatchResult::Error(error),
            };
            send_post(
                client,
                base_url,
                "/api/code/control/cancel",
                control_token,
                Some(&params.controller_token),
                json!({}),
            )
            .await
        }
        "events.subscribe" => {
            let params = match parse_events_subscribe_params(request.params) {
                Ok(params) => params,
                Err(error) => return DispatchResult::Error(error),
            };
            let cursor = params.cursor.unwrap_or(0);
            return DispatchResult::Subscribe {
                response: json_rpc_success(
                    id,
                    json!({
                        "subscribed": true,
                        "requestedWire": BUILT_IN_CODE_EVENTS_SSE_WIRE_VERSION,
                        "requestedCursor": cursor,
                    }),
                ),
                cursor,
            };
        }
        "task.dispatch" => {
            let params = match parse_params::<TaskDispatchParams>(request.params) {
                Ok(params) => params,
                Err(error) => return DispatchResult::Error(error),
            };
            send_post(
                client,
                base_url,
                "/api/code/task/dispatch",
                control_token,
                Some(&params.controller_token),
                json!({ "agent": params.agent, "prompt": params.prompt }),
            )
            .await
        }
        "goal.start" => {
            // OC-Phase 6 P6.6 — Goal mode entrypoint for automation.
            // Same contract as the historical interactive `/goal start <objective>`
            // (parses the objective, validates shape, mints
            // `GoalEvent::Created` in the active session).
            let params = match parse_params::<GoalStartParams>(request.params) {
                Ok(params) => params,
                Err(error) => return DispatchResult::Error(error),
            };
            send_post(
                client,
                base_url,
                "/api/code/goal/start",
                control_token,
                Some(&params.controller_token),
                json!({ "objective": params.objective }),
            )
            .await
        }
        "goal.status" => {
            // Read-only observe endpoint (loopback only). No
            // controller token required at this layer.
            send_get(client, base_url, "/api/code/goal/status").await
        }
        "goal.cancel" => {
            let params = match parse_params::<GoalCancelParams>(request.params) {
                Ok(params) => params,
                Err(error) => return DispatchResult::Error(error),
            };
            send_post(
                client,
                base_url,
                "/api/code/goal/cancel",
                control_token,
                Some(&params.controller_token),
                json!({ "reason": params.reason }),
            )
            .await
        }
        _ => {
            return DispatchResult::Error(JsonRpcErrorObject {
                code: -32601,
                message: format!("Method not found: {method}"),
                data: None,
            });
        }
    };

    match result {
        Ok(result) if request.id.is_some() => {
            DispatchResult::Response(json_rpc_success(id, result))
        }
        Ok(_) => DispatchResult::NotificationOnly,
        Err(error) => DispatchResult::Error(error),
    }
}

fn parse_params<T: DeserializeOwned>(params: Option<Value>) -> Result<T, JsonRpcErrorObject> {
    let params = params.ok_or_else(|| JsonRpcErrorObject {
        code: -32602,
        message: "Invalid params: params object is required".to_string(),
        data: None,
    })?;
    serde_json::from_value(params).map_err(|error| JsonRpcErrorObject {
        code: -32602,
        message: format!("Invalid params: {error}"),
        data: None,
    })
}

fn parse_events_subscribe_params(
    params: Option<Value>,
) -> Result<EventsSubscribeParams, JsonRpcErrorObject> {
    let Some(params) = params else {
        return Ok(EventsSubscribeParams::default());
    };
    let Some(object) = params.as_object() else {
        return Err(JsonRpcErrorObject {
            code: -32602,
            message: "Invalid params: events.subscribe params must be an object".to_string(),
            data: None,
        });
    };
    let cursor =
        match object.get("cursor") {
            None => None,
            Some(cursor) => Some(cursor.as_u64().ok_or_else(|| {
                JsonRpcErrorObject {
                    code: -32602,
                    message:
                        "Invalid params: events.subscribe cursor must be a non-negative integer"
                            .to_string(),
                    data: None,
                }
            })?),
        };
    Ok(EventsSubscribeParams { cursor })
}

async fn send_get(
    client: &Client,
    base_url: &Url,
    endpoint: &str,
) -> Result<Value, JsonRpcErrorObject> {
    let url = endpoint_url(base_url, endpoint)?;
    let response = client.get(url).send().await.map_err(transport_error)?;
    response_json_or_error(response).await
}

async fn send_post(
    client: &Client,
    base_url: &Url,
    endpoint: &str,
    control_token: &str,
    controller_token: Option<&str>,
    body: Value,
) -> Result<Value, JsonRpcErrorObject> {
    let url = endpoint_url(base_url, endpoint)?;
    let request = client.post(url).json(&body);
    let request = apply_control_headers(request, control_token, controller_token);
    let response = request.send().await.map_err(transport_error)?;
    response_json_or_error(response).await
}

pub(crate) fn apply_control_headers(
    request: RequestBuilder,
    control_token: &str,
    controller_token: Option<&str>,
) -> RequestBuilder {
    let request = request.header("x-libra-control-token", control_token);
    if let Some(controller_token) = controller_token {
        request.header("x-code-controller-token", controller_token)
    } else {
        request
    }
}

async fn response_json_or_error(response: reqwest::Response) -> Result<Value, JsonRpcErrorObject> {
    let status = response.status();
    let body = response.text().await.map_err(transport_error)?;
    let parsed = if body.trim().is_empty() {
        Value::Null
    } else {
        serde_json::from_str::<Value>(&body).map_err(|error| JsonRpcErrorObject {
            code: -32603,
            message: format!("HTTP response was not valid JSON: {error}"),
            data: Some(json!({ "status": status.as_u16() })),
        })?
    };

    if status.is_success() {
        return Ok(parsed);
    }

    let libra_error = parsed.get("error");
    let libra_code = libra_error
        .and_then(|value| value.get("code"))
        .and_then(Value::as_str)
        .unwrap_or("HTTP_ERROR");
    let default_message = status.canonical_reason().unwrap_or("HTTP request failed");
    let libra_message = libra_error
        .and_then(|value| value.get("message"))
        .and_then(Value::as_str)
        .unwrap_or(default_message);
    Err(JsonRpcErrorObject {
        code: -32000,
        message: libra_message.to_string(),
        data: Some(json!({
            "status": status.as_u16(),
            "code": libra_code,
        })),
    })
}

fn transport_error(error: reqwest::Error) -> JsonRpcErrorObject {
    JsonRpcErrorObject {
        code: -32001,
        message: format!("Transport error: {error}"),
        data: None,
    }
}

fn endpoint_url(base_url: &Url, endpoint: &str) -> Result<Url, JsonRpcErrorObject> {
    let mut url = base_url.clone();
    let base_path = url.path().trim_end_matches('/');
    let endpoint = endpoint.trim_start_matches('/');
    url.set_path(&format!("{base_path}/{endpoint}"));
    url.set_query(None);
    url.set_fragment(None);
    Ok(url)
}

fn events_subscription_url(
    base_url: &Url,
    wire_version: u8,
    cursor: Option<u64>,
) -> Result<Url, JsonRpcErrorObject> {
    let mut url = endpoint_url(base_url, "/api/code/events")?;
    let wire = wire_version.to_string();
    url.query_pairs_mut().append_pair("wire", &wire);
    if wire_version == BUILT_IN_CODE_EVENTS_SSE_WIRE_VERSION
        && let Some(cursor) = cursor
    {
        url.query_pairs_mut()
            .append_pair("cursor", &cursor.to_string());
    }
    Ok(url)
}

#[cfg(test)]
fn built_in_events_subscription_url(
    base_url: &Url,
    cursor: u64,
) -> Result<Url, JsonRpcErrorObject> {
    events_subscription_url(
        base_url,
        BUILT_IN_CODE_EVENTS_SSE_WIRE_VERSION,
        Some(cursor),
    )
}

async fn request_event_stream(
    client: &Client,
    base_url: &Url,
    wire_version: u8,
    cursor: Option<u64>,
) -> CliResult<reqwest::Response> {
    let url = events_subscription_url(base_url, wire_version, cursor).map_err(|error| {
        CliError::fatal(format!(
            "failed to build events endpoint URL: {}",
            error.message
        ))
    })?;
    client.get(url).send().await.map_err(|error| {
        CliError::fatal(format!(
            "failed to subscribe to events using SSE wire {wire_version}: {error}"
        ))
    })
}

async fn event_stream_error_body(response: reqwest::Response) -> (StatusCode, String) {
    let status = response.status();
    let body = match response.text().await {
        Ok(body) => body,
        Err(error) => format!("failed to read error body: {error}"),
    };
    (status, body)
}

fn event_stream_error(status: StatusCode, body: &str) -> CliError {
    CliError::fatal(format!(
        "events.subscribe failed with HTTP {}: {}",
        status.as_u16(),
        body
    ))
}

fn event_stream_error_code(body: &str) -> Option<String> {
    serde_json::from_str::<Value>(body)
        .ok()?
        .get("error")?
        .get("code")?
        .as_str()
        .map(str::to_owned)
}

struct OpenedEventStream {
    response: reqwest::Response,
    recovery: Option<Value>,
}

async fn open_event_stream(
    client: &Client,
    base_url: &Url,
    cursor: u64,
) -> CliResult<OpenedEventStream> {
    let response = request_event_stream(
        client,
        base_url,
        BUILT_IN_CODE_EVENTS_SSE_WIRE_VERSION,
        Some(cursor),
    )
    .await?;
    if response.status() == StatusCode::OK {
        return Ok(OpenedEventStream {
            response,
            recovery: None,
        });
    }

    let (status, body) = event_stream_error_body(response).await;
    let code = event_stream_error_code(&body);
    if status == StatusCode::CONFLICT && code.as_deref() == Some(WIRE_V2_CURSOR_AHEAD) {
        let restarted = request_event_stream(
            client,
            base_url,
            BUILT_IN_CODE_EVENTS_SSE_WIRE_VERSION,
            Some(0),
        )
        .await?;
        if restarted.status() != StatusCode::OK {
            let (restart_status, restart_body) = event_stream_error_body(restarted).await;
            return Err(event_stream_error(restart_status, &restart_body));
        }
        return Ok(OpenedEventStream {
            response: restarted,
            recovery: Some(json!({
                "code": WIRE_V2_CURSOR_AHEAD,
                "reason": "the requested cursor belongs to a later or different durable session; the client dropped it and restarted from cursor 0",
                "lastCursor": cursor,
                "durableTail": 0,
                "action": "fetch_snapshot",
            })),
        });
    }
    // DF-08: the v1 snapshot fallback was removed together with the
    // server-side v1 wire (0.22.0). A session without a durable hub now
    // surfaces its stable 503 directly — there is no other wire to try.
    if status == StatusCode::SERVICE_UNAVAILABLE
        && code.as_deref() == Some(WIRE_V2_REQUIRES_DURABLE_SESSION)
    {
        return Err(event_stream_error(
            status,
            &format!(
                "{body}\n(events.subscribe requires a durable v2 session; the legacy v1 \
                 fallback was removed in 0.22.0 — v0.21.29 is the last release with wire v1)"
            ),
        ));
    }
    Err(event_stream_error(status, &body))
}

#[derive(Debug)]
enum EventStreamOutcome {
    Ended,
    ResyncRequired {
        data: Value,
        durable_tail: u64,
        forwarded: usize,
    },
}

/// DF-08: the v1 snapshot wire was deleted in 0.22.0 — an events stream
/// that emits any legacy envelope event name is a server regression the
/// automation client must fail on, never forward.
fn reject_legacy_v1_notification(notification: &Value) -> CliResult<()> {
    let event = notification["params"]["event"].as_str().unwrap_or_default();
    if matches!(
        event,
        "session_updated" | "status_changed" | "controller_changed"
    ) {
        return Err(CliError::fatal(format!(
            "events.subscribe received removed v1 envelope event '{event}' (SSE wire v1 was deleted in 0.22.0)"
        )));
    }
    Ok(())
}

fn resync_durable_tail(notification: &Value) -> CliResult<Option<u64>> {
    if notification
        .pointer("/params/event")
        .and_then(Value::as_str)
        != Some("resync")
    {
        return Ok(None);
    }
    let data = &notification["params"]["data"];
    if data.get("code").and_then(Value::as_str) != Some(WIRE_V2_RESYNC_REQUIRED) {
        return Err(CliError::fatal(format!(
            "events.subscribe received an invalid v2 resync event without code {WIRE_V2_RESYNC_REQUIRED}"
        )));
    }
    data.get("durableTail")
        .and_then(Value::as_u64)
        .map(Some)
        .ok_or_else(|| {
            CliError::fatal(
                "events.subscribe received a v2 resync event without numeric durableTail",
            )
        })
}

async fn forward_event_stream<F>(
    response: reqwest::Response,
    on_notification: &mut F,
) -> CliResult<EventStreamOutcome>
where
    F: FnMut(&Value) -> CliResult<()>,
{
    let mut parser = SseParser::default();
    let mut stream = response.bytes_stream();
    let mut forwarded = 0usize;
    while let Some(chunk) = stream.next().await {
        let chunk = chunk.map_err(|error| {
            CliError::fatal(format!("failed to read SSE event stream: {error}"))
        })?;
        for notification in parser.push(&chunk) {
            reject_legacy_v1_notification(&notification)?;
            if let Some(durable_tail) = resync_durable_tail(&notification)? {
                return Ok(EventStreamOutcome::ResyncRequired {
                    data: notification["params"]["data"].clone(),
                    durable_tail,
                    forwarded,
                });
            }
            on_notification(&notification)?;
            forwarded = forwarded.saturating_add(1);
        }
    }
    for notification in parser.finish() {
        reject_legacy_v1_notification(&notification)?;
        if let Some(durable_tail) = resync_durable_tail(&notification)? {
            return Ok(EventStreamOutcome::ResyncRequired {
                data: notification["params"]["data"].clone(),
                durable_tail,
                forwarded,
            });
        }
        on_notification(&notification)?;
        forwarded = forwarded.saturating_add(1);
    }
    Ok(EventStreamOutcome::Ended)
}

fn resync_notification(mut data: Value, snapshot: Value) -> Value {
    if let Some(object) = data.as_object_mut() {
        object.insert("snapshot".to_string(), snapshot);
    }
    json!({
        "jsonrpc": "2.0",
        "method": "events.notification",
        "params": {
            "event": "resync",
            "data": data,
        }
    })
}

async fn forward_resync_snapshot<F>(
    client: &Client,
    base_url: &Url,
    data: Value,
    on_notification: &mut F,
) -> CliResult<()>
where
    F: FnMut(&Value) -> CliResult<()>,
{
    let snapshot = send_get(client, base_url, "/api/code/session")
        .await
        .map_err(|error| {
            CliError::fatal(format!(
                "events.subscribe failed to fetch the session snapshot required for v2 resync: {}",
                error.message
            ))
        })?;
    on_notification(&resync_notification(data, snapshot))
}

async fn stream_events_with_handler<F>(
    client: &Client,
    base_url: &Url,
    cursor: u64,
    mut on_notification: F,
) -> CliResult<()>
where
    F: FnMut(&Value) -> CliResult<()>,
{
    let mut opened = open_event_stream(client, base_url, cursor).await?;
    if let Some(recovery) = opened.recovery.take() {
        forward_resync_snapshot(client, base_url, recovery, &mut on_notification).await?;
    }
    let mut consecutive_resyncs = 0usize;
    loop {
        let outcome = forward_event_stream(opened.response, &mut on_notification).await?;
        match outcome {
            EventStreamOutcome::Ended => return Ok(()),
            EventStreamOutcome::ResyncRequired {
                data,
                durable_tail,
                forwarded,
            } => {
                consecutive_resyncs = if forwarded == 0 {
                    consecutive_resyncs.saturating_add(1)
                } else {
                    1
                };
                if consecutive_resyncs > MAX_CONSECUTIVE_EVENT_STREAM_RESYNCS {
                    return Err(CliError::fatal(format!(
                        "events.subscribe could not recover after {MAX_CONSECUTIVE_EVENT_STREAM_RESYNCS} consecutive v2 resync requests"
                    )));
                }
                forward_resync_snapshot(client, base_url, data, &mut on_notification).await?;

                let response = request_event_stream(
                    client,
                    base_url,
                    BUILT_IN_CODE_EVENTS_SSE_WIRE_VERSION,
                    Some(durable_tail),
                )
                .await?;
                if response.status() != StatusCode::OK {
                    let (status, body) = event_stream_error_body(response).await;
                    return Err(event_stream_error(status, &body));
                }
                opened = OpenedEventStream {
                    response,
                    recovery: None,
                };
            }
        }
    }
}

async fn stream_events(client: &Client, base_url: &Url, cursor: u64) -> CliResult<()> {
    stream_events_with_handler(client, base_url, cursor, write_json_value).await
}

#[derive(Default)]
struct SseParser {
    pending: Vec<u8>,
    event_name: Option<String>,
    data_lines: Vec<String>,
}

impl SseParser {
    fn push(&mut self, chunk: &[u8]) -> Vec<Value> {
        self.pending.extend_from_slice(chunk);
        let mut notifications = Vec::new();
        while let Some(newline) = self.pending.iter().position(|byte| *byte == b'\n') {
            let line = self.pending.drain(..=newline).collect::<Vec<_>>();
            if let Some(notification) = self.process_line(&line) {
                notifications.push(notification);
            }
        }
        notifications
    }

    fn finish(&mut self) -> Vec<Value> {
        if !self.pending.is_empty() {
            let line = std::mem::take(&mut self.pending);
            if let Some(notification) = self.process_line(&line) {
                return vec![notification];
            }
        }
        self.dispatch_event().into_iter().collect()
    }

    fn process_line(&mut self, raw_line: &[u8]) -> Option<Value> {
        let mut line = String::from_utf8_lossy(raw_line).to_string();
        while line.ends_with('\n') || line.ends_with('\r') {
            line.pop();
        }
        if line.is_empty() {
            return self.dispatch_event();
        }
        if let Some(event) = line.strip_prefix("event:") {
            self.event_name = Some(event.trim().to_string());
        } else if let Some(data) = line.strip_prefix("data:") {
            self.data_lines.push(data.trim_start().to_string());
        }
        None
    }

    fn dispatch_event(&mut self) -> Option<Value> {
        if self.event_name.is_none() && self.data_lines.is_empty() {
            return None;
        }
        let event = self
            .event_name
            .take()
            .unwrap_or_else(|| "message".to_string());
        let data = self.data_lines.join("\n");
        self.data_lines.clear();
        let data = match serde_json::from_str::<Value>(&data) {
            Ok(value) => value,
            Err(_) => Value::String(data),
        };
        Some(json!({
            "jsonrpc": "2.0",
            "method": "events.notification",
            "params": {
                "event": event,
                "data": data,
            }
        }))
    }
}

fn json_rpc_success(id: Value, result: Value) -> JsonRpcResponse {
    JsonRpcResponse {
        jsonrpc: "2.0",
        result: Some(result),
        error: None,
        id,
    }
}

fn json_rpc_error(id: Value, error: JsonRpcErrorObject) -> JsonRpcResponse {
    JsonRpcResponse {
        jsonrpc: "2.0",
        result: None,
        error: Some(error),
        id,
    }
}

fn write_json_rpc_response(response: &JsonRpcResponse) -> CliResult<()> {
    write_json_value(&serde_json::to_value(response).map_err(|error| {
        CliError::fatal(format!("failed to serialize JSON-RPC response: {error}"))
    })?)
}

fn write_json_value(value: &Value) -> CliResult<()> {
    let line = serde_json::to_string(value)
        .map_err(|error| CliError::fatal(format!("failed to serialize JSON output: {error}")))?;
    let mut stdout = io::stdout().lock();
    stdout
        .write_all(line.as_bytes())
        .and_then(|_| stdout.write_all(b"\n"))
        .and_then(|_| stdout.flush())
        .map_err(|error| CliError::fatal(format!("failed to write JSON output: {error}")))
}

#[cfg(test)]
mod tests {
    use std::{collections::HashMap, net::SocketAddr, sync::Arc};

    use axum::{
        Json, Router,
        extract::{Query, State},
        http::HeaderMap,
        response::{IntoResponse, Response},
        routing::{get, post},
    };
    use tokio::sync::{Mutex, oneshot};

    use super::*;

    #[test]
    fn malformed_json_maps_to_parse_error() {
        let error = parse_json_rpc_request("{not-json").unwrap_err();

        assert_eq!(error.code, -32700);
    }

    #[test]
    fn ensure_loopback_control_url_rejects_remote_hosts() {
        let remote = Url::parse("https://evil.example/api").expect("url");
        let err = ensure_loopback_control_url(&remote).expect_err("remote must fail closed");
        assert!(
            err.to_string().contains("loopback"),
            "expected loopback guidance; got={err}"
        );

        let by_name = Url::parse("http://localhost:3000").expect("url");
        let err = ensure_loopback_control_url(&by_name).expect_err("localhost hostname rejected");
        assert!(
            err.to_string().contains("localhost") || err.to_string().contains("literal"),
            "expected hostname rejection; got={err}"
        );

        let local = Url::parse("http://127.0.0.1:3000").expect("url");
        ensure_loopback_control_url(&local).expect("loopback http must be accepted");
        let v6 = Url::parse("http://[::1]:3000").expect("url");
        ensure_loopback_control_url(&v6).expect("loopback ipv6 must be accepted");
    }

    #[test]
    fn sse_parser_emits_json_rpc_notifications() {
        let mut parser = SseParser::default();

        let output =
            parser.push(b"event: code_workflow\ndata: {\"cursor\":1,\"kind\":\"status\"}\n\n");

        assert_eq!(output.len(), 1);
        assert_eq!(output[0]["method"], "events.notification");
        assert_eq!(output[0]["params"]["event"], "code_workflow");
        assert_eq!(output[0]["params"]["data"]["cursor"], 1);
    }

    /// DF-08: any removed v1 envelope event name on the (v2-only) stream
    /// must fail the subscription instead of being forwarded.
    #[test]
    fn removed_v1_event_names_fail_the_subscription() {
        for legacy in ["session_updated", "status_changed", "controller_changed"] {
            let notification = json!({
                "method": "events.notification",
                "params": { "event": legacy, "data": {} }
            });
            let error = reject_legacy_v1_notification(&notification)
                .expect_err("legacy event names must be rejected");
            assert!(
                error.to_string().contains("deleted in 0.22.0"),
                "removal guidance expected: {error}"
            );
        }
        let ok = json!({
            "method": "events.notification",
            "params": { "event": "code_workflow", "data": {"cursor": 1} }
        });
        assert!(reject_legacy_v1_notification(&ok).is_ok());
    }

    #[test]
    fn resync_payload_validation_fails_closed() {
        let missing_code = json!({
            "params": {
                "event": "resync",
                "data": { "durableTail": 4 }
            }
        });
        let error = resync_durable_tail(&missing_code).expect_err("code is required");
        assert!(error.to_string().contains(WIRE_V2_RESYNC_REQUIRED));

        let invalid_tail = json!({
            "params": {
                "event": "resync",
                "data": {
                    "code": WIRE_V2_RESYNC_REQUIRED,
                    "durableTail": "four"
                }
            }
        });
        let error = resync_durable_tail(&invalid_tail).expect_err("numeric tail is required");
        assert!(error.to_string().contains("numeric durableTail"));
    }

    #[test]
    fn built_in_events_subscription_url_defaults_to_wire_v2_and_resumes_cursor() {
        let base = Url::parse("http://127.0.0.1:3000/control").expect("base url");
        let url = built_in_events_subscription_url(&base, 41).expect("events url");

        assert_eq!(url.path(), "/control/api/code/events");
        assert_eq!(url.query(), Some("wire=2&cursor=41"));
    }

    #[tokio::test]
    async fn event_stream_surfaces_durable_session_requirement_without_v1_fallback() {
        #[derive(Default)]
        struct MockState {
            wires: Mutex<Vec<String>>,
        }

        async fn events(
            State(state): State<Arc<MockState>>,
            Query(query): Query<HashMap<String, String>>,
        ) -> Response {
            let wire = query.get("wire").cloned().unwrap_or_default();
            let cursor = query.get("cursor").map(String::as_str).unwrap_or("-");
            state.wires.lock().await.push(format!("{wire}:{cursor}"));
            if wire == "2" {
                return (
                    StatusCode::SERVICE_UNAVAILABLE,
                    Json(json!({
                        "error": {
                            "code": "WIRE_V2_REQUIRES_DURABLE_SESSION",
                            "message": "durable session required"
                        }
                    })),
                )
                    .into_response();
            }
            (
                [("content-type", "text/event-stream")],
                "event: session_updated\ndata: {\"seq\":1}\n\n",
            )
                .into_response()
        }

        let state = Arc::new(MockState::default());
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("listener");
        let addr = listener.local_addr().expect("listener address");
        let app = Router::new()
            .route("/api/code/events", get(events))
            .with_state(state.clone());
        let (shutdown_tx, shutdown_rx) = oneshot::channel::<()>();
        let server = tokio::spawn(async move {
            axum::serve(listener, app)
                .with_graceful_shutdown(async move {
                    let _ = shutdown_rx.await;
                })
                .await
        });

        let base_url = Url::parse(&format!("http://{addr}")).expect("base url");
        // DF-08: the v1 fallback is gone — the durable-session requirement
        // surfaces directly, with removal guidance, after exactly ONE
        // (v2) request.
        let message = match open_event_stream(&Client::new(), &base_url, 41).await {
            Ok(_) => panic!("no durable session must be a terminal error now"),
            Err(error) => error.to_string(),
        };
        assert!(
            message.contains("WIRE_V2_REQUIRES_DURABLE_SESSION")
                && message.contains("removed in 0.22.0"),
            "durable-session error with removal guidance expected: {message}"
        );
        assert_eq!(
            *state.wires.lock().await,
            ["2:41"],
            "the client must not retry with the removed wire v1"
        );

        let _ = shutdown_tx.send(());
        let _ = server.await;
    }

    #[tokio::test]
    async fn event_stream_durable_session_requirement_reaches_the_handler_caller() {
        async fn events(Query(query): Query<HashMap<String, String>>) -> Response {
            if query.get("wire").map(String::as_str) == Some("2") {
                return (
                    StatusCode::SERVICE_UNAVAILABLE,
                    Json(json!({
                        "error": {
                            "code": WIRE_V2_REQUIRES_DURABLE_SESSION,
                            "message": "durable session required"
                        }
                    })),
                )
                    .into_response();
            }
            (
                [("content-type", "text/event-stream")],
                "event: session_updated\ndata: {\"seq\":1}\n\n",
            )
                .into_response()
        }

        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("listener");
        let addr = listener.local_addr().expect("listener address");
        let app = Router::new().route("/api/code/events", get(events));
        let (shutdown_tx, shutdown_rx) = oneshot::channel::<()>();
        let server = tokio::spawn(async move {
            axum::serve(listener, app)
                .with_graceful_shutdown(async move {
                    let _ = shutdown_rx.await;
                })
                .await
        });

        let base_url = Url::parse(&format!("http://{addr}")).expect("base url");
        let mut notifications = Vec::new();
        let error = stream_events_with_handler(&Client::new(), &base_url, 9, |notification| {
            notifications.push(notification.clone());
            Ok(())
        })
        .await
        .expect_err("DF-08: no v1 fallback — the 503 must reach the caller");
        assert!(
            error
                .to_string()
                .contains("WIRE_V2_REQUIRES_DURABLE_SESSION"),
            "stable code expected: {error}"
        );
        assert!(
            notifications.is_empty(),
            "no legacy notifications may be forwarded"
        );

        let _ = shutdown_tx.send(());
        let _ = server.await;
    }

    #[tokio::test]
    async fn event_stream_uses_v2_without_fallback_when_durable_session_is_available() {
        #[derive(Default)]
        struct MockState {
            wires: Mutex<Vec<String>>,
        }

        async fn events(
            State(state): State<Arc<MockState>>,
            Query(query): Query<HashMap<String, String>>,
        ) -> Response {
            let wire = query.get("wire").cloned().unwrap_or_default();
            let cursor = query.get("cursor").map(String::as_str).unwrap_or("-");
            state.wires.lock().await.push(format!("{wire}:{cursor}"));
            (
                [("content-type", "text/event-stream")],
                "event: code_workflow\ndata: {\"cursor\":1,\"kind\":\"projection.status_changed\"}\n\n",
            )
                .into_response()
        }

        let state = Arc::new(MockState::default());
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("listener");
        let addr = listener.local_addr().expect("listener address");
        let app = Router::new()
            .route("/api/code/events", get(events))
            .with_state(state.clone());
        let (shutdown_tx, shutdown_rx) = oneshot::channel::<()>();
        let server = tokio::spawn(async move {
            axum::serve(listener, app)
                .with_graceful_shutdown(async move {
                    let _ = shutdown_rx.await;
                })
                .await
        });

        let base_url = Url::parse(&format!("http://{addr}")).expect("base url");
        let opened = open_event_stream(&Client::new(), &base_url, 17)
            .await
            .expect("v2 event stream");
        assert_eq!(opened.response.status(), StatusCode::OK);
        assert!(
            opened
                .response
                .text()
                .await
                .expect("SSE body")
                .contains("code_workflow")
        );
        assert_eq!(*state.wires.lock().await, ["2:17"]);

        let _ = shutdown_tx.send(());
        let _ = server.await;
    }

    #[tokio::test]
    async fn event_stream_fetches_snapshot_and_resumes_from_resync_durable_tail() {
        #[derive(Default)]
        struct MockState {
            requests: Mutex<Vec<String>>,
            snapshot_fetches: Mutex<usize>,
        }

        async fn events(
            State(state): State<Arc<MockState>>,
            Query(query): Query<HashMap<String, String>>,
        ) -> Response {
            let wire = query.get("wire").cloned().unwrap_or_default();
            let cursor = query.get("cursor").cloned().unwrap_or_default();
            state.requests.lock().await.push(format!("{wire}:{cursor}"));
            let body = if cursor == "7" {
                concat!(
                    "event: resync\n",
                    "data: {\"code\":\"WIRE_V2_RESYNC_REQUIRED\",\"reason\":\"bootstrap_window_exceeded\",\"lastCursor\":7,\"durableTail\":41,\"action\":\"fetch_snapshot\"}\n\n"
                )
            } else {
                concat!(
                    "event: code_workflow\n",
                    "data: {\"cursor\":42,\"kind\":\"projection.status_changed\"}\n\n"
                )
            };
            ([("content-type", "text/event-stream")], body).into_response()
        }

        async fn session(State(state): State<Arc<MockState>>) -> Json<Value> {
            *state.snapshot_fetches.lock().await += 1;
            Json(json!({ "sessionId": "session-after-resync", "status": "idle" }))
        }

        let state = Arc::new(MockState::default());
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("listener");
        let addr = listener.local_addr().expect("listener address");
        let app = Router::new()
            .route("/api/code/events", get(events))
            .route("/api/code/session", get(session))
            .with_state(state.clone());
        let (shutdown_tx, shutdown_rx) = oneshot::channel::<()>();
        let server = tokio::spawn(async move {
            axum::serve(listener, app)
                .with_graceful_shutdown(async move {
                    let _ = shutdown_rx.await;
                })
                .await
        });

        let base_url = Url::parse(&format!("http://{addr}")).expect("base url");
        let mut notifications = Vec::new();
        stream_events_with_handler(&Client::new(), &base_url, 7, |notification| {
            notifications.push(notification.clone());
            Ok(())
        })
        .await
        .expect("resync recovery");

        assert_eq!(*state.requests.lock().await, ["2:7", "2:41"]);
        assert_eq!(*state.snapshot_fetches.lock().await, 1);
        assert_eq!(notifications.len(), 2);
        assert_eq!(notifications[0]["params"]["event"], "resync");
        assert_eq!(
            notifications[0]["params"]["data"]["snapshot"]["sessionId"],
            "session-after-resync"
        );
        assert_eq!(notifications[1]["params"]["event"], "code_workflow");
        assert_eq!(notifications[1]["params"]["data"]["cursor"], 42);

        let _ = shutdown_tx.send(());
        let _ = server.await;
    }

    #[tokio::test]
    async fn event_stream_recovers_from_cursor_ahead_with_snapshot_and_cursor_zero() {
        #[derive(Default)]
        struct MockState {
            requests: Mutex<Vec<String>>,
            snapshot_fetches: Mutex<usize>,
        }

        async fn events(
            State(state): State<Arc<MockState>>,
            Query(query): Query<HashMap<String, String>>,
        ) -> Response {
            let wire = query.get("wire").cloned().unwrap_or_default();
            let cursor = query.get("cursor").cloned().unwrap_or_default();
            state.requests.lock().await.push(format!("{wire}:{cursor}"));
            if cursor == "500" {
                return (
                    StatusCode::CONFLICT,
                    Json(json!({
                        "error": {
                            "code": WIRE_V2_CURSOR_AHEAD,
                            "message": "cursor is ahead of durable tail"
                        }
                    })),
                )
                    .into_response();
            }
            (
                [("content-type", "text/event-stream")],
                "event: code_workflow\ndata: {\"cursor\":1,\"kind\":\"projection.status_changed\"}\n\n",
            )
                .into_response()
        }

        async fn session(State(state): State<Arc<MockState>>) -> Json<Value> {
            *state.snapshot_fetches.lock().await += 1;
            Json(json!({ "sessionId": "new-session", "status": "idle" }))
        }

        let state = Arc::new(MockState::default());
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("listener");
        let addr = listener.local_addr().expect("listener address");
        let app = Router::new()
            .route("/api/code/events", get(events))
            .route("/api/code/session", get(session))
            .with_state(state.clone());
        let (shutdown_tx, shutdown_rx) = oneshot::channel::<()>();
        let server = tokio::spawn(async move {
            axum::serve(listener, app)
                .with_graceful_shutdown(async move {
                    let _ = shutdown_rx.await;
                })
                .await
        });

        let base_url = Url::parse(&format!("http://{addr}")).expect("base url");
        let mut notifications = Vec::new();
        stream_events_with_handler(&Client::new(), &base_url, 500, |notification| {
            notifications.push(notification.clone());
            Ok(())
        })
        .await
        .expect("ahead cursor recovery");

        assert_eq!(*state.requests.lock().await, ["2:500", "2:0"]);
        assert_eq!(*state.snapshot_fetches.lock().await, 1);
        assert_eq!(notifications.len(), 2);
        assert_eq!(
            notifications[0]["params"]["data"]["code"],
            WIRE_V2_CURSOR_AHEAD
        );
        assert_eq!(
            notifications[0]["params"]["data"]["snapshot"]["sessionId"],
            "new-session"
        );
        assert_eq!(notifications[1]["params"]["data"]["cursor"], 1);

        let _ = shutdown_tx.send(());
        let _ = server.await;
    }

    #[tokio::test]
    async fn event_stream_bounds_zero_progress_resync_recovery() {
        #[derive(Default)]
        struct MockState {
            requests: Mutex<usize>,
            snapshot_fetches: Mutex<usize>,
        }

        async fn events(State(state): State<Arc<MockState>>) -> Response {
            *state.requests.lock().await += 1;
            (
                [("content-type", "text/event-stream")],
                concat!(
                    "event: resync\n",
                    "data: {\"code\":\"WIRE_V2_RESYNC_REQUIRED\",\"reason\":\"bootstrap_window_exceeded\",\"lastCursor\":7,\"durableTail\":7,\"action\":\"fetch_snapshot\"}\n\n"
                ),
            )
                .into_response()
        }

        async fn session(State(state): State<Arc<MockState>>) -> Json<Value> {
            *state.snapshot_fetches.lock().await += 1;
            Json(json!({ "sessionId": "stuck-session" }))
        }

        let state = Arc::new(MockState::default());
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("listener");
        let addr = listener.local_addr().expect("listener address");
        let app = Router::new()
            .route("/api/code/events", get(events))
            .route("/api/code/session", get(session))
            .with_state(state.clone());
        let (shutdown_tx, shutdown_rx) = oneshot::channel::<()>();
        let server = tokio::spawn(async move {
            axum::serve(listener, app)
                .with_graceful_shutdown(async move {
                    let _ = shutdown_rx.await;
                })
                .await
        });

        let base_url = Url::parse(&format!("http://{addr}")).expect("base url");
        let mut notifications = Vec::new();
        let error = stream_events_with_handler(&Client::new(), &base_url, 7, |notification| {
            notifications.push(notification.clone());
            Ok(())
        })
        .await
        .expect_err("zero-progress resync loop must be bounded");

        assert!(error.to_string().contains("3 consecutive"));
        assert_eq!(*state.requests.lock().await, 4);
        assert_eq!(*state.snapshot_fetches.lock().await, 3);
        assert_eq!(notifications.len(), 3);

        let _ = shutdown_tx.send(());
        let _ = server.await;
    }

    #[tokio::test]
    async fn events_subscribe_dispatch_preserves_resume_cursor() {
        let base_url = Url::parse("http://127.0.0.1:9").expect("base url");
        let response = dispatch_json_rpc_request(
            &Client::new(),
            &base_url,
            "process-token",
            JsonRpcRequest {
                jsonrpc: Some("2.0".to_string()),
                method: Some("events.subscribe".to_string()),
                params: Some(json!({ "cursor": 41 })),
                id: Some(json!(7)),
            },
        )
        .await;

        let DispatchResult::Subscribe { response, cursor } = response else {
            panic!("events.subscribe must enter subscription mode");
        };
        assert_eq!(cursor, 41);
        let result = response.result.expect("subscription result");
        assert_eq!(result["subscribed"], true);
        assert_eq!(result["requestedWire"], 2);
        assert_eq!(result["requestedCursor"], 41);
    }

    #[tokio::test]
    async fn events_subscribe_dispatch_defaults_to_initial_cursor_zero() {
        let base_url = Url::parse("http://127.0.0.1:9").expect("base url");
        let response = dispatch_json_rpc_request(
            &Client::new(),
            &base_url,
            "process-token",
            JsonRpcRequest {
                jsonrpc: Some("2.0".to_string()),
                method: Some("events.subscribe".to_string()),
                params: None,
                id: Some(json!(8)),
            },
        )
        .await;

        let DispatchResult::Subscribe { response, cursor } = response else {
            panic!("events.subscribe must enter subscription mode");
        };
        assert_eq!(cursor, 0);
        assert_eq!(
            response.result.expect("subscription result")["requestedCursor"],
            0
        );
    }

    #[tokio::test]
    async fn events_subscribe_dispatch_rejects_negative_cursor() {
        let base_url = Url::parse("http://127.0.0.1:9").expect("base url");
        let response = dispatch_json_rpc_request(
            &Client::new(),
            &base_url,
            "process-token",
            JsonRpcRequest {
                jsonrpc: Some("2.0".to_string()),
                method: Some("events.subscribe".to_string()),
                params: Some(json!({ "cursor": -1 })),
                id: Some(json!(9)),
            },
        )
        .await;

        let DispatchResult::Error(error) = response else {
            panic!("negative cursor must be rejected before subscription");
        };
        assert_eq!(error.code, -32602);
        assert!(error.message.contains("cursor"));
    }

    #[tokio::test]
    async fn json_rpc_dispatch_maps_attach_submit_and_detach_to_http() {
        #[derive(Default)]
        struct MockState {
            calls: Mutex<Vec<Value>>,
        }

        async fn attach(
            State(state): State<Arc<MockState>>,
            headers: HeaderMap,
            Json(body): Json<Value>,
        ) -> Json<Value> {
            state
                .calls
                .lock()
                .await
                .push(json!({ "path": "attach", "token": headers.get("x-libra-control-token").and_then(|value| value.to_str().ok()), "body": body }));
            Json(json!({
                "controllerToken": "lease-token",
                "leaseExpiresAt": "2026-04-30T00:00:00Z",
                "controller": { "kind": "automation", "canWrite": true, "loopbackOnly": true }
            }))
        }

        async fn messages(
            State(state): State<Arc<MockState>>,
            headers: HeaderMap,
            Json(body): Json<Value>,
        ) -> Json<Value> {
            state
                .calls
                .lock()
                .await
                .push(json!({ "path": "messages", "token": headers.get("x-libra-control-token").and_then(|value| value.to_str().ok()), "controller": headers.get("x-code-controller-token").and_then(|value| value.to_str().ok()), "body": body }));
            Json(json!({ "accepted": true }))
        }

        async fn task_dispatch(
            State(state): State<Arc<MockState>>,
            headers: HeaderMap,
            Json(body): Json<Value>,
        ) -> Json<Value> {
            state
                .calls
                .lock()
                .await
                .push(json!({ "path": "task.dispatch", "token": headers.get("x-libra-control-token").and_then(|value| value.to_str().ok()), "controller": headers.get("x-code-controller-token").and_then(|value| value.to_str().ok()), "body": body }));
            Json(json!({ "accepted": true, "result": "Task `task-1` completed" }))
        }

        async fn detach(
            State(state): State<Arc<MockState>>,
            headers: HeaderMap,
            Json(body): Json<Value>,
        ) -> Json<Value> {
            state
                .calls
                .lock()
                .await
                .push(json!({ "path": "detach", "token": headers.get("x-libra-control-token").and_then(|value| value.to_str().ok()), "controller": headers.get("x-code-controller-token").and_then(|value| value.to_str().ok()), "body": body }));
            Json(json!({ "detached": true }))
        }

        let state = Arc::new(MockState::default());
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr: SocketAddr = listener.local_addr().unwrap();
        let app = Router::new()
            .route("/api/code/controller/attach", post(attach))
            .route("/api/code/messages", post(messages))
            .route("/api/code/task/dispatch", post(task_dispatch))
            .route("/api/code/controller/detach", post(detach))
            .route("/api/code/session", get(|| async { Json(json!({})) }))
            .with_state(state.clone());
        let (shutdown_tx, shutdown_rx) = oneshot::channel::<()>();
        let server = tokio::spawn(async move {
            axum::serve(listener, app)
                .with_graceful_shutdown(async move {
                    let _ = shutdown_rx.await;
                })
                .await
        });
        let base_url = Url::parse(&format!("http://{addr}")).unwrap();
        let client = Client::new();

        let attach_response = dispatch_json_rpc_request(
            &client,
            &base_url,
            "process-token",
            JsonRpcRequest {
                jsonrpc: Some("2.0".to_string()),
                method: Some("controller.attach".to_string()),
                params: Some(json!({ "clientId": "test-client", "kind": "automation" })),
                id: Some(json!(1)),
            },
        )
        .await;
        assert!(matches!(attach_response, DispatchResult::Response(_)));

        let submit_response = dispatch_json_rpc_request(
            &client,
            &base_url,
            "process-token",
            JsonRpcRequest {
                jsonrpc: Some("2.0".to_string()),
                method: Some("message.submit".to_string()),
                params: Some(json!({ "text": "hello", "controllerToken": "lease-token" })),
                id: Some(json!(2)),
            },
        )
        .await;
        assert!(matches!(submit_response, DispatchResult::Response(_)));

        let task_response = dispatch_json_rpc_request(
            &client,
            &base_url,
            "process-token",
            JsonRpcRequest {
                jsonrpc: Some("2.0".to_string()),
                method: Some("task.dispatch".to_string()),
                params: Some(json!({
                    "agent": "explorer",
                    "prompt": "grep TODO src/",
                    "controllerToken": "lease-token"
                })),
                id: Some(json!(4)),
            },
        )
        .await;
        assert!(matches!(task_response, DispatchResult::Response(_)));

        let detach_response = dispatch_json_rpc_request(
            &client,
            &base_url,
            "process-token",
            JsonRpcRequest {
                jsonrpc: Some("2.0".to_string()),
                method: Some("controller.detach".to_string()),
                params: Some(
                    json!({ "clientId": "test-client", "controllerToken": "lease-token" }),
                ),
                id: Some(json!(3)),
            },
        )
        .await;
        assert!(matches!(detach_response, DispatchResult::Response(_)));

        let calls = state.calls.lock().await.clone();
        assert_eq!(calls.len(), 4);
        assert_eq!(calls[0]["path"], "attach");
        assert_eq!(calls[0]["token"], "process-token");
        assert_eq!(calls[1]["path"], "messages");
        assert_eq!(calls[1]["controller"], "lease-token");
        assert_eq!(calls[2]["path"], "task.dispatch");
        assert_eq!(calls[2]["controller"], "lease-token");
        assert_eq!(calls[2]["body"]["agent"], "explorer");
        assert_eq!(calls[2]["body"]["prompt"], "grep TODO src/");
        assert_eq!(calls[3]["path"], "detach");

        let _ = shutdown_tx.send(());
        let _ = server.await;
    }
}
