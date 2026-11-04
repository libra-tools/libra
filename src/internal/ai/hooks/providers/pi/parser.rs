//! Pure decoding of the Pi observe envelope (ADR-PI-06).
//!
//! The extension forwards one JSON object per event with a fixed field set.
//! A frame with a field outside that set, a missing field, a missing
//! nullable key or a value of the wrong type is refused before it can reach
//! capture. Prompts, message bodies, tool input/output and thinking are not
//! part of the envelope; `sessionFile` is never used as a transcript source.
//! This module does not read files or persist state.
//!
//! Correlation (PI-12) needs no state either. Each recorded event names the
//! envelope field that is its replay identity ([`pi_replay_identity_keys`]);
//! the receiver hashes that field with the event name and session ID, so a
//! frame delivered again after a restart is the same action:
//!
//! - the session key is Pi's `sessionId`;
//! - a tool event's record key is its `toolCallId` and its phase is the
//!   event name, so `tool_call` and `tool_result` are separate actions that
//!   are each recorded at most once, in whichever order they arrive;
//! - `session_start` and a user `before_agent_start` carry no Pi ID, so
//!   they are keyed by `frameId`, a random UUID the extension gives every
//!   frame it builds and keeps when it delivers that frame again. A resume,
//!   a reload or a start from another Pi process is a new frame and a new
//!   action; the same frame is not.
//!
//! A nested tool event (`parentToolCallId` set) is recorded under its own
//! `toolCallId`, which Pi derives as `<parentToolCallId>/<n>`; a frame whose
//! ID is not a direct child of the parent it names is refused. Nested calls
//! have no transcript entry of their own, so nothing waits for one.

use anyhow::{Result, bail};
use chrono::{DateTime, Utc};
use serde_json::{Map, Value, json};

use super::super::super::lifecycle::{LifecycleEvent, LifecycleEventKind, SessionHookEnvelope};

/// Envelope version written by the bundled extension. Version 2 (PI-12)
/// added `frameId`; version 3 (PI-19) added the nullable `outcome` on
/// `agent_settled`. Frames written for another schema are refused.
pub const PI_ENVELOPE_SCHEMA: u64 = 3;

/// Fields every frame carries.
pub const PI_COMMON_FIELDS: [&str; 7] = [
    "schema",
    "event",
    "frameId",
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
        "agent_settled" => Some(&["aborted", "outcome"]),
        _ => None,
    }
}

pub fn recognizes_pi_event(name: &str) -> bool {
    PI_OBSERVED_EVENTS.contains(&name)
}

/// Events this receiver records. `agent_settled` is the run terminal
/// (ADR-PI-04); `turn_end` only feeds the extension's run-outcome tracking
/// and `session_shutdown` stays wire-only until a later card maps it.
pub fn pi_event_is_ingested(name: &str) -> bool {
    matches!(
        name,
        "session_start" | "before_agent_start" | "tool_call" | "tool_result" | "agent_settled"
    )
}

/// The envelope field that identifies one recorded event across replays.
/// The receiver commits to the field's name and value together with the
/// event name and session ID, so these names are part of the stable
/// identity and must not change.
pub fn pi_replay_identity_keys(event: &str) -> &'static [&'static str] {
    match event {
        "tool_call" | "tool_result" => &["toolCallId"],
        "session_start" | "before_agent_start" | "agent_settled" => &["frameId"],
        _ => &[],
    }
}

/// The name a `before_agent_start` takes when its message did not come from
/// the user, such as a `custom` message. It is not an observed event, so the
/// receiver skips the frame without writing anything: only a user message is
/// a prompt (ADR-PI-04).
pub const PI_NON_USER_RUN_START: &str = "before_agent_start.non_user";

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
    let frame_id = required_text(&object, "frameId")?;
    if !is_canonical_uuid(&frame_id) {
        bail!("pi hook frame frameId is not a UUID");
    }
    let mut extra = Map::new();
    extra.insert("frameId".to_string(), Value::String(frame_id));
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
            "outcome" => value.is_null() || matches!(value.as_str(), Some("completed" | "error")),
            _ => value.as_str().is_some_and(|text| !text.is_empty()),
        };
        if !valid {
            bail!("pi hook frame has a field of the wrong type for its event");
        }
        extra.insert((*key).to_string(), value.clone());
    }
    // Only tool events carry these keys, and they were checked above.
    if let Some(parent) = object.get("parentToolCallId").and_then(Value::as_str) {
        let child = object
            .get("toolCallId")
            .and_then(Value::as_str)
            .and_then(|id| id.strip_prefix(parent))
            .and_then(|rest| rest.strip_prefix('/'));
        if !child.is_some_and(|segment| !segment.is_empty() && !segment.contains('/')) {
            bail!("pi hook frame names a parent tool call it does not belong to");
        }
    }
    if event == "before_agent_start"
        && object.get("messageType").and_then(Value::as_str) != Some("user")
    {
        return Ok(SessionHookEnvelope {
            hook_event_name: PI_NON_USER_RUN_START.to_string(),
            session_id,
            cwd,
            transcript_path: None,
            extra: Map::new(),
        });
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
    let (kind, tool_name, partial, outcome) = match hook_event_name {
        "session_start" => (LifecycleEventKind::SessionStart, None, false, None),
        "before_agent_start" => (LifecycleEventKind::TurnStart, None, false, None),
        "tool_call" | "tool_result" => (
            LifecycleEventKind::ToolUse,
            envelope
                .extra
                .get("toolName")
                .and_then(Value::as_str)
                .map(str::to_string),
            false,
            None,
        ),
        // ADR-PI-04: only `agent_settled` ends a run. An aborted run is an
        // interruption, never a normal completion. A non-aborted run keeps
        // its checkpoint even when its last turn ended in an error, or when
        // the run produced no `turn_end` at all (outcome unknown), but both
        // are reported as partial rather than as a clean completion.
        "agent_settled" => {
            let aborted = envelope
                .extra
                .get("aborted")
                .and_then(Value::as_bool)
                .unwrap_or(false);
            let outcome = envelope.extra.get("outcome").and_then(Value::as_str);
            if aborted {
                (
                    LifecycleEventKind::TurnInterrupted,
                    None,
                    false,
                    Some("aborted"),
                )
            } else {
                match outcome {
                    Some("completed") => {
                        (LifecycleEventKind::TurnEnd, None, false, Some("completed"))
                    }
                    Some("error") => {
                        eprintln!("libra: pi run ended with an error; its checkpoint was kept");
                        (LifecycleEventKind::TurnEnd, None, true, Some("error"))
                    }
                    _ => {
                        eprintln!(
                            "libra: pi run ended without a completed turn; its checkpoint is partial"
                        );
                        (LifecycleEventKind::TurnEnd, None, true, Some("unknown"))
                    }
                }
            }
        }
        _ => bail!("pi hook event is not recorded by this receiver"),
    };
    Ok(LifecycleEvent {
        kind,
        session_id: envelope.session_id.clone(),
        session_ref: None,
        prompt: None,
        model: None,
        source: Some(match outcome {
            Some(outcome) => json!({
                "provider": "pi",
                "event": hook_event_name,
                "outcome": outcome,
                "partial": partial,
            }),
            None => json!({ "provider": "pi", "event": hook_event_name }),
        }),
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

/// A lowercase, hyphenated UUID, the form `crypto.randomUUID()` produces.
fn is_canonical_uuid(text: &str) -> bool {
    uuid::Uuid::try_parse(text).is_ok_and(|id| id.hyphenated().to_string() == text)
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
