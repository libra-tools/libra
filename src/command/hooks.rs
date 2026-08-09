//! `libra hooks <provider> <subcommand>` — the stable entry point invoked
//! by hook configurations the `HookProvider`s install (Claude Code's
//! `.claude/settings.json`, Codex's `$CODEX_HOME/hooks.json`). Adds the
//! `Commands::Hooks(...)` variant promised in
//! `docs/development/commands/_general.md` (sections 1.2 and 6.1).
//!
//! Both installed surfaces (`hooks claude`, `hooks codex`) route to the
//! external-agent capture path (`HookTarget::AgentTraces`,
//! `refs/libra/traces` + `agent_session`/`agent_checkpoint`), matching the
//! first-batch capture contract in `docs/development/tracing/agent.md`.
//! Claude historically routed to the `refs/libra/intent` writer
//! (`HookTarget::AiIntent`); that drift was recorded in plan.md Task A4
//! and resolved by Task A6.5 when the real-CLI capture smoke exposed it
//! (an installed claude hook produced no `agent session list` row).

use clap::{Args, Subcommand};

use crate::{
    internal::ai::hooks::{
        HookTarget, process_hook_event_with_target,
        provider::ProviderHookCommand,
        providers::{claude_provider, codex, codex_provider},
    },
    utils::{
        error::{CliError, CliResult},
        output::OutputConfig,
    },
};

/// `--help` examples shown in `libra hooks --help` output.
///
/// `hooks` is the entry point invoked by external AI agent hook
/// configurations (Claude Code, Gemini) — it reads the hook event JSON
/// on stdin and records it into the libra session store. Each provider
/// exposes the seven Claude-Code-style lifecycle events; the banner
/// pins the most commonly wired ones (`session-start`, `prompt`,
/// `tool-use`, `stop`, `session-end`) for both providers so operators
/// see what to put in their hook config without reading the design
/// doc. Cross-cutting `--help` EXAMPLES rollout per
/// `docs/development/commands/_general.md` item B.
pub const HOOKS_EXAMPLES: &str = "\
EXAMPLES:
    libra hooks claude session-start         Claude SessionStart hook entry (reads JSON on stdin)
    libra hooks claude prompt                Claude UserPromptSubmit hook entry
    libra hooks claude tool-use              Claude PreToolUse / PostToolUse hook entry
    libra hooks claude stop                  Claude Stop hook entry
    libra hooks claude session-end           Claude SessionEnd hook entry
    libra hooks codex session-start          Codex SessionStart hook entry (AG-19 capture path)
    libra hooks codex stop                   Codex Stop hook entry (checkpoint boundary)
    libra hooks codex subagent-start         Codex SubagentStart hook entry
    libra hooks gemini <event>               Rejected with a hint: gemini is uninstall-only
                                             (remove stale configs with 'libra agent remove gemini')";

#[derive(Args, Debug)]
#[command(after_help = HOOKS_EXAMPLES)]
pub struct HooksArgs {
    #[command(subcommand)]
    pub command: HooksProviderSubcommand,
}

#[derive(Subcommand, Debug)]
pub enum HooksProviderSubcommand {
    /// `libra hooks claude <subcommand>`. Invoked by Claude Code hook configs.
    #[command(about = "Claude Code hook entry point")]
    Claude {
        #[command(subcommand)]
        command: ProviderHookSubcommand,
    },
    /// `libra hooks codex <subcommand>`. Invoked by Codex hook configs
    /// (AG-19) — the stable surface written into `$CODEX_HOME/hooks.json`.
    /// Routes to the AgentTraces capture path (`refs/libra/traces`), per
    /// the Codex capture contract in `docs/development/tracing/agent.md`.
    #[command(about = "Codex hook entry point")]
    Codex {
        #[command(subcommand)]
        command: ProviderHookSubcommand,
    },
    /// `libra hooks gemini <subcommand>`. Invoked by Gemini hook configs.
    #[command(about = "Gemini hook entry point")]
    Gemini {
        #[command(subcommand)]
        command: ProviderHookSubcommand,
    },
}

#[derive(Subcommand, Debug, Clone, Copy)]
pub enum ProviderHookSubcommand {
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

impl ProviderHookSubcommand {
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

pub async fn execute_safe(args: HooksArgs, _output: &OutputConfig) -> CliResult<()> {
    match args.command {
        // A6.5: the installed claude surface records into the AgentTraces
        // capture path (`refs/libra/traces` + `agent_session` /
        // `agent_checkpoint`), same as codex below — the first-batch
        // capture contract requires `agent session/checkpoint list` to see
        // real claude sessions driven through the installed hooks.
        HooksProviderSubcommand::Claude { command } => {
            let cmd = command.as_command();
            let expected_kind = cmd.lifecycle_event_kind();
            process_hook_event_with_target(
                cmd,
                expected_kind,
                claude_provider(),
                HookTarget::AgentTraces,
            )
            .await
            .map_err(|err| CliError::fatal(format!("hook ingestion failed: {err}")))
        }
        // AG-19: codex hook entries route to the AgentTraces capture path
        // (`refs/libra/traces`) — the stable installed surface per the
        // Codex capture contract.
        HooksProviderSubcommand::Codex { command } => {
            let cmd = command.as_command();
            let expected_kind = cmd.lifecycle_event_kind();
            if process_hook_event_with_target(
                cmd,
                expected_kind,
                codex_provider(),
                HookTarget::AgentTraces,
            )
            .await
            .is_err()
            {
                // Codex treats hook failures as task failures. Capture is
                // auxiliary, so acknowledge the callback and keep any
                // diagnostic free of hook payload or filesystem details.
                tracing::warn!(
                    target: "agent.hook.ingest",
                    provider = "codex",
                    verb = %cmd,
                    reason = "capture_failed",
                    "Codex hook capture failed; acknowledging callback"
                );
            }
            // Codex trust-gap banner (AG-19): SessionStart is the single
            // banner point; stderr only, never blocks the hook.
            if matches!(command, ProviderHookSubcommand::SessionStart)
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
        // AG-19 (plan.md Task A4): gemini is uninstall-only (AG-17
        // demoted it out of the supported roster), so this hidden entry —
        // still invoked by hooks installed before the demotion — no
        // longer ingests. It rejects with an actionable hint instead of
        // silently capturing data for an unsupported agent
        // (ingest-reject-with-hint, keeping the CLI surface so existing
        // configs fail with guidance rather than a clap usage error).
        HooksProviderSubcommand::Gemini { command: _ } => Err(CliError::fatal(
            "gemini hook ingestion is disabled: gemini is uninstall-only \
             (not in the supported agent roster)",
        )
        .with_hint(
            "remove the stale hook config with 'libra agent remove gemini'; \
             previously captured gemini sessions stay readable",
        )),
    }
}
