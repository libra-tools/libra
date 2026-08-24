//! Bridge protocol v1 — the authoritative machine contract for
//! `libra agent bridge --stdio` (plan-20260818 LB-01, ADR-LB-01/02/03).
//!
//! Everything a peer must agree on to speak to the bridge lives here:
//! protocol version, capability matrix, the exact 20-method allowlist, the
//! v1 frame/batch limits, JSON-RPC 2.0 frame shapes and the stable error
//! catalogue with retryability. The TypeScript plugin consumes the fixture
//! generated from these constants; it must not redefine any of them.
//!
//! All constraints are `const` so the schema, tests and documentation can
//! never drift from the implementation (GC-LB-02).

use serde::Serialize;
use serde_json::Value;

/// Fixed bridge source identity (ADR-LB-02). `deepseek-harness` is NOT an
/// `AgentKind`; it is the source label on every bridge durable row.
pub const SOURCE_DEEPSEEK_HARNESS: &str = "deepseek-harness";

/// The negotiated protocol version. `major` is a hard fail-closed contract:
/// a peer that declares a different major is refused before any request is
/// processed (GC-LB-05). `minor` allows additive, backward-compatible fields
/// on the same major.
pub const PROTOCOL_MAJOR: u32 = 1;
pub const PROTOCOL_MINOR: u32 = 0;

/// JSON-RPC 2.0 framing (ADR-LB-01): newline-delimited JSON on stdout;
/// diagnostics only on stderr (GC-LB-04).
pub const JSONRPC_VERSION: &str = "2.0";

// ---------------------------------------------------------------------------
// v1 contract limits (shared with `docs/development/tracing/deepseek-harness.md`
// and both `plan-20260818.md` files; the fixture asserts these values).
// ---------------------------------------------------------------------------

/// A single NDJSON frame (request line or response line) may not exceed this.
pub const MAX_FRAME_BYTES: usize = 256 * 1024;
/// Maximum concurrent in-flight requests on one connection.
pub const MAX_INFLIGHT: usize = 64;
/// Maximum events in a single `event.append` batch.
pub const MAX_BATCH_EVENTS: usize = 64;
/// Maximum total payload bytes across one `event.append` batch.
pub const MAX_BATCH_BYTES: usize = 256 * 1024;
/// Maximum size of one event payload.
pub const MAX_EVENT_BYTES: usize = 256 * 1024;
/// Maximum size of one method result (response payload).
pub const MAX_RESULT_BYTES: usize = 256 * 1024;
/// Maximum rows returned for a history/checkpoint page.
pub const MAX_PAGE: usize = 100;
/// Maximum bridge session rows scanned in one paginated read.
pub const MAX_SESSION_PAGE: usize = 500;

// ---------------------------------------------------------------------------
// The v1 method allowlist (20 methods). Implementations must dispatch on
// these literal names; the model-visible tool aliases (`libra_context`, …)
// are a TypeScript facade and are NOT bridge methods.
// ---------------------------------------------------------------------------

/// The complete, ordered v1 method allowlist. This is the single source of
/// truth for which methods the bridge may ever dispatch; unknown methods are
/// rejected fail-closed (GC-LB-05).
pub const METHOD_ALLOWLIST: &[&str] = &[
    "initialize",
    "session.open",
    "event.append",
    "session.flush",
    "session.close",
    "evidence.append",
    "provenance.append",
    "context.get",
    "status.get",
    "diff.get",
    "history.search",
    "checkpoint.create",
    "checkpoint.list",
    "checkpoint.show",
    "checkpoint.restore",
    "commit.create",
    "review.run",
    "workspace.claim",
    "workspace.renew",
    "workspace.release",
];

/// The allowlist as an efficient set for O(1) membership checks.
pub struct MethodAllowlist;

impl MethodAllowlist {
    /// `true` when `method` is one of the 20 v1 methods.
    pub fn contains(method: &str) -> bool {
        METHOD_ALLOWLIST.contains(&method)
    }
}

/// Protocol version handed to a peer in the `initialize` result and echoed on
/// every response so the peer can detect silent downgrade (GC-LB-05).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub struct ProtocolVersion {
    pub major: u32,
    pub minor: u32,
}

impl ProtocolVersion {
    pub const fn current() -> Self {
        Self {
            major: PROTOCOL_MAJOR,
            minor: PROTOCOL_MINOR,
        }
    }
}

/// The capabilities a peer negotiates and the bridge confirms. `methods` is
/// the full allowlist; a peer may advertise a subset it intends to use, but
/// the bridge always enforces the full allowlist server-side.
#[derive(Debug, Clone, Serialize)]
pub struct BridgeCapability {
    pub protocol: ProtocolVersion,
    /// Frame/batch/result limits the peer must respect.
    pub limits: BridgeLimits,
    /// Methods the bridge accepts. Always the full allowlist.
    pub methods: Vec<&'static str>,
    /// Fixed source identity for this bridge.
    pub source: &'static str,
}

/// Hard v1 limits advertised to the peer (GC-LB-11). The fixture asserts
/// these exact values.
#[derive(Debug, Clone, Serialize)]
pub struct BridgeLimits {
    pub max_frame_bytes: usize,
    pub max_inflight: usize,
    pub max_batch_events: usize,
    pub max_batch_bytes: usize,
    pub max_event_bytes: usize,
    pub max_result_bytes: usize,
    pub max_page: usize,
    pub request_deadline_secs: u64,
}

impl BridgeLimits {
    pub const fn v1() -> Self {
        Self {
            max_frame_bytes: MAX_FRAME_BYTES,
            max_inflight: MAX_INFLIGHT,
            max_batch_events: MAX_BATCH_EVENTS,
            max_batch_bytes: MAX_BATCH_BYTES,
            max_event_bytes: MAX_EVENT_BYTES,
            max_result_bytes: MAX_RESULT_BYTES,
            max_page: MAX_PAGE,
            request_deadline_secs: super::REQUEST_DEADLINE_SECS,
        }
    }
}

/// A parsed inbound JSON-RPC 2.0 request (or notification when `id` is
/// absent). Extra fields are preserved for schema validation.
#[derive(Debug, Clone)]
pub struct BridgeRequest {
    pub jsonrpc: String,
    pub method: String,
    pub params: Option<Value>,
    pub id: Option<Value>,
}

/// A JSON-RPC 2.0 response (result or error).
#[derive(Debug, Clone, Serialize)]
pub struct BridgeResponse {
    pub jsonrpc: &'static str,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub result: Option<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<BridgeErrorObject>,
    pub id: Value,
}

/// The stable, retryable error catalogue for the bridge. Every error carries a
/// stable `code`, an actionable `message`, retryability, and (when known) the
/// request id and resource scope.
#[derive(Debug, Clone, Serialize)]
pub struct BridgeErrorObject {
    pub code: i64,
    pub message: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub data: Option<BridgeErrorData>,
}

/// Structured error data beyond the message — stable sub-code, retryability
/// and the affected resource scope (GC-LB-06).
#[derive(Debug, Clone, Serialize)]
pub struct BridgeErrorData {
    pub stable_code: &'static str,
    pub retryable: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub resource: Option<String>,
}

/// JSON-RPC 2.0 standard error codes plus the bridge's own domain codes.
pub mod code {
    // JSON-RPC 2.0 standard codes.
    pub const PARSE_ERROR: i64 = -32700;
    pub const INVALID_REQUEST: i64 = -32600;
    pub const METHOD_NOT_FOUND: i64 = -32601;
    pub const INVALID_PARAMS: i64 = -32602;
    pub const INTERNAL_ERROR: i64 = -32603;

    // Bridge domain codes (start at 1000 to avoid collision with the
    // JSON-RPC standard range and Code control's transport codes).
    /// Protocol major mismatch — the peer speaks a different protocol major.
    pub const PROTOCOL_MAJOR_MISMATCH: i64 = 1000;
    /// A frame exceeded the v1 byte cap.
    pub const FRAME_TOO_LARGE: i64 = 1001;
    /// The connection has too many in-flight requests.
    pub const INFLIGHT_LIMIT: i64 = 1002;
    /// A method is not in the v1 allowlist.
    pub const UNKNOWN_METHOD: i64 = 1003;
    /// Method params failed schema/limit validation.
    pub const INVALID_PARAMS_BRIDGE: i64 = 1004;
    /// The method is allowlisted but not implemented by this build yet.
    pub const NOT_IMPLEMENTED: i64 = 1005;
    /// Repository/worktree/workspace/actor scope conflict (fail-closed).
    pub const SCOPE_MISMATCH: i64 = 1006;
    /// A duplicate `(session_id, event_seq)` carried a different digest.
    pub const DIGEST_CONFLICT: i64 = 1007;
    /// Redaction or payload-size validation could not be satisfied; the
    /// original payload was refused (no raw fallback).
    pub const REDACTION_FAILED: i64 = 1008;
    /// A mutating operation was denied by policy or approval.
    pub const DENIED: i64 = 1009;
    /// A request did not complete within its deadline.
    pub const TIMEOUT: i64 = 1010;
    /// A mutation's pre-checked fence (HEAD / index / worktree) drifted, so the
    /// operation was refused before any write.
    pub const STALE_FENCE: i64 = 1011;
}

/// A structured bridge error combining a JSON-RPC code, a stable `LBR-*`
/// label and retryability. Used to build both protocol-level rejections and
/// method-level failures.
#[derive(Debug, Clone)]
pub struct BridgeError {
    pub code: i64,
    pub stable_code: &'static str,
    pub message: String,
    pub retryable: bool,
    pub resource: Option<String>,
}

impl BridgeError {
    pub fn new(code: i64, stable_code: &'static str, message: impl Into<String>) -> Self {
        Self {
            code,
            stable_code,
            message: message.into(),
            retryable: false,
            resource: None,
        }
    }

    pub fn retryable(mut self) -> Self {
        self.retryable = true;
        self
    }

    pub fn resource(mut self, resource: impl Into<String>) -> Self {
        self.resource = Some(resource.into());
        self
    }

    /// Standard JSON-RPC parse error.
    pub fn parse_error(message: impl Into<String>) -> Self {
        Self::new(code::PARSE_ERROR, "LBR-AGENT-024", message)
    }

    /// Standard JSON-RPC invalid request.
    pub fn invalid_request(message: impl Into<String>) -> Self {
        Self::new(code::INVALID_REQUEST, "LBR-AGENT-025", message)
    }

    /// Standard JSON-RPC method not found (not in allowlist).
    pub fn method_not_found(method: &str) -> Self {
        Self::new(
            code::METHOD_NOT_FOUND,
            "LBR-AGENT-026",
            format!("unknown bridge method '{method}'; the v1 allowlist has 20 methods"),
        )
    }

    /// Standard JSON-RPC invalid params.
    pub fn invalid_params(message: impl Into<String>) -> Self {
        Self::new(code::INVALID_PARAMS, "LBR-AGENT-027", message)
    }

    /// A frame exceeded the byte cap.
    pub fn frame_too_large(actual: usize) -> Self {
        Self::new(
            code::FRAME_TOO_LARGE,
            "LBR-AGENT-028",
            format!(
                "bridge frame exceeds the v1 cap of {MAX_FRAME_BYTES} bytes (got {actual}); refusing"
            ),
        )
        .resource("frame")
    }

    /// Too many in-flight requests.
    pub fn inflight_limit() -> Self {
        Self::new(
            code::INFLIGHT_LIMIT,
            "LBR-AGENT-029",
            format!("bridge connection already has {MAX_INFLIGHT} in-flight requests"),
        )
        .resource("connection")
    }

    /// Allowlisted but not implemented by this build yet.
    pub fn not_implemented(method: &str) -> Self {
        Self::new(
            code::NOT_IMPLEMENTED,
            "LBR-AGENT-030",
            format!("bridge method '{method}' is allowlisted but not implemented by this build"),
        )
    }

    /// Protocol major mismatch — fail-closed before any request processing.
    pub fn major_mismatch(peer_major: u64) -> Self {
        Self::new(
            code::PROTOCOL_MAJOR_MISMATCH,
            "LBR-AGENT-031",
            format!(
                "bridge protocol major mismatch: peer speaks v{peer_major}.x but this build is v{PROTOCOL_MAJOR}.{PROTOCOL_MINOR}; refusing all requests"
            ),
        )
    }

    /// Scope conflict — a self-reported or stale scope disagrees with the
    /// trusted handshake/context (GC-LB-06 / GC-LB-07).
    pub fn scope_mismatch(message: impl Into<String>) -> Self {
        Self::new(code::SCOPE_MISMATCH, "LBR-AGENT-032", message).resource("scope")
    }

    /// Payload digest conflict — a duplicate `(session_id,event_seq)` or
    /// `operation_id` (or an association relink) carried a different payload /
    /// target than the durable row. Fail-closed; never last-writer-wins.
    pub fn digest_conflict(message: impl Into<String>) -> Self {
        Self::new(code::DIGEST_CONFLICT, "LBR-AGENT-033", message).resource("payload")
    }

    /// A mutating operation was denied by policy/approval.
    pub fn denied(message: impl Into<String>) -> Self {
        Self::new(code::DENIED, "LBR-AGENT-035", message).resource("operation")
    }

    /// Workspace lease held — another live record already claims this linked
    /// identity or canonical path (LB-06). Maps to the existing dedicated
    /// `LBR-AGENT-022` code so a client can distinguish \"lease held, backoff
    /// and retry\" from an unrelated denial. Retryable: the holder may release
    /// or expire the lease, so a bounded backoff-and-retry is appropriate.
    pub fn workspace_lease_held(message: impl Into<String>) -> Self {
        Self::new(code::DENIED, "LBR-AGENT-022", message)
            .retryable()
            .resource("workspace")
    }

    /// Workspace lease owner/fence is stale — reclaimed with a higher fence or
    /// already settled (LB-06). Maps to the existing dedicated `LBR-AGENT-023`
    /// code so a client can distinguish \"lease lost\" from a scope mismatch.
    /// Not retryable: a lost lease requires re-claiming, not a bare retry.
    pub fn workspace_lease_lost(message: impl Into<String>) -> Self {
        Self::new(code::SCOPE_MISMATCH, "LBR-AGENT-023", message).resource("workspace")
    }

    /// A request timed out.
    pub fn timeout(message: impl Into<String>) -> Self {
        Self::new(code::TIMEOUT, "LBR-AGENT-036", message)
            .retryable()
            .resource("request")
    }

    /// Standard JSON-RPC internal error (DB / VCS / generic failure).
    pub fn internal(message: impl Into<String>) -> Self {
        Self::new(code::INTERNAL_ERROR, "LBR-AGENT-037", message)
    }

    /// A mutation's pre-checked fence drifted between admission and execution:
    /// HEAD moved, or the index/worktree is dirty when the operation requires a
    /// clean one (LB-05). Fail-closed **before** any write, so nothing is
    /// partially applied. Not retryable as-is: the caller must re-read the
    /// current state and re-issue with a fresh fence.
    pub fn stale_fence(message: impl Into<String>) -> Self {
        Self::new(code::STALE_FENCE, "LBR-AGENT-038", message).resource("worktree")
    }
}

impl BridgeError {
    /// Render as a JSON-RPC error object for a given request id.
    pub fn into_object(self, id: Value) -> BridgeResponse {
        BridgeResponse {
            jsonrpc: JSONRPC_VERSION,
            result: None,
            error: Some(BridgeErrorObject {
                code: self.code,
                message: self.message,
                data: Some(BridgeErrorData {
                    stable_code: self.stable_code,
                    retryable: self.retryable,
                    resource: self.resource,
                }),
            }),
            id,
        }
    }
}

impl From<BridgeError> for BridgeResponse {
    fn from(err: BridgeError) -> Self {
        err.into_object(Value::Null)
    }
}

/// Parse a single NDJSON line into a `BridgeRequest`, applying JSON-RPC 2.0
/// validation. Returns a `BridgeError` suitable for a response with the
/// request id when it can be extracted.
pub fn parse_request_line(line: &str) -> Result<BridgeRequest, BridgeError> {
    let value: Value = serde_json::from_str(line)
        .map_err(|e| BridgeError::parse_error(format!("invalid JSON frame: {e}")))?;
    let obj = value
        .as_object()
        .ok_or_else(|| BridgeError::invalid_request("request frame must be a JSON object"))?;

    let jsonrpc = obj
        .get("jsonrpc")
        .and_then(Value::as_str)
        .ok_or_else(|| BridgeError::invalid_request("missing or non-string 'jsonrpc' field"))?
        .to_string();
    if jsonrpc != JSONRPC_VERSION {
        return Err(BridgeError::invalid_request(format!(
            "unsupported jsonrpc version '{jsonrpc}'; expected '{JSONRPC_VERSION}'"
        )));
    }

    let method = obj
        .get("method")
        .and_then(Value::as_str)
        .ok_or_else(|| BridgeError::invalid_request("missing or non-string 'method' field"))?
        .to_string();

    let params = obj.get("params").cloned();
    let id = obj.get("id").cloned();

    Ok(BridgeRequest {
        jsonrpc,
        method,
        params,
        id,
    })
}

/// Validate that a method is in the v1 allowlist, returning a `MethodNotFound`
/// style error otherwise.
pub fn validate_method(method: &str) -> Result<(), BridgeError> {
    if MethodAllowlist::contains(method) {
        Ok(())
    } else {
        Err(BridgeError::method_not_found(method))
    }
}

/// Enforce the v1 frame byte cap on an inbound line.
pub fn check_frame_size(line: &str) -> Result<(), BridgeError> {
    if line.len() > MAX_FRAME_BYTES {
        return Err(BridgeError::frame_too_large(line.len()));
    }
    Ok(())
}

/// The LB-01 handshake dispatcher: handles `initialize` (protocol/capability
/// negotiation) and rejects every other allowlisted method as not-yet
/// implemented. Higher cards (LB-03..LB-06) extend dispatch.
///
/// Returns `Ok(Some(response))` for a request. Notification handling (no
/// reply for a frame without an `id`) is the transport's responsibility
/// (GC-LB-05); this dispatcher always produces a response, and the transport
/// suppresses it for notifications. `Err` is a fail-closed rejection.
pub fn dispatch_handshake(request: &BridgeRequest) -> Result<Option<BridgeResponse>, BridgeError> {
    match request.method.as_str() {
        "initialize" => {
            let params = request.params.as_ref();
            let major = params
                .and_then(|p| p.get("protocol"))
                .and_then(|p| p.get("major"))
                .and_then(Value::as_u64)
                .ok_or_else(|| {
                    BridgeError::invalid_params("initialize requires params.protocol.major (u32)")
                })?;
            if major != PROTOCOL_MAJOR as u64 {
                return Err(BridgeError::major_mismatch(major));
            }

            // Scope binding happens at session.open / the trusted context
            // (GC-LB-06/07): the handshake itself only negotiates protocol and
            // capabilities, and never trusts a self-reported repository id.

            let capability = BridgeCapability {
                protocol: ProtocolVersion::current(),
                limits: BridgeLimits::v1(),
                methods: METHOD_ALLOWLIST.to_vec(),
                source: SOURCE_DEEPSEEK_HARNESS,
            };
            let result = serde_json::to_value(capability).map_err(|e| {
                BridgeError::internal(format!("failed to serialize capability: {e}"))
            })?;
            let id = request.id.clone().unwrap_or(Value::Null);
            Ok(Some(BridgeResponse {
                jsonrpc: JSONRPC_VERSION,
                result: Some(result),
                error: None,
                id,
            }))
        }
        other => Ok(Some(
            BridgeError::not_implemented(other)
                .into_object(request.id.clone().unwrap_or(Value::Null)),
        )),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn allowlist_has_exactly_20_methods() {
        assert_eq!(METHOD_ALLOWLIST.len(), 20);
        // The four handshake/session/ingress methods, projection, reads,
        // mutation and workspace groups must all be present.
        for m in [
            "initialize",
            "session.open",
            "event.append",
            "session.flush",
            "session.close",
            "evidence.append",
            "provenance.append",
            "context.get",
            "status.get",
            "diff.get",
            "history.search",
            "checkpoint.create",
            "checkpoint.list",
            "checkpoint.show",
            "checkpoint.restore",
            "commit.create",
            "review.run",
            "workspace.claim",
            "workspace.renew",
            "workspace.release",
        ] {
            assert!(
                MethodAllowlist::contains(m),
                "missing allowlisted method {m}"
            );
        }
    }

    #[test]
    fn allowlist_rejects_unknown_methods() {
        assert!(!MethodAllowlist::contains("create_intent"));
        assert!(!MethodAllowlist::contains("session.get"));
        assert!(!MethodAllowlist::contains("shell.exec"));
    }

    #[test]
    fn v1_limits_match_the_documented_contract() {
        let limits = BridgeLimits::v1();
        assert_eq!(limits.max_frame_bytes, 256 * 1024);
        assert_eq!(limits.max_inflight, 64);
        assert_eq!(limits.max_batch_events, 64);
        assert_eq!(limits.max_batch_bytes, 256 * 1024);
        assert_eq!(limits.max_event_bytes, 256 * 1024);
        assert_eq!(limits.max_result_bytes, 256 * 1024);
        assert_eq!(limits.max_page, 100);
        assert_eq!(limits.request_deadline_secs, 30);
    }

    #[test]
    fn parses_a_valid_request_line() {
        let req = parse_request_line(
            r#"{"jsonrpc":"2.0","method":"initialize","params":{"protocol":{"major":1,"minor":0}},"id":1}"#,
        )
        .expect("valid request parses");
        assert_eq!(req.method, "initialize");
        assert_eq!(req.id, Some(Value::from(1)));
    }

    #[test]
    fn rejects_non_json_and_non_object_frames() {
        assert!(parse_request_line("not json").is_err());
        assert!(parse_request_line("[1,2,3]").is_err());
        assert!(parse_request_line(r#"{"jsonrpc":"1.0","method":"initialize"}"#).is_err());
        assert!(parse_request_line(r#"{"jsonrpc":"2.0"}"#).is_err());
    }

    #[test]
    fn major_mismatch_fails_closed() {
        let err = BridgeError::major_mismatch(2);
        assert_eq!(err.code, code::PROTOCOL_MAJOR_MISMATCH);
        assert!(!err.retryable);
    }

    #[test]
    fn frame_cap_is_enforced() {
        let huge = "x".repeat(MAX_FRAME_BYTES + 1);
        let err = check_frame_size(&huge).expect_err("oversized frame rejected");
        assert_eq!(err.code, code::FRAME_TOO_LARGE);
    }

    /// Every bridge stable error code maps to exactly one semantic, matching
    /// `docs/error-codes.md` LBR-AGENT-024..038 (and the pre-existing
    /// workspace lease codes LBR-AGENT-022/023, reused per LB-06). This pins
    /// the protocol-authority contract so a future renumbering cannot silently
    /// drift from the docs the TypeScript plugin consumes (GC-LB-02). The
    /// redaction code (034) lives in `redaction.rs`; it is asserted here so a
    /// cross-module collision is caught.
    #[test]
    fn bridge_error_stable_codes_are_unique_and_documented() {
        // (constructor, expected code) — one row per bridge error. The
        // workspace lease codes 022/023 are pre-existing StableErrorCode
        // values reused by the bridge (LB-06) and are included here so their
        // uniqueness is pinned against the bridge's own 024..038 catalogue.
        let pairs: &[(&'static str, &str)] = &[
            (BridgeError::parse_error("x").stable_code, "LBR-AGENT-024"),
            (
                BridgeError::invalid_request("x").stable_code,
                "LBR-AGENT-025",
            ),
            (
                BridgeError::method_not_found("m").stable_code,
                "LBR-AGENT-026",
            ),
            (
                BridgeError::invalid_params("x").stable_code,
                "LBR-AGENT-027",
            ),
            (BridgeError::frame_too_large(1).stable_code, "LBR-AGENT-028"),
            (BridgeError::inflight_limit().stable_code, "LBR-AGENT-029"),
            (
                BridgeError::not_implemented("m").stable_code,
                "LBR-AGENT-030",
            ),
            (BridgeError::major_mismatch(2).stable_code, "LBR-AGENT-031"),
            (
                BridgeError::scope_mismatch("x").stable_code,
                "LBR-AGENT-032",
            ),
            (
                BridgeError::digest_conflict("x").stable_code,
                "LBR-AGENT-033",
            ),
            // 034 is redaction_failed (redaction.rs) — asserted below.
            (BridgeError::denied("x").stable_code, "LBR-AGENT-035"),
            (BridgeError::timeout("x").stable_code, "LBR-AGENT-036"),
            (BridgeError::internal("x").stable_code, "LBR-AGENT-037"),
            (BridgeError::stale_fence("x").stable_code, "LBR-AGENT-038"),
            // LB-06 workspace lease codes (pre-existing StableErrorCode).
            (
                BridgeError::workspace_lease_held("x").stable_code,
                "LBR-AGENT-022",
            ),
            (
                BridgeError::workspace_lease_lost("x").stable_code,
                "LBR-AGENT-023",
            ),
        ];
        let mut seen: Vec<&'static str> = Vec::new();
        for (code, expected) in pairs {
            assert_eq!(
                *code, *expected,
                "stable code drifted from the documented contract"
            );
            assert!(
                !seen.contains(code),
                "stable code {code} is reused for two semantics"
            );
            seen.push(code);
        }
        // redaction_failed (redaction.rs) must be 034 and distinct from every
        // protocol constructor.
        let redact = super::super::redaction::redaction_failed("x").stable_code;
        assert_eq!(redact, "LBR-AGENT-034");
        assert!(
            !seen.contains(&redact),
            "LBR-AGENT-034 must be unique to redaction"
        );
        // The two workspace lease codes must match the documented StableErrorCode
        // spellings (src/utils/error.rs) so the bridge and CLI never disagree.
        assert_eq!(
            BridgeError::workspace_lease_held("x").stable_code,
            crate::utils::error::StableErrorCode::AgentWorkspaceLeaseHeld.as_str()
        );
        assert_eq!(
            BridgeError::workspace_lease_lost("x").stable_code,
            crate::utils::error::StableErrorCode::AgentWorkspaceLeaseLost.as_str()
        );
    }
}
