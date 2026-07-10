//! AG-18 external `libra-agent-<name>` protocol v2 + security tests
//! (`docs/development/tracing/agent.md` E2; plan.md Task A3).
//!
//! Each test plants a small `#!/bin/sh` fixture as the binary and
//! exercises one negotiation / security edge. Scripts only use shell
//! built-ins (`read`, `printf`, `exit`) plus absolute-path `/bin/sleep`,
//! so they survive the `env_clear()` spawn contract.

#![cfg(unix)]

use libra::internal::ai::observed_agents::{
    RPC_PROTOCOL_VERSION, RpcAgent, RpcAgentBinary, discover_rpc_agents, rpc::RPC_MAX_REQUEST_BYTES,
};
use serial_test::serial;

fn plant_script(dir: &std::path::Path, slug: &str, body: &str) -> RpcAgentBinary {
    use std::os::unix::fs::PermissionsExt;
    let path = dir.join(format!("libra-agent-{slug}"));
    std::fs::write(&path, body).unwrap();
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
    RpcAgentBinary {
        slug: slug.to_string(),
        binary_path: path,
    }
}

const EIGHT_CAPS: &str = r#"{"hooks":true,"transcript_analyzer":false,"transcript_preparer":false,"token_calculator":false,"compact_transcript":false,"text_generator":false,"hook_response_writer":false,"subagent_aware_extractor":false}"#;

/// v2 negotiation happy path: `info` yields protocol_version 2 plus the
/// E1 capability object, then the mandatory v1 `capabilities` method
/// still answers; the negotiated method set gates later invokes.
#[test]
fn info_success_registers_binary() {
    let dir = tempfile::tempdir().unwrap();
    let body = format!(
        "#!/bin/sh\n\
         read _l; printf '{{\"jsonrpc\":\"2.0\",\"id\":1,\"result\":{{\"protocol_version\":2,\"name\":\"demo\",\"type\":\"external\",\"capabilities\":{EIGHT_CAPS}}}}}\\n'\n\
         read _l; printf '{{\"jsonrpc\":\"2.0\",\"id\":2,\"result\":{{\"methods\":[\"capabilities\",\"info\",\"provider_kind\"]}}}}\\n'\n\
         read _l; printf '{{\"jsonrpc\":\"2.0\",\"id\":3,\"result\":{{\"kind\":\"demo\"}}}}\\n'\n"
    );
    let binary = plant_script(dir.path(), "demo", &body);
    let mut agent = RpcAgent::spawn(binary).unwrap();

    let info = agent
        .negotiate_info()
        .expect("info negotiation succeeds")
        .expect("binary answers info");
    assert_eq!(info.protocol_version, Some(2));
    assert_eq!(info.name, "demo");
    assert!(info.capabilities.hooks);
    assert_eq!(agent.negotiated_protocol_version(), 2);

    let caps = agent.negotiate_capabilities().expect("v1 method preserved");
    assert!(caps.iter().any(|m| m == "provider_kind"));

    let result = agent.invoke("provider_kind", None).expect("gated invoke");
    assert_eq!(result["kind"], "demo");
}

/// A binary speaking a NEWER protocol than this runtime is rejected
/// fail-closed with an actionable reason (`LBR-AGENT-003` semantics).
#[test]
fn version_mismatch_is_skipped_with_reason() {
    let dir = tempfile::tempdir().unwrap();
    let body = "#!/bin/sh\n\
        read _l; printf '{\"jsonrpc\":\"2.0\",\"id\":1,\"result\":{\"protocol_version\":99,\"name\":\"future\"}}\\n'\n";
    let binary = plant_script(dir.path(), "future", body);
    let mut agent = RpcAgent::spawn(binary).unwrap();

    let err = agent.negotiate_info().unwrap_err();
    let message = err.to_string();
    assert!(
        message.contains("protocol version 99")
            && message.contains(&RPC_PROTOCOL_VERSION.to_string()),
        "mismatch reason names both versions: {message}"
    );
}

/// A v1 binary without `info` still negotiates via the mandatory v1
/// `capabilities` method (client order: info → v1 → skip-and-log).
#[test]
fn v1_binary_without_info_falls_back_to_capabilities() {
    let dir = tempfile::tempdir().unwrap();
    let body = "#!/bin/sh\n\
        read _l; printf '{\"jsonrpc\":\"2.0\",\"id\":1,\"error\":{\"code\":-32601,\"message\":\"method not found\"}}\\n'\n\
        read _l; printf '{\"jsonrpc\":\"2.0\",\"id\":2,\"result\":{\"methods\":[\"capabilities\"]}}\\n'\n";
    let binary = plant_script(dir.path(), "v1only", body);
    let mut agent = RpcAgent::spawn(binary).unwrap();

    let info = agent.negotiate_info().expect("v1 fallback is not an error");
    assert!(info.is_none(), "no info payload for a v1 binary");
    assert_eq!(agent.negotiated_protocol_version(), 1);
    let caps = agent.negotiate_capabilities().expect("v1 negotiation");
    assert_eq!(caps, ["capabilities"]);
}

/// Timeouts kill the child and oversize requests are refused before the
/// pipe is touched — both fail closed with typed errors.
#[test]
fn timeout_and_oversize_are_fail_closed() {
    let dir = tempfile::tempdir().unwrap();
    // Timeout: reads the request, never answers.
    let body = "#!/bin/sh\nread _l\n/bin/sleep 5\n";
    let binary = plant_script(dir.path(), "sleepy", body);
    let mut agent = RpcAgent::spawn(binary).unwrap();
    let err = agent
        .invoke_with_timeout("capabilities", None, std::time::Duration::from_millis(200))
        .unwrap_err();
    assert!(err.to_string().contains("timed out"), "{err}");

    // Oversize request: rejected before writing.
    let body2 = "#!/bin/sh\nread _l\nprintf '{\"jsonrpc\":\"2.0\",\"id\":1,\"result\":{}}\\n'\n";
    let binary2 = plant_script(dir.path(), "cap", body2);
    let mut agent2 = RpcAgent::spawn(binary2).unwrap();
    let oversized = serde_json::json!({"blob": "x".repeat(RPC_MAX_REQUEST_BYTES)});
    let err2 = agent2
        .invoke_with_timeout(
            "capabilities",
            Some(oversized),
            std::time::Duration::from_secs(2),
        )
        .unwrap_err();
    assert!(err2.to_string().contains("exceeds limit"), "{err2}");
}

/// The capability gate is fail-closed: a method outside the negotiated
/// set is refused without touching the wire (`LBR-AGENT-004` semantics).
#[test]
fn undeclared_capability_method_is_rejected() {
    let dir = tempfile::tempdir().unwrap();
    let body = "#!/bin/sh\n\
        read _l; printf '{\"jsonrpc\":\"2.0\",\"id\":1,\"result\":{\"methods\":[\"capabilities\"]}}\\n'\n";
    let binary = plant_script(dir.path(), "narrow", body);
    let mut agent = RpcAgent::spawn(binary).unwrap();
    agent.negotiate_capabilities().unwrap();

    let err = agent.invoke("model_extract", None).unwrap_err();
    assert!(
        err.to_string()
            .contains("does not advertise method 'model_extract'"),
        "{err}"
    );
}

/// Child stderr is captured (never inherited), capped at 64 KiB, and only
/// redacted excerpts surface — a leaked AWS-style key must not appear.
#[test]
fn stderr_is_capped_redacted_and_not_inherited() {
    let dir = tempfile::tempdir().unwrap();
    // One secret INSIDE the 64 KiB cap (first line), ~96 KiB of noise,
    // and a second secret BEYOND the cap; then a valid answer.
    let body = "#!/bin/sh\n\
        printf 'leaked credential AKIAIOSFODNN7EXAMPLE end\\n' >&2\n\
        i=0\n\
        while [ $i -lt 1536 ]; do printf 'noise-%s-0123456789012345678901234567890123456789012345678901234567\\n' $i >&2; i=$((i+1)); done\n\
        printf 'late secret AKIAIOSFODNN7EXAMPL2 end\\n' >&2\n\
        read _l; printf '{\"jsonrpc\":\"2.0\",\"id\":1,\"result\":{\"methods\":[\"capabilities\"]}}\\n'\n";
    let binary = plant_script(dir.path(), "noisy", body);
    let mut agent = RpcAgent::spawn(binary).unwrap();
    agent.negotiate_capabilities().unwrap();
    // Give the stderr pump a moment to drain the pipe fully.
    std::thread::sleep(std::time::Duration::from_millis(200));

    let excerpt = agent
        .redacted_stderr_excerpt()
        .expect("stderr was captured, not inherited");
    assert!(
        excerpt.contains("[stderr capped at 64 KiB]"),
        "cap marker present: {excerpt}"
    );
    // The in-cap secret must be REPLACED by the redaction placeholder —
    // asserting both raw absence AND marker presence proves redaction
    // actually ran (not just the cap dropping the line).
    assert!(
        !excerpt.contains("AKIAIOSFODNN7EXAMPLE"),
        "raw in-cap AWS key must not surface: {excerpt}"
    );
    assert!(
        excerpt.contains("<REDACTED:aws-access-key-id>"),
        "redaction placeholder must be present for the in-cap secret: {excerpt}"
    );
    assert!(
        !excerpt.contains("AKIAIOSFODNN7EXAMPL2"),
        "beyond-cap secret must be dropped by the cap: {excerpt}"
    );
}

/// `env_clear()` + allowlist: the child sees the derived protocol
/// variables and the non-secret passthrough allowlist (`PATH`, `HOME`, …)
/// that real external CLIs need — but none of the parent's secrets.
#[test]
#[serial(rpc_env_probe)]
fn spawn_clears_parent_env_and_injects_allowlist() {
    let dir = tempfile::tempdir().unwrap();
    let body = "#!/bin/sh\n\
        read _l; printf '{\"jsonrpc\":\"2.0\",\"id\":1,\"result\":{\"methods\":[\"capabilities\",\"env_probe\"]}}\\n'\n\
        read _l; printf '{\"jsonrpc\":\"2.0\",\"id\":2,\"result\":{\"secret\":\"%s\",\"proto\":\"%s\",\"repo\":\"%s\",\"home\":\"%s\",\"haspath\":\"%s\"}}\\n' \"$LIBRA_TEST_SECRET_PROBE\" \"$LIBRA_AGENT_PROTOCOL_VERSION\" \"$LIBRA_REPO_ROOT\" \"$HOME\" \"${PATH:+yes}\"\n";
    let binary = plant_script(dir.path(), "envprobe", body);

    let home_sentinel = dir.path().join("home-sentinel");
    let original_home = std::env::var_os("HOME");
    // SAFETY: serialized via #[serial(rpc_env_probe)]; removed/restored below.
    unsafe {
        std::env::set_var("LIBRA_TEST_SECRET_PROBE", "super-secret-value");
        std::env::set_var("HOME", &home_sentinel);
    }
    let repo_root = dir.path().join("repo");
    std::fs::create_dir_all(&repo_root).unwrap();
    let mut agent = RpcAgent::spawn_in_repo(binary, Some(&repo_root)).unwrap();
    agent.negotiate_capabilities().unwrap();
    let result = agent.invoke("env_probe", None).unwrap();
    unsafe {
        std::env::remove_var("LIBRA_TEST_SECRET_PROBE");
        match original_home {
            Some(prev) => std::env::set_var("HOME", prev),
            None => std::env::remove_var("HOME"),
        }
    }

    // Secrets stay cleared by env_clear(); the derived + passthrough
    // allowlist reaches the child so external CLIs can actually run.
    assert_eq!(result["secret"], "", "parent secrets must not leak");
    assert_eq!(result["proto"], RPC_PROTOCOL_VERSION.to_string());
    assert_eq!(result["repo"], repo_root.display().to_string());
    assert_eq!(
        result["home"],
        home_sentinel.display().to_string(),
        "HOME must pass through the allowlist"
    );
    assert_eq!(
        result["haspath"], "yes",
        "PATH must pass through the allowlist so the child can find deps"
    );
}

/// Built-in slug impersonation is skipped-and-logged at discovery.
#[test]
#[serial(rpc_path_env)]
fn discovery_skips_builtin_slug_impersonation() {
    let dir = tempfile::tempdir().unwrap();
    plant_script(dir.path(), "claude-code", "#!/bin/sh\nexit 0\n");
    plant_script(dir.path(), "legit", "#!/bin/sh\nexit 0\n");

    let original = std::env::var_os("PATH");
    // SAFETY: serialized via #[serial(rpc_path_env)]; restored below.
    unsafe {
        std::env::set_var("PATH", dir.path());
    }
    let agents = discover_rpc_agents();
    unsafe {
        match original {
            Some(prev) => std::env::set_var("PATH", prev),
            None => std::env::remove_var("PATH"),
        }
    }

    assert_eq!(agents.len(), 1, "impersonator filtered: {agents:?}");
    assert_eq!(agents[0].slug, "legit");
}

/// The v1 fallback is STRICT: only JSON-RPC −32601 downgrades to v1;
/// any other `info` failure (here a −32000 error frame) propagates
/// fail-closed instead of silently degrading.
#[test]
fn non_method_not_found_info_failure_propagates() {
    let dir = tempfile::tempdir().unwrap();
    let body = "#!/bin/sh\n\
        read _l; printf '{\"jsonrpc\":\"2.0\",\"id\":1,\"error\":{\"code\":-32000,\"message\":\"boom\"}}\\n'\n";
    let binary = plant_script(dir.path(), "strict", body);
    let mut agent = RpcAgent::spawn(binary).unwrap();
    let err = agent.negotiate_info().unwrap_err();
    assert!(
        err.to_string().contains("info"),
        "info failure propagates: {err}"
    );
}

/// A0-08: `env_allowlist_extra` passes an operator-approved extra var through
/// the cleared child, but a credential name (`*_API_KEY`) is dropped even when
/// explicitly requested; the forbidden-name classifier and trusted-dir
/// containment predicate are pinned here too.
#[test]
#[serial(rpc_env_probe)]
fn trusted_dirs_env_allowlist_extra() {
    use libra::internal::ai::observed_agents::{env_name_is_forbidden, path_within_trusted_dirs};

    let dir = tempfile::tempdir().unwrap();
    let body = "#!/bin/sh\n\
        read _l; printf '{\"jsonrpc\":\"2.0\",\"id\":1,\"result\":{\"methods\":[\"capabilities\",\"env_probe\"]}}\\n'\n\
        read _l; printf '{\"jsonrpc\":\"2.0\",\"id\":2,\"result\":{\"extra\":\"%s\",\"secret\":\"%s\"}}\\n' \"$MY_EXTRA_VAR\" \"$MYSERVICE_API_KEY\"\n";
    let binary = plant_script(dir.path(), "extraenv", body);

    let original_extra = std::env::var_os("MY_EXTRA_VAR");
    let original_secret = std::env::var_os("MYSERVICE_API_KEY");
    // SAFETY: serialized via #[serial(rpc_env_probe)]; removed/restored below.
    unsafe {
        std::env::set_var("MY_EXTRA_VAR", "extra-value");
        std::env::set_var("MYSERVICE_API_KEY", "sk-should-be-dropped");
    }
    // The operator requests BOTH; the forbidden *_API_KEY must still be dropped.
    let extra_allowlist = vec!["MY_EXTRA_VAR".to_string(), "MYSERVICE_API_KEY".to_string()];
    let mut agent = RpcAgent::spawn_in_repo_with_env(binary, None, &extra_allowlist).unwrap();
    agent.negotiate_capabilities().unwrap();
    let result = agent.invoke("env_probe", None).unwrap();
    unsafe {
        match original_extra {
            Some(v) => std::env::set_var("MY_EXTRA_VAR", v),
            None => std::env::remove_var("MY_EXTRA_VAR"),
        }
        match original_secret {
            Some(v) => std::env::set_var("MYSERVICE_API_KEY", v),
            None => std::env::remove_var("MYSERVICE_API_KEY"),
        }
    }
    assert_eq!(
        result["extra"], "extra-value",
        "an approved extra var passes through"
    );
    assert_eq!(
        result["secret"], "",
        "a *_API_KEY is dropped even when explicitly requested"
    );

    // The forbidden-name classifier (case-insensitive, wildcard-rejecting).
    for forbidden in [
        "OPENAI_API_KEY",
        "gh_token",
        "MY_SECRET",
        "DB_PASSWORD",
        "LIBRA_STORAGE_BUCKET",
        "LIBRA_D1_ACCOUNT_ID",
        "FOO*",
        "",
    ] {
        assert!(
            env_name_is_forbidden(forbidden),
            "{forbidden:?} must be forbidden"
        );
    }
    for allowed in ["MY_EXTRA_VAR", "RUST_LOG", "HTTP_PROXY"] {
        assert!(
            !env_name_is_forbidden(allowed),
            "{allowed:?} must be allowed"
        );
    }

    // Trusted-directory containment (pure).
    let root = dir.path().canonicalize().unwrap();
    let inside = root.join("sub").join("libra-agent-x");
    assert!(path_within_trusted_dirs(
        &inside,
        std::slice::from_ref(&root)
    ));
    let outside = std::path::Path::new("/usr/bin/libra-agent-x");
    assert!(!path_within_trusted_dirs(outside, &[root]));
}
