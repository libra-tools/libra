//! Pinned OpenCode event taxonomy shared by parsing and plugin conformance.
//!
//! The OG-00 source record pins the native 2.0.26 `data` envelope. The OG-02 plugin consumer will
//! lower that envelope to Libra's bounded hook frame before this registry is
//! consulted. Legacy names remain separate from the current forwarding set.

use crate::internal::ai::hooks::{LifecycleEventKind, ProviderHookCommand};

pub const OPENCODE_PIN: &str = "2.0.26";
pub const OPENCODE_COMMIT: &str = "9b4ec5714d481559990db0a816d5dec19541a814";

/// Native terminal reasons; shutdown preserves restart continuity.
pub const OPENCODE_TERMINAL_INTERRUPTION_REASONS: &[&str] = &["user", "superseded", "inactivity"];
pub const OPENCODE_IDLE_STATUS: &str = "idle";
pub const OPENCODE_FILTERED_STATUSES: &[&str] = &["busy", "retry"];

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OpenCodeForwarding {
    Standalone,
    PluginHook,
    PluginMerged,
    Skip,
    LegacyCompatibility,
    DeprecatedAlias,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OpenCodePayloadPolicy {
    Identity,
    UserDelivery,
    IdleStatus,
    ExecutionInterrupted,
    ToolObservation,
    /// Required string prompt (empty allowed); an omitted role is accepted.
    /// The current legacy forwarder always sends an empty prompt.
    /// Text-part pairing belongs to OG-02; missing prompt is malformed.
    LegacyPrompt,
    NotForwarded,
}

#[derive(Debug, Clone, Copy)]
pub struct OpenCodeHookEventSpec {
    pub name: &'static str,
    pub kind: Option<LifecycleEventKind>,
    pub command: Option<ProviderHookCommand>,
    pub forwarding: OpenCodeForwarding,
    pub payload_policy: OpenCodePayloadPolicy,
}

use OpenCodeForwarding as Forward;
use OpenCodePayloadPolicy as Payload;

/// Current bus events and the tool observation hook. Enqueue supplies prompt
/// text; only delivery opens a turn. Non-forwarded names have no disposition.
pub const OPENCODE_HOOK_EVENT_SPECS: &[OpenCodeHookEventSpec] = &[
    OpenCodeHookEventSpec {
        name: "session.created",
        kind: Some(LifecycleEventKind::SessionStart),
        command: Some(ProviderHookCommand::SessionStart),
        forwarding: Forward::Standalone,
        payload_policy: Payload::Identity,
    },
    OpenCodeHookEventSpec {
        name: "session.inbox.delivered",
        kind: Some(LifecycleEventKind::TurnStart),
        command: Some(ProviderHookCommand::Prompt),
        forwarding: Forward::Standalone,
        payload_policy: Payload::UserDelivery,
    },
    OpenCodeHookEventSpec {
        name: "session.execution.succeeded",
        kind: Some(LifecycleEventKind::TurnEnd),
        command: Some(ProviderHookCommand::Stop),
        forwarding: Forward::Standalone,
        payload_policy: Payload::Identity,
    },
    OpenCodeHookEventSpec {
        name: "session.execution.failed",
        kind: Some(LifecycleEventKind::TurnEnd),
        command: Some(ProviderHookCommand::Stop),
        forwarding: Forward::Standalone,
        payload_policy: Payload::Identity,
    },
    OpenCodeHookEventSpec {
        name: "session.execution.interrupted",
        kind: Some(LifecycleEventKind::TurnEnd),
        command: Some(ProviderHookCommand::Stop),
        forwarding: Forward::Standalone,
        payload_policy: Payload::ExecutionInterrupted,
    },
    OpenCodeHookEventSpec {
        name: "session.deleted",
        kind: Some(LifecycleEventKind::SessionEnd),
        command: Some(ProviderHookCommand::SessionEnd),
        forwarding: Forward::Standalone,
        payload_policy: Payload::Identity,
    },
    OpenCodeHookEventSpec {
        name: "session.compaction.ended",
        kind: Some(LifecycleEventKind::Compaction),
        command: Some(ProviderHookCommand::Compaction),
        forwarding: Forward::Standalone,
        payload_policy: Payload::Identity,
    },
    OpenCodeHookEventSpec {
        name: "tool.execute.after",
        kind: Some(LifecycleEventKind::ToolUse),
        command: Some(ProviderHookCommand::ToolUse),
        forwarding: Forward::PluginHook,
        payload_policy: Payload::ToolObservation,
    },
    OpenCodeHookEventSpec {
        name: "session.inbox.enqueued",
        kind: None,
        command: None,
        forwarding: Forward::PluginMerged,
        payload_policy: Payload::NotForwarded,
    },
    OpenCodeHookEventSpec {
        name: "session.error",
        kind: None,
        command: None,
        forwarding: Forward::Skip,
        payload_policy: Payload::NotForwarded,
    },
];

/// Compatibility inputs outside the advertised native event set. The managed
/// plugin also constructs a disposed envelope for its local cleanup/exit path,
/// and retains the other branches for older hosts. A disposed envelope must
/// already carry its tracked session identity.
pub const OPENCODE_LEGACY_EVENT_SPECS: &[OpenCodeHookEventSpec] = &[
    OpenCodeHookEventSpec {
        name: "session.status",
        kind: Some(LifecycleEventKind::TurnEnd),
        command: Some(ProviderHookCommand::Stop),
        forwarding: Forward::LegacyCompatibility,
        payload_policy: Payload::IdleStatus,
    },
    OpenCodeHookEventSpec {
        name: "message.updated",
        kind: Some(LifecycleEventKind::TurnStart),
        command: Some(ProviderHookCommand::Prompt),
        forwarding: Forward::LegacyCompatibility,
        payload_policy: Payload::LegacyPrompt,
    },
    OpenCodeHookEventSpec {
        name: "session.compacted",
        kind: Some(LifecycleEventKind::Compaction),
        command: Some(ProviderHookCommand::Compaction),
        forwarding: Forward::LegacyCompatibility,
        payload_policy: Payload::Identity,
    },
    OpenCodeHookEventSpec {
        name: "server.instance.disposed",
        kind: Some(LifecycleEventKind::SessionEnd),
        command: Some(ProviderHookCommand::SessionEnd),
        forwarding: Forward::LegacyCompatibility,
        payload_policy: Payload::Identity,
    },
];

pub const OPENCODE_DEPRECATED_ALIASES: &[OpenCodeHookEventSpec] = &[OpenCodeHookEventSpec {
    name: "session.idle",
    kind: Some(LifecycleEventKind::TurnEnd),
    command: Some(ProviderHookCommand::Stop),
    forwarding: Forward::DeprecatedAlias,
    payload_policy: Payload::Identity,
}];

pub fn event_spec(name: &str) -> Option<&'static OpenCodeHookEventSpec> {
    OPENCODE_HOOK_EVENT_SPECS
        .iter()
        .chain(OPENCODE_LEGACY_EVENT_SPECS)
        .chain(OPENCODE_DEPRECATED_ALIASES)
        .find(|spec| spec.name == name)
}

pub fn recognizes_event(name: &str) -> bool {
    event_spec(name).is_some_and(|spec| spec.kind.is_some())
}
