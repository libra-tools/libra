//! `libra agent rpc …` — discover, trust and invoke external
//! `libra-agent-<name>` RPC binaries.
//!
//! AG-18 security model (see `docs/development/tracing/agent.md` E2):
//!
//! - The whole external-agent surface is gated behind
//!   `agent.external_agents.enabled` (default **false**); `list`
//!   discovery, `trust` and `invoke` all refuse with `LBR-AGENT-002`
//!   until the operator opts in. Only `untrust` stays available while
//!   gated (revoking trust strictly tightens security).
//! - Discovered binaries are **quarantined** by default — `list` shows
//!   them (once enabled) but they are not callable until
//!   `rpc trust <slug>` records their provenance (path + sha256 +
//!   device/inode/mtime); trusting a binary whose parent directory is
//!   world-writable is refused outright.
//! - Every `invoke` revalidates provenance immediately before spawn;
//!   any drift revokes trust and fails closed (`LBR-AGENT-005`).
//! - Built-in slug impersonation is rejected (`LBR-AGENT-006`), the
//!   child environment is cleared to an allowlist, and stderr is
//!   captured/capped/redacted (never inherited).

use clap::{Args, Subcommand};
use serde::Serialize;

use crate::{
    internal::ai::observed_agents::{
        AgentKind, RpcAgent, discover_rpc_agents, external_agents_enabled, read_trust,
        record_trust, revalidate_trust, revoke_trust,
        rpc::{RpcFailureKind, rpc_failure_kind},
    },
    utils::{
        error::{CliError, CliResult, StableErrorCode},
        output::{OutputConfig, emit_json_data},
    },
};

#[derive(Subcommand, Debug)]
pub enum AgentRpcSubcommand {
    /// List every `libra-agent-<name>` binary discovered on `$PATH`.
    #[command(about = "List discovered libra-agent-* binaries on PATH")]
    List(AgentRpcListArgs),
    /// Record trust (path + sha256 + device/inode/mtime provenance) for a
    /// discovered binary so `invoke` may spawn it.
    #[command(about = "Trust a discovered libra-agent binary (records provenance)")]
    Trust(AgentRpcTrustArgs),
    /// Remove a previously recorded trust entry (returns the binary to
    /// quarantine).
    #[command(about = "Revoke trust for a libra-agent binary")]
    Untrust(AgentRpcUntrustArgs),
    /// Spawn a binary and invoke a single JSON-RPC method against it.
    /// Exits non-zero if the binary returns an `error` frame.
    #[command(about = "Invoke a single JSON-RPC method on a libra-agent binary")]
    Invoke(AgentRpcInvokeArgs),
}

#[derive(Args, Debug)]
pub struct AgentRpcListArgs {}

#[derive(Args, Debug)]
pub struct AgentRpcTrustArgs {
    /// Slug after `libra-agent-`. The binary must be on `$PATH` and live under
    /// a trusted directory (register one with `--dir` first).
    #[arg(required_unless_present = "dir", conflicts_with = "dir")]
    pub slug: Option<String>,
    /// Register a trusted directory: external agent binaries are only
    /// trustable when their canonical path lives under one of these. The path
    /// is canonicalized and must be an existing, non-world-writable directory.
    #[arg(long, value_name = "PATH", required_unless_present = "slug")]
    pub dir: Option<std::path::PathBuf>,
}

#[derive(Args, Debug)]
pub struct AgentRpcUntrustArgs {
    /// Slug after `libra-agent-`.
    pub slug: String,
}

#[derive(Args, Debug)]
pub struct AgentRpcInvokeArgs {
    /// Slug after `libra-agent-`. The binary must already be on
    /// `$PATH` and trusted via `libra agent rpc trust`.
    pub slug: String,
    /// JSON-RPC method name (e.g. `provider_kind`,
    /// `read_transcript`, `protected_dirs`).
    pub method: String,
    /// Optional JSON params object. Defaults to `null`.
    #[arg(long, value_name = "JSON")]
    pub params: Option<String>,
}

#[derive(Debug, Serialize)]
struct RpcBinaryRow {
    slug: String,
    binary_path: String,
    trusted: bool,
    quarantined: bool,
}

pub async fn execute_safe(cmd: AgentRpcSubcommand, output: &OutputConfig) -> CliResult<()> {
    match cmd {
        AgentRpcSubcommand::List(args) => list(args, output).await,
        AgentRpcSubcommand::Trust(args) => trust(args, output).await,
        AgentRpcSubcommand::Untrust(args) => untrust(args, output).await,
        AgentRpcSubcommand::Invoke(args) => invoke(args, output).await,
    }
}

/// `LBR-AGENT-002` gate shared by `list`, `trust` and `invoke` — while
/// external agents are disabled (the default), no `agent rpc` entry
/// point scans `$PATH` or touches external binaries. Only `untrust`
/// bypasses the gate: revoking trust strictly tightens security and
/// must never require opting back in.
async fn require_external_agents_enabled() -> CliResult<()> {
    let enabled = external_agents_enabled()
        .await
        .map_err(|e| CliError::fatal(format!("read external-agents gate: {e}")))?;
    if !enabled {
        return Err(CliError::fatal(
            "external libra-agent-* agents are disabled by default; opt in with \
             'libra config set agent.external_agents.enabled true' (repo-local) first",
        )
        .with_stable_code(StableErrorCode::AgentExternalAgentsDisabled));
    }
    Ok(())
}

/// Map a typed RPC failure to its E10 stable code (AG-18):
/// exceeding an IO hard cap is the redaction/cap security failure
/// (`LBR-AGENT-007`); timeout, broken transport and malformed frames
/// are the RPC transport failure (`LBR-AGENT-012`); a negotiated
/// version mismatch is `LBR-AGENT-003`. Ordinary JSON-RPC error frames
/// carry no stable code.
fn stable_code_for_rpc_failure(error: &anyhow::Error) -> Option<StableErrorCode> {
    match rpc_failure_kind(error)? {
        RpcFailureKind::IoCap => Some(StableErrorCode::AgentIoRedactionSecurityFailure),
        RpcFailureKind::Timeout | RpcFailureKind::Transport | RpcFailureKind::Protocol => {
            Some(StableErrorCode::AgentRpcTransportFailed)
        }
        RpcFailureKind::ProtocolVersion => Some(StableErrorCode::AgentProtocolVersionMismatch),
        RpcFailureKind::ErrorFrame(_) => None,
    }
}

/// Wrap an RPC-layer error into a `CliError`, attaching the stable code
/// derived from its typed classification when there is one.
fn rpc_cli_error(prefix: &str, error: anyhow::Error) -> CliError {
    let code = stable_code_for_rpc_failure(&error);
    let cli = CliError::fatal(format!("{prefix}: {error:#}"));
    match code {
        Some(code) => cli.with_stable_code(code),
        None => cli,
    }
}

/// `LBR-AGENT-006`: a slug that collides with a built-in agent can never
/// be trusted or invoked as an external binary.
fn reject_builtin_impersonation(slug: &str) -> CliResult<()> {
    if AgentKind::from_cli_slug(slug).is_some() {
        return Err(CliError::fatal(format!(
            "'{slug}' is a built-in agent slug; an external libra-agent-{slug} binary \
             cannot impersonate it"
        ))
        .with_stable_code(StableErrorCode::AgentBuiltinSlugImpersonation));
    }
    Ok(())
}

async fn list(_args: AgentRpcListArgs, output: &OutputConfig) -> CliResult<()> {
    require_external_agents_enabled().await?;
    let binaries = discover_rpc_agents();
    let mut rows: Vec<RpcBinaryRow> = Vec::with_capacity(binaries.len());
    for binary in &binaries {
        let trusted = read_trust(&binary.slug)
            .await
            .map_err(|e| CliError::fatal(format!("read trust for '{}': {e}", binary.slug)))?
            .is_some();
        rows.push(RpcBinaryRow {
            slug: binary.slug.clone(),
            binary_path: binary.binary_path.display().to_string(),
            trusted,
            quarantined: !trusted,
        });
    }
    if output.is_json() {
        return emit_json_data("agent_rpc_binaries", &rows, output);
    }
    if output.quiet {
        return Ok(());
    }
    if rows.is_empty() {
        println!("(no libra-agent-* binaries discovered on PATH)");
        return Ok(());
    }
    println!("{:<24}  {:<12}  binary_path", "slug", "state");
    for row in &rows {
        println!(
            "{:<24}  {:<12}  {}",
            row.slug,
            if row.trusted {
                "trusted"
            } else {
                "quarantined"
            },
            row.binary_path
        );
    }
    Ok(())
}

async fn trust(args: AgentRpcTrustArgs, output: &OutputConfig) -> CliResult<()> {
    require_external_agents_enabled().await?;

    // A0-08: `--dir <path>` registers a trusted directory (canonicalized +
    // must be an existing, non-world-writable directory).
    if let Some(dir) = &args.dir {
        let canonical = crate::internal::ai::observed_agents::add_trusted_dir(dir)
            .await
            .map_err(|e| {
                CliError::fatal(format!("cannot trust directory {}: {e}", dir.display()))
                    .with_stable_code(StableErrorCode::AgentProvenanceRejected)
            })?;
        if output.is_json() {
            let payload = serde_json::json!({ "trusted_dir": canonical.display().to_string() });
            return emit_json_data("agent_rpc_trust_dir", &payload, output);
        }
        if !output.quiet {
            println!("registered trusted directory {}", canonical.display());
        }
        return Ok(());
    }

    // Slug branch — clap guarantees `slug` is present when `--dir` is absent.
    let slug = args.slug.as_deref().ok_or_else(|| {
        CliError::command_usage(
            "pass a slug to trust a binary, or --dir <path> to trust a directory",
        )
    })?;
    reject_builtin_impersonation(slug)?;
    let binary = discover_rpc_agents()
        .into_iter()
        .find(|b| b.slug == slug)
        .ok_or_else(|| CliError::fatal(format!("no libra-agent-{slug} binary found on PATH")))?;
    // Provenance is only meaningful for a binary nobody else can swap:
    // refuse to trust one whose (canonical) parent directory is
    // world-writable (`LBR-AGENT-005`). `record_trust` re-enforces this
    // internally, but checking here keeps the stable code at the CLI
    // boundary.
    let canonical = binary.binary_path.canonicalize().map_err(|e| {
        CliError::fatal(format!(
            "cannot trust '{slug}': canonicalize {}: {e}",
            binary.binary_path.display()
        ))
        .with_stable_code(StableErrorCode::AgentProvenanceRejected)
    })?;
    crate::internal::ai::observed_agents::ensure_parent_not_world_writable(&canonical).map_err(
        |e| {
            CliError::fatal(format!("cannot trust '{slug}': {e}"))
                .with_stable_code(StableErrorCode::AgentProvenanceRejected)
        },
    )?;
    // record_trust also enforces the A0-08 trusted-directory allowlist; a
    // rejection there is a provenance refusal too (`LBR-AGENT-005`).
    let record = record_trust(slug, &binary.binary_path).await.map_err(|e| {
        CliError::fatal(format!("record trust for '{slug}': {e}"))
            .with_stable_code(StableErrorCode::AgentProvenanceRejected)
    })?;
    if output.is_json() {
        let payload = serde_json::json!({
            "slug": slug,
            "path": record.path.display().to_string(),
            "sha256": record.sha256,
        });
        return emit_json_data("agent_rpc_trust", &payload, output);
    }
    if !output.quiet {
        println!(
            "trusted libra-agent-{slug} at {} (sha256 {})",
            record.path.display(),
            record.sha256
        );
    }
    Ok(())
}

async fn untrust(args: AgentRpcUntrustArgs, output: &OutputConfig) -> CliResult<()> {
    let removed = revoke_trust(&args.slug)
        .await
        .map_err(|e| CliError::fatal(format!("revoke trust for '{}': {e}", args.slug)))?;
    if output.is_json() {
        let payload = serde_json::json!({ "slug": args.slug, "removed": removed });
        return emit_json_data("agent_rpc_untrust", &payload, output);
    }
    if !output.quiet {
        if removed {
            println!(
                "revoked trust for libra-agent-{} (back to quarantine)",
                args.slug
            );
        } else {
            println!(
                "libra-agent-{} was not trusted; nothing to revoke",
                args.slug
            );
        }
    }
    Ok(())
}

async fn invoke(args: AgentRpcInvokeArgs, output: &OutputConfig) -> CliResult<()> {
    require_external_agents_enabled().await?;
    reject_builtin_impersonation(&args.slug)?;
    let params = match args.params.as_deref() {
        Some(s) => Some(
            serde_json::from_str(s)
                .map_err(|e| CliError::command_usage(format!("--params is not valid JSON: {e}")))?,
        ),
        None => None,
    };

    // Quarantine gate: only trusted binaries are callable, and their
    // provenance is revalidated immediately before spawn (best-effort
    // TOCTOU mitigation tier — canonical path + parent-dir permissions +
    // sha256/device/inode/mtime; fd-derived exec is future work).
    let record = read_trust(&args.slug)
        .await
        .map_err(|e| CliError::fatal(format!("read trust for '{}': {e}", args.slug)))?
        .ok_or_else(|| {
            CliError::fatal(format!(
                "libra-agent-{} is quarantined (not trusted); run \
                 'libra agent rpc trust {}' after verifying the binary",
                args.slug, args.slug
            ))
            .with_stable_code(StableErrorCode::AgentProvenanceRejected)
        })?;
    let provenance = revalidate_trust(&args.slug, &record).await.map_err(|e| {
        CliError::fatal(format!("provenance revalidation failed: {e}"))
            .with_stable_code(StableErrorCode::AgentProvenanceRejected)
    })?;
    crate::internal::ai::observed_agents::ensure_parent_not_world_writable(
        &provenance.canonical_path,
    )
    .map_err(|e| {
        CliError::fatal(format!("spawn-surface check failed: {e}"))
            .with_stable_code(StableErrorCode::AgentProvenanceRejected)
    })?;

    let repo_root = std::env::current_dir().ok();
    let binary = crate::internal::ai::observed_agents::RpcAgentBinary {
        slug: args.slug.clone(),
        binary_path: provenance.canonical_path.clone(),
    };
    // A0-08: thread the operator-approved extra env passthrough (forbidden
    // credential/endpoint names already filtered out) into the cleared child.
    let extra_env = crate::internal::ai::observed_agents::env_allowlist_extra()
        .await
        .map_err(|e| CliError::fatal(format!("read env_allowlist_extra: {e}")))?;
    let mut agent = RpcAgent::spawn_in_repo_with_env(binary, repo_root.as_deref(), &extra_env)
        .map_err(|e| CliError::fatal(format!("spawn libra-agent-{}: {e}", args.slug)))?;

    // v2 negotiation order (E2): `info` first (optional), then the
    // mandatory v1 `capabilities` method.
    let info = agent
        .negotiate_info()
        .map_err(|e| rpc_cli_error("info negotiation failed", e))?;
    let caps = agent
        .negotiate_capabilities()
        .map_err(|e| rpc_cli_error("capabilities negotiation failed", e))?;
    if args.method != "capabilities"
        && args.method != "info"
        && !caps.iter().any(|m| m == &args.method)
    {
        return Err(CliError::fatal(format!(
            "binary libra-agent-{} does not advertise method '{}' (capabilities: {:?})",
            args.slug, args.method, caps
        ))
        .with_stable_code(StableErrorCode::AgentCapabilityUndeclared));
    }

    let result = if args.method == "capabilities" {
        // Caller wanted to see the capability set itself — return it
        // verbatim so scripted consumers don't have to make a second
        // call.
        serde_json::json!({"methods": caps})
    } else {
        agent
            .invoke(&args.method, params)
            .map_err(|e| rpc_cli_error("RPC invoke failed", e))?
    };

    if output.is_json() {
        let payload = serde_json::json!({
            "slug": args.slug,
            "method": args.method,
            "protocol_version": info.as_ref().and_then(|i| i.protocol_version).unwrap_or(1),
            "result": result,
        });
        return emit_json_data("agent_rpc_invoke", &payload, output);
    }
    if !output.quiet {
        let pretty = serde_json::to_string_pretty(&result).unwrap_or_else(|_| result.to_string());
        println!("{pretty}");
    }
    Ok(())
}

#[cfg(test)]
mod stable_code_mapping_tests {
    use super::*;

    fn err_with(kind: RpcFailureKind) -> anyhow::Error {
        anyhow::anyhow!("synthetic failure").context(kind)
    }

    /// Pin the AG-18 failure-kind -> E10 stable-code classification.
    /// A silent re-bucketing (e.g. moving `Timeout` back under the
    /// IO-cap/redaction code) would change the CLI contract without
    /// failing any behavioural test, so each arm is pinned here.
    #[test]
    fn rpc_failure_kinds_map_to_pinned_stable_codes() {
        for (kind, expected) in [
            (
                RpcFailureKind::Timeout,
                StableErrorCode::AgentRpcTransportFailed,
            ),
            (
                RpcFailureKind::Transport,
                StableErrorCode::AgentRpcTransportFailed,
            ),
            (
                RpcFailureKind::Protocol,
                StableErrorCode::AgentRpcTransportFailed,
            ),
            (
                RpcFailureKind::IoCap,
                StableErrorCode::AgentIoRedactionSecurityFailure,
            ),
            (
                RpcFailureKind::ProtocolVersion,
                StableErrorCode::AgentProtocolVersionMismatch,
            ),
        ] {
            assert_eq!(
                stable_code_for_rpc_failure(&err_with(kind)),
                Some(expected),
                "{kind:?} must map to {expected:?}"
            );
        }
        assert_eq!(
            stable_code_for_rpc_failure(&err_with(RpcFailureKind::ErrorFrame(-32601))),
            None,
            "ordinary JSON-RPC error frames carry no stable code"
        );
        assert_eq!(
            stable_code_for_rpc_failure(&anyhow::anyhow!("untyped")),
            None,
            "errors without a typed marker carry no stable code"
        );
    }
}
