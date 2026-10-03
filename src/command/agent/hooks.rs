//! `libra agent hooks <agent> <subcommand>` — hook entry point invoked by
//! per-agent hook config that `libra agent enable` writes out.
//!
//! Every verb routes to [`process_hook_event_with_target`] with
//! [`HookTarget::AgentTraces`]: validated ingress, the provider-neutral
//! capture coordinator, and its catalog/checkpoint store ports persist the
//! redacted `agent_session` / `agent_checkpoint` projection and checkpoint
//! commits on `refs/libra/traces`.

use clap::Subcommand;

use crate::{
    command::hooks::{
        CaptureErrorSurface, gemini_hook_rejection, map_capture_ingest_error,
        settle_codex_unsupported_platform,
    },
    internal::ai::{
        capture::ingress::CaptureDeadline,
        hooks::{
            HookTarget, process_hook_event_with_target,
            provider::ProviderHookCommand,
            providers::{claude_provider, codex, codex_provider, opencode_provider},
        },
    },
    utils::{error::CliResult, output::OutputConfig},
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
    PermissionRequest,
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
            Self::PermissionRequest => ProviderHookCommand::PermissionRequest,
            Self::ModelUpdate => ProviderHookCommand::ModelUpdate,
            Self::Compaction => ProviderHookCommand::Compaction,
            Self::Stop => ProviderHookCommand::Stop,
            Self::SessionEnd => ProviderHookCommand::SessionEnd,
            Self::SubagentStart => ProviderHookCommand::SubagentStart,
            Self::SubagentEnd => ProviderHookCommand::SubagentEnd,
        }
    }
}

pub async fn execute_safe(
    cmd: AgentHooksSubcommand,
    _output: &OutputConfig,
    deadline: Option<CaptureDeadline>,
) -> CliResult<()> {
    match cmd {
        AgentHooksSubcommand::ClaudeCode { command } => {
            run(claude_provider(), command, deadline).await
        }
        AgentHooksSubcommand::Codex { command } => {
            run_codex(command, deadline).await?;
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
        AgentHooksSubcommand::Gemini { command: _ } => {
            Err(gemini_hook_rejection("agent hook ingestion failed").await)
        }
        AgentHooksSubcommand::Opencode { command } => {
            run(opencode_provider(), command, deadline).await
        }
    }
}

async fn run(
    provider: &'static dyn crate::internal::ai::hooks::provider::HookProvider,
    sub: HookCommandKind,
    deadline: Option<CaptureDeadline>,
) -> CliResult<()> {
    let cmd = sub.as_command();
    let expected_kind = cmd.lifecycle_event_kind();
    process_hook_event_with_target(
        cmd,
        expected_kind,
        provider,
        HookTarget::AgentTraces,
        deadline,
    )
    .await
    .map_err(|err| {
        map_capture_ingest_error(
            err,
            "agent hook ingestion failed",
            CaptureErrorSurface::FailClosed,
        )
    })
}

/// The hidden legacy Codex alias intentionally remains fail-closed for every
/// ordinary ingress error, including malformed envelopes. The one exception
/// is the fixed platform capability result: nonterminal callbacks must not
/// break a Codex task, but they still print its safe Unix-host remedy; a
/// SessionEnd returns the same fatal capability diagnostic as `libra hooks`.
async fn run_codex(sub: HookCommandKind, deadline: Option<CaptureDeadline>) -> CliResult<()> {
    let cmd = sub.as_command();
    let expected_kind = cmd.lifecycle_event_kind();
    let result = process_hook_event_with_target(
        cmd,
        expected_kind,
        codex_provider(),
        HookTarget::AgentTraces,
        deadline,
    )
    .await;
    settle_legacy_codex_capture_result(cmd, result)
}

fn settle_legacy_codex_capture_result(
    cmd: ProviderHookCommand,
    capture_result: anyhow::Result<()>,
) -> CliResult<()> {
    match capture_result {
        Ok(()) => Ok(()),
        Err(error) => {
            if let Some(result) =
                settle_codex_unsupported_platform(cmd, &error, "agent hook ingestion failed")
            {
                result
            } else {
                Err(map_capture_ingest_error(
                    error,
                    "agent hook ingestion failed",
                    CaptureErrorSurface::FailClosed,
                ))
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        internal::ai::{
            capture::ingress::HookEnvelopeInvalid,
            hooks::runtime::{
                CAPTURE_UNSUPPORTED_PLATFORM_REMEDY, advisory_no_evidence_error,
                terminal_persistence_failure_error, unsupported_platform_scope_binding_failure,
            },
        },
        utils::error::StableErrorCode,
    };

    /// R86 #4: the hidden alias stays fail-closed outside a Libra repository,
    /// but with the shipped repository-not-found contract (`LBR-REPO-001`)
    /// rather than an internal capture failure, for every event.
    #[test]
    fn legacy_codex_alias_outside_repository_reports_repository_not_found() {
        use crate::internal::ai::{
            capture::scope_binding::no_active_repository_scope_binding_failure,
            hooks::runtime::classify_capture_ingress_for_target,
        };

        for cmd in [
            ProviderHookCommand::SessionStart,
            ProviderHookCommand::Stop,
            ProviderHookCommand::SessionEnd,
        ] {
            let classified = classify_capture_ingress_for_target(
                Err(no_active_repository_scope_binding_failure()),
                crate::internal::ai::hooks::HookTarget::AgentTraces,
                cmd.lifecycle_event_kind(),
            )
            .map(|_| ());
            let error = settle_legacy_codex_capture_result(cmd, classified)
                .expect_err("the legacy alias remains fail-closed outside a repository");
            assert_eq!(
                error.stable_code(),
                StableErrorCode::RepoNotFound,
                "{cmd}: the legacy alias must keep the shipped repository-not-found code"
            );
            assert!(
                error
                    .render()
                    .contains("not a libra repository (or any of the parent directories): .libra"),
                "{cmd}: the legacy alias must keep the fixed repository-not-found message: {error}"
            );
        }
    }

    /// R88 #8: the hidden legacy Codex alias is fail-closed, so a damaged
    /// active repository keeps the retired preflight's repository stable code
    /// and fixed remedy for every event (not the generic internal failure),
    /// whether the class arrives from scope binding or post-binding setup.
    #[test]
    fn legacy_codex_alias_restores_active_repository_classes() {
        use crate::internal::ai::{
            capture::scope_binding::{
                ActiveRepositoryFailureClass, active_repository_failure,
                trusted_active_repository_failure,
            },
            hooks::runtime::classify_capture_ingress_for_target,
        };

        for cmd in [ProviderHookCommand::Stop, ProviderHookCommand::SessionEnd] {
            let scope = classify_capture_ingress_for_target(
                Err(trusted_active_repository_failure(
                    ActiveRepositoryFailureClass::StorageUnresolved,
                )),
                crate::internal::ai::hooks::HookTarget::AgentTraces,
                cmd.lifecycle_event_kind(),
            )
            .map(|_| ());
            let error = settle_legacy_codex_capture_result(cmd, scope)
                .expect_err("the legacy alias stays fail-closed for a damaged worktree");
            assert_eq!(
                error.stable_code(),
                StableErrorCode::RepoStateInvalid,
                "{cmd}: a damaged linked worktree must keep LBR-REPO-003"
            );
            assert!(
                error.render().contains(
                    "agent hook ingestion failed: could not resolve the active repository storage"
                ) && error
                    .render()
                    .contains("`libra worktree repair --confirm <worktree-path>`"),
                "{cmd}: the legacy alias must render the fixed worktree remedy: {error}"
            );

            let database = terminal_persistence_failure_error(active_repository_failure(
                ActiveRepositoryFailureClass::DatabaseMissing,
            ));
            let error = settle_legacy_codex_capture_result(cmd, Err(database))
                .expect_err("the legacy alias stays fail-closed for a missing database");
            assert_eq!(
                error.stable_code(),
                StableErrorCode::RepoCorrupt,
                "{cmd}: a missing repository database must keep LBR-REPO-002"
            );
            assert!(
                !error.render().contains("capture could not be completed"),
                "{cmd}: the restored class must replace the generic message: {error}"
            );
        }
    }

    #[test]
    fn legacy_codex_alias_keeps_invalid_envelopes_fail_closed_but_shares_platform_policy() {
        let invalid = HookEnvelopeInvalid("invalid test envelope".to_string()).into();
        let invalid =
            settle_legacy_codex_capture_result(ProviderHookCommand::SessionStart, Err(invalid))
                .expect_err("legacy alias must keep malformed envelopes fail-closed");
        assert_eq!(
            invalid.stable_code(),
            StableErrorCode::AgentHookEnvelopeInvalid
        );

        let nonterminal = advisory_no_evidence_error(unsupported_platform_scope_binding_failure());
        assert!(
            settle_legacy_codex_capture_result(ProviderHookCommand::SessionStart, Err(nonterminal))
                .is_ok(),
            "the fixed unsupported-platform capability must acknowledge a nonterminal legacy callback"
        );

        let terminal =
            terminal_persistence_failure_error(unsupported_platform_scope_binding_failure());
        let terminal =
            settle_legacy_codex_capture_result(ProviderHookCommand::SessionEnd, Err(terminal))
                .expect_err(
                    "a legacy Codex SessionEnd must surface the platform capability failure",
                );
        assert_eq!(
            terminal.stable_code(),
            StableErrorCode::Unsupported,
            "the legacy terminal result must preserve the shared fixed capability code"
        );
        assert!(
            terminal
                .render()
                .contains(CAPTURE_UNSUPPORTED_PLATFORM_REMEDY),
            "the legacy terminal diagnostic must retain the same safe Unix-host remedy: {terminal}"
        );
    }
}
