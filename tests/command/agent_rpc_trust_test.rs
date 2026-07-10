//! CLI-level coverage for the AG-18 external-agent security surface:
//! the `agent.external_agents.enabled` gate (`LBR-AGENT-002`), the
//! trust/quarantine flow, provenance revalidation (`LBR-AGENT-005`),
//! transport fail-closed (`LBR-AGENT-012`) and stderr redaction at the
//! CLI boundary.

#![cfg(unix)]

use std::path::Path;

use super::{init_repo_via_cli, run_libra_command_with_stdin_and_env};

fn plant_script(dir: &Path, slug: &str, body: &str) -> std::path::PathBuf {
    use std::os::unix::fs::PermissionsExt;
    let path = dir.join(format!("libra-agent-{slug}"));
    std::fs::write(&path, body).unwrap();
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
    path
}

/// Run libra with a PATH that contains the fixture dir (the spawned
/// binary discovers `libra-agent-*` through its own PATH).
fn run_with_path(args: &[&str], cwd: &Path, fixture_dir: &Path) -> std::process::Output {
    run_with_path_and_env(args, cwd, fixture_dir, &[])
}

fn run_with_path_and_env(
    args: &[&str],
    cwd: &Path,
    fixture_dir: &Path,
    extra_env: &[(&str, &str)],
) -> std::process::Output {
    let path_var = format!(
        "{}:{}",
        fixture_dir.display(),
        std::env::var("PATH").unwrap_or_default()
    );
    let mut envs: Vec<(&str, &str)> = vec![("PATH", &path_var)];
    envs.extend_from_slice(extra_env);
    run_libra_command_with_stdin_and_env(args, cwd, "", &envs)
}

/// Init a repo with a fixture-bin dir and the external-agents gate open.
/// A0-08: the fixture dir is registered as a trusted directory so
/// `rpc trust <slug>` binaries planted there pass the allowlist enforcement.
fn setup_enabled(temp: &Path) -> (std::path::PathBuf, std::path::PathBuf) {
    let repo = temp.join("repo");
    init_repo_via_cli(&repo);
    let fixtures = temp.join("bin");
    std::fs::create_dir_all(&fixtures).unwrap();
    let set = run_with_path(
        &["config", "set", "agent.external_agents.enabled", "true"],
        &repo,
        &fixtures,
    );
    assert!(set.status.success(), "config set must succeed");
    let trust_dir = run_with_path(
        &["agent", "rpc", "trust", "--dir", fixtures.to_str().unwrap()],
        &repo,
        &fixtures,
    );
    assert!(
        trust_dir.status.success(),
        "trust --dir must succeed: {}",
        String::from_utf8_lossy(&trust_dir.stderr)
    );
    (repo, fixtures)
}

const ANSWERER: &str = "#!/bin/sh\n\
    read _l; printf '{\"jsonrpc\":\"2.0\",\"id\":1,\"error\":{\"code\":-32601,\"message\":\"no info\"}}\\n'\n\
    read _l; printf '{\"jsonrpc\":\"2.0\",\"id\":2,\"result\":{\"methods\":[\"capabilities\",\"provider_kind\"]}}\\n'\n\
    read _l; printf '{\"jsonrpc\":\"2.0\",\"id\":3,\"result\":{\"kind\":\"ext\"}}\\n'\n";

/// The external-agent surface is fail-closed by default: `list`, `trust`
/// and `invoke` all refuse with `LBR-AGENT-002` until the operator opts
/// in (`untrust` stays available); after opting in, an untrusted binary
/// stays quarantined (`LBR-AGENT-005`), and the full trust → invoke path
/// works end to end.
#[test]
fn external_agents_gate_trust_and_invoke_flow() {
    let temp = tempfile::tempdir().expect("tempdir");
    let repo = temp.path().join("repo");
    init_repo_via_cli(&repo);
    let fixtures = temp.path().join("bin");
    std::fs::create_dir_all(&fixtures).unwrap();
    plant_script(&fixtures, "ext", ANSWERER);

    // Gate off: every entry point that touches external binaries —
    // list discovery included — refuses with LBR-AGENT-002.
    for args in [
        vec!["agent", "rpc", "list"],
        vec!["agent", "rpc", "invoke", "ext", "provider_kind"],
        vec!["agent", "rpc", "trust", "ext"],
    ] {
        let out = run_with_path(&args, &repo, &fixtures);
        assert!(!out.status.success(), "{args:?} must refuse while gated");
        let stderr = String::from_utf8_lossy(&out.stderr);
        assert!(
            stderr.contains("LBR-AGENT-002"),
            "{args:?} carries the gate code: {stderr}"
        );
        assert!(
            stderr.contains("agent.external_agents.enabled"),
            "{args:?} names the opt-in key: {stderr}"
        );
    }

    // untrust bypasses the gate (revoking trust only tightens security).
    let gated_untrust = run_with_path(&["agent", "rpc", "untrust", "ext"], &repo, &fixtures);
    assert!(
        gated_untrust.status.success(),
        "untrust works while gated: {}",
        String::from_utf8_lossy(&gated_untrust.stderr)
    );

    // Opt in.
    let set = run_with_path(
        &["config", "set", "agent.external_agents.enabled", "true"],
        &repo,
        &fixtures,
    );
    assert!(set.status.success(), "config set must succeed");
    // A0-08: register the fixture dir as trusted so `trust ext` (below) passes
    // the trusted-directory allowlist.
    let trust_dir = run_with_path(
        &["agent", "rpc", "trust", "--dir", fixtures.to_str().unwrap()],
        &repo,
        &fixtures,
    );
    assert!(
        trust_dir.status.success(),
        "trust --dir must succeed: {}",
        String::from_utf8_lossy(&trust_dir.stderr)
    );

    // list now works and shows the binary as quarantined.
    let list = run_with_path(&["agent", "rpc", "list"], &repo, &fixtures);
    assert!(list.status.success());
    let list_out = String::from_utf8_lossy(&list.stdout);
    assert!(list_out.contains("ext") && list_out.contains("quarantined"));

    // Still quarantined before trust: invoke refuses with LBR-AGENT-005.
    let untrusted = run_with_path(
        &["agent", "rpc", "invoke", "ext", "provider_kind"],
        &repo,
        &fixtures,
    );
    assert!(!untrusted.status.success());
    let untrusted_err = String::from_utf8_lossy(&untrusted.stderr);
    assert!(
        untrusted_err.contains("LBR-AGENT-005") && untrusted_err.contains("quarantined"),
        "untrusted invoke fails closed: {untrusted_err}"
    );

    // Trust, then invoke succeeds.
    let trust = run_with_path(&["agent", "rpc", "trust", "ext"], &repo, &fixtures);
    assert!(
        trust.status.success(),
        "trust: {}",
        String::from_utf8_lossy(&trust.stderr)
    );
    let invoke = run_with_path(
        &["agent", "rpc", "invoke", "ext", "provider_kind"],
        &repo,
        &fixtures,
    );
    assert!(
        invoke.status.success(),
        "trusted invoke: {}",
        String::from_utf8_lossy(&invoke.stderr)
    );
    assert!(String::from_utf8_lossy(&invoke.stdout).contains("ext"));

    // Tamper with the binary: provenance drift revokes trust fail-closed.
    plant_script(&fixtures, "ext", "#!/bin/sh\n# tampered\nexit 0\n");
    let tampered = run_with_path(
        &["agent", "rpc", "invoke", "ext", "provider_kind"],
        &repo,
        &fixtures,
    );
    assert!(!tampered.status.success());
    let tampered_err = String::from_utf8_lossy(&tampered.stderr);
    assert!(
        tampered_err.contains("LBR-AGENT-005"),
        "drift is a provenance rejection: {tampered_err}"
    );
    // And the trust record is gone — the binary is quarantined again.
    let relist = run_with_path(&["agent", "rpc", "list"], &repo, &fixtures);
    assert!(String::from_utf8_lossy(&relist.stdout).contains("quarantined"));
}

/// mtime-only drift (identical bytes, touched timestamp) is enough to
/// revoke trust: a swap-and-swap-back attack shows up as an mtime
/// change even when the hash matches again.
#[test]
fn mtime_only_drift_revokes_trust() {
    let temp = tempfile::tempdir().expect("tempdir");
    let (repo, fixtures) = setup_enabled(temp.path());
    let binary = plant_script(&fixtures, "ext", ANSWERER);

    let trust = run_with_path(&["agent", "rpc", "trust", "ext"], &repo, &fixtures);
    assert!(
        trust.status.success(),
        "trust: {}",
        String::from_utf8_lossy(&trust.stderr)
    );

    // Backdate the binary's mtime — bytes unchanged.
    let touch = std::process::Command::new("touch")
        .args(["-t", "202001010000"])
        .arg(&binary)
        .status()
        .expect("run touch");
    assert!(touch.success(), "touch must backdate the fixture");

    let invoke = run_with_path(
        &["agent", "rpc", "invoke", "ext", "provider_kind"],
        &repo,
        &fixtures,
    );
    assert!(!invoke.status.success(), "mtime drift must fail closed");
    let stderr = String::from_utf8_lossy(&invoke.stderr);
    assert!(
        stderr.contains("LBR-AGENT-005"),
        "mtime drift is a provenance rejection: {stderr}"
    );
    let relist = run_with_path(&["agent", "rpc", "list"], &repo, &fixtures);
    assert!(
        String::from_utf8_lossy(&relist.stdout).contains("quarantined"),
        "drifted binary returns to quarantine"
    );
}

/// A binary sitting in a world-writable directory can never be trusted:
/// any local user could swap it between trust and invoke.
#[test]
fn world_writable_dir_binary_cannot_be_trusted() {
    use std::os::unix::fs::PermissionsExt;
    let temp = tempfile::tempdir().expect("tempdir");
    let (repo, fixtures) = setup_enabled(temp.path());
    plant_script(&fixtures, "ext", ANSWERER);
    std::fs::set_permissions(&fixtures, std::fs::Permissions::from_mode(0o777)).unwrap();

    let trust = run_with_path(&["agent", "rpc", "trust", "ext"], &repo, &fixtures);
    assert!(
        !trust.status.success(),
        "world-writable parent must refuse trust"
    );
    let stderr = String::from_utf8_lossy(&trust.stderr);
    assert!(
        stderr.contains("LBR-AGENT-005") && stderr.contains("world-writable"),
        "trust rejection is a provenance rejection naming the cause: {stderr}"
    );
}

/// A trusted binary that dies before answering the invoked method is a
/// transport failure: the CLI surfaces `LBR-AGENT-012`, not a raw IO
/// error or the IO-cap/redaction code.
#[test]
fn transport_failure_surfaces_stable_code_at_cli() {
    let temp = tempfile::tempdir().expect("tempdir");
    let (repo, fixtures) = setup_enabled(temp.path());
    // Answers info (error frame) + capabilities, then exits before the
    // method call — the invoke sees EOF / broken pipe.
    let body = "#!/bin/sh\n\
        read _l; printf '{\"jsonrpc\":\"2.0\",\"id\":1,\"error\":{\"code\":-32601,\"message\":\"no info\"}}\\n'\n\
        read _l; printf '{\"jsonrpc\":\"2.0\",\"id\":2,\"result\":{\"methods\":[\"capabilities\",\"provider_kind\"]}}\\n'\n\
        exit 0\n";
    plant_script(&fixtures, "ext", body);
    let trust = run_with_path(&["agent", "rpc", "trust", "ext"], &repo, &fixtures);
    assert!(trust.status.success());

    let invoke = run_with_path(
        &["agent", "rpc", "invoke", "ext", "provider_kind"],
        &repo,
        &fixtures,
    );
    assert!(!invoke.status.success());
    let stderr = String::from_utf8_lossy(&invoke.stderr);
    assert!(
        stderr.contains("LBR-AGENT-012"),
        "early exit is a transport failure: {stderr}"
    );
}

/// Child stderr never reaches the operator raw: secrets are redacted in
/// the default human error output and in the structured JSON error
/// payload alike.
#[test]
fn stderr_redaction_holds_at_cli_boundary() {
    let temp = tempfile::tempdir().expect("tempdir");
    let (repo, fixtures) = setup_enabled(temp.path());
    // Leaks an AWS access key id to stderr, then dies before answering
    // the method so the failure path attaches the stderr excerpt.
    let body = "#!/bin/sh\n\
        printf 'leaked credential AKIAIOSFODNN7EXAMPLE end\\n' >&2\n\
        read _l; printf '{\"jsonrpc\":\"2.0\",\"id\":1,\"error\":{\"code\":-32601,\"message\":\"no info\"}}\\n'\n\
        read _l; printf '{\"jsonrpc\":\"2.0\",\"id\":2,\"result\":{\"methods\":[\"capabilities\",\"provider_kind\"]}}\\n'\n\
        exit 0\n";
    plant_script(&fixtures, "ext", body);
    let trust = run_with_path(&["agent", "rpc", "trust", "ext"], &repo, &fixtures);
    assert!(trust.status.success());

    // Default human output.
    let invoke = run_with_path(
        &["agent", "rpc", "invoke", "ext", "provider_kind"],
        &repo,
        &fixtures,
    );
    assert!(!invoke.status.success());
    let human = format!(
        "{}{}",
        String::from_utf8_lossy(&invoke.stdout),
        String::from_utf8_lossy(&invoke.stderr)
    );
    assert!(
        !human.contains("AKIAIOSFODNN7EXAMPLE"),
        "raw secret must not leak into default output: {human}"
    );

    // Structured JSON error payload.
    let json_invoke = run_with_path_and_env(
        &["agent", "rpc", "invoke", "ext", "provider_kind"],
        &repo,
        &fixtures,
        &[("LIBRA_ERROR_JSON", "1")],
    );
    assert!(!json_invoke.status.success());
    let json_out = format!(
        "{}{}",
        String::from_utf8_lossy(&json_invoke.stdout),
        String::from_utf8_lossy(&json_invoke.stderr)
    );
    assert!(
        !json_out.contains("AKIAIOSFODNN7EXAMPLE"),
        "raw secret must not leak into JSON error payload: {json_out}"
    );
    assert!(
        json_out.contains("LBR-AGENT-012"),
        "JSON error still carries the stable code: {json_out}"
    );
}

/// A PATH binary whose slug impersonates a built-in agent is invisible to
/// discovery and rejected by trust AND invoke with `LBR-AGENT-006`.
#[test]
fn builtin_slug_impersonation_is_rejected() {
    let temp = tempfile::tempdir().expect("tempdir");
    let (repo, fixtures) = setup_enabled(temp.path());
    plant_script(&fixtures, "claude-code", ANSWERER);

    let list = run_with_path(&["agent", "rpc", "list"], &repo, &fixtures);
    assert!(
        !String::from_utf8_lossy(&list.stdout).contains("claude-code"),
        "impersonator must not be discovered"
    );

    for args in [
        vec!["agent", "rpc", "trust", "claude-code"],
        vec!["agent", "rpc", "invoke", "claude-code", "provider_kind"],
    ] {
        let out = run_with_path(&args, &repo, &fixtures);
        assert!(!out.status.success(), "{args:?} must refuse impersonation");
        assert!(
            String::from_utf8_lossy(&out.stderr).contains("LBR-AGENT-006"),
            "{args:?} rejects impersonation with the stable code"
        );
    }
}

/// A0-08: `rpc trust --dir` registers a trusted directory; a binary under a
/// trusted dir is trustable, one outside every trusted dir is rejected
/// (`LBR-AGENT-005`), and a world-writable directory can never be registered.
#[test]
fn agent_rpc_trust_dir() {
    use std::os::unix::fs::PermissionsExt;
    let temp = tempfile::tempdir().expect("tempdir");
    // setup_enabled opens the gate AND registers `fixtures` as a trusted dir.
    let (repo, fixtures) = setup_enabled(temp.path());

    // (a) `trust --dir` emits the canonical trusted directory (JSON).
    let other = temp.path().join("other-bin");
    std::fs::create_dir_all(&other).unwrap();
    let reg = run_with_path(
        &[
            "--json",
            "agent",
            "rpc",
            "trust",
            "--dir",
            other.to_str().unwrap(),
        ],
        &repo,
        &fixtures,
    );
    assert!(
        reg.status.success(),
        "trust --dir must succeed: {}",
        String::from_utf8_lossy(&reg.stderr)
    );
    let json: serde_json::Value = serde_json::from_slice(&reg.stdout).unwrap_or_default();
    assert!(
        json["data"]["trusted_dir"]
            .as_str()
            .unwrap_or("")
            .contains("other-bin"),
        "trust --dir emits the canonical dir: {}",
        String::from_utf8_lossy(&reg.stdout)
    );

    // (b) a binary under a trusted dir is trustable.
    plant_script(&fixtures, "inn", ANSWERER);
    let ok = run_with_path(&["agent", "rpc", "trust", "inn"], &repo, &fixtures);
    assert!(
        ok.status.success(),
        "binary under a trusted dir must be trustable: {}",
        String::from_utf8_lossy(&ok.stderr)
    );

    // (c) a binary NOT under any trusted directory is rejected (LBR-AGENT-005).
    let untrusted_dir = temp.path().join("untrusted-bin");
    std::fs::create_dir_all(&untrusted_dir).unwrap();
    plant_script(&untrusted_dir, "out", ANSWERER);
    let path_var = format!(
        "{}:{}:{}",
        untrusted_dir.display(),
        fixtures.display(),
        std::env::var("PATH").unwrap_or_default()
    );
    let rej = run_libra_command_with_stdin_and_env(
        &["agent", "rpc", "trust", "out"],
        &repo,
        "",
        &[("PATH", &path_var)],
    );
    assert!(
        !rej.status.success(),
        "a binary outside every trusted dir must be rejected"
    );
    let stderr = String::from_utf8_lossy(&rej.stderr);
    assert!(
        stderr.contains("LBR-AGENT-005") && stderr.contains("trusted director"),
        "rejection is a provenance refusal naming the cause: {stderr}"
    );

    // (d) a world-writable directory can never be registered.
    let ww = temp.path().join("ww-bin");
    std::fs::create_dir_all(&ww).unwrap();
    std::fs::set_permissions(&ww, std::fs::Permissions::from_mode(0o777)).unwrap();
    let ww_reg = run_with_path(
        &["agent", "rpc", "trust", "--dir", ww.to_str().unwrap()],
        &repo,
        &fixtures,
    );
    assert!(
        !ww_reg.status.success(),
        "a world-writable directory must be refused"
    );
    assert!(
        String::from_utf8_lossy(&ww_reg.stderr).contains("world-writable"),
        "refusal names the cause: {}",
        String::from_utf8_lossy(&ww_reg.stderr)
    );
}
