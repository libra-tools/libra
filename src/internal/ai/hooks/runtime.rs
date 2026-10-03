//! Shared runtime for provider lifecycle hook ingestion.
//!
//! When an external provider invokes `libra hooks <command>`, control lands in
//! [`process_hook_event_from_stdin`]. Its shared ingress boundary:
//! 1. Delegates bounded frame parsing and canonical envelope validation to
//!    `capture::ingress`.
//! 2. Asks the provider adapter to lower a validated envelope into a
//!    [`LifecycleEvent`](super::lifecycle::LifecycleEvent).
//! 3. Dispatches the validated command to an intent or trace target.
//! 4. The target then loads (or recovers) persistent state, deduplicates the event,
//!    applies it, and on `SessionEnd` writes a content-addressed `ai_session` blob
//!    plus a history reference so other tools can read the session later.
//!
//! The legacy intent target and its `MAX_*` bounds live in `hooks::intent`;
//! the killable cwd/replay-key binding helper lives in
//! `capture::scope_binding`, and the shared live boundary (execution
//! deadlines, session identity, bounded pure reads, the ingest span and the
//! scoped read probes) in `capture::live`. AgentTraces commands continue in
//! `capture::live_pipeline`, whose checkpoint writers live in
//! `capture::live_checkpoint`. The provider's live-capture policy is resolved
//! exactly once per callback (`LiveCaptureBinding::resolve`, from the typed
//! `HookProviderIdentity`) and handed down; no layer below maps a provider
//! name onto a catalog kind.

use std::time::Instant;

use anyhow::{Result, anyhow};
use thiserror::Error;

#[cfg(test)]
pub(crate) use super::intent::append_normalized_event;
// Compatibility re-exports for the moved live boundary (ADR-ACF-10): the
// legacy intent writer lives in `hooks::intent`, the killable scope-binding
// helper in `capture::scope_binding`, and the canonical session identity in
// `capture::live`.
pub use super::intent::{AI_SESSION_SCHEMA, AI_SESSION_TYPE};
use super::{
    intent::process_ai_intent_ingress, lifecycle::LifecycleEventKind, provider::HookProvider,
};
/// Compatibility re-exports for existing doctor and crash-replay callers.
/// The implementation lives at the checkpoint persistence boundary.
pub use crate::internal::ai::capture::checkpoint::{
    AgentCheckpointRow, SubagentCheckpointRow, insert_agent_checkpoint_row_idempotent,
    insert_subagent_checkpoint_row_idempotent,
};
pub(crate) use crate::internal::ai::capture::scope_binding::is_capture_unsupported_platform_error;
// Only command-adapter platform-policy tests construct this error now; the
// runtime keeps re-exporting it so those callers keep one stable path.
#[cfg_attr(not(test), allow(unused_imports))]
pub(crate) use crate::internal::ai::capture::scope_binding::unsupported_platform_scope_binding_failure;
pub use crate::internal::ai::capture::{
    ingress::HookEnvelopeInvalid,
    live::build_ai_session_id,
    scope_binding::{
        CAPTURE_SCOPE_BINDING_HELPER_ARG, CAPTURE_SCOPE_BINDING_HELPER_INPUT_CAP,
        CAPTURE_SCOPE_BINDING_HELPER_OUTPUT_CAP, run_capture_scope_binding_helper,
        run_capture_scope_binding_helper_to_writer,
    },
};
use crate::internal::ai::{
    capture::{
        ingress::{CaptureDeadline, CaptureIngressCommand, CaptureIngressOutcome},
        live::{
            HookExecutionDeadline, effective_capture_deadline, hook_execution_deadline,
            new_ingest_span,
        },
        live_pipeline::ingest_agent_traces,
        scope_binding::{HookTrustedScopeBindingFailure, bind_capture_scope_cwd_bounded},
    },
    observed_agents::live_capture::LiveCaptureBinding,
};

/// Establish the effective deadline at the outer AgentTraces boundary, before
/// ingress scope binding can create the repository replay key. Intent hooks
/// have no capture final-commit contract and preserve their legacy behavior.
fn effective_hook_capture_deadline(
    target: HookTarget,
    binding: LiveCaptureBinding,
    deadline: Option<CaptureDeadline>,
) -> Result<Option<CaptureDeadline>> {
    if target == HookTarget::AgentTraces {
        effective_capture_deadline(binding.missing_host_capture_budget(), deadline)
    } else {
        Ok(deadline)
    }
}

/// Where a parsed hook event should land.
///
/// CEX-EntireIO Phase 1.5 introduces this enum so the same parsing /
/// validation pipeline can fan out to two refs:
///
/// - [`HookTarget::AiIntent`] — the canonical `refs/libra/intent` writer used
///   by `libra code` and the existing Claude/Gemini hook configs.
/// - [`HookTarget::AgentTraces`] — the external-Agent capture writer that
///   lives on `refs/libra/traces`. Fully wired: the runtime ingests the
///   lifecycle event into `agent_session` and writes E4-libra checkpoint
///   commits on `TurnEnd`, `SessionEnd`, and subagent boundaries when
///   repository storage is available (see [`ingest_agent_traces`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HookTarget {
    AiIntent,
    AgentTraces,
}

/// A hook callback stopped before a validated terminal ingress command could
/// be formed. Codex may acknowledge this advisory outcome because there is no
/// trusted session/scope identity from which durable recovery evidence could
/// be created; callers must not confuse it with a failed terminal write.
#[derive(Debug, Error)]
#[error("hook callback ended before trusted terminal capture ingress: {source}")]
pub struct HookAdvisoryNoEvidence {
    #[source]
    source: anyhow::Error,
}

/// A validated, scope-bound SessionEnd could not leave its required durable
/// receipt. This remains fatal for hosts whose hook exit status is meaningful;
/// acknowledging it would silently lose a recoverable terminal boundary.
#[derive(Debug, Error)]
#[error("trusted terminal capture could not persist recovery evidence: {source}")]
pub struct HookTerminalPersistenceFailure {
    #[source]
    source: anyhow::Error,
}

/// Preserve the distinction between an untrusted callback and a failed
/// terminal receipt at the shared runtime boundary.  Keeping construction in
/// this module prevents command adapters from recreating the error chain by
/// message text.
pub(crate) fn advisory_no_evidence_error(source: anyhow::Error) -> anyhow::Error {
    HookAdvisoryNoEvidence { source }.into()
}

/// Wrap a failure that occurred after a trusted terminal capture boundary.
/// See [`advisory_no_evidence_error`] for why callers use constructors rather
/// than matching rendered error text.
pub(crate) fn terminal_persistence_failure_error(source: anyhow::Error) -> anyhow::Error {
    HookTerminalPersistenceFailure { source }.into()
}

pub(crate) use crate::internal::ai::capture::key::{
    CAPTURE_UNSUPPORTED_PLATFORM_REMEDY, CaptureSourceCommitmentDomain,
    derive_capture_source_commitment_in_scope_until,
    derive_snapshot_content_commitment_in_scope_until,
};
#[cfg(test)]
use crate::internal::ai::capture::scope_binding::{
    trusted_scope_binding_failure, unverified_scope_binding_failure,
};

/// Top-level entry for `libra hooks <command>`.
///
/// Functional scope:
/// - Delegates all untrusted stdin framing, parsing, validation, and provider
///   lowering to [`CaptureIngressCommand`].
/// - Loads the persistent session (creating a fresh one if missing, recovering
///   from corruption by archiving the bad cache file and starting clean).
/// - Updates session metadata, applies the lifecycle event, records dedup keys,
///   and on `SessionEnd` writes the final blob to the AI history ref.
///
/// Boundary conditions:
/// - Out-of-order delivery (e.g. the very first observed event is `ToolUse`)
///   creates a synthetic session marked with `recovered_from_out_of_order`.
/// - Corrupt session caches are archived for forensic inspection rather than
///   discarded silently. The recovered metadata records only an opaque
///   `corrupt_session_backup_archived` marker, never the path containing a
///   provider session identity.
/// - Errors during final persistence are surfaced; the partially-mutated session
///   is still saved so retries can converge.
///
/// See: `hooks::intent::tests::v2_payload_contains_state_machine_and_summary`.
pub async fn process_hook_event_from_stdin(
    command: super::provider::ProviderHookCommand,
    expected_kind: LifecycleEventKind,
    provider: &dyn HookProvider,
) -> Result<()> {
    process_hook_event_with_target(command, expected_kind, provider, HookTarget::AiIntent, None)
        .await
}

/// Parametric form of [`process_hook_event_from_stdin`] that selects the
/// writer destination via [`HookTarget`].
///
/// Both targets consume the same validated ingress command. Unknown provider
/// event names are logged as safe no-ops before either target can open a
/// storage/database handle. [`HookTarget::AgentTraces`] then redacts and
/// upserts into `agent_session`; checkpoint class comes from
/// `capture::state::reduce_lifecycle` and covers `TurnEnd`, `SessionEnd`,
/// `SubagentStart`, and `SubagentEnd`.
pub async fn process_hook_event_with_target(
    command: super::provider::ProviderHookCommand,
    expected_kind: LifecycleEventKind,
    provider: &dyn HookProvider,
    target: HookTarget,
    deadline: Option<CaptureDeadline>,
) -> Result<()> {
    let ingest_span = new_ingest_span(command, provider);
    // The callback's only provider lookup: typed identity plus the static
    // live-capture capability, handed to every capture layer below.
    let binding = LiveCaptureBinding::resolve(provider);
    // A provider without an installer-provided capture budget (its exporter
    // owns the window) establishes its one pair before ingress scope binding:
    // that trusted path can initialize the repository replay key, so creating
    // it later in the payload would allow an unbounded durable mutation before
    // capture work. Host-provided deadlines retain their exact pair.
    let deadline = effective_hook_capture_deadline(target, binding, deadline)?;
    // Establish this once, before stdin ingress or scope/key binding. A
    // SessionEnd gets its fixed settlement slice exactly once rather than
    // re-anchoring a fresh grace period after potentially slow ingress work.
    let terminal_target =
        target == HookTarget::AgentTraces && expected_kind == LifecycleEventKind::SessionEnd;
    let execution_deadline = deadline
        .map(|deadline| hook_execution_deadline(deadline, terminal_target))
        .transpose()?;
    let ingress = validate_capture_ingress_from_stdin(
        command,
        expected_kind,
        provider,
        deadline,
        execution_deadline,
        &ingest_span,
    )
    .await;
    let Some(ingress_command) =
        classify_capture_ingress_for_target(ingress, target, expected_kind)?
    else {
        return Ok(());
    };

    let trusted_terminal = target == HookTarget::AgentTraces
        && ingress_command.event_kind() == LifecycleEventKind::SessionEnd;
    let deadline = ingress_command.deadline();
    let capture = async {
        match target {
            HookTarget::AiIntent => process_ai_intent_ingress(ingress_command, provider).await,
            HookTarget::AgentTraces => {
                ingest_agent_traces(ingress_command, binding, &ingest_span).await
            }
        }
    };
    let result = await_hook_capture_with_deadline(
        target,
        deadline,
        trusted_terminal,
        execution_deadline,
        capture,
    )
    .await;
    match result {
        Err(error) if trusted_terminal => Err(terminal_persistence_failure_error(error)),
        other => other,
    }
}

/// Await the selected hook target while preserving the capture commit
/// linearization boundary.
///
/// Legacy intent handling has no paired final-commit contract, so it retains
/// its whole-operation timeout. AgentTraces does not: its lower layers bound
/// cancellable preparation with the monotonic deadline and linearize every
/// normal durable write with SQLite immediately before an acknowledgement
/// that must not be cancelled.
async fn await_hook_capture_with_deadline<T, F>(
    target: HookTarget,
    deadline: Option<CaptureDeadline>,
    trusted_terminal: bool,
    established_execution_deadline: Option<HookExecutionDeadline>,
    capture: F,
) -> Result<T>
where
    F: std::future::Future<Output = Result<T>>,
{
    let Some(deadline) = deadline else {
        return capture.await;
    };
    let execution_deadline = if trusted_terminal {
        established_execution_deadline
            .ok_or_else(|| anyhow!("trusted terminal hook lost its managed execution deadline"))?
    } else {
        hook_execution_deadline(deadline, false)?
    };
    if Instant::now() >= execution_deadline.monotonic {
        return Err(anyhow!(
            "capture ingress deadline expired before hook processing"
        ));
    }
    if target == HookTarget::AiIntent {
        return tokio::time::timeout_at(
            tokio::time::Instant::from_std(execution_deadline.monotonic),
            capture,
        )
        .await
        .map_err(|_| anyhow!("capture hook processing exceeded its managed deadline"))?;
    }

    // AgentTraces carries the paired deadline into every normal mutable
    // transaction. Its final SQLite authorization decides whether the
    // deadline was met, after which COMMIT must be awaited without
    // cancellation: SQLx may have dispatched it before an enclosing timeout
    // drops the future. Do not wrap the whole capture here; doing so could
    // report a hook timeout after a durable checkpoint/catalog/marker write.
    // Pre-commit work remains bounded by the ingress deadline at each
    // lower-layer I/O and transaction boundary.
    capture.await
}

/// Preserve scope-proof classification while adapting a validated ingress
/// result to its destination. This is deliberately separate from stdin
/// parsing so regression tests can prove that helper transport failures keep
/// their classification all the way to the host policy.
pub(crate) fn classify_capture_ingress_for_target(
    ingress: Result<Option<Box<CaptureIngressCommand>>>,
    target: HookTarget,
    expected_kind: LifecycleEventKind,
) -> Result<Option<Box<CaptureIngressCommand>>> {
    match ingress {
        Ok(ingress) => Ok(ingress),
        Err(error)
            if target == HookTarget::AgentTraces
                && expected_kind == LifecycleEventKind::SessionEnd
                && error
                    .chain()
                    .any(|cause| cause.is::<HookTrustedScopeBindingFailure>()) =>
        {
            Err(terminal_persistence_failure_error(error))
        }
        Err(error) if target == HookTarget::AgentTraces => Err(advisory_no_evidence_error(error)),
        Err(error) => Err(error),
    }
}

/// Production stdin adapter. The raw frame is read and consumed entirely by
/// `capture::ingress`; this runtime sees only an outcome composed of bounded
/// diagnostics or a canonical command.
async fn validate_capture_ingress_from_stdin(
    command: super::provider::ProviderHookCommand,
    expected_kind: LifecycleEventKind,
    provider: &dyn HookProvider,
    deadline: Option<CaptureDeadline>,
    scope_binding_deadline: Option<HookExecutionDeadline>,
    ingest_span: &tracing::Span,
) -> Result<Option<Box<CaptureIngressCommand>>> {
    let ingress = match CaptureIngressCommand::from_stdin(
        command,
        expected_kind,
        provider,
        deadline,
        |scope_input| bind_capture_scope_cwd_bounded(scope_input, scope_binding_deadline),
    )
    .await
    {
        Ok(ingress) => ingress,
        Err(error) => {
            ingest_span.record("validated", false);
            return Err(error);
        }
    };
    ingest_span.record("validated", true);

    match ingress {
        CaptureIngressOutcome::Command(command) => {
            ingest_span.record("frame_bytes", command.frame_bytes() as u64);
            Ok(Some(command))
        }
        CaptureIngressOutcome::UnknownEvent {
            frame_bytes,
            event_name_len,
        } => {
            ingest_span.record("frame_bytes", frame_bytes as u64);
            ingest_span.record("partial", true);
            let _entered = ingest_span.enter();
            tracing::warn!(
                target: "agent.hook.ingest",
                provider = provider.provider_name(),
                event_name_len,
                reason = "unknown_event_type",
                "skipping unrecognized lifecycle event name"
            );
            Ok(None)
        }
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use std::time::Duration;

    use chrono::Utc;

    use super::*;
    use crate::internal::ai::hooks::providers::{claude_provider, opencode_provider};

    #[test]
    fn opencode_missing_host_deadline_establishes_one_runtime_capture_pair_before_ingress() {
        let claude = LiveCaptureBinding::resolve(claude_provider());
        let opencode = LiveCaptureBinding::resolve(opencode_provider());
        let host_deadline = CaptureDeadline::from_budget_millis(1_000)
            .expect("construct host-provided capture deadline");
        assert_eq!(
            effective_capture_deadline(claude.missing_host_capture_budget(), None)
                .expect("non-OpenCode default has no deadline"),
            None,
            "only OpenCode gets the exporter-derived default budget"
        );
        assert_eq!(
            effective_capture_deadline(opencode.missing_host_capture_budget(), Some(host_deadline))
                .expect("preserve host-provided OpenCode deadline"),
            Some(host_deadline),
            "runtime must retain the host pair rather than re-anchor it"
        );

        assert_eq!(
            effective_hook_capture_deadline(HookTarget::AiIntent, opencode, None)
                .expect("intent hook retains its legacy no-deadline behavior"),
            None,
            "the exporter default applies only to AgentTraces"
        );

        let deadline = effective_hook_capture_deadline(HookTarget::AgentTraces, opencode, None)
            .expect("establish OpenCode default capture deadline before ingress")
            .expect("OpenCode must always carry an effective capture deadline");
        assert!(
            Instant::now() < deadline.monotonic(),
            "the default OpenCode capture deadline must remain live at ingress"
        );
        assert!(
            deadline.absolute_millis() >= Utc::now().timestamp_millis(),
            "the default OpenCode capture deadline must include its paired SQLite boundary"
        );
    }

    #[tokio::test]
    async fn agent_traces_outer_deadline_does_not_cancel_a_post_deadline_commit_ack() {
        // The traces module's file-backed reader-lock regression proves that
        // a COMMIT may be dispatched before its acknowledgement arrives.
        // This companion guard proves the hook wrapper does not reintroduce
        // cancellation around that lower-layer durable boundary.
        let deadline = CaptureDeadline::from_budget_millis(100)
            .expect("construct managed agent-traces deadline");
        let result = await_hook_capture_with_deadline(
            HookTarget::AgentTraces,
            Some(deadline),
            false,
            None,
            async move {
                tokio::time::sleep_until(tokio::time::Instant::from_std(
                    deadline.monotonic() + Duration::from_millis(25),
                ))
                .await;
                Ok::<(), anyhow::Error>(())
            },
        )
        .await;
        assert!(
            result.is_ok(),
            "AgentTraces must await a post-deadline durable acknowledgement rather than cancel it: {result:?}"
        );
    }

    #[test]
    fn agent_traces_terminal_ingress_preserves_scope_proof_classification() {
        let advisory = match classify_capture_ingress_for_target(
            Err(unverified_scope_binding_failure(
                "test helper ended before a scope proof",
            )),
            HookTarget::AgentTraces,
            LifecycleEventKind::SessionEnd,
        ) {
            Ok(_) => panic!("unverified scope failure must not become an ingress command"),
            Err(error) => error,
        };
        assert!(
            advisory
                .chain()
                .any(|cause| cause.is::<HookAdvisoryNoEvidence>()),
            "pre-proof helper failures must remain advisory through AgentTraces ingress: {advisory:#}"
        );

        let terminal = match classify_capture_ingress_for_target(
            Err(trusted_scope_binding_failure(
                "test helper failed after a trusted scope proof",
            )),
            HookTarget::AgentTraces,
            LifecycleEventKind::SessionEnd,
        ) {
            Ok(_) => panic!("trusted scope failure must not become an ingress command"),
            Err(error) => error,
        };
        assert!(
            terminal
                .chain()
                .any(|cause| cause.is::<HookTerminalPersistenceFailure>()),
            "post-proof helper failures must remain terminal through AgentTraces ingress: {terminal:#}"
        );
    }

    #[test]
    fn unsupported_platform_capability_survives_agent_traces_classification() {
        let nonterminal = match classify_capture_ingress_for_target(
            Err(unsupported_platform_scope_binding_failure()),
            HookTarget::AgentTraces,
            LifecycleEventKind::SessionStart,
        ) {
            Ok(_) => panic!("unsupported-platform scope binding must not form an ingress command"),
            Err(error) => error,
        };
        assert!(
            nonterminal
                .chain()
                .any(|cause| cause.is::<HookAdvisoryNoEvidence>()),
            "a nonterminal capability failure remains advisory for the Codex host policy: {nonterminal:#}"
        );
        assert!(
            is_capture_unsupported_platform_error(&nonterminal),
            "the parent policy must still identify the fixed platform capability after advisory wrapping"
        );

        let terminal = match classify_capture_ingress_for_target(
            Err(unsupported_platform_scope_binding_failure()),
            HookTarget::AgentTraces,
            LifecycleEventKind::SessionEnd,
        ) {
            Ok(_) => panic!("unsupported-platform scope binding must not form an ingress command"),
            Err(error) => error,
        };
        assert!(
            terminal
                .chain()
                .any(|cause| cause.is::<HookTerminalPersistenceFailure>()),
            "a terminal capability failure must retain the durable-failure classification: {terminal:#}"
        );
        assert!(
            is_capture_unsupported_platform_error(&terminal),
            "the parent policy must still identify the fixed platform capability after terminal wrapping"
        );
    }
}
