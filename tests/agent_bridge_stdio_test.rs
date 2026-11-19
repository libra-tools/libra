//! `tests/agent_bridge_stdio_test.rs` — stdout-purity and framing tests for
//! the bridge transport (plan-20260818 LB-01).
//!
//! Proves the transport satisfies GC-LB-04 (stdout carries exactly one
//! parseable NDJSON frame per response, nothing else) and ER-LB-09 (errors
//! never masquerade as a success ack; malformed frames don't poison stdout).

use std::io::Cursor;

use libra::internal::ai::agent_bridge::transport::{HandshakeHandler, run};

/// A minor-0 handshake cannot unlock experimental VCS methods, and it still
/// serves the original allowlist.
#[tokio::test]
async fn minor_zero_handshake_rejects_unnegotiated_vcs_methods() {
    let input = concat!(
        r#"{"jsonrpc":"2.0","method":"initialize","params":{"protocol":{"major":1,"minor":0}},"id":1}"#,
        "\n",
        r#"{"jsonrpc":"2.0","method":"vcs.commit","id":2}"#,
        "\n",
        r#"{"jsonrpc":"2.0","method":"status.get","id":3}"#,
        "\n",
    );
    let mut output = Vec::new();
    run(
        Cursor::new(input),
        &mut output,
        &HandshakeHandler,
        std::time::Duration::from_secs(30),
    )
    .await
    .expect("transport");
    let responses: Vec<serde_json::Value> = String::from_utf8(output)
        .expect("utf8")
        .lines()
        .map(|line| serde_json::from_str(line).expect("frame"))
        .collect();
    assert!(responses[0].get("result").is_some());
    assert!(
        !responses[0]["result"]["methods"]
            .as_array()
            .expect("methods")
            .iter()
            .any(|method| method == "vcs.fence")
    );
    assert!(
        responses[1]["error"]["message"]
            .as_str()
            .unwrap_or_default()
            .contains("requires negotiated protocol minor")
    );
    assert_eq!(
        responses[2]["error"]["data"]["stable_code"], "LBR-AGENT-030",
        "status.get remains accepted and reaches the handshake handler"
    );
}
#[tokio::test]
async fn disabled_vcs_methods_never_reach_the_handler() {
    use std::sync::atomic::{AtomicUsize, Ordering};

    use async_trait::async_trait;
    use libra::internal::ai::agent_bridge::{
        protocol::{BridgeError, BridgeRequest, BridgeResponse},
        transport::BridgeHandler,
    };

    struct SideEffectHandler(AtomicUsize);
    #[async_trait(?Send)]
    impl BridgeHandler for SideEffectHandler {
        async fn handle(
            &self,
            request: &BridgeRequest,
        ) -> Result<Option<BridgeResponse>, BridgeError> {
            self.0.fetch_add(1, Ordering::SeqCst);
            Ok(Some(BridgeResponse {
                jsonrpc: "2.0",
                result: Some(serde_json::json!({"handled": true})),
                error: None,
                id: request.id.clone().unwrap_or(serde_json::Value::Null),
            }))
        }
    }
    let names = [
        "vcs.identity",
        "vcs.status",
        "vcs.diff",
        "vcs.log",
        "vcs.show",
        "vcs.stage",
        "vcs.unstage",
        "vcs.commit",
    ];
    let mut input = String::new();
    for (id, method) in names.iter().enumerate() {
        input.push_str(&format!(
            "{}\n",
            serde_json::json!({"jsonrpc":"2.0", "method":method, "id":id})
        ));
    }
    input.push_str("{\"jsonrpc\":\"2.0\",\"method\":\"status.get\",\"id\":99}\n");
    let handler = SideEffectHandler(AtomicUsize::new(0));
    let mut output = Vec::new();
    run(
        Cursor::new(input),
        &mut output,
        &handler,
        std::time::Duration::from_secs(30),
    )
    .await
    .expect("transport");
    let text = String::from_utf8(output).expect("utf8");
    let responses: Vec<serde_json::Value> = text
        .lines()
        .map(|line| serde_json::from_str(line).expect("frame"))
        .collect();
    assert_eq!(responses.len(), names.len() + 1);
    for (id, response) in responses[..names.len()].iter().enumerate() {
        assert_eq!(response["id"], id);
        assert_eq!(response["error"]["code"], -32602);
        assert!(
            response["error"]["message"]
                .as_str()
                .unwrap_or_default()
                .contains("requires negotiated protocol minor")
        );
        assert!(response.get("result").is_none());
    }
    assert_eq!(responses[names.len()]["result"]["handled"], true);
    assert_eq!(
        handler.0.load(Ordering::SeqCst),
        1,
        "only the existing method reached the handler"
    );
}

/// Every stdout line must be a valid NDJSON frame; diagnostics never reach
/// stdout.
#[tokio::test]
async fn stdout_contains_only_frames() {
    // A mixed stream: one valid initialize, one unknown method, one
    // malformed frame, one oversized frame.
    let huge = "x".repeat(256 * 1024 + 1);
    let oversized = format!(r#"{{"jsonrpc":"2.0","method":"status.get","id":3,"pad":"{huge}"}}"#);
    let input = format!(
        "{}\n{}\n{}\n{}\n",
        r#"{"jsonrpc":"2.0","method":"initialize","params":{"protocol":{"major":1,"minor":0}},"id":1}"#,
        r#"{"jsonrpc":"2.0","method":"not-a-method","id":2}"#,
        "this-is-not-json",
        oversized,
    );

    let mut out = Vec::new();
    run(
        Cursor::new(input.as_bytes()),
        &mut out,
        &HandshakeHandler,
        std::time::Duration::from_secs(30),
    )
    .await
    .expect("transport runs");

    let text = String::from_utf8(out).expect("stdout is utf8");
    assert!(!text.is_empty(), "expected some response frames");
    // Every non-empty stdout line parses as a JSON object frame.
    for line in text.lines() {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        let v: serde_json::Value = serde_json::from_str(line)
            .unwrap_or_else(|e| panic!("stdout line is not valid JSON ({e}): {line}"));
        assert!(v.is_object(), "stdout frame must be an object: {line}");
    }
}

/// Errors are never returned as a success result; a rejected request yields
/// an error frame, never a bogus `result`.
#[tokio::test]
async fn errors_never_masquerade_as_success() {
    let input = format!(
        "{}\n{}\n",
        r#"{"jsonrpc":"2.0","method":"definitely-unknown","id":9}"#,
        r#"{"jsonrpc":"2.0","method":"initialize"}"#, // missing params -> invalid params
    );
    let mut out = Vec::new();
    run(
        Cursor::new(input.as_bytes()),
        &mut out,
        &HandshakeHandler,
        std::time::Duration::from_secs(30),
    )
    .await
    .expect("runs");

    let text = String::from_utf8(out).expect("utf8");
    for line in text.lines() {
        let v: serde_json::Value = serde_json::from_str(line).expect("valid frame");
        // A rejected request must never carry a `result`.
        assert!(
            v.get("result").is_none(),
            "rejected request leaked a result: {line}"
        );
        assert!(v.get("error").is_some(), "expected an error frame: {line}");
    }
}

/// An oversized frame is refused with a stable code, and the connection stays
/// usable for the next request (fail-closed, not fatal).
#[tokio::test]
async fn oversized_frame_is_refused_and_connection_survives() {
    let huge = "y".repeat(256 * 1024 + 5);
    let oversized = format!(r#"{{"jsonrpc":"2.0","method":"initialize","id":1,"pad":"{huge}"}}"#);
    let input = format!(
        "{}\n{}\n",
        oversized,
        r#"{"jsonrpc":"2.0","method":"initialize","params":{"protocol":{"major":1,"minor":0}},"id":2}"#,
    );
    let mut out = Vec::new();
    run(
        Cursor::new(input.as_bytes()),
        &mut out,
        &HandshakeHandler,
        std::time::Duration::from_secs(30),
    )
    .await
    .expect("runs");

    let text = String::from_utf8(out).expect("utf8");
    let frames: Vec<&str> = text.lines().filter(|l| !l.trim().is_empty()).collect();
    assert_eq!(frames.len(), 2, "both frames produced: {text}");
    // First frame: frame-too-large error (stable code LBR-AGENT-028).
    let first: serde_json::Value = serde_json::from_str(frames[0]).expect("valid");
    let data = &first["error"]["data"];
    assert_eq!(data["stable_code"], "LBR-AGENT-028");
    // Second frame: successful initialize.
    let second: serde_json::Value = serde_json::from_str(frames[1]).expect("valid");
    assert!(second.get("result").is_some());
}
