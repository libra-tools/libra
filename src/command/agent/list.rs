//! `libra agent list` — focused capability matrix for known agents (AG-17).
//!
//! The rows derive from the AG-16 static registry
//! (`observed_agents::registry`), overlaid with the runtime installation
//! state for hook-installable agents. The `--json` shape is a frozen public
//! contract (see `docs/development/tracing/agent.md` AG-17): key set changes
//! require a schema bump and a compat-test update in the same PR.
//!
//! The listing surfaces only the supported roster (`claude-code` / `codex` /
//! `opencode`). Per E9 the non-first-batch agents (`gemini` / `cursor` /
//! `copilot` / `factory-ai`) stay `registered` in the static registry so
//! historical `agent_session` data remains readable and doctor can still
//! reason about residual hooks — but they are intentionally omitted from this
//! listing rather than shown as unsupported rows.

use clap::Args;
use serde::Serialize;

use crate::{
    internal::ai::observed_agents::{
        AgentKind, AgentStability, DeclaredAgentCaps, agent_for, registration_for,
    },
    utils::{
        error::{CliError, CliResult},
        output::{OutputConfig, emit_json_data},
    },
};

/// Bump when the `list --json` row shape changes (frozen AG-17 contract).
pub const AGENT_LIST_SCHEMA_VERSION: u32 = 1;

#[derive(Args, Debug)]
pub struct ListArgs {
    /// Machine schema version (1 keeps the frozen legacy shape; 2 adds
    /// versioned discovery/import/export method availability).
    #[arg(long, value_name = "N", default_value_t = AGENT_LIST_SCHEMA_VERSION)]
    pub schema_version: u32,
}

#[derive(Debug, Serialize)]
struct ListOutput {
    schema_version: u32,
    agents: Vec<ListAgentRow>,
}

/// One agent row. Field set frozen per AG-17 (`slug`/`agent_kind`/
/// `stability`/`hook_installable`/`installed`/`transcript_readable`/
/// `external_binary` + the roster fields `supported`/`support_wave`/
/// `launchable_*`; the install-state wire key is `installed`).
#[derive(Debug, Clone, Serialize)]
struct ListAgentRow {
    slug: &'static str,
    agent_kind: &'static str,
    db_value: &'static str,
    provider_name: &'static str,
    stability: AgentStability,
    supported: bool,
    /// `null` for rows outside the supported roster — the key itself is
    /// part of the frozen row shape.
    support_wave: Option<&'static str>,
    registered: bool,
    transcript_readable: bool,
    hook_installable: bool,
    installed: bool,
    launchable_review: bool,
    launchable_investigate: bool,
    external_binary: bool,
    config_paths: &'static [&'static str],
    protected_dirs: &'static [&'static str],
    capabilities: DeclaredAgentCaps,
}

#[derive(Debug, Serialize)]
struct ListOutputV2 {
    schema_version: u32,
    agents: Vec<ListAgentRowV2>,
}

#[derive(Debug, Serialize)]
struct ListAgentRowV2 {
    #[serde(flatten)]
    base: ListAgentRow,
    methods: Vec<AgentMethod>,
}

#[derive(Debug, Serialize)]
struct AgentMethod {
    name: &'static str,
    supported: bool,
    available: bool,
    unavailable_reason: Option<&'static str>,
}

fn methods_for(
    kind: AgentKind,
    secure_file_import_available: bool,
    opencode_available: bool,
) -> Vec<AgentMethod> {
    let file_backed = matches!(kind, AgentKind::ClaudeCode | AgentKind::Codex);
    let export_backed = kind == AgentKind::OpenCode;
    vec![
        AgentMethod {
            name: "transcript_discoverable",
            supported: file_backed,
            available: file_backed && secure_file_import_available,
            unavailable_reason: if file_backed && !secure_file_import_available {
                Some("secure provider-root file opening is unavailable on this platform")
            } else if !file_backed {
                Some(
                    "OpenCode has no bounded local transcript discovery; select an explicit session id",
                )
            } else {
                None
            },
        },
        AgentMethod {
            name: "importable",
            supported: file_backed || export_backed,
            available: (file_backed && secure_file_import_available)
                || (export_backed && opencode_available),
            unavailable_reason: if file_backed && !secure_file_import_available {
                Some("secure provider-root file opening is unavailable on this platform")
            } else if export_backed && !opencode_available {
                Some("trusted OpenCode exporter or required offline sandbox unavailable")
            } else {
                None
            },
        },
        AgentMethod {
            name: "export_bridge",
            supported: export_backed,
            available: export_backed && opencode_available,
            unavailable_reason: if export_backed && !opencode_available {
                Some("trusted OpenCode exporter or required offline sandbox unavailable")
            } else if !export_backed {
                Some("agent uses an authorized transcript file")
            } else {
                None
            },
        },
    ]
}

pub async fn execute_safe(args: ListArgs, output: &OutputConfig) -> CliResult<()> {
    if !matches!(args.schema_version, 1 | 2) {
        return Err(CliError::command_usage(format!(
            "unsupported agent list schema version {}; supported versions are 1 and 2",
            args.schema_version
        ))
        .with_stable_code(crate::utils::error::StableErrorCode::AgentSchemaVersionUnsupported));
    }
    let mut agents = Vec::with_capacity(AgentKind::all().len());
    for kind in AgentKind::all() {
        let row = registration_for(*kind);
        // Only the supported roster (claude-code / codex / opencode) is
        // surfaced. Unsupported agents stay in the static registry so
        // historical sessions remain readable and doctor can still reason
        // about residual hooks, but they are omitted from this listing.
        if !row.supported {
            continue;
        }
        let adapter = agent_for(*kind);
        // Runtime install state is only meaningful (and per E9 only
        // allowed) for supported, hook-installable agents; everything
        // else is pinned to `installed=false` regardless of leftover
        // provider config (doctor surfaces residual legacy hooks). An
        // inspection failure (e.g. unreadable provider settings) is a
        // real error, not `installed=false`.
        let installed = match adapter.as_hooks() {
            Some(hooks) if row.hook_installable => hooks.hooks_are_installed().map_err(|err| {
                CliError::fatal(format!(
                    "failed to inspect '{}' hook installation state: {err}",
                    row.slug
                ))
            })?,
            _ => false,
        };
        agents.push(ListAgentRow {
            slug: row.slug,
            agent_kind: row.agent_kind,
            db_value: row.db_value,
            provider_name: adapter.provider_name(),
            stability: adapter.stability(),
            supported: row.supported,
            support_wave: row.support_wave,
            registered: row.registered,
            transcript_readable: row.transcript_readable,
            hook_installable: row.hook_installable,
            installed,
            launchable_review: row.launchable_review,
            launchable_investigate: row.launchable_investigate,
            external_binary: row.external_binary,
            config_paths: row.config_paths,
            protected_dirs: adapter.protected_dirs(),
            capabilities: row.capabilities,
        });
    }

    let payload = ListOutput {
        schema_version: AGENT_LIST_SCHEMA_VERSION,
        agents,
    };
    if output.is_json() {
        if args.schema_version == 2 {
            let opencode_available = crate::internal::ai::observed_agents::opencode_export::trusted_opencode_binary()
                .await
                .is_ok()
                && crate::internal::ai::observed_agents::opencode_export::trusted_bwrap_available()
                    .await;
            let agents = payload
                .agents
                .iter()
                .cloned()
                .map(|base| {
                    let kind = AgentKind::from_db_str(base.agent_kind).ok_or_else(|| {
                        CliError::internal("agent list registry emitted an unknown db kind")
                    })?;
                    Ok(ListAgentRowV2 {
                        methods: methods_for(kind, cfg!(unix), opencode_available),
                        base,
                    })
                })
                .collect::<CliResult<Vec<_>>>()?;
            return emit_json_data(
                "agent_list",
                &ListOutputV2 {
                    schema_version: 2,
                    agents,
                },
                output,
            );
        }
        return emit_json_data("agent_list", &payload, output);
    }
    if output.quiet {
        return Ok(());
    }
    println!(
        "{:<13} {:<12} {:<11} {:<10} {:<11}",
        "SLUG", "WAVE", "HOOKS", "INSTALLED", "TRANSCRIPT"
    );
    for row in &payload.agents {
        println!(
            "{:<13} {:<12} {:<11} {:<10} {:<11}",
            row.slug,
            row.support_wave.unwrap_or("-"),
            if row.hook_installable {
                "installable"
            } else {
                "-"
            },
            if row.installed { "yes" } else { "no" },
            if row.transcript_readable {
                "readable"
            } else {
                "-"
            },
        );
    }
    println!(
        "\nSupported roster: {}. Use 'libra agent add <name>' to install hooks.",
        payload
            .agents
            .iter()
            .filter(|row| row.supported)
            .map(|row| row.slug)
            .collect::<Vec<_>>()
            .join(", ")
    );
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn schema_v2_reports_file_import_unavailable_without_secure_open_support() {
        let methods = methods_for(AgentKind::ClaudeCode, false, false);
        for name in ["transcript_discoverable", "importable"] {
            let method = methods
                .iter()
                .find(|method| method.name == name)
                .expect("file-backed method");
            assert!(method.supported);
            assert!(!method.available);
            assert_eq!(
                method.unavailable_reason,
                Some("secure provider-root file opening is unavailable on this platform")
            );
        }
    }
}
