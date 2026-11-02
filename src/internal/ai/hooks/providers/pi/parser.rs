//! Pure decoding of the Pi observe envelope (ADR-PI-06).
//!
//! The extension forwards one JSON object per event with a fixed field set.
//! A frame with a field outside that set, a missing field, a missing
//! nullable key or a value of the wrong type is refused before it can reach
//! capture. Prompts, message bodies, tool input/output and thinking are not
//! part of the envelope; `sessionFile` is never used as a transcript source.
//! This module does not read files or persist state.

use anyhow::{Result, bail};
use chrono::{DateTime, Utc};
use serde_json::{Map, Value, json};

use super::super::super::lifecycle::{LifecycleEvent, LifecycleEventKind, SessionHookEnvelope};

/// Envelope version written by the bundled extension.
pub const PI_ENVELOPE_SCHEMA: u64 = 1;

/// Fields every frame carries.
pub const PI_COMMON_FIELDS: [&str; 6] = [
    "schema",
    "event",
    "sessionId",
    "sessionFile",
    "cwd",
    "emittedAt",
];

/// Keys that are always present but may hold JSON `null`.
pub const PI_NULLABLE_FIELDS: [&str; 4] = [
    "sessionFile",
    "previousSessionFile",
    "targetSessionFile",
    "parentToolCallId",
];

/// The events the extension subscribes to, in ADR-PI-06 order.
pub const PI_OBSERVED_EVENTS: [&str; 7] = [
    "session_start",
    "session_shutdown",
    "before_agent_start",
    "turn_end",
    "tool_call",
    "tool_result",
    "agent_settled",
];

/// Per-event fields beyond [`PI_COMMON_FIELDS`].
pub fn pi_event_fields(event: &str) -> Option<&'static [&'static str]> {
    match event {
        "session_start" => Some(&["reason", "previousSessionFile"]),
        "session_shutdown" => Some(&["reason", "targetSessionFile"]),
        "before_agent_start" => Some(&["messageType"]),
        "turn_end" => Some(&[
            "turnIndex",
            "messageEntryId",
            "toolResultEntryIds",
            "outcome",
        ]),
        "tool_call" | "tool_result" => Some(&["toolCallId", "parentToolCallId", "toolName"]),
        "agent_settled" => Some(&["aborted"]),
        _ => None,
    }
}

pub fn recognizes_pi_event(name: &str) -> bool {
    PI_OBSERVED_EVENTS.contains(&name)
}

/// Events this receiver records. The run, turn and session-end boundaries
/// are accepted on the wire but not recorded until their mapping lands
/// (PI-12 correlation, PI-19 run terminal).
pub fn pi_event_is_ingested(name: &str) -> bool {
    matches!(name, "session_start" | "tool_call" | "tool_result")
}

/// Lower one frame to the shared envelope. Error text is fixed and never
/// echoes frame content.
pub fn lower_pi_frame(frame: &str) -> Result<SessionHookEnvelope> {
    let Some(object) = unique_object(frame) else {
        bail!("pi hook frame is not a JSON object with unique keys");
    };
    let event = match object.get("event") {
        Some(Value::String(event)) if !event.is_empty() => event.clone(),
        _ => bail!("pi hook frame has no event name"),
    };
    let session_id = required_text(&object, "sessionId")?;
    let cwd = required_text(&object, "cwd")?;
    let Some(fields) = pi_event_fields(&event) else {
        // A newer extension event: keep only its name so the receiver can
        // skip it as an unknown event without reading anything else.
        return Ok(SessionHookEnvelope {
            hook_event_name: event,
            session_id,
            cwd,
            transcript_path: None,
            extra: Map::new(),
        });
    };
    let expected = PI_COMMON_FIELDS.len() + fields.len();
    if object.len() != expected
        || !PI_COMMON_FIELDS
            .iter()
            .chain(fields.iter())
            .all(|key| object.contains_key(*key))
    {
        bail!("pi hook frame fields do not match the envelope for its event");
    }
    if object.get("schema").and_then(Value::as_u64) != Some(PI_ENVELOPE_SCHEMA) {
        bail!("pi hook frame has an unsupported envelope schema");
    }
    let emitted_at = required_text(&object, "emittedAt")?;
    if DateTime::parse_from_rfc3339(&emitted_at).is_err() {
        bail!("pi hook frame emittedAt is not an RFC 3339 time");
    }
    nullable_text(&object, "sessionFile")?;
    let mut extra = Map::new();
    for key in fields {
        let value = &object[*key];
        let valid = match *key {
            "previousSessionFile" | "targetSessionFile" | "parentToolCallId" => {
                nullable_text(&object, key)?;
                true
            }
            "turnIndex" => value.as_u64().is_some(),
            "toolResultEntryIds" => value.as_array().is_some_and(|ids| {
                ids.iter()
                    .all(|id| id.as_str().is_some_and(|id| !id.is_empty()))
            }),
            "aborted" => value.is_boolean(),
            _ => value.as_str().is_some_and(|text| !text.is_empty()),
        };
        if !valid {
            bail!("pi hook frame has a field of the wrong type for its event");
        }
        extra.insert((*key).to_string(), value.clone());
    }
    Ok(SessionHookEnvelope {
        hook_event_name: event,
        session_id,
        cwd,
        transcript_path: None,
        extra,
    })
}

/// Decode a lowered envelope into a lifecycle event.
pub fn parse_pi_hook_event(
    hook_event_name: &str,
    envelope: &SessionHookEnvelope,
) -> Result<LifecycleEvent> {
    if !pi_event_is_ingested(hook_event_name) {
        bail!("pi hook event is not recorded by this receiver");
    }
    if envelope.transcript_path.is_some() {
        bail!("pi hook does not accept a transcript path");
    }
    let (kind, tool_name) = match hook_event_name {
        "session_start" => (LifecycleEventKind::SessionStart, None),
        "tool_call" | "tool_result" => (
            LifecycleEventKind::ToolUse,
            envelope
                .extra
                .get("toolName")
                .and_then(Value::as_str)
                .map(str::to_string),
        ),
        _ => bail!("pi hook event is not recorded by this receiver"),
    };
    Ok(LifecycleEvent {
        kind,
        session_id: envelope.session_id.clone(),
        session_ref: None,
        prompt: None,
        model: None,
        source: Some(json!({ "provider": "pi", "event": hook_event_name })),
        tool_name,
        tool_input: None,
        tool_response: None,
        assistant_message: None,
        timestamp: Utc::now(),
    })
}

/// Parse one JSON object, refusing a repeated key that `serde_json::Value`
/// would otherwise resolve silently to its last value.
fn unique_object(frame: &str) -> Option<Map<String, Value>> {
    struct UniqueObject(Map<String, Value>);

    impl<'de> serde::Deserialize<'de> for UniqueObject {
        fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
            struct ObjectVisitor;

            impl<'de> serde::de::Visitor<'de> for ObjectVisitor {
                type Value = UniqueObject;

                fn expecting(&self, formatter: &mut std::fmt::Formatter) -> std::fmt::Result {
                    formatter.write_str("a JSON object")
                }

                fn visit_map<A: serde::de::MapAccess<'de>>(
                    self,
                    mut access: A,
                ) -> Result<UniqueObject, A::Error> {
                    let mut object = Map::new();
                    while let Some((key, value)) = access.next_entry::<String, Value>()? {
                        if object.insert(key, value).is_some() {
                            return Err(serde::de::Error::custom("repeated key"));
                        }
                    }
                    Ok(UniqueObject(object))
                }
            }

            deserializer.deserialize_map(ObjectVisitor)
        }
    }

    serde_json::from_str::<UniqueObject>(frame)
        .ok()
        .map(|object| object.0)
}

fn required_text(object: &Map<String, Value>, key: &str) -> Result<String> {
    match object.get(key) {
        Some(Value::String(text)) if !text.is_empty() => Ok(text.clone()),
        _ => bail!("pi hook frame is missing a required field"),
    }
}

fn nullable_text(object: &Map<String, Value>, key: &str) -> Result<()> {
    match object.get(key) {
        Some(Value::Null) => Ok(()),
        Some(Value::String(text)) if !text.is_empty() => Ok(()),
        _ => bail!("pi hook frame has a nullable field of the wrong type"),
    }
}
