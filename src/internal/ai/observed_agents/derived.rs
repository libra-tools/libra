//! Back-derive [`ToolCallRecord`] entries from a captured agent
//! session's `normalized_events` stream.
//!
//! Phase 4.3 (entire.md §14.4 item 3). The hook runtime appends a
//! projection-friendly summary of each lifecycle event onto
//! `SessionState.metadata["normalized_events"]`. The actual
//! normalized event schema (see `hooks::runtime::append_normalized_event`)
//! is:
//!
//! ```json
//! {
//!   "provider": "claude",
//!   "kind": "tool_use",         // LifecycleEventKind::ToolUse
//!   "timestamp": "2026-05-05T10:00:00+00:00",
//!   "prompt": null,
//!   "tool_name": "Read",
//!   "assistant_message": null,
//!   "has_model": false,
//!   "has_tool_input": true,
//!   "has_tool_response": true
//! }
//! ```
//!
//! There is no `pre_tool_use` / `post_tool_use` distinction at the
//! lifecycle layer — those are HookEvent envelope kinds upstream.
//! Each lifecycle `tool_use` event represents one envelope, and the
//! `has_tool_input` / `has_tool_response` flags encode whether the
//! envelope was the pre or post side. We turn each `tool_use` event
//! into one [`ToolCallRecord`].
//!
//! Mapping rules:
//! - `kind == "tool_use"` qualifies; everything else is skipped.
//! - `action`:
//!   - `"call"` when both `has_tool_input` and `has_tool_response`
//!     are true (a complete envelope captured both sides).
//!   - `"invoke_only"` when only `has_tool_input` is true.
//!   - `"observe_only"` when only `has_tool_response` is true.
//!   - `"unspecified"` when neither flag is true (rare — would happen
//!     if a future provider emits a tool_use event with no payload).
//! - `success` here means "the response side of the envelope was
//!   observed" — NOT "the tool call returned without error". The
//!   captured normalized stream does not preserve the response
//!   payload, so we cannot inspect the actual success/error code; v2
//!   adapters can re-parse the per-agent transcript for that. We set
//!   it to `true` only when `has_tool_response` is true, on the
//!   conservative grounds that a tool whose response was never
//!   captured cannot be considered successful.
//! - `paths_read` / `paths_written` / `arguments_json` / `summary` /
//!   `diffs` are left empty: the normalized stream stores
//!   `tool_name` plus boolean flags, not the raw envelope.
//!
//! Per-envelope vs per-logical-call:
//! - Both `PreToolUse` and `PostToolUse` Claude/Gemini hook
//!   envelopes map to `LifecycleEventKind::ToolUse` (see
//!   `hooks::providers::claude::parser::map_kind`). Since DR-00
//!   (plan-20260713) `CLAUDE_HOOK_FORWARD_MAP` in `claude/settings.rs`
//!   registers **both** `PreToolUse` and `PostToolUse`. Note that the
//!   installed Claude hooks capture via the AgentTraces path
//!   (`agent_session` liveness only — they do NOT populate this
//!   normalized `tool_use` stream or `tool_use_count`); this derived
//!   builder is exercised by the AiIntent/derived consumers, where a
//!   pre/post pair yields one record per envelope (see below).
//! - The schema does NOT carry a `tool_use_id`, so if a future
//!   provider DOES register both pre- and post-hooks, this function
//!   would emit two records per call. That outcome is documented in
//!   the trace as `invoke_only` (pre) followed by `observe_only`
//!   (post), preserving the audit trail. Pairing requires either a
//!   correlation id or a heuristic match — both are deferred to v2
//!   when adapter contracts harden.

use serde_json::Value;

use crate::internal::ai::{orchestrator::types::ToolCallRecord, session::SessionState};

/// Walk `session.metadata["normalized_events"]` and return one
/// [`ToolCallRecord`] per `tool_use` lifecycle event. Returns an empty
/// vec when the session has no normalized events or none are
/// tool-use events.
pub fn derive_tool_call_records(session: &SessionState) -> Vec<ToolCallRecord> {
    let Some(Value::Array(events)) = session.metadata.get("normalized_events") else {
        return Vec::new();
    };

    let mut out: Vec<ToolCallRecord> = Vec::new();
    for event in events {
        let Some(kind) = event.get("kind").and_then(Value::as_str) else {
            continue;
        };
        if kind != "tool_use" {
            continue;
        }
        let tool_name = event
            .get("tool_name")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string();
        let has_input = event
            .get("has_tool_input")
            .and_then(Value::as_bool)
            .unwrap_or(false);
        let has_response = event
            .get("has_tool_response")
            .and_then(Value::as_bool)
            .unwrap_or(false);
        let action = match (has_input, has_response) {
            (true, true) => "call",
            (true, false) => "invoke_only",
            (false, true) => "observe_only",
            (false, false) => "unspecified",
        };
        out.push(ToolCallRecord {
            tool_name,
            action: action.to_string(),
            arguments_json: None,
            paths_read: Vec::new(),
            paths_written: Vec::new(),
            success: has_response,
            summary: None,
            diffs: Vec::new(),
        });
    }
    out
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    fn fixture_session(events: Vec<Value>) -> SessionState {
        let mut session = SessionState::new("/tmp/fixture");
        session
            .metadata
            .insert("normalized_events".to_string(), Value::Array(events));
        session
    }

    #[test]
    fn returns_empty_when_no_normalized_events_present() {
        let session = SessionState::new("/tmp/fixture");
        assert!(derive_tool_call_records(&session).is_empty());
    }

    #[test]
    fn returns_empty_when_normalized_events_has_no_tool_use_kinds() {
        let session = fixture_session(vec![
            json!({"kind": "session_start", "timestamp": "2026-05-05T10:00:00Z"}),
            json!({"kind": "turn_start", "timestamp": "2026-05-05T10:01:00Z"}),
        ]);
        assert!(derive_tool_call_records(&session).is_empty());
    }

    /// The full envelope (input + response captured) — typical Claude
    /// PostToolUse hook envelope. action="call", success=true.
    #[test]
    fn full_envelope_maps_to_call_with_success() {
        let session = fixture_session(vec![json!({
            "provider": "claude",
            "kind": "tool_use",
            "timestamp": "2026-05-05T10:00:00Z",
            "prompt": null,
            "tool_name": "Read",
            "assistant_message": null,
            "has_model": false,
            "has_tool_input": true,
            "has_tool_response": true,
        })]);
        let records = derive_tool_call_records(&session);
        assert_eq!(records.len(), 1);
        assert_eq!(records[0].tool_name, "Read");
        assert_eq!(records[0].action, "call");
        assert!(records[0].success);
    }

    /// Pre-only envelope: invoke_only, success=false. The agent
    /// started the tool call but the post-envelope never landed
    /// (hook dropout / agent crash).
    #[test]
    fn input_only_envelope_marks_invoke_only_and_unsuccessful() {
        let session = fixture_session(vec![json!({
            "kind": "tool_use",
            "tool_name": "Bash",
            "timestamp": "2026-05-05T10:00:00Z",
            "has_tool_input": true,
            "has_tool_response": false,
        })]);
        let records = derive_tool_call_records(&session);
        assert_eq!(records.len(), 1);
        assert_eq!(records[0].tool_name, "Bash");
        assert_eq!(records[0].action, "invoke_only");
        assert!(!records[0].success);
    }

    /// Post-only envelope: observe_only, success=true. Pre-envelope
    /// dropped but the response landed.
    #[test]
    fn response_only_envelope_marks_observe_only_and_succeeds() {
        let session = fixture_session(vec![json!({
            "kind": "tool_use",
            "tool_name": "Edit",
            "timestamp": "2026-05-05T10:00:00Z",
            "has_tool_input": false,
            "has_tool_response": true,
        })]);
        let records = derive_tool_call_records(&session);
        assert_eq!(records.len(), 1);
        assert_eq!(records[0].tool_name, "Edit");
        assert_eq!(records[0].action, "observe_only");
        assert!(records[0].success);
    }

    /// Defensive case: tool_use event with neither flag set. We still
    /// emit a record (so the audit trail is honest) marked
    /// `unspecified` and unsuccessful.
    #[test]
    fn no_flags_envelope_marks_unspecified() {
        let session = fixture_session(vec![json!({
            "kind": "tool_use",
            "tool_name": "Mystery",
            "timestamp": "2026-05-05T10:00:00Z",
        })]);
        let records = derive_tool_call_records(&session);
        assert_eq!(records.len(), 1);
        assert_eq!(records[0].tool_name, "Mystery");
        assert_eq!(records[0].action, "unspecified");
        assert!(!records[0].success);
    }

    /// One record per event, in event order.
    #[test]
    fn order_is_event_order_with_one_record_per_tool_use() {
        let session = fixture_session(vec![
            json!({
                "kind": "tool_use",
                "tool_name": "First",
                "has_tool_input": true,
                "has_tool_response": true,
            }),
            json!({"kind": "session_start"}),
            json!({
                "kind": "tool_use",
                "tool_name": "Second",
                "has_tool_input": true,
                "has_tool_response": false,
            }),
        ]);
        let records = derive_tool_call_records(&session);
        assert_eq!(records.len(), 2);
        assert_eq!(records[0].tool_name, "First");
        assert_eq!(records[0].action, "call");
        assert_eq!(records[1].tool_name, "Second");
        assert_eq!(records[1].action, "invoke_only");
    }

    /// When both `PreToolUse` and `PostToolUse` envelopes reach this
    /// builder — since DR-00, `claude/settings.rs::CLAUDE_HOOK_FORWARD_MAP`
    /// registers both (installed Claude capture uses the AgentTraces path,
    /// so this normalized stream is the AiIntent/derived consumer) — the
    /// runtime emits two `tool_use` normalized events for one logical
    /// call: one with `has_tool_input=true` and one with
    /// `has_tool_response=true`. This test pins the documented
    /// per-envelope behavior so a regression to silent merging or
    /// silent dropping fails loudly. v2 is expected to introduce
    /// pairing once adapter contracts carry a correlation id.
    #[test]
    fn pre_then_post_envelopes_emit_two_records_documenting_v1_limitation() {
        let session = fixture_session(vec![
            json!({
                "kind": "tool_use",
                "tool_name": "Bash",
                "has_tool_input": true,
                "has_tool_response": false,
            }),
            json!({
                "kind": "tool_use",
                "tool_name": "Bash",
                "has_tool_input": false,
                "has_tool_response": true,
            }),
        ]);
        let records = derive_tool_call_records(&session);
        assert_eq!(records.len(), 2, "v1 emits one record per envelope");
        assert_eq!(records[0].action, "invoke_only");
        assert!(!records[0].success);
        assert_eq!(records[1].action, "observe_only");
        assert!(records[1].success);
    }

    /// True end-to-end: drives the actual `runtime::append_normalized_event`
    /// writer (the same function the live hook ingestion path uses)
    /// and then asserts the derivation reads it correctly. If the
    /// runtime schema changes — a key is added, removed, renamed, or
    /// retyped — and the derivation isn't updated in lockstep, this
    /// test fails loudly. There is no parallel JSON fixture to drift.
    #[test]
    fn integration_with_hook_runtime_normalized_event_schema() {
        use chrono::Utc;

        use crate::internal::ai::hooks::{
            lifecycle::{LifecycleEvent, LifecycleEventKind},
            runtime::append_normalized_event,
        };

        let event = LifecycleEvent {
            kind: LifecycleEventKind::ToolUse,
            session_id: "sess-1".to_string(),
            session_ref: None,
            prompt: None,
            model: None,
            source: None,
            tool_name: Some("Bash".to_string()),
            tool_input: Some(json!({"cmd": "ls"})),
            tool_response: Some(json!({"out": "ok"})),
            assistant_message: None,
            timestamp: Utc::now(),
        };
        let mut session = SessionState::new("/tmp/fixture");
        append_normalized_event(&mut session, &event, "claude");

        let records = derive_tool_call_records(&session);
        assert_eq!(records.len(), 1);
        assert_eq!(records[0].tool_name, "Bash");
        assert_eq!(records[0].action, "call");
        assert!(records[0].success);
    }
}
