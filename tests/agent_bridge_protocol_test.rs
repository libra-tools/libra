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
fn protocol_v1_allowlist_is_exactly_28_methods() {
    assert_eq!(
        libra::internal::ai::agent_bridge::protocol::METHOD_ALLOWLIST.len(),
        28
    );
    assert_eq!(
        libra::internal::ai::agent_bridge::protocol::ProtocolVersion::current().minor,
        2
    );
}

/// The typed repository reads and writes are negotiated: a peer that
/// negotiated the matching minor sees them, an older peer does not, and the
/// removed fence method stays out.
#[test]
fn typed_vcs_methods_are_negotiated_by_minor() {
    use libra::internal::ai::agent_bridge::protocol::{
        admit_for_minor, dispatch_handshake, validate_method,
    };

    let reads = [
        "vcs.identity",
        "vcs.status",
        "vcs.diff",
        "vcs.log",
        "vcs.show",
    ];
    let writes = ["vcs.stage", "vcs.unstage", "vcs.commit"];

    let request = parse_request_line(r#"{"jsonrpc":"2.0","method":"initialize","params":{"protocol":{"major":1,"minor":2}},"id":1}"#).expect("request");
    let result = dispatch_handshake(&request)
        .expect("handshake")
        .expect("response")
        .result
        .expect("result");
    let advertised = result["methods"].as_array().expect("methods");
    for method in reads {
        assert!(
            advertised
                .iter()
                .any(|entry| entry.as_str() == Some(method)),
            "not advertised {method}"
        );
        assert!(validate_method(method).is_ok(), "not accepted {method}");
    }
    for method in writes {
        assert!(
            advertised
                .iter()
                .any(|entry| entry.as_str() == Some(method)),
            "not advertised {method}"
        );
        assert!(validate_method(method).is_ok(), "not accepted {method}");
    }
    assert!(
        !advertised
            .iter()
            .any(|entry| entry.as_str() == Some("vcs.fence")),
        "advertised vcs.fence"
    );
    assert!(validate_method("vcs.fence").is_err());

    // A peer that negotiated minor 0 does not see the typed methods and cannot
    // invoke them.
    let old = parse_request_line(r#"{"jsonrpc":"2.0","method":"initialize","params":{"protocol":{"major":1,"minor":0}},"id":1}"#).expect("request");
    let old_result = dispatch_handshake(&old)
        .expect("handshake")
        .expect("response")
        .result
        .expect("result");
    let old_advertised = old_result["methods"].as_array().expect("methods");
    for method in reads.iter().chain(writes.iter()) {
        assert!(
            !old_advertised
                .iter()
                .any(|entry| entry.as_str() == Some(method)),
            "advertised {method} to a minor 0 peer"
        );
        assert!(
            admit_for_minor(0, method).is_err(),
            "admitted {method} for a minor 0 peer"
        );
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
