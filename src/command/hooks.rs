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
    internal::ai::{
        capture::{
            ingress::CaptureDeadline,
            scope_binding::{
                ActiveRepositoryFailureClass, active_repository_failure_class,
                is_no_active_repository_error,
            },
        },
        hooks::{
            HookAdvisoryNoEvidence, HookEnvelopeInvalid, HookTarget, LifecycleEventKind,
            process_hook_event_with_target,
            provider::{MAX_HOOK_CAPTURE_BUDGET_MILLIS, ProviderHookCommand},
            providers::{claude_provider, codex, codex_provider},
            runtime::{CAPTURE_UNSUPPORTED_PLATFORM_REMEDY, is_capture_unsupported_platform_error},
        },
    },
    utils::{
        error::{CliError, CliResult, StableErrorCode},
        output::OutputConfig,
    },
};

/// `--help` examples shown in `libra hooks --help` output.
///
/// `hooks` is the entry point invoked by external AI agent hook
/// configurations (Claude Code, Gemini) — it reads the hook event JSON
/// on stdin and records it into the libra session store. Each provider
/// exposes the provider's lifecycle events; the banner
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
    libra hooks codex permission-request    Codex PermissionRequest hook entry
    libra hooks codex stop                   Codex Stop hook entry (checkpoint boundary)
    libra hooks codex subagent-start         Codex SubagentStart hook entry
    libra hooks gemini <event>               Rejected with a hint: gemini is uninstall-only
                                             (remove stale configs with 'libra agent remove gemini')";

#[derive(Args, Debug)]
#[command(after_help = HOOKS_EXAMPLES)]
pub struct HooksArgs {
    /// Installer-owned portion of the provider hook timeout.  This is hidden
    /// because users configure the provider timeout, not the internal capture
    /// budget; managed hook commands persist it so runtime has an absolute
    /// deadline before it reads stdin.
    #[arg(
        long,
        global = true,
        hide = true,
        value_name = "MILLISECONDS",
        value_parser = parse_capture_budget_ms
    )]
    pub capture_budget_ms: Option<u64>,
    #[command(subcommand)]
    pub command: HooksProviderSubcommand,
}

/// Parse the bounded installer-owned capture budget without accepting an
/// arbitrary duration from an untrusted hook command line.
pub(crate) fn parse_capture_budget_ms(input: &str) -> Result<u64, String> {
    let value = input
        .parse::<u64>()
        .map_err(|_| "capture budget must be a whole number of milliseconds".to_string())?;
    if !(1..=MAX_HOOK_CAPTURE_BUDGET_MILLIS).contains(&value) {
        return Err(format!(
            "capture budget must be between 1 and {MAX_HOOK_CAPTURE_BUDGET_MILLIS} milliseconds"
        ));
    }
    Ok(value)
}

/// Convert the bounded command-line budget into the one monotonic deadline
/// used by capture ingress and every downstream I/O boundary.
pub(crate) fn capture_deadline_from_budget_ms(
    capture_budget_ms: Option<u64>,
) -> CliResult<Option<CaptureDeadline>> {
    match capture_budget_ms {
        None => Ok(None),
        Some(budget_ms) => CaptureDeadline::from_budget_millis(budget_ms)
            .map(Some)
            .map_err(|error| {
                CliError::fatal(format!(
                    "unable to establish hook capture deadline: {error}"
                ))
            }),
    }
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

impl ProviderHookSubcommand {
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
    args: HooksArgs,
    _output: &OutputConfig,
    deadline: Option<CaptureDeadline>,
) -> CliResult<()> {
    let HooksArgs { command, .. } = args;
    match command {
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
                deadline,
            )
            .await
            .map_err(|err| {
                map_capture_ingest_error(
                    err,
                    "hook ingestion failed",
                    CaptureErrorSurface::FailClosed,
                )
            })
        }
        // AG-19: codex hook entries route to the AgentTraces capture path
        // (`refs/libra/traces`) — the stable installed surface per the
        // Codex capture contract.
        HooksProviderSubcommand::Codex { command } => {
            let cmd = command.as_command();
            let expected_kind = cmd.lifecycle_event_kind();
            let capture_result = process_hook_event_with_target(
                cmd,
                expected_kind,
                codex_provider(),
                HookTarget::AgentTraces,
                deadline,
            )
            .await;
            settle_codex_capture_result(cmd, capture_result)?;
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
        HooksProviderSubcommand::Gemini { command: _ } => {
            Err(gemini_hook_rejection("hook ingestion failed").await)
        }
    }
}

/// Shared rejection for the uninstall-only `hooks gemini` and hidden
/// `agent hooks gemini` entries, which read no stdin and never ingest.
///
/// These entries keep the repository contract the CLI preflight produced
/// before hook dispatch became lazy, rendered path-free: outside any Libra
/// repository the fixed repository-not-found error (`LBR-REPO-001`, exit
/// 128); in a repository whose storage cannot be resolved (detached,
/// migrating or corrupt linked worktree) `LBR-REPO-003`; with a missing
/// repository database or an unsupported object format `LBR-REPO-002`; with
/// an unopenable database (including one a newer Libra wrote, or whose
/// pending schema upgrade fails) or an unreadable object format
/// `LBR-IO-001`. Like that preflight, the open applies pending repository
/// schema migrations. A healthy repository rejects with the actionable
/// uninstall hint.
pub(crate) async fn gemini_hook_rejection(context: &str) -> CliError {
    let storage = match crate::utils::util::try_get_storage_path(None) {
        Ok(storage) => storage,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return CliError::repo_not_found();
        }
        Err(_) => {
            return active_repository_failure_error(
                ActiveRepositoryFailureClass::StorageUnresolved,
                context,
            );
        }
    };
    if let Some(class) = probe_active_repository_database(&storage).await {
        return active_repository_failure_error(class, context);
    }
    CliError::fatal(
        "gemini hook ingestion is disabled: gemini is uninstall-only \
         (not in the supported agent roster)",
    )
    .with_hint(
        "remove the stale hook config with 'libra agent remove gemini'; \
         previously captured gemini sessions stay readable",
    )
}

/// Replacement for the database half of the retired hook preflight: open the
/// repository database the way every repository command opens it (pending
/// schema migrations are applied; a schema written by a newer Libra is
/// refused) and validate its `core.objectformat` (without changing the
/// process-wide hash kind). Returns only the closed failure class; the
/// database path and any underlying message are never surfaced.
async fn probe_active_repository_database(
    storage: &std::path::Path,
) -> Option<ActiveRepositoryFailureClass> {
    let database = storage.join(crate::utils::util::DATABASE);
    let Some(database) = database.to_str() else {
        return Some(ActiveRepositoryFailureClass::DatabaseUnavailable);
    };
    let conn = match crate::internal::db::establish_connection(database).await {
        Ok(conn) => conn,
        Err(error) => {
            return Some(ActiveRepositoryFailureClass::for_database_open(
                error.kind(),
            ));
        }
    };
    let object_format = crate::internal::ai::capture::live::read_repository_hash_kind(&conn).await;
    // Best-effort close of this private probe connection; the
    // classification above is already decided.
    let _ = conn.close().await;
    match object_format {
        Ok(_) => None,
        Err(error) => Some(
            active_repository_failure_class(&error)
                .unwrap_or(ActiveRepositoryFailureClass::ObjectFormatUnreadable),
        ),
    }
}

/// Render one closed active-repository failure class as the fixed, path-free
/// public error the generic CLI preflight produced for these hook surfaces
/// before dispatch became lazy. The stable codes and remedies match `libra
/// agent doctor` and the repository preflight of every other command:
/// unresolvable storage is `LBR-REPO-003`, a missing database or unsupported
/// object format `LBR-REPO-002`, and an unopenable database or unreadable
/// object format `LBR-IO-001`.
fn active_repository_failure_error(class: ActiveRepositoryFailureClass, context: &str) -> CliError {
    match class {
        ActiveRepositoryFailureClass::StorageUnresolved => CliError::fatal(format!(
            "{context}: could not resolve the active repository storage (detached, migrating \
             or corrupt linked worktree); from the main worktree run `libra worktree repair \
             --confirm <worktree-path>` (or re-add the worktree), then retry the hook"
        ))
        .with_stable_code(StableErrorCode::RepoStateInvalid),
        ActiveRepositoryFailureClass::DatabaseMissing => CliError::fatal(format!(
            "{context}: repository database not found; restore the repository's .libra \
             storage (for example from a backup), then retry the hook"
        ))
        .with_stable_code(StableErrorCode::RepoCorrupt),
        ActiveRepositoryFailureClass::DatabaseUnavailable => CliError::fatal(format!(
            "{context}: could not open the repository database; if it was written by a newer \
             Libra, install a newer Libra binary; otherwise restore repository storage, then \
             retry the hook"
        ))
        .with_stable_code(StableErrorCode::IoReadFailed),
        ActiveRepositoryFailureClass::ObjectFormatUnreadable => CliError::fatal(format!(
            "{context}: could not read the repository object format; repair the repository \
             database, then retry the hook"
        ))
        .with_stable_code(StableErrorCode::IoReadFailed),
        ActiveRepositoryFailureClass::ObjectFormatUnsupported => CliError::fatal(format!(
            "{context}: the repository configuration names an unsupported object format; \
             repair core.objectformat, then retry the hook"
        ))
        .with_stable_code(StableErrorCode::RepoCorrupt),
    }
}

/// Stable, path-free stderr prefix for the fixed capture-platform diagnostic.
///
/// The rest of this line is [`CAPTURE_UNSUPPORTED_PLATFORM_REMEDY`], which is
/// the only helper failure safe to render to an external hook host.
const CODEX_CAPTURE_UNSUPPORTED_PLATFORM_STDERR_PREFIX: &str = "libra: ";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum CodexUnsupportedPlatformDisposition {
    Acknowledge,
    Fatal,
}

fn codex_unsupported_platform_disposition(
    cmd: ProviderHookCommand,
) -> CodexUnsupportedPlatformDisposition {
    if cmd.lifecycle_event_kind() == LifecycleEventKind::SessionEnd {
        CodexUnsupportedPlatformDisposition::Fatal
    } else {
        CodexUnsupportedPlatformDisposition::Acknowledge
    }
}

fn write_codex_unsupported_platform_diagnostic<W: std::io::Write>(
    output: &mut W,
) -> std::io::Result<()> {
    writeln!(
        output,
        "{CODEX_CAPTURE_UNSUPPORTED_PLATFORM_STDERR_PREFIX}{CAPTURE_UNSUPPORTED_PLATFORM_REMEDY}"
    )
}

/// Apply the one fixed non-Unix capability policy shared by both Codex hook
/// entry points. This intentionally does not handle other advisory results:
/// the hidden legacy alias remains fail-closed for malformed envelopes and
/// all ordinary failures.
pub(crate) fn settle_codex_unsupported_platform(
    cmd: ProviderHookCommand,
    error: &anyhow::Error,
    context: &'static str,
) -> Option<CliResult<()>> {
    if !is_capture_unsupported_platform_error(error) {
        return None;
    }

    match codex_unsupported_platform_disposition(cmd) {
        CodexUnsupportedPlatformDisposition::Acknowledge => {
            tracing::warn!(
                target: "agent.hook.ingest",
                provider = "codex",
                verb = %cmd,
                reason = "capture_unsupported_platform",
                "Codex Session Capture is unavailable on this platform; acknowledging nonterminal callback"
            );
            let _ = write_codex_unsupported_platform_diagnostic(&mut std::io::stderr().lock());
            Some(Ok(()))
        }
        CodexUnsupportedPlatformDisposition::Fatal => Some(Err(CliError::fatal(format!(
            "{context}: {CAPTURE_UNSUPPORTED_PLATFORM_REMEDY}"
        ))
        .with_stable_code(StableErrorCode::Unsupported))),
    }
}

/// Apply Codex's deliberately narrow advisory policy after runtime ingress
/// has classified the failure. SessionEnd is acknowledged only when no
/// trusted capture identity was established; all post-proof terminal failures
/// surface to the hook host.
pub(crate) fn settle_codex_capture_result(
    cmd: ProviderHookCommand,
    capture_result: anyhow::Result<()>,
) -> CliResult<()> {
    match capture_result {
        Ok(()) => Ok(()),
        Err(error) => {
            if let Some(result) =
                settle_codex_unsupported_platform(cmd, &error, "Codex hook ingestion failed")
            {
                return result;
            }
            if error
                .chain()
                .any(|cause| cause.is::<HookAdvisoryNoEvidence>())
            {
                // No validated frame/scope exists, so this callback has no
                // trusted identity from which a terminal receipt could be
                // constructed. Keep Codex's advisory fail-open only for this
                // explicitly classified pre-ingress outcome.
                tracing::warn!(
                    target: "agent.hook.ingest",
                    provider = "codex",
                    verb = %cmd,
                    reason = "advisory_no_capture_evidence",
                    "Codex hook ended before trusted capture ingress; acknowledging callback"
                );
                return Ok(());
            }
            if cmd.lifecycle_event_kind() != LifecycleEventKind::SessionEnd {
                // Nonterminal capture remains advisory for Codex: a later
                // lifecycle boundary can retry/repair its state and this provider
                // treats any non-zero hook exit as a task failure. SessionEnd is
                // deliberately excluded — a trusted terminal failure must
                // surface rather than be silently acknowledged without recovery
                // evidence.
                tracing::warn!(
                    target: "agent.hook.ingest",
                    provider = "codex",
                    verb = %cmd,
                    reason = "nonterminal_capture_failed",
                    "Codex nonterminal hook capture failed; acknowledging callback"
                );
                return Ok(());
            }
            // A damaged active repository is reported with the same fixed,
            // path-free repository code and remedy as the fail-closed
            // surfaces; the generic diagnostic would wrongly suggest that a
            // retry can succeed.
            if let Some(class) = active_repository_failure_class(&error) {
                return Err(active_repository_failure_error(
                    class,
                    "Codex hook ingestion failed",
                ));
            }
            Err(map_capture_ingest_error(
                error,
                "Codex hook ingestion failed",
                CaptureErrorSurface::CodexTerminal,
            ))
        }
    }
}

/// Which public policy renders a capture failure that reached a non-zero
/// hook exit.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum CaptureErrorSurface {
    /// The installed Claude surface and the hidden `libra agent hooks`
    /// aliases (Claude Code / OpenCode / Codex), which fail closed. A damaged
    /// active repository keeps the repository stable code and remedy the
    /// generic CLI preflight reported before hook dispatch became lazy.
    FailClosed,
    /// A trusted installed-Codex `SessionEnd`. It keeps its terminal
    /// classification and exit code; a damaged active repository is rendered
    /// with its repository code by `settle_codex_capture_result` before this
    /// mapping, and every other trusted failure keeps the generic diagnostic.
    CodexTerminal,
}

/// Map a shared capture-ingress failure to the public CLI contract.
///
/// Both the installed `libra hooks claude …` surface and the hidden
/// `libra agent hooks …` diagnostic surface use the same parser. Envelope
/// rejects therefore retain their safe, actionable `LBR-AGENT-008` detail.
/// All other post-ingress failures become one fixed actionable fatal: runtime
/// errors can include provider-controlled paths or session identities and
/// must not be rendered on the hook's stderr surface.
///
/// A well-formed callback invoked outside any Libra repository keeps the
/// shipped repository-not-found contract (`LBR-REPO-001`, exit 128) with the
/// fixed, path-free message; it is never reported as an internal failure.
///
/// The one fixed platform-capability refusal (non-Unix hosts cannot
/// initialize the repository-private replay key) is safe to render: these
/// fail-closed surfaces report the same path-free Unix-host remedy and
/// `LBR-UNSUPPORTED-001` code as a Codex `SessionEnd`, rather than the
/// generic retry message, because retrying on the same host can never
/// succeed.
///
/// On a [`CaptureErrorSurface::FailClosed`] surface, a typed, closed
/// active-repository class (unresolvable worktree storage, a missing or
/// unopenable repository database, an unreadable or unsupported object
/// format) next restores the preflight's `LBR-REPO-003` / `LBR-REPO-002` /
/// `LBR-IO-001` error with a fixed, path-free remedy. Precedence is:
/// envelope-invalid and no-repository, unsupported platform, the restored
/// repository classes, then the generic message.
pub(crate) fn map_capture_ingest_error(
    err: anyhow::Error,
    context: &str,
    surface: CaptureErrorSurface,
) -> CliError {
    if is_no_active_repository_error(&err) {
        return CliError::repo_not_found();
    }
    let envelope_invalid = err.chain().any(|cause| cause.is::<HookEnvelopeInvalid>());
    if !envelope_invalid && is_capture_unsupported_platform_error(&err) {
        return CliError::fatal(format!("{context}: {CAPTURE_UNSUPPORTED_PLATFORM_REMEDY}"))
            .with_stable_code(StableErrorCode::Unsupported);
    }
    if !envelope_invalid
        && surface == CaptureErrorSurface::FailClosed
        && let Some(class) = active_repository_failure_class(&err)
    {
        return active_repository_failure_error(class, context);
    }
    if envelope_invalid {
        CliError::fatal(format!("{context}: {err}"))
            .with_stable_code(StableErrorCode::AgentHookEnvelopeInvalid)
    } else {
        CliError::fatal(format!(
            "{context}: capture could not be completed; retry the hook or inspect the local repository"
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hidden_capture_budget_is_bounded_and_forms_a_deadline() {
        assert_eq!(parse_capture_budget_ms("1"), Ok(1));
        assert_eq!(
            parse_capture_budget_ms(&MAX_HOOK_CAPTURE_BUDGET_MILLIS.to_string()),
            Ok(MAX_HOOK_CAPTURE_BUDGET_MILLIS)
        );
        assert!(parse_capture_budget_ms("0").is_err());
        assert!(parse_capture_budget_ms("not-a-number").is_err());
        assert!(
            parse_capture_budget_ms(&(MAX_HOOK_CAPTURE_BUDGET_MILLIS + 1).to_string()).is_err()
        );
        let deadline = capture_deadline_from_budget_ms(Some(1)).expect("bounded deadline");
        assert!(deadline.is_some());
        assert_eq!(
            capture_deadline_from_budget_ms(None).expect("no budget"),
            None
        );
    }

    #[test]
    fn codex_terminal_policy_acknowledges_only_unverified_scope_failures() {
        let advisory = crate::internal::ai::hooks::runtime::advisory_no_evidence_error(
            anyhow::anyhow!("test helper ended before a scope proof"),
        );
        assert!(
            settle_codex_capture_result(ProviderHookCommand::SessionEnd, Err(advisory)).is_ok(),
            "a SessionEnd without a trusted scope proof must remain advisory"
        );

        let terminal = crate::internal::ai::hooks::runtime::terminal_persistence_failure_error(
            anyhow::anyhow!("test helper transport failed after a trusted scope proof"),
        );
        let error =
            match settle_codex_capture_result(ProviderHookCommand::SessionEnd, Err(terminal)) {
                Ok(()) => panic!("a trusted SessionEnd failure must surface to Codex"),
                Err(error) => error,
            };
        assert!(
            error
                .to_string()
                .contains("Codex hook ingestion failed: capture could not be completed"),
            "Codex must retain a terminal runtime failure rather than acknowledge it: {error}"
        );

        assert!(
            settle_codex_capture_result(
                ProviderHookCommand::Stop,
                Err(anyhow::anyhow!("test nonterminal capture failure")),
            )
            .is_ok(),
            "the existing nonterminal Codex advisory policy remains unchanged"
        );
    }

    /// R86 #4: a well-formed callback outside every Libra repository has no
    /// scope to bind. It must never become a trusted terminal failure: Codex
    /// acknowledges it (including `SessionEnd`), while the fail-closed surfaces
    /// report the shipped, path-free repository-not-found contract.
    #[test]
    fn outside_repository_capture_keeps_shipped_hook_contract() {
        use crate::internal::ai::{
            capture::scope_binding::no_active_repository_scope_binding_failure,
            hooks::runtime::{HookTerminalPersistenceFailure, classify_capture_ingress_for_target},
        };

        for cmd in [
            ProviderHookCommand::SessionStart,
            ProviderHookCommand::Prompt,
            ProviderHookCommand::ToolUse,
            ProviderHookCommand::PermissionRequest,
            ProviderHookCommand::ModelUpdate,
            ProviderHookCommand::Compaction,
            ProviderHookCommand::Stop,
            ProviderHookCommand::SessionEnd,
            ProviderHookCommand::SubagentStart,
            ProviderHookCommand::SubagentEnd,
        ] {
            let classified = match classify_capture_ingress_for_target(
                Err(no_active_repository_scope_binding_failure()),
                HookTarget::AgentTraces,
                cmd.lifecycle_event_kind(),
            ) {
                Ok(_) => panic!("{cmd}: an outside-repository callback must not bind"),
                Err(error) => error,
            };
            assert!(
                classified
                    .chain()
                    .any(|cause| cause.is::<HookAdvisoryNoEvidence>())
                    && !classified
                        .chain()
                        .any(|cause| cause.is::<HookTerminalPersistenceFailure>()),
                "{cmd}: a callback outside any repository must stay advisory: {classified:#}"
            );
            assert!(
                is_no_active_repository_error(&classified),
                "{cmd}: the advisory wrapper must keep the typed no-repository cause"
            );

            let codex = settle_codex_capture_result(
                cmd,
                classify_capture_ingress_for_target(
                    Err(no_active_repository_scope_binding_failure()),
                    HookTarget::AgentTraces,
                    cmd.lifecycle_event_kind(),
                )
                .map(|_| ()),
            );
            assert!(
                codex.is_ok(),
                "{cmd}: Codex must acknowledge a callback outside any Libra repository: {codex:?}"
            );

            let fail_closed = map_capture_ingest_error(
                classified,
                "hook ingestion failed",
                CaptureErrorSurface::FailClosed,
            );
            assert_eq!(
                fail_closed.stable_code(),
                StableErrorCode::RepoNotFound,
                "{cmd}: the fail-closed surfaces must keep the shipped repository-not-found code"
            );
            let rendered = fail_closed.render();
            assert!(
                rendered
                    .contains("not a libra repository (or any of the parent directories): .libra")
                    && !rendered.contains("capture could not be completed"),
                "{cmd}: outside-repository callbacks must not be reported as internal capture failures: {rendered}"
            );
        }
    }

    #[test]
    fn capture_ingest_error_omits_internal_error_chain_from_hook_stderr() {
        let sentinel = "codex__/pi__native-session-secret-9d6c";
        let rendered = map_capture_ingest_error(
            anyhow::anyhow!("session cache at /tmp/{sentinel} could not be read"),
            "hook ingestion failed",
            CaptureErrorSurface::FailClosed,
        )
        .render();
        assert!(
            !rendered.contains(sentinel),
            "post-ingress hook failures must not render an internal chain: {rendered}"
        );
        assert!(
            rendered.contains(
                "capture could not be completed; retry the hook or inspect the local repository"
            ),
            "post-ingress hook failures must retain an actionable fixed reason: {rendered}"
        );

        let envelope = map_capture_ingest_error(
            HookEnvelopeInvalid("hook cwd is outside the active worktree".to_string()).into(),
            "hook ingestion failed",
            CaptureErrorSurface::FailClosed,
        );
        assert_eq!(
            envelope.stable_code(),
            StableErrorCode::AgentHookEnvelopeInvalid,
            "validated envelope rejects retain their public stable code"
        );
        assert!(
            envelope
                .render()
                .contains("hook cwd is outside the active worktree"),
            "safe envelope diagnostics remain actionable"
        );
    }

    /// R87 (A): the fail-closed installed Claude surface (`hook ingestion
    /// failed`) and the hidden `libra agent hooks` Claude Code / OpenCode
    /// alias (`agent hook ingestion failed`) must render the same fixed,
    /// path-free Unix-host remedy and `LBR-UNSUPPORTED-001` code as a Codex
    /// `SessionEnd` for the non-Unix capability refusal, not the generic
    /// "retry the hook" message. The error is constructed directly so this
    /// runs on every platform, and it carries a path-like context to prove
    /// the remedy is the only text rendered from the chain.
    #[test]
    fn fail_closed_surfaces_render_unsupported_platform_remedy() {
        use crate::internal::ai::hooks::runtime::{
            classify_capture_ingress_for_target, unsupported_platform_scope_binding_failure,
        };

        let sentinel = "/tmp/claude__native-session-secret-4f1e/.libra/capture";
        let unsupported = || {
            unsupported_platform_scope_binding_failure()
                .context(format!("replay key directory {sentinel} is unavailable"))
        };
        let classified = |cmd: ProviderHookCommand| match classify_capture_ingress_for_target(
            Err(unsupported()),
            HookTarget::AgentTraces,
            cmd.lifecycle_event_kind(),
        ) {
            Ok(_) => panic!("{cmd}: an unsupported-platform scope failure must not bind"),
            Err(error) => error,
        };

        for context in ["hook ingestion failed", "agent hook ingestion failed"] {
            for (label, error) in [
                ("unclassified", unsupported()),
                ("nonterminal", classified(ProviderHookCommand::Stop)),
                ("terminal", classified(ProviderHookCommand::SessionEnd)),
            ] {
                assert!(
                    is_capture_unsupported_platform_error(&error)
                        && !is_no_active_repository_error(&error)
                        && format!("{error:#}").contains(sentinel),
                    "{context}/{label}: fixture must be the typed capability refusal carrying a path: {error:#}"
                );
                let mapped =
                    map_capture_ingest_error(error, context, CaptureErrorSurface::FailClosed);
                assert_eq!(
                    mapped.stable_code(),
                    StableErrorCode::Unsupported,
                    "{context}/{label}: the capability refusal must keep its fixed safe code"
                );
                let rendered = mapped.render();
                assert!(
                    rendered.contains(&format!("{context}: {CAPTURE_UNSUPPORTED_PLATFORM_REMEDY}")),
                    "{context}/{label}: the fail-closed surface must render the Unix-host remedy: {rendered}"
                );
                assert!(
                    !rendered.contains(sentinel)
                        && !rendered.contains("replay key directory")
                        && !rendered.contains("capture could not be completed"),
                    "{context}/{label}: only the fixed remedy may be rendered: {rendered}"
                );
            }
        }
    }

    /// Every closed active-repository class with the stable code, fixed
    /// reason and remedy the retired CLI repository preflight reported.
    const ACTIVE_REPOSITORY_CLASS_CONTRACT: [(
        ActiveRepositoryFailureClass,
        StableErrorCode,
        &str,
        &str,
    ); 5] = [
        (
            ActiveRepositoryFailureClass::StorageUnresolved,
            StableErrorCode::RepoStateInvalid,
            "could not resolve the active repository storage",
            "from the main worktree run `libra worktree repair --confirm <worktree-path>`",
        ),
        (
            ActiveRepositoryFailureClass::DatabaseMissing,
            StableErrorCode::RepoCorrupt,
            "repository database not found",
            "restore the repository's .libra storage",
        ),
        (
            ActiveRepositoryFailureClass::DatabaseUnavailable,
            StableErrorCode::IoReadFailed,
            "could not open the repository database",
            "install a newer Libra binary; otherwise restore repository storage",
        ),
        (
            ActiveRepositoryFailureClass::ObjectFormatUnreadable,
            StableErrorCode::IoReadFailed,
            "could not read the repository object format",
            "repair the repository database",
        ),
        (
            ActiveRepositoryFailureClass::ObjectFormatUnsupported,
            StableErrorCode::RepoCorrupt,
            "the repository configuration names an unsupported object format",
            "repair core.objectformat",
        ),
    ];

    /// R88 #8: the fail-closed surfaces restore the repository stable code
    /// and remedy of the retired CLI preflight for every typed, closed
    /// active-repository class, whichever runtime wrapper (bare post-binding
    /// failure, advisory, trusted scope failure, trusted terminal) carries
    /// it. The underlying (path-bearing) chain is never rendered, and the
    /// installed Codex terminal keeps its generic diagnostic and code.
    #[test]
    fn fail_closed_surfaces_restore_active_repository_classes() {
        use crate::internal::ai::{
            capture::scope_binding::{
                active_repository_failure, trusted_active_repository_failure,
            },
            hooks::runtime::{
                advisory_no_evidence_error, classify_capture_ingress_for_target,
                terminal_persistence_failure_error,
            },
        };

        let sentinel = "/tmp/claude__native-session-secret-91aa/.libra/libra.db";
        for (class, code, reason, remedy) in ACTIVE_REPOSITORY_CLASS_CONTRACT {
            let failure = || {
                active_repository_failure(class)
                    .context(format!("repository database {sentinel} failed"))
            };
            let scope_failure = |cmd: ProviderHookCommand| match classify_capture_ingress_for_target(
                Err(trusted_active_repository_failure(class)),
                HookTarget::AgentTraces,
                cmd.lifecycle_event_kind(),
            ) {
                Ok(_) => panic!("{class:?}: a repository failure must not bind"),
                Err(error) => error,
            };
            for context in ["hook ingestion failed", "agent hook ingestion failed"] {
                for (label, error) in [
                    ("post-binding", failure()),
                    ("advisory", advisory_no_evidence_error(failure())),
                    ("terminal", terminal_persistence_failure_error(failure())),
                    (
                        "scope-nonterminal",
                        scope_failure(ProviderHookCommand::Stop),
                    ),
                    (
                        "scope-terminal",
                        scope_failure(ProviderHookCommand::SessionEnd),
                    ),
                ] {
                    assert_eq!(
                        active_repository_failure_class(&error),
                        Some(class),
                        "{class:?}/{label}: fixture must carry the typed class"
                    );
                    let mapped =
                        map_capture_ingest_error(error, context, CaptureErrorSurface::FailClosed);
                    assert_eq!(
                        mapped.stable_code(),
                        code,
                        "{class:?}/{context}/{label}: the preflight stable code must be restored"
                    );
                    let rendered = mapped.render();
                    assert!(
                        rendered.contains(&format!("{context}: {reason}"))
                            && rendered.contains(remedy),
                        "{class:?}/{context}/{label}: the fixed reason and remedy must render: {rendered}"
                    );
                    assert!(
                        !rendered.contains(sentinel)
                            && !rendered.contains("repository database /tmp")
                            && !rendered.contains("capture could not be completed"),
                        "{class:?}/{context}/{label}: only the fixed path-free text may render: {rendered}"
                    );
                }
            }

            // Installed Codex: nonterminal stays advisory and a trusted
            // SessionEnd reports the same repository code and remedy.
            assert!(
                settle_codex_capture_result(
                    ProviderHookCommand::Stop,
                    Err(terminal_persistence_failure_error(failure())),
                )
                .is_ok(),
                "{class:?}: nonterminal Codex capture failures stay advisory"
            );
            let codex = match settle_codex_capture_result(
                ProviderHookCommand::SessionEnd,
                Err(terminal_persistence_failure_error(failure())),
            ) {
                Ok(()) => panic!("{class:?}: a trusted Codex SessionEnd failure must surface"),
                Err(error) => error,
            };
            let expected = active_repository_failure_error(class, "Codex hook ingestion failed");
            assert_eq!(
                codex.stable_code(),
                expected.stable_code(),
                "{class:?}: a damaged repository keeps its repository code on Codex SessionEnd"
            );
            assert_eq!(
                codex.render(),
                expected.render(),
                "{class:?}: Codex SessionEnd renders the fixed repository remedy: {codex}"
            );
            assert!(
                !codex.render().contains("capture could not be completed")
                    && !codex.render().contains(sentinel),
                "{class:?}: no generic retry advice or path on Codex SessionEnd: {codex}"
            );
        }

        // Errors without a typed class keep the generic fixed message even if
        // their text resembles a repository failure.
        let unclassified = map_capture_ingest_error(
            anyhow::anyhow!("repository database not found at {sentinel}"),
            "hook ingestion failed",
            CaptureErrorSurface::FailClosed,
        );
        assert_eq!(
            unclassified.stable_code(),
            StableErrorCode::InternalInvariant
        );
        assert!(
            unclassified
                .render()
                .contains("hook ingestion failed: capture could not be completed")
                && !unclassified.render().contains(sentinel),
            "classification must be typed, never inferred from message text"
        );
    }

    /// R88 #8 / R89: the uninstall-only gemini entries restore the retired
    /// preflight's database half — the same migrate-and-fence open every
    /// repository command performs — and return only the closed class
    /// (missing, unopenable or newer-Libra database, unreadable or
    /// unsupported object format), never the database path.
    #[tokio::test]
    async fn gemini_repository_probe_classifies_database_failures() {
        use sea_orm::ConnectionTrait;

        let root = tempfile::tempdir().expect("create gemini probe tempdir");
        let storage = root.path().join(".libra");
        std::fs::create_dir_all(&storage).expect("create probe storage");
        let database = storage.join(crate::utils::util::DATABASE);

        assert_eq!(
            probe_active_repository_database(&storage).await,
            Some(ActiveRepositoryFailureClass::DatabaseMissing),
            "a missing database must keep the LBR-REPO-002 class"
        );

        std::fs::create_dir(&database).expect("replace database with a directory");
        assert_eq!(
            probe_active_repository_database(&storage).await,
            Some(ActiveRepositoryFailureClass::DatabaseUnavailable),
            "an unopenable database must keep the LBR-IO-001 class"
        );
        std::fs::remove_dir(&database).expect("remove directory fixture");

        let database_str = database.to_str().expect("UTF-8 probe database path");
        let latest = crate::internal::db::migration::latest_builtin_schema_version()
            .expect("built-in migration registry")
            .expect("at least one built-in migration");
        let max_receipt = |conn: sea_orm::DatabaseConnection| async move {
            let row = conn
                .query_one_raw(sea_orm::Statement::from_string(
                    conn.get_database_backend(),
                    "SELECT MAX(version) FROM schema_versions",
                ))
                .await
                .expect("query schema receipts")
                .expect("schema receipt row");
            let version: Option<i64> = row.try_get_by_index(0).expect("decode schema receipt");
            let _ = conn.close().await;
            version
        };

        // A current-schema database without its configuration table has no
        // object format to read; opening a compatible schema adds nothing.
        let conn = crate::internal::db::create_database(database_str)
            .await
            .expect("create probe repository database");
        conn.execute_unprepared("DROP TABLE config_kv")
            .await
            .expect("drop configuration table");
        let _ = conn.close().await;
        assert_eq!(
            probe_active_repository_database(&storage).await,
            Some(ActiveRepositoryFailureClass::ObjectFormatUnreadable),
            "an unreadable object format must keep the LBR-IO-001 class"
        );
        std::fs::remove_file(&database).expect("remove configuration-less fixture");

        // A schema written by a newer Libra is refused before any read, as
        // the repository preflight refused it (LBR-IO-001 newer-binary class),
        // and is left exactly as found.
        let conn = crate::internal::db::create_database(database_str)
            .await
            .expect("create future-schema probe database");
        conn.execute_raw(sea_orm::Statement::from_sql_and_values(
            conn.get_database_backend(),
            "INSERT INTO schema_versions (version, name, applied_at) VALUES (?, 'future', 'fixture')",
            [(latest + 1).into()],
        ))
        .await
        .expect("record a newer-Libra schema receipt");
        let _ = conn.close().await;
        assert_eq!(
            probe_active_repository_database(&storage).await,
            Some(ActiveRepositoryFailureClass::DatabaseUnavailable),
            "a newer-Libra schema must keep the LBR-IO-001 class"
        );
        let conn = crate::internal::db::open_database_without_migrations(&database)
            .await
            .expect("reopen future-schema fixture");
        assert_eq!(max_receipt(conn).await, Some(latest + 1));
        std::fs::remove_file(&database).expect("remove future-schema fixture");

        // A database last migrated by an older Libra is brought up to date
        // first, as every repository command does, then gets the hint.
        let conn = crate::internal::db::create_database(database_str)
            .await
            .expect("create stale probe database");
        let runner =
            crate::internal::db::migration::builtin_runner().expect("built-in migration runner");
        let previous = crate::internal::db::migration::builtin_migrations()
            .iter()
            .rev()
            .nth(1)
            .map(|migration| migration.version)
            .expect("a previous built-in migration");
        runner
            .rollback_to(&conn, previous)
            .await
            .expect("roll the newest migration back");
        let _ = conn.close().await;
        assert_eq!(
            probe_active_repository_database(&storage).await,
            None,
            "a healthy stale repository must reach the uninstall hint"
        );
        let conn = crate::internal::db::open_database_without_migrations(&database)
            .await
            .expect("reopen migrated fixture");
        assert_eq!(
            max_receipt(conn).await,
            Some(latest),
            "the gemini entry must apply pending migrations like the retired preflight"
        );
        std::fs::remove_file(&database).expect("remove migrated fixture");

        let conn = crate::internal::db::create_database(database_str)
            .await
            .expect("create probe repository database");
        crate::internal::config::ConfigKv::set_with_conn(
            &conn,
            "core.objectformat",
            "not-a-format",
            false,
        )
        .await
        .expect("seed unsupported object format");
        let _ = conn.close().await;
        assert_eq!(
            probe_active_repository_database(&storage).await,
            Some(ActiveRepositoryFailureClass::ObjectFormatUnsupported),
            "an unsupported object format must keep the LBR-REPO-002 class"
        );

        for (class, code, reason, _) in ACTIVE_REPOSITORY_CLASS_CONTRACT {
            let rendered = active_repository_failure_error(class, "agent hook ingestion failed");
            assert_eq!(rendered.stable_code(), code, "{class:?}");
            let text = rendered.render();
            assert!(
                text.contains(&format!("agent hook ingestion failed: {reason}"))
                    && !text.contains(&*root.path().to_string_lossy()),
                "{class:?}: the gemini rendering must be fixed and path-free: {text}"
            );
        }
    }

    #[test]
    fn codex_unsupported_platform_is_visible_and_event_specific() {
        let mut diagnostic = Vec::new();
        write_codex_unsupported_platform_diagnostic(&mut diagnostic)
            .expect("render fixed Codex capability diagnostic");
        assert_eq!(
            String::from_utf8(diagnostic).expect("fixed diagnostic is UTF-8"),
            "libra: Session Capture is unavailable on this platform because secure repository-private key initialization requires Unix descriptor-relative no-replace file APIs; run the hook on a Unix host\n",
            "the acknowledged callback must emit this exact path-free stderr diagnostic"
        );

        let nonterminal = crate::internal::ai::hooks::runtime::advisory_no_evidence_error(
            crate::internal::ai::hooks::runtime::unsupported_platform_scope_binding_failure(),
        );
        assert!(
            settle_codex_capture_result(ProviderHookCommand::SessionStart, Err(nonterminal))
                .is_ok(),
            "a nonterminal Codex callback must remain exit-zero after reporting the capability failure"
        );

        let terminal = crate::internal::ai::hooks::runtime::terminal_persistence_failure_error(
            crate::internal::ai::hooks::runtime::unsupported_platform_scope_binding_failure(),
        );
        let terminal = settle_codex_capture_result(ProviderHookCommand::SessionEnd, Err(terminal))
            .expect_err("a Codex SessionEnd must surface the capability failure");
        assert_eq!(
            terminal.stable_code(),
            StableErrorCode::Unsupported,
            "the terminal platform-capability result must retain its fixed safe code"
        );
        assert!(
            terminal
                .render()
                .contains(CAPTURE_UNSUPPORTED_PLATFORM_REMEDY),
            "the terminal error must carry the same safe Unix-host remedy: {terminal}"
        );
    }
}
