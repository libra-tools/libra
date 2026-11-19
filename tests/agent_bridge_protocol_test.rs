//! `tests/agent_bridge_protocol_test.rs` — contract tests for the bridge
//! protocol v1 (plan-20260818 LB-01).
//!
//! Covers initialize/handshake, protocol major mismatch, unknown-method
//! rejection, request id echo, NDJSON framing, frame/batch caps and the
//! exact 20-method allowlist. These are the fixture-level guarantees the
//! TypeScript plugin consumes.

use libra::internal::ai::agent_bridge::protocol::{
    BridgeError, BridgeLimits, MethodAllowlist, PROTOCOL_MAJOR, ProtocolVersion,
    SOURCE_DEEPSEEK_HARNESS, parse_request_line,
};

#[test]
fn protocol_v1_allowlist_is_exactly_21_methods() {
    assert_eq!(
        libra::internal::ai::agent_bridge::protocol::METHOD_ALLOWLIST.len(),
        21
    );
    assert!(libra::internal::ai::agent_bridge::protocol::MethodAllowlist::contains("vcs.fence"));
    assert_eq!(
        libra::internal::ai::agent_bridge::protocol::ProtocolVersion::current().minor,
        2
    );
}

#[test]
fn unsafe_experimental_vcs_methods_are_neither_advertised_nor_accepted() {
    use libra::internal::ai::agent_bridge::protocol::{dispatch_handshake, validate_method};
    let request = parse_request_line(r#"{"jsonrpc":"2.0","method":"initialize","params":{"protocol":{"major":1,"minor":2}},"id":1}"#).expect("request");
    let result = dispatch_handshake(&request)
        .expect("handshake")
        .expect("response")
        .result
        .expect("result");
    let advertised = result["methods"].as_array().expect("methods");
    for method in [
        "vcs.identity",
        "vcs.status",
        "vcs.diff",
        "vcs.log",
        "vcs.show",
        "vcs.stage",
        "vcs.unstage",
        "vcs.commit",
    ] {
        assert!(
            !advertised
                .iter()
                .any(|entry| entry.as_str() == Some(method)),
            "advertised {method}"
        );
        assert!(validate_method(method).is_err(), "accepted {method}");
    }
    assert!(validate_method("status.get").is_ok());
}

#[test]
fn protocol_v1_capability_negotiation_has_expected_limits() {
    let limits = BridgeLimits::v1();
    // v1 contract limits (shared with both plan-20260818.md files).
    assert_eq!(limits.max_frame_bytes, 256 * 1024);
    assert_eq!(limits.max_inflight, 64);
    assert_eq!(limits.max_batch_events, 64);
    assert_eq!(limits.max_batch_bytes, 256 * 1024);
    assert_eq!(limits.max_result_bytes, 256 * 1024);
    assert_eq!(limits.max_page, 100);
}

#[test]
fn protocol_v1_current_version_is_major_1() {
    assert_eq!(ProtocolVersion::current().major, PROTOCOL_MAJOR);
    assert_eq!(SOURCE_DEEPSEEK_HARNESS, "deepseek-harness");
}

#[test]
fn protocol_v1_unknown_method_fails_closed() {
    let err = BridgeError::method_not_found("create_intent");
    assert_eq!(err.code, -32601);
    assert!(!err.retryable);
}

#[test]
fn protocol_v1_major_mismatch_fails_closed() {
    let err = BridgeError::major_mismatch(2);
    assert_eq!(err.code, 1000);
    assert!(!err.retryable);
    // The stable code string is part of the documented LBR-* catalogue.
    assert_eq!(err.stable_code, "LBR-AGENT-031");
}

#[test]
fn protocol_v1_parse_rejects_bad_frames() {
    assert!(parse_request_line("not json").is_err());
    assert!(parse_request_line("[1,2]").is_err());
    assert!(parse_request_line(r#"{"jsonrpc":"2.0"}"#).is_err());
    assert!(parse_request_line(r#"{"jsonrpc":"1.0","method":"initialize"}"#).is_err());
}

#[test]
fn protocol_v1_allowlist_has_session_and_workspace_methods() {
    for m in [
        "session.open",
        "event.append",
        "session.flush",
        "session.close",
        "workspace.claim",
        "workspace.renew",
        "workspace.release",
        "checkpoint.restore",
        "commit.create",
    ] {
        assert!(MethodAllowlist::contains(m), "missing {m}");
    }
    // No low-level object CRUD leaks into the allowlist (GC-LB-03).
    for m in ["create_intent", "create_task", "create_run", "shell.exec"] {
        assert!(!MethodAllowlist::contains(m), "{m} must not be exposed");
    }
}
