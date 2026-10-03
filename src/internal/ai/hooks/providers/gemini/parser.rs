//! Gemini hook event parser.
//!
//! Boundary: parser accepts the canonical Gemini hook envelope and rejects unknown or
//! malformed event shapes with contextual errors. Unit tests cover every supported hook
//! name and missing-field edge cases.

use anyhow::{Result, bail};
use serde_json::Value;

use super::super::super::lifecycle::{
    LifecycleEvent, LifecycleEventKind, SessionHookEnvelope, build_lifecycle_event,
};

/// Every Gemini hook event name [`parse_gemini_hook_event`] understands.
/// Keep in sync with its `match`; consulted via
/// `HookProvider::recognizes_event` (AG-19 skip-and-log for unknown
/// upstream event names).
pub(super) const GEMINI_HOOK_EVENT_NAMES: &[&str] = &[
    "SessionStart",
    "BeforeAgent",
    "UserPromptSubmit",
    "Prompt",
    "PostToolUse",
    "ToolUse",
    "BeforeTool",
    "AfterTool",
    "BeforeModel",
    "ModelUpdate",
    "PreCompress",
    "Compaction",
    "AfterAgent",
    "Stop",
    "SessionStop",
    "SessionEnd",
];

pub(super) fn parse_gemini_hook_event(
    hook_event_name: &str,
    envelope: &SessionHookEnvelope,
) -> Result<LifecycleEvent> {
    let kind = match hook_event_name {
        "SessionStart" => LifecycleEventKind::SessionStart,
        "BeforeAgent" | "UserPromptSubmit" | "Prompt" => LifecycleEventKind::TurnStart,
        "PostToolUse" | "ToolUse" | "BeforeTool" | "AfterTool" => LifecycleEventKind::ToolUse,
        "BeforeModel" | "ModelUpdate" => LifecycleEventKind::ModelUpdate,
        "PreCompress" | "Compaction" => LifecycleEventKind::Compaction,
        "AfterAgent" | "Stop" | "SessionStop" => LifecycleEventKind::TurnEnd,
        "SessionEnd" => LifecycleEventKind::SessionEnd,
        other => bail!("unknown Gemini CLI hook event: '{other}'"),
    };

    let mut event = build_lifecycle_event(kind, envelope);
    if matches!(kind, LifecycleEventKind::ModelUpdate)
        && event.model.is_none()
        && let Some(model) = envelope
            .extra
            .get("llm_request")
            .and_then(Value::as_object)
            .and_then(|request| request.get("model"))
    {
        event.model = Some(model.clone());
    }
    Ok(event)
}

#[cfg(test)]
mod tests {
    use serde_json::{Map, json};

    use super::*;

    fn canonical_envelope() -> SessionHookEnvelope {
        SessionHookEnvelope {
            hook_event_name: "SessionStart".to_string(),
            session_id: "s1".to_string(),
            cwd: "/tmp".to_string(),
            transcript_path: Some("/tmp/transcript.jsonl".to_string()),
            extra: {
                let mut map = Map::new();
                map.insert(
                    "llm_request".to_string(),
                    json!({"model": "gemini-2.5-pro"}),
                );
                map
            },
        }
    }

    #[test]
    fn parser_maps_canonical_and_native_hooks() {
        let envelope = canonical_envelope();
        let cases = [
            ("SessionStart", LifecycleEventKind::SessionStart),
            ("UserPromptSubmit", LifecycleEventKind::TurnStart),
            ("PostToolUse", LifecycleEventKind::ToolUse),
            ("Stop", LifecycleEventKind::TurnEnd),
            ("SessionEnd", LifecycleEventKind::SessionEnd),
            ("BeforeAgent", LifecycleEventKind::TurnStart),
            ("AfterAgent", LifecycleEventKind::TurnEnd),
            ("BeforeModel", LifecycleEventKind::ModelUpdate),
            ("PreCompress", LifecycleEventKind::Compaction),
            ("AfterTool", LifecycleEventKind::ToolUse),
        ];

        for (name, kind) in cases {
            let event = parse_gemini_hook_event(name, &envelope).expect("parse should succeed");
            assert_eq!(event.kind, kind);
        }
    }

    #[test]
    fn parser_rejects_unknown_hook() {
        let mut envelope = canonical_envelope();
        envelope.hook_event_name = "UnknownHook".to_string();
        assert!(parse_gemini_hook_event("UnknownHook", &envelope).is_err());
    }

    #[test]
    fn extract_model_from_llm_request() {
        let event =
            parse_gemini_hook_event("BeforeModel", &canonical_envelope()).expect("parse succeed");
        assert_eq!(event.model, Some(json!("gemini-2.5-pro")));
    }
}
