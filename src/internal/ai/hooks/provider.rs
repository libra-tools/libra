//! Provider contracts for lifecycle hook ingestion and setup.
//!
//! Each LLM provider Libra integrates with (Claude, Gemini, etc.) implements the
//! [`HookProvider`] trait declared here. The trait separates two concerns:
//! 1. **Parsing** — translate the provider's native hook envelope into a canonical
//!    [`LifecycleEvent`].
//! 2. **Setup** — install/uninstall the provider's hook binding files (settings.json,
//!    extension manifests, etc.) so the provider's runtime actually invokes Libra
//!    when one of its lifecycle events fires.
//!
//! Keeping these behind a trait lets the rest of the agent stack remain unaware of
//! provider-specific details and lets new providers be added without touching
//! event normalisation or the hook runner.

use std::{fmt, path::Path};

use anyhow::{Result, bail};
use serde_json::Value;

use super::lifecycle::{LifecycleEvent, LifecycleEventKind, SessionHookEnvelope};
use crate::internal::ai::{observed_agents::AgentKind, session::SessionState};

/// Identity field names that providers most often use to make a hook envelope
/// uniquely identifiable. Listed in priority order: the first one that yields a
/// non-null value is used as the dedup primary key.
pub const CANONICAL_DEDUP_IDENTITY_KEYS: &[&str] = &[
    "event_id",
    "request_id",
    "turn_id",
    "message_id",
    "tool_use_id",
];

/// Native tool callbacks are commonly emitted more than once during one turn.
/// A turn identifier is therefore insufficient to identify an individual tool
/// callback: prefer the provider's tool-call identifier before falling back to
/// a turn identifier.  The ingress HMAC also includes the native hook name, so
/// a pre- and post-callback for the same tool remain distinct deliveries.
pub const TOOL_USE_DEDUP_IDENTITY_KEYS: &[&str] = &[
    "event_id",
    "tool_use_id",
    "request_id",
    "message_id",
    "turn_id",
];

/// Canonical hook command surface exposed by Libra.
///
/// Each variant maps to a CLI subcommand the provider's hook configuration is told
/// to invoke (e.g. `libra hooks tool-use`). Internally each command is paired with a
/// [`LifecycleEventKind`] so the runner can apply the right session-state mutation.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ProviderHookCommand {
    SessionStart,
    Prompt,
    ToolUse,
    PermissionRequest,
    ModelUpdate,
    Compaction,
    Stop,
    SessionEnd,
    /// AG-19: nested sub-agent run started. Codex emits this natively
    /// (`SubagentStart` hook event); other providers may synthesize it
    /// from transcript analysis.
    SubagentStart,
    /// AG-19: nested sub-agent run finished (Codex `SubagentStop`).
    SubagentEnd,
}

impl ProviderHookCommand {
    /// Map a hook command to its corresponding lifecycle event kind.
    ///
    /// Boundary conditions: the mapping is total — every command has exactly one
    /// lifecycle kind, so this method never fails.
    pub fn lifecycle_event_kind(self) -> LifecycleEventKind {
        match self {
            ProviderHookCommand::SessionStart => LifecycleEventKind::SessionStart,
            ProviderHookCommand::Prompt => LifecycleEventKind::TurnStart,
            ProviderHookCommand::ToolUse => LifecycleEventKind::ToolUse,
            ProviderHookCommand::PermissionRequest => LifecycleEventKind::PermissionRequest,
            ProviderHookCommand::ModelUpdate => LifecycleEventKind::ModelUpdate,
            ProviderHookCommand::Compaction => LifecycleEventKind::Compaction,
            ProviderHookCommand::Stop => LifecycleEventKind::TurnEnd,
            ProviderHookCommand::SessionEnd => LifecycleEventKind::SessionEnd,
            ProviderHookCommand::SubagentStart => LifecycleEventKind::SubagentStart,
            ProviderHookCommand::SubagentEnd => LifecycleEventKind::SubagentEnd,
        }
    }
}

impl fmt::Display for ProviderHookCommand {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let value = match self {
            ProviderHookCommand::SessionStart => "session-start",
            ProviderHookCommand::Prompt => "prompt",
            ProviderHookCommand::ToolUse => "tool-use",
            ProviderHookCommand::PermissionRequest => "permission-request",
            ProviderHookCommand::ModelUpdate => "model-update",
            ProviderHookCommand::Compaction => "compaction",
            ProviderHookCommand::Stop => "stop",
            ProviderHookCommand::SessionEnd => "session-end",
            ProviderHookCommand::SubagentStart => "subagent-start",
            ProviderHookCommand::SubagentEnd => "subagent-end",
        };
        write!(f, "{value}")
    }
}

/// Generic install options passed from the command layer into a provider installer.
///
/// Both fields are optional so the installer can fall back to provider-specific
/// defaults (the path to the running `libra` binary, a sensible timeout, etc.).
#[derive(Debug, Clone, Default)]
pub struct ProviderInstallOptions {
    pub binary_path: Option<String>,
    pub timeout_secs: Option<u64>,
}

/// Time deliberately retained for the hook process to persist a terminal
/// pending receipt and return control to its host.  The provider-owned timeout
/// remains the outer authority; this is only the portion available to capture
/// work inside the managed command line.
pub(crate) const HOOK_CAPTURE_CLEANUP_MARGIN_MILLIS: u64 = 1_000;
/// Largest whole-second host timeout that can be represented by the hidden
/// millisecond argument. This is an arithmetic bound, not a product policy:
/// provider configurations have historically accepted any positive timeout,
/// including values greater than Codex's 600-second default.
#[cfg(test)]
pub(crate) const MAX_REPRESENTABLE_HOOK_TIMEOUT_SECS: u64 = i64::MAX as u64 / 1_000 + 1;
/// Terminal finalizer receipts use signed SQLite milliseconds, so the hidden
/// command-line argument may not exceed that durable representation.
pub(crate) const MAX_HOOK_CAPTURE_BUDGET_MILLIS: u64 = i64::MAX as u64;

/// Derive the bounded capture portion of a provider-owned hook timeout.
///
/// The installed command persists this value so runtime never has to guess
/// whether a user changed a provider configuration after installation.
pub(crate) fn capture_budget_millis_for_host_timeout(timeout_secs: u64) -> Result<u64> {
    if timeout_secs == 0 {
        bail!("invalid hook timeout: value must be greater than zero seconds");
    }
    let host_millis = timeout_secs
        .checked_mul(1_000)
        .ok_or_else(|| anyhow::anyhow!("invalid hook timeout: capture budget overflow"))?;
    // Retain a bounded fraction of very short, historically accepted host
    // timeouts.  In particular, a one-second legacy timeout needs enough
    // time to parse a frame and persist a terminal receipt; handing capture
    // a synthetic one-millisecond slice would make every such hook expire
    // before ingress while silently claiming it remains supported.
    let cleanup_margin = HOOK_CAPTURE_CLEANUP_MARGIN_MILLIS.min(host_millis / 2);
    let budget = host_millis
        .checked_sub(cleanup_margin)
        .ok_or_else(|| anyhow::anyhow!("invalid hook timeout: capture budget underflow"))?;
    if budget > MAX_HOOK_CAPTURE_BUDGET_MILLIS {
        bail!("invalid hook timeout: capture budget exceeds the persistent finalizer range");
    }
    Ok(budget)
}

/// Recognize only the canonical budget values generated by a managed provider
/// installer.  This keeps uninstall/upsert ownership detection narrow while
/// allowing the hidden CLI itself to accept a smaller test or emergency budget.
pub(crate) fn is_managed_capture_budget_millis(budget_millis: u64) -> bool {
    // A one-second legacy host timeout reserves half of its window for the
    // provider and leaves the canonical 500ms capture slice.  Longer
    // whole-second timeouts retain the fixed one-second cleanup margin.
    if budget_millis == 500 {
        return true;
    }
    if !budget_millis.is_multiple_of(1_000) {
        return false;
    }
    let Some(timeout_secs) = budget_millis
        .checked_div(1_000)
        .and_then(|seconds| seconds.checked_add(1))
    else {
        return false;
    };
    capture_budget_millis_for_host_timeout(timeout_secs)
        .is_ok_and(|expected| expected == budget_millis)
}

/// Closed catalog identity of a hook provider (ADR-ACF-10 "Typed identity").
///
/// The capture runtime takes a provider's durable `agent_session.agent_kind`
/// value and its live-capture capability from this typed [`AgentKind`]; it
/// never derives either from [`HookProvider::provider_name`] (AG-19: no
/// name-string bridge). The builtin providers declare their kind together in
/// [`super::providers`].
pub trait HookProviderIdentity {
    /// The observed-agent catalog kind this provider's callbacks are captured
    /// under.
    fn agent_kind(&self) -> AgentKind;
}

/// A statically registered provider that can parse lifecycle payloads and manage hook setup.
///
/// Implementations are expected to be cheap to construct (typically zero-sized
/// types) and are reached at runtime through the observed-agents registry
/// (`AgentKind` -> `agent_for` -> `as_hooks()`) or the typed singleton
/// accessors in [`super::providers`] — never by name string (AG-19). Every
/// provider also declares its typed catalog identity through the
/// [`HookProviderIdentity`] supertrait.
/// All methods are sync because hook ingestion runs on the agent's main thread and
/// IO that providers perform is bounded by user-controlled config files.
pub trait HookProvider: HookProviderIdentity + Sync {
    /// Human-readable provider identifier used in logs and CLI feedback.
    fn provider_name(&self) -> &'static str;
    /// Tag applied to ingested events when persisted to session metadata, allowing
    /// downstream consumers to attribute an event to its origin provider.
    fn source_name(&self) -> &'static str;
    /// Hook commands this provider knows how to install and parse.
    fn supported_commands(&self) -> &'static [ProviderHookCommand];
    /// Translate a provider envelope into the canonical [`LifecycleEvent`].
    ///
    /// Returns an error when the envelope is malformed or names a hook event
    /// the provider does not support.
    fn parse_hook_event(
        &self,
        hook_event_name: &str,
        envelope: &SessionHookEnvelope,
    ) -> Result<LifecycleEvent>;
    /// Whether this provider's parser recognizes `hook_event_name`.
    ///
    /// AG-19 forward compatibility: when a newer upstream agent emits an
    /// event name Libra does not know yet, the dispatcher must
    /// skip-and-log (`unknown_event_type`) instead of failing the whole
    /// ingest — never panic, never write a checkpoint, never block later
    /// known events. The conservative default is `false`: a permissive
    /// default would send a future arbitrary provider spelling to parser
    /// error paths, which can both defeat the skip-and-log compatibility
    /// contract and reflect untrusted text in a diagnostic. New providers
    /// must opt in by publishing their supported name table.
    fn recognizes_event(&self, _hook_event_name: &str) -> bool {
        false
    }
    /// Identity field names this provider checks when building dedup keys.
    fn dedup_identity_keys(&self) -> &'static [&'static str];
    /// Native identity priority for one particular callback.  Tool-use
    /// callbacks intentionally prefer `tool_use_id` to `turn_id`: a single
    /// turn can legitimately contain many tool uses, and collapsing them
    /// would lose capture work. Providers may override this for a stricter
    /// documented event contract.
    fn dedup_identity_keys_for_event(
        &self,
        _hook_event_name: &str,
        event_kind: LifecycleEventKind,
    ) -> &'static [&'static str] {
        if matches!(event_kind, LifecycleEventKind::ToolUse) {
            TOOL_USE_DEDUP_IDENTITY_KEYS
        } else {
            self.dedup_identity_keys()
        }
    }
    /// Optional command-level output payload (e.g. JSON the provider expects in
    /// stdout) — defaults to `None` for providers that signal purely via exit code.
    fn command_output(&self, _command: ProviderHookCommand) -> Option<Value> {
        None
    }
    /// Hook the provider can use to apply additional state mutations after the
    /// canonical event has been recorded — e.g. linking transcripts to objects on
    /// disk. Default impl is a no-op.
    fn post_process_event(
        &self,
        _command: ProviderHookCommand,
        _storage_path: &Path,
        _session: &mut SessionState,
        _envelope: &SessionHookEnvelope,
        _event: &LifecycleEvent,
    ) -> Result<()> {
        Ok(())
    }
    /// Materialise the provider's hook configuration files on disk.
    fn install_hooks(&self, options: &ProviderInstallOptions) -> Result<()>;
    /// Remove anything previously written by [`Self::install_hooks`].
    fn uninstall_hooks(&self) -> Result<()>;
    /// Detect whether the provider's hooks are currently wired up. Used for status
    /// reporting and idempotent installs.
    fn hooks_are_installed(&self) -> Result<bool>;
}

#[cfg(test)]
mod tests {
    use super::*;

    /// `ProviderHookCommand::lifecycle_event_kind` is the canonical
    /// command-to-lifecycle mapping. Pin every
    /// pair so a future renumbering / variant addition surfaces
    /// here — the runner's session-state mutation table depends on
    /// this exact mapping.
    #[test]
    fn provider_hook_command_lifecycle_event_kind_table() {
        let cases = [
            (
                ProviderHookCommand::SessionStart,
                LifecycleEventKind::SessionStart,
            ),
            (ProviderHookCommand::Prompt, LifecycleEventKind::TurnStart),
            (ProviderHookCommand::ToolUse, LifecycleEventKind::ToolUse),
            (
                ProviderHookCommand::PermissionRequest,
                LifecycleEventKind::PermissionRequest,
            ),
            (
                ProviderHookCommand::ModelUpdate,
                LifecycleEventKind::ModelUpdate,
            ),
            (
                ProviderHookCommand::Compaction,
                LifecycleEventKind::Compaction,
            ),
            (ProviderHookCommand::Stop, LifecycleEventKind::TurnEnd),
            (
                ProviderHookCommand::SessionEnd,
                LifecycleEventKind::SessionEnd,
            ),
        ];
        for (command, expected) in cases {
            assert_eq!(
                command.lifecycle_event_kind(),
                expected,
                "command {command:?} must map to {expected:?}",
            );
        }
    }

    /// `ProviderHookCommand::Display` produces kebab-case strings
    /// matching the CLI subcommand names. Pin all 7 variants so a
    /// future rename gets caught at this gate — the CLI surface and
    /// the provider config files both depend on these exact strings.
    #[test]
    fn provider_hook_command_display_uses_kebab_case() {
        let cases = [
            (ProviderHookCommand::SessionStart, "session-start"),
            (ProviderHookCommand::Prompt, "prompt"),
            (ProviderHookCommand::ToolUse, "tool-use"),
            (ProviderHookCommand::PermissionRequest, "permission-request"),
            (ProviderHookCommand::ModelUpdate, "model-update"),
            (ProviderHookCommand::Compaction, "compaction"),
            (ProviderHookCommand::Stop, "stop"),
            (ProviderHookCommand::SessionEnd, "session-end"),
        ];
        for (command, expected) in cases {
            assert_eq!(command.to_string(), expected);
        }
    }

    /// `ProviderHookCommand::Copy` + `Eq` + `Hash` are required for
    /// `HashMap<Command, ...>` lookup tables in provider installers.
    /// Pin the derives via a static type-system check +
    /// duplicate-detection via HashSet.
    #[test]
    fn provider_hook_command_derives_copy_and_hash() {
        use std::collections::HashSet;
        let set: HashSet<ProviderHookCommand> = [
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
        ]
        .into_iter()
        .collect();
        // 10 distinct variants must populate 10 hash buckets.
        assert_eq!(set.len(), 10, "all variants must be hash-distinct");
    }

    /// `ProviderInstallOptions::default()` initialises both fields
    /// to `None` so providers can fall back to their own defaults
    /// (binary path discovery, timeout heuristics).
    #[test]
    fn provider_install_options_default_is_none_for_both_fields() {
        let opts = ProviderInstallOptions::default();
        assert!(opts.binary_path.is_none());
        assert!(opts.timeout_secs.is_none());
    }

    #[test]
    fn managed_hook_budget_preserves_positive_provider_timeouts() {
        assert_eq!(
            capture_budget_millis_for_host_timeout(10).expect("ten-second host timeout"),
            9_000
        );
        assert_eq!(
            capture_budget_millis_for_host_timeout(1).expect("legacy one-second host timeout"),
            500
        );
        assert_eq!(
            capture_budget_millis_for_host_timeout(601).expect("long host timeout"),
            600_000
        );
        assert!(
            capture_budget_millis_for_host_timeout(MAX_REPRESENTABLE_HOOK_TIMEOUT_SECS)
                .expect("maximum representable host timeout")
                <= MAX_HOOK_CAPTURE_BUDGET_MILLIS
        );
        assert!(capture_budget_millis_for_host_timeout(0).is_err());
        assert!(
            capture_budget_millis_for_host_timeout(MAX_REPRESENTABLE_HOOK_TIMEOUT_SECS + 1)
                .is_err()
        );
        assert!(is_managed_capture_budget_millis(500));
        assert!(is_managed_capture_budget_millis(9_000));
        assert!(is_managed_capture_budget_millis(600_000));
        assert!(!is_managed_capture_budget_millis(9_001));
    }

    /// `CANONICAL_DEDUP_IDENTITY_KEYS` priority ordering matters —
    /// the first non-null field wins. Pin the exact order so
    /// providers documented to rely on `event_id` as primary key
    /// keep that precedence.
    #[test]
    fn canonical_dedup_identity_keys_priority_order_is_pinned() {
        assert_eq!(
            CANONICAL_DEDUP_IDENTITY_KEYS,
            &[
                "event_id",
                "request_id",
                "turn_id",
                "message_id",
                "tool_use_id",
            ],
        );
        // Length pin so a new key addition forces a deliberate test
        // update (and surfaces priority-ordering review).
        assert_eq!(CANONICAL_DEDUP_IDENTITY_KEYS.len(), 5);
    }
}
