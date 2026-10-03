//! Codex CLI hook envelope parser (AG-19).
//!
//! Maps each Codex `hook_event_name` (PascalCase, as configured in
//! `hooks.json` / `[[hooks.<EventName>]]`) to a canonical
//! [`LifecycleEventKind`] and delegates field extraction to
//! [`build_lifecycle_event`]. The upstream event taxonomy was verified
//! against codex-cli 0.142.4 (probed live 2026-07-05, cross-checked against
//! source `rust-v0.142.4` @57d253ad).
//!
//! Event mapping:
//!
//! | Codex event         | Lifecycle kind        | Notes                                   |
//! |---------------------|-----------------------|-----------------------------------------|
//! | `SessionStart`      | SessionStart          | payload adds `source` (startup\|resume\|clear\|compact) |
//! | `UserPromptSubmit`  | TurnStart             | payload adds `prompt` + `turn_id`       |
//! | `PreToolUse`        | ToolUse               | payload adds `tool_name`, `tool_input`, `tool_use_id` |
//! | `PostToolUse`       | ToolUse               | additionally carries `tool_response`    |
//! | `Stop`              | TurnEnd               | payload adds `stop_hook_active`, `last_assistant_message` |
//! | `PreCompact`        | Compaction            |                                         |
//! | `PostCompact`       | CompactionCompleted   |                                         |
//! | `SubagentStart`     | SubagentStart         | AG-19 sub-agent boundary variants       |
//! | `SubagentStop`      | SubagentEnd           |                                         |
//! | `PermissionRequest` | PermissionRequest     |                                         |
//!
//! Codex's hook stdin payload is Claude Code-compatible single-line JSON
//! (`session_id`, `transcript_path`, `cwd`, `hook_event_name`, `model`,
//! `permission_mode`, …), so the shared [`build_lifecycle_event`] extractor
//! works unchanged: it reads exactly the keys Codex emits (`prompt`,
//! `tool_name`, `tool_input`, `tool_response`, `last_assistant_message`,
//! `model`, `source`) from `envelope.extra`.
//!
//! Codex has no `ModelUpdate` hook. Current Codex releases also provide
//! `SessionEnd`, which is mapped to the canonical session teardown event.
//!
//! Unknown event names are a hard error (mirroring the Claude parser) so
//! breaking upstream changes surface immediately; the dispatcher
//! skip-and-logs genuinely-new names via `HookProvider::recognizes_event`.

use anyhow::{Result, bail};

use super::super::super::lifecycle::{
    LifecycleEvent, LifecycleEventKind, SessionHookEnvelope, build_lifecycle_event,
};

/// Every Codex hook event name [`parse_codex_hook_event`] understands
/// (the full current 11-event taxonomy). Keep in sync with its
/// `match`; the dispatcher consults this via `HookProvider::recognizes_event`
/// to skip-and-log names a newer Codex emits that this build does not know
/// yet (AG-19).
pub(super) const CODEX_HOOK_EVENT_NAMES: &[&str] = &[
    "PreToolUse",
    "PermissionRequest",
    "PostToolUse",
    "PreCompact",
    "PostCompact",
    "SessionStart",
    "UserPromptSubmit",
    "SubagentStart",
    "SubagentStop",
    "Stop",
    "SessionEnd",
];

/// Translate a Codex hook event name into a canonical lifecycle event.
///
/// Functional scope: routes each known Codex hook (see the module-level
/// mapping table) through [`build_lifecycle_event`], which extracts the
/// standard fields from `envelope.extra` (Codex's payload is Claude
/// Code-compatible, so the shared key vocabulary matches).
///
/// Boundary conditions: unknown event names produce a hard error so that
/// upstream changes are surfaced immediately rather than silently dropped.
///
/// See: `tests::parser_maps_canonical_hooks`, `tests::parser_rejects_unknown_hook`.
pub(super) fn parse_codex_hook_event(
    hook_event_name: &str,
    envelope: &SessionHookEnvelope,
) -> Result<LifecycleEvent> {
    let kind = match hook_event_name {
        "SessionStart" => LifecycleEventKind::SessionStart,
        "UserPromptSubmit" => LifecycleEventKind::TurnStart,
        "PreToolUse" | "PostToolUse" => LifecycleEventKind::ToolUse,
        "Stop" => LifecycleEventKind::TurnEnd,
        "PreCompact" => LifecycleEventKind::Compaction,
        "PostCompact" => LifecycleEventKind::CompactionCompleted,
        "SubagentStart" => LifecycleEventKind::SubagentStart,
        "SubagentStop" => LifecycleEventKind::SubagentEnd,
        "PermissionRequest" => LifecycleEventKind::PermissionRequest,
        "SessionEnd" => LifecycleEventKind::SessionEnd,
        other => bail!("unknown Codex hook event: '{other}'"),
    };
    Ok(build_lifecycle_event(kind, envelope))
}

#[cfg(test)]
mod tests {
    use serde_json::{Map, Value, json};

    use super::*;

    fn canonical_envelope() -> SessionHookEnvelope {
        SessionHookEnvelope {
            hook_event_name: "SessionStart".to_string(),
            session_id: "s1".to_string(),
            cwd: "/tmp".to_string(),
            transcript_path: Some("/tmp/transcript.jsonl".to_string()),
            extra: {
                let mut map = Map::new();
                map.insert("prompt".to_string(), Value::String("hello".to_string()));
                map
            },
        }
    }

    // Scenario: the well-known Codex hook names map to their canonical kinds.
    #[test]
    fn parser_maps_canonical_hooks() {
        let envelope = canonical_envelope();
        let cases = [
            ("SessionStart", LifecycleEventKind::SessionStart),
            ("UserPromptSubmit", LifecycleEventKind::TurnStart),
            ("PreToolUse", LifecycleEventKind::ToolUse),
            ("PostToolUse", LifecycleEventKind::ToolUse),
            ("Stop", LifecycleEventKind::TurnEnd),
            ("PreCompact", LifecycleEventKind::Compaction),
            ("PostCompact", LifecycleEventKind::CompactionCompleted),
            ("PermissionRequest", LifecycleEventKind::PermissionRequest),
        ];

        for (name, kind) in cases {
            let event = parse_codex_hook_event(name, &envelope).expect("parse should succeed");
            assert_eq!(event.kind, kind, "event '{name}' must map to {kind:?}");
        }
    }

    // Scenario: the AG-19 sub-agent boundary events map to the new
    // SubagentStart / SubagentEnd lifecycle variants (not to Session*/Turn*).
    #[test]
    fn parser_maps_subagent_hooks_to_subagent_variants() {
        let envelope = canonical_envelope();
        let start = parse_codex_hook_event("SubagentStart", &envelope).expect("parse");
        assert_eq!(start.kind, LifecycleEventKind::SubagentStart);
        let stop = parse_codex_hook_event("SubagentStop", &envelope).expect("parse");
        assert_eq!(stop.kind, LifecycleEventKind::SubagentEnd);
    }

    // Scenario: a hook name not in the known set returns an error. Codex has
    // no ModelUpdate hook, while SessionEnd is supported.
    #[test]
    fn parser_rejects_unknown_hook() {
        let envelope = canonical_envelope();
        for name in ["UnknownHook", "ModelUpdate", "sessionstart"] {
            assert!(
                parse_codex_hook_event(name, &envelope).is_err(),
                "event '{name}' must be rejected",
            );
        }
    }

    // Scenario: the recognizes_event name table and the parser match stay in
    // sync — every advertised name must parse.
    #[test]
    fn every_advertised_event_name_parses() {
        let envelope = canonical_envelope();
        for name in CODEX_HOOK_EVENT_NAMES {
            assert!(
                parse_codex_hook_event(name, &envelope).is_ok(),
                "advertised event '{name}' must parse",
            );
        }
        assert_eq!(CODEX_HOOK_EVENT_NAMES.len(), 11);
    }

    // Scenario: Codex's Claude Code-compatible payload keys flow through
    // `build_lifecycle_event` — prompt, tool fields, and the last assistant
    // message all land on the canonical event.
    #[test]
    fn claude_compatible_payload_fields_flow_through() {
        let mut envelope = canonical_envelope();
        envelope.hook_event_name = "PostToolUse".to_string();
        envelope
            .extra
            .insert("tool_name".to_string(), json!("shell"));
        envelope
            .extra
            .insert("tool_input".to_string(), json!({"command": "ls"}));
        envelope
            .extra
            .insert("tool_response".to_string(), json!({"exit_code": 0}));
        envelope
            .extra
            .insert("tool_use_id".to_string(), json!("call_1"));

        let event = parse_codex_hook_event("PostToolUse", &envelope).expect("parse");
        assert_eq!(event.tool_name.as_deref(), Some("shell"));
        assert_eq!(event.tool_input, Some(json!({"command": "ls"})));
        assert_eq!(event.tool_response, Some(json!({"exit_code": 0})));

        let mut stop_envelope = canonical_envelope();
        stop_envelope.hook_event_name = "Stop".to_string();
        stop_envelope
            .extra
            .insert("last_assistant_message".to_string(), json!("done"));
        stop_envelope
            .extra
            .insert("stop_hook_active".to_string(), json!(false));
        let stop = parse_codex_hook_event("Stop", &stop_envelope).expect("parse");
        assert_eq!(stop.assistant_message.as_deref(), Some("done"));

        let prompt = parse_codex_hook_event("UserPromptSubmit", &canonical_envelope())
            .expect("parse should succeed");
        assert_eq!(prompt.prompt.as_deref(), Some("hello"));
    }
}
