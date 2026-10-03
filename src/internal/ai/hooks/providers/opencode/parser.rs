//! OpenCode hook envelope parser (AG-19).
//!
//! Maps each OpenCode event type string forwarded by the Libra-managed
//! `.opencode/plugin/libra-hooks.js` plugin to a canonical
//! [`LifecycleEventKind`] and delegates field extraction to
//! [`build_lifecycle_event`]. The upstream event contract was verified
//! against opencode 1.17.13 (probed live 2026-07-05).
//!
//! Event mapping — Libra-side inference rules are marked explicitly, per
//! docs/development/tracing/agent.md "OpenCode 安装流程契约":
//!
//! | OpenCode event       | Lifecycle kind | Notes                                                     |
//! |----------------------|----------------|-----------------------------------------------------------|
//! | `session.created`    | SessionStart   | official bus event, observed live                         |
//! | `message.updated`    | TurnStart      | plugin forwards role=user only (see below)                |
//! | `tool.execute.after` | ToolUse        | plugin hook handler, not a bus event                      |
//! | `session.idle`       | TurnEnd        | **Libra-side inference**: fires at the end of each        |
//! |                      |                | headless run and is the reliable turn-complete marker;    |
//! |                      |                | it is NOT an official OpenCode terminal event             |
//! | `session.deleted`    | SessionEnd     | declared in the SDK; not observed in headless probing     |
//! | `session.compacted`  | Compaction     | declared in the SDK; not observed in headless probing     |
//!
//! `message.updated` maps to TurnStart **unconditionally** here because the
//! generated plugin only forwards `message.updated` events whose
//! `properties.info.role == "user"` (assistant updates and the streaming
//! `message.part.updated` / delta events are dropped plugin-side and never
//! reach Libra). The role filter therefore lives in the plugin, not in this
//! parser.
//!
//! Unknown event names are a hard error (mirroring the Claude parser) so
//! breaking upstream changes surface immediately; the dispatcher
//! skip-and-logs genuinely-new names via `HookProvider::recognizes_event`.

use anyhow::{Result, bail};

use super::super::super::lifecycle::{
    LifecycleEvent, LifecycleEventKind, SessionHookEnvelope, build_lifecycle_event,
};

/// Every OpenCode event name [`parse_opencode_hook_event`] understands.
/// Keep in sync with its `match`; the dispatcher consults this via
/// `HookProvider::recognizes_event` to skip-and-log names a newer OpenCode
/// emits that this build does not know yet (AG-19).
pub(super) const OPENCODE_HOOK_EVENT_NAMES: &[&str] = &[
    "session.created",
    "message.updated",
    "tool.execute.after",
    "session.idle",
    "session.deleted",
    "session.compacted",
];

/// Translate an OpenCode event type string into a canonical lifecycle event.
///
/// Functional scope: routes each known OpenCode event (see the module-level
/// mapping table) through [`build_lifecycle_event`], which extracts the
/// standard fields from `envelope.extra`.
///
/// Boundary conditions: unknown event names produce a hard error so that
/// upstream changes are surfaced immediately rather than silently dropped.
///
/// See: `tests::parser_maps_opencode_events`, `tests::parser_rejects_unknown_event`.
pub(super) fn parse_opencode_hook_event(
    hook_event_name: &str,
    envelope: &SessionHookEnvelope,
) -> Result<LifecycleEvent> {
    let kind = match hook_event_name {
        "session.created" => LifecycleEventKind::SessionStart,
        // The plugin only forwards role=user message.updated events, so an
        // unconditional TurnStart mapping is safe here (see module docs).
        "message.updated" => LifecycleEventKind::TurnStart,
        "tool.execute.after" => LifecycleEventKind::ToolUse,
        // Libra-side inference: end-of-turn marker in headless runs, NOT an
        // official OpenCode terminal event (agent.md "OpenCode 安装流程契约").
        "session.idle" => LifecycleEventKind::TurnEnd,
        "session.deleted" => LifecycleEventKind::SessionEnd,
        "session.compacted" => LifecycleEventKind::Compaction,
        other => bail!("unknown OpenCode hook event: '{other}'"),
    };
    Ok(build_lifecycle_event(kind, envelope))
}

#[cfg(test)]
mod tests {
    use serde_json::{Map, Value};

    use super::*;

    fn canonical_envelope() -> SessionHookEnvelope {
        SessionHookEnvelope {
            hook_event_name: "session.created".to_string(),
            session_id: "ses_0123456789abcdef".to_string(),
            cwd: "/tmp".to_string(),
            transcript_path: None,
            extra: {
                let mut map = Map::new();
                map.insert("prompt".to_string(), Value::String("hello".to_string()));
                map.insert("role".to_string(), Value::String("user".to_string()));
                map
            },
        }
    }

    // Scenario: every mapped OpenCode event name resolves to its canonical kind.
    #[test]
    fn parser_maps_opencode_events() {
        let envelope = canonical_envelope();
        let cases = [
            ("session.created", LifecycleEventKind::SessionStart),
            ("message.updated", LifecycleEventKind::TurnStart),
            ("tool.execute.after", LifecycleEventKind::ToolUse),
            ("session.idle", LifecycleEventKind::TurnEnd),
            ("session.deleted", LifecycleEventKind::SessionEnd),
            ("session.compacted", LifecycleEventKind::Compaction),
        ];

        for (name, kind) in cases {
            let event = parse_opencode_hook_event(name, &envelope).expect("parse should succeed");
            assert_eq!(event.kind, kind, "event '{name}' must map to {kind:?}");
        }
    }

    // Scenario: streaming and unknown event names return an error instead of
    // being silently forwarded (the plugin never sends them, and a hard error
    // here surfaces upstream drift immediately).
    #[test]
    fn parser_rejects_unknown_event() {
        let envelope = canonical_envelope();
        for name in ["message.part.updated", "session.status", "UnknownHook"] {
            assert!(
                parse_opencode_hook_event(name, &envelope).is_err(),
                "event '{name}' must be rejected",
            );
        }
    }

    // Scenario: the recognizes_event name table and the parser match stay in
    // sync — every advertised name must parse.
    #[test]
    fn every_advertised_event_name_parses() {
        let envelope = canonical_envelope();
        for name in OPENCODE_HOOK_EVENT_NAMES {
            assert!(
                parse_opencode_hook_event(name, &envelope).is_ok(),
                "advertised event '{name}' must parse",
            );
        }
    }
}
