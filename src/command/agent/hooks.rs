//! `libra agent hooks <agent> <subcommand>` — hook entry point invoked by
//! per-agent hook config that `libra agent enable` writes out.
//!
//! Phase 1 routes to [`process_hook_event_with_target`] with
//! [`HookTarget::AgentTraces`]; the runtime there does a minimal ingest
//! (parse → redact → upsert into `agent_session`). Phase 2 will extend the
//! AgentTraces branch to additionally write checkpoint commits on
//! `refs/libra/traces`.

use clap::Subcommand;

use crate::{
    internal::ai::hooks::{
        HookEnvelopeInvalid, HookTarget, process_hook_event_with_target,
        provider::ProviderHookCommand,
        providers::{claude_provider, codex, codex_provider, opencode_provider},
    },
    utils::{
        error::{CliError, CliResult, StableErrorCode},
        output::OutputConfig,
    },
};

#[derive(Subcommand, Debug)]
pub enum AgentHooksSubcommand {
    /// `libra agent hooks claude-code <subcommand>` family.
    #[command(about = "Claude Code hook entry points")]
    ClaudeCode {
        #[command(subcommand)]
        command: HookCommandKind,
    },
    /// `libra agent hooks codex <subcommand>` family (AG-19).
    #[command(about = "Codex hook entry points")]
    Codex {
        #[command(subcommand)]
        command: HookCommandKind,
    },
    /// `libra agent hooks gemini <subcommand>` family.
    #[command(about = "Gemini hook entry points")]
    Gemini {
        #[command(subcommand)]
        command: HookCommandKind,
    },
    /// `libra agent hooks opencode <subcommand>` family (AG-19).
    #[command(about = "OpenCode hook entry points")]
    Opencode {
        #[command(subcommand)]
        command: HookCommandKind,
    },
}

#[derive(Subcommand, Debug, Clone, Copy)]
pub enum HookCommandKind {
    SessionStart,
    Prompt,
    ToolUse,
    ModelUpdate,
    Compaction,
    Stop,
    SessionEnd,
    /// AG-19: nested sub-agent run started (Codex `SubagentStart`).
    SubagentStart,
    /// AG-19: nested sub-agent run finished (Codex `SubagentStop`).
    SubagentEnd,
}

impl HookCommandKind {
    fn as_command(self) -> ProviderHookCommand {
        match self {
            Self::SessionStart => ProviderHookCommand::SessionStart,
            Self::Prompt => ProviderHookCommand::Prompt,
            Self::ToolUse => ProviderHookCommand::ToolUse,
            Self::ModelUpdate => ProviderHookCommand::ModelUpdate,
            Self::Compaction => ProviderHookCommand::Compaction,
            Self::Stop => ProviderHookCommand::Stop,
            Self::SessionEnd => ProviderHookCommand::SessionEnd,
            Self::SubagentStart => ProviderHookCommand::SubagentStart,
            Self::SubagentEnd => ProviderHookCommand::SubagentEnd,
        }
    }
}

pub async fn execute_safe(cmd: AgentHooksSubcommand, _output: &OutputConfig) -> CliResult<()> {
    match cmd {
        AgentHooksSubcommand::ClaudeCode { command } => run(claude_provider(), command).await,
        AgentHooksSubcommand::Codex { command } => {
            run(codex_provider(), command).await?;
            // AG-19 Codex trust-gap banner: after a successful
            // SessionStart ingest, tell the operator (stderr, banner
            // only — never blocks the hook) how many Libra-managed Codex
            // hooks still lack a current local approval. Structural
            // key-presence comparison only; SessionStart is the single
            // banner point per `agent.md`.
            if matches!(command, HookCommandKind::SessionStart)
                && let Ok(gaps) = codex::codex_hook_trust_gaps()
                && gaps > 0
            {
                eprintln!(
                    "libra: {gaps} Libra-managed Codex hook(s) are not locally approved \
                     (untrusted hooks are skipped silently by codex); re-run \
                     'libra agent enable --agent codex' to refresh trust entries"
                );
            }
            Ok(())
        }
        // AG-19: same ingest-reject-with-hint as the top-level
        // `libra hooks gemini` entry — gemini is uninstall-only (E9), so
        // neither hook entry point may keep capturing for it.
        AgentHooksSubcommand::Gemini { command: _ } => Err(CliError::fatal(
            "gemini hook ingestion is disabled: gemini is uninstall-only \
             (not in the supported agent roster)",
        )
        .with_hint(
            "remove the stale hook config with 'libra agent remove gemini'; \
             previously captured gemini sessions stay readable",
        )),
        AgentHooksSubcommand::Opencode { command } => run(opencode_provider(), command).await,
    }
}

async fn run(
    provider: &'static dyn crate::internal::ai::hooks::provider::HookProvider,
    sub: HookCommandKind,
) -> CliResult<()> {
    let cmd = sub.as_command();
    let expected_kind = cmd.lifecycle_event_kind();
    process_hook_event_with_target(cmd, expected_kind, provider, HookTarget::AgentTraces)
        .await
        .map_err(map_ingest_error)
}

/// A0-03: map an ingest failure to a `CliError`, attaching the stable
/// `LBR-AGENT-008` code when the error chain carries a
/// [`HookEnvelopeInvalid`] (envelope size / UTF-8 / JSON / schema / path
/// reject). All other ingest failures stay generic fatals so a genuine
/// runtime error never masquerades as an envelope reject and vice-versa.
fn map_ingest_error(err: anyhow::Error) -> CliError {
    let envelope_invalid = err.chain().any(|cause| cause.is::<HookEnvelopeInvalid>());
    let cli = CliError::fatal(format!("agent hook ingestion failed: {err}"));
    if envelope_invalid {
        cli.with_stable_code(StableErrorCode::AgentHookEnvelopeInvalid)
    } else {
        cli
    }
}
