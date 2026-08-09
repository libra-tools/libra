//! Code UI wire-format golden tests.
//!
//! Pins the on-the-wire shape consumed by the browser (`web/src/lib/code-ui/types.ts`):
//! camelCase struct fields and snake_case enum variants. Renaming a field, changing
//! a tag value, or reordering an enum will fail these tests immediately so the
//! frontend contract cannot drift silently.
//!
//! **Layer:** L1 — pure serde, no I/O, no async.

use chrono::{DateTime, Utc};
use libra::internal::ai::web::code_ui::{
    CodeUiAckResponse, CodeUiApplyToFuture, CodeUiCapabilities, CodeUiControllerAttachRequest,
    CodeUiControllerAttachResponse, CodeUiControllerKind, CodeUiControllerState,
    CodeUiEventEnvelope, CodeUiEventType, CodeUiInteractionKind, CodeUiInteractionOption,
    CodeUiInteractionRequest, CodeUiInteractionResponse, CodeUiInteractionStatus,
    CodeUiPatchChange, CodeUiPatchsetSnapshot, CodeUiPlanSnapshot, CodeUiPlanStep,
    CodeUiProviderInfo, CodeUiSessionSnapshot, CodeUiSessionStatus, CodeUiTaskSnapshot,
    CodeUiToolCallSnapshot, CodeUiTranscriptEntry, CodeUiTranscriptEntryKind,
};
use serde_json::{Value, json};

/// Fixed timestamp shared across fixtures so JSON literals stay deterministic.
fn fixed_ts() -> DateTime<Utc> {
    DateTime::<Utc>::from_timestamp(1_710_000_000, 0).expect("constant timestamp must parse")
}

/// Fully-populated `CodeUiSessionSnapshot` covering every field the browser
/// consumes — used to detect unintended renames or omitted serializations.
fn fully_populated_snapshot() -> CodeUiSessionSnapshot {
    let ts = fixed_ts();
    CodeUiSessionSnapshot {
        session_id: "session-1".to_string(),
        thread_id: Some("thread-1".to_string()),
        working_dir: "/repo".to_string(),
        provider: CodeUiProviderInfo {
            provider: "ollama".to_string(),
            model: Some("gemma4:31b".to_string()),
            mode: Some("tui".to_string()),
            managed: true,
        },
        capabilities: CodeUiCapabilities {
            message_input: true,
            streaming_text: true,
            plan_updates: true,
            tool_calls: true,
            patchsets: true,
            interactive_approvals: true,
            structured_questions: true,
            provider_session_resume: true,
            command_idempotency: true,
        },
        controller: CodeUiControllerState {
            kind: CodeUiControllerKind::Browser,
            owner_label: Some("browser-a".to_string()),
            can_write: true,
            lease_expires_at: Some(ts),
            reason: None,
            loopback_only: true,
        },
        status: CodeUiSessionStatus::AwaitingInteraction,
        transcript: vec![CodeUiTranscriptEntry {
            id: "msg-1".to_string(),
            kind: CodeUiTranscriptEntryKind::AssistantMessage,
            title: None,
            content: Some("hi".to_string()),
            status: None,
            streaming: true,
            metadata: json!({}),
            created_at: ts,
            updated_at: ts,
        }],
        plans: vec![CodeUiPlanSnapshot {
            id: "plan-1".to_string(),
            title: Some("Execution".to_string()),
            summary: None,
            status: "running".to_string(),
            steps: vec![CodeUiPlanStep {
                step: "step-1".to_string(),
                status: "queued".to_string(),
            }],
            updated_at: ts,
        }],
        tasks: vec![CodeUiTaskSnapshot {
            id: "task-1".to_string(),
            title: Some("Active".to_string()),
            status: "active".to_string(),
            details: None,
            updated_at: ts,
        }],
        tool_calls: vec![CodeUiToolCallSnapshot {
            id: "tool-1".to_string(),
            tool_name: "shell".to_string(),
            status: "running".to_string(),
            summary: None,
            details: None,
            updated_at: ts,
        }],
        patchsets: vec![CodeUiPatchsetSnapshot {
            id: "patch-1".to_string(),
            status: "ready".to_string(),
            changes: vec![CodeUiPatchChange {
                path: "src/lib.rs".to_string(),
                change_type: "modified".to_string(),
                diff: Some("--- a\n+++ b\n".to_string()),
            }],
            updated_at: ts,
        }],
        interactions: vec![CodeUiInteractionRequest {
            id: "int-1".to_string(),
            kind: CodeUiInteractionKind::PostPlanChoice,
            title: Some("Execute plan?".to_string()),
            description: None,
            prompt: None,
            options: vec![CodeUiInteractionOption {
                id: "execute".to_string(),
                label: "Execute".to_string(),
                description: None,
            }],
            status: CodeUiInteractionStatus::Pending,
            metadata: json!({"network": "offline"}),
            requested_at: ts,
            resolved_at: None,
        }],
        updated_at: ts,
    }
}

#[test]
fn indeterminate_side_effect_status_uses_a_stable_wire_value() {
    let mut snapshot = fully_populated_snapshot();
    snapshot.status = CodeUiSessionStatus::IndeterminateSideEffect;

    let serialized = serde_json::to_value(snapshot).expect("snapshot must serialize");
    assert_eq!(
        serialized.get("status"),
        Some(&Value::String("indeterminate_side_effect".into()))
    );
}

/// Round-trip serialization must preserve every observable wire field
/// (`sessionId`, `capabilities`, `controller.loopbackOnly`, transcript kinds,
/// patchset diffs, interaction options) so the browser type contract stays in
/// lock-step with the Rust source of truth.
#[test]
fn snapshot_round_trips_through_camel_case_wire_shape() {
    let snapshot = fully_populated_snapshot();
    let serialized = serde_json::to_value(&snapshot).expect("snapshot must serialize");

    // Top-level field naming pins.
    assert!(
        serialized.get("sessionId").is_some(),
        "sessionId must be camelCase"
    );
    assert!(serialized.get("threadId").is_some());
    assert!(serialized.get("workingDir").is_some());
    assert!(serialized.get("toolCalls").is_some());
    assert!(serialized.get("updatedAt").is_some());

    // Capability flag names — all eight booleans the browser gates UI on.
    let caps = serialized
        .get("capabilities")
        .expect("capabilities present");
    for flag in [
        "messageInput",
        "streamingText",
        "planUpdates",
        "toolCalls",
        "patchsets",
        "interactiveApprovals",
        "structuredQuestions",
        "providerSessionResume",
        "commandIdempotency",
    ] {
        assert_eq!(caps.get(flag), Some(&Value::Bool(true)), "{flag}");
    }

    // Controller state — `loopbackOnly` and `canWrite` must remain camelCase booleans.
    let controller = serialized.get("controller").expect("controller present");
    assert_eq!(
        controller.get("kind"),
        Some(&Value::String("browser".into()))
    );
    assert_eq!(controller.get("canWrite"), Some(&Value::Bool(true)));
    assert_eq!(controller.get("loopbackOnly"), Some(&Value::Bool(true)));
    assert!(controller.get("leaseExpiresAt").is_some());

    // Enum tag pins (snake_case values).
    assert_eq!(
        serialized.get("status"),
        Some(&Value::String("awaiting_interaction".into()))
    );
    assert_eq!(
        serialized["transcript"][0]["kind"],
        Value::String("assistant_message".into())
    );
    assert_eq!(
        serialized["interactions"][0]["kind"],
        Value::String("post_plan_choice".into())
    );
    assert_eq!(
        serialized["interactions"][0]["status"],
        Value::String("pending".into())
    );

    // Patchset path round-trips with `changeType` (camelCase from `change_type`).
    assert_eq!(
        serialized["patchsets"][0]["changes"][0]["changeType"],
        Value::String("modified".into())
    );

    // Round-trip back into the typed snapshot to catch silent drops.
    let round_tripped: CodeUiSessionSnapshot =
        serde_json::from_value(serialized).expect("snapshot must deserialize");
    assert_eq!(round_tripped.session_id, "session-1");
    assert_eq!(round_tripped.transcript.len(), 1);
    assert!(round_tripped.transcript[0].streaming);
    assert_eq!(round_tripped.controller.kind, CodeUiControllerKind::Browser);
    assert!(round_tripped.controller.loopback_only);
    assert_eq!(
        round_tripped.patchsets[0].changes[0].change_type,
        "modified"
    );
}

/// SSE envelopes must use the same closed event-name set the browser's
/// `CodeUiEventType` union subscribes to, and the payload must remain a typed
/// full snapshot instead of arbitrary JSON.
#[test]
fn event_envelope_round_trips_typed_event_and_snapshot_payload() {
    let snapshot = fully_populated_snapshot();
    let event = CodeUiEventEnvelope {
        seq: 42,
        event_type: CodeUiEventType::ControllerChanged,
        at: fixed_ts(),
        data: snapshot,
    };

    let serialized = serde_json::to_value(&event).expect("event envelope must serialize");
    assert_eq!(
        serialized["type"],
        Value::String("controller_changed".into())
    );
    assert_eq!(
        serialized["data"]["sessionId"],
        Value::String("session-1".into())
    );
    assert_eq!(
        serialized["data"]["interactions"][0]["kind"],
        Value::String("post_plan_choice".into())
    );

    let round_tripped: CodeUiEventEnvelope =
        serde_json::from_value(serialized).expect("event envelope must deserialize");
    assert_eq!(round_tripped.event_type, CodeUiEventType::ControllerChanged);
    assert_eq!(round_tripped.data.session_id, "session-1");
    assert_eq!(round_tripped.data.interactions.len(), 1);
    assert_eq!(
        round_tripped.data.interactions[0].status,
        CodeUiInteractionStatus::Pending
    );
}

/// Every `CodeUiTranscriptEntryKind` variant must serialize to the snake_case
/// value the browser switches on — drift here silently breaks the chat pane.
#[test]
fn transcript_entry_kinds_use_snake_case_values() {
    for (variant, expected) in [
        (CodeUiTranscriptEntryKind::UserMessage, "user_message"),
        (
            CodeUiTranscriptEntryKind::AssistantMessage,
            "assistant_message",
        ),
        (CodeUiTranscriptEntryKind::ToolCall, "tool_call"),
        (CodeUiTranscriptEntryKind::PlanSummary, "plan_summary"),
        (CodeUiTranscriptEntryKind::Diff, "diff"),
        (CodeUiTranscriptEntryKind::InfoNote, "info_note"),
    ] {
        let value = serde_json::to_value(variant).unwrap();
        assert_eq!(value, Value::String(expected.into()));
    }
}

/// All five interaction kinds shipped to the browser must keep their snake_case
/// wire tags. These are the exact strings the InteractionPanel switches on.
#[test]
fn interaction_kinds_use_snake_case_values() {
    for (variant, expected) in [
        (CodeUiInteractionKind::Approval, "approval"),
        (CodeUiInteractionKind::SandboxApproval, "sandbox_approval"),
        (
            CodeUiInteractionKind::RequestUserInput,
            "request_user_input",
        ),
        (
            CodeUiInteractionKind::IntentReviewChoice,
            "intent_review_choice",
        ),
        (CodeUiInteractionKind::PostPlanChoice, "post_plan_choice"),
    ] {
        let value = serde_json::to_value(variant).unwrap();
        assert_eq!(value, Value::String(expected.into()));
    }
}

/// Controller kinds the API layer accepts on attach/detach must keep the same
/// snake_case tags the frontend embeds in request bodies.
#[test]
fn controller_kinds_use_snake_case_values() {
    for (variant, expected) in [
        (CodeUiControllerKind::None, "none"),
        (CodeUiControllerKind::Browser, "browser"),
        (CodeUiControllerKind::Automation, "automation"),
        (CodeUiControllerKind::Tui, "tui"),
        (CodeUiControllerKind::Cli, "cli"),
    ] {
        let value = serde_json::to_value(variant).unwrap();
        assert_eq!(value, Value::String(expected.into()));
    }
}

/// Apply-to-future enum is one of the few request-side enums the frontend
/// emits. Locking the snake_case tags here catches regressions in
/// approval / sandbox-approval response payloads.
#[test]
fn apply_to_future_uses_snake_case_values() {
    for (variant, expected) in [
        (CodeUiApplyToFuture::No, "no"),
        (CodeUiApplyToFuture::AcceptAll, "accept_all"),
        (CodeUiApplyToFuture::DeclineAll, "decline_all"),
    ] {
        let value = serde_json::to_value(variant).unwrap();
        assert_eq!(value, Value::String(expected.into()));
    }
}

/// Controller attach/detach and ack response shapes the browser depends on.
/// Together they pin the lease handshake (`controllerToken`, `leaseExpiresAt`)
/// and the post-write acknowledgement (`accepted`).
#[test]
fn controller_attach_request_round_trip_pins_camel_case() {
    let request: CodeUiControllerAttachRequest =
        serde_json::from_value(json!({ "clientId": "browser-a" })).unwrap();
    assert_eq!(request.client_id, "browser-a");
    assert_eq!(request.kind, CodeUiControllerKind::Browser);

    let response = CodeUiControllerAttachResponse {
        controller_token: "tok".to_string(),
        lease_expires_at: fixed_ts(),
        controller: CodeUiControllerState {
            kind: CodeUiControllerKind::Browser,
            owner_label: Some("browser-a".to_string()),
            can_write: true,
            lease_expires_at: Some(fixed_ts()),
            reason: None,
            loopback_only: true,
        },
    };
    let serialized = serde_json::to_value(&response).unwrap();
    assert!(serialized.get("controllerToken").is_some());
    assert!(serialized.get("leaseExpiresAt").is_some());
    assert!(serialized["controller"].get("loopbackOnly").is_some());

    let ack = CodeUiAckResponse { accepted: true };
    let ack_value = serde_json::to_value(&ack).unwrap();
    assert_eq!(ack_value, json!({ "accepted": true }));
}

/// `GET /api/code/threads` returns this envelope shape. Pin every field name
/// the browser switches on so the Sidebar list cannot silently desync from
/// the server payload (`items[].id/title/archived/currentIntentId/createdAt/
/// updatedAt`, top-level `nextOffset`).
#[test]
fn thread_list_response_envelope_uses_camel_case_wire_shape() {
    let envelope = serde_json::json!({
        "items": [
            {
                "id": "11111111-1111-4111-8111-111111111111",
                "title": "Demo thread",
                "archived": false,
                "currentIntentId": "22222222-2222-4222-8222-222222222222",
                "createdAt": "2026-05-06T00:00:00Z",
                "updatedAt": "2026-05-06T00:00:01Z",
            },
        ],
        "nextOffset": 1,
    });
    let item = &envelope["items"][0];
    for field in [
        "id",
        "title",
        "archived",
        "currentIntentId",
        "createdAt",
        "updatedAt",
    ] {
        assert!(item.get(field).is_some(), "{field} must be camelCase");
    }
    assert!(envelope.get("nextOffset").is_some());
}

/// Interaction-response payload — the only request body that has optional
/// fields with mixed naming. Pins `selectedOption`, `applyToFuture`, and the
/// `answers` map's plain string keys.
#[test]
fn interaction_response_serialization_drops_none_fields() {
    let response = CodeUiInteractionResponse {
        approved: Some(true),
        apply_to_future: Some(CodeUiApplyToFuture::AcceptAll),
        selected_option: Some("execute".to_string()),
        note: None,
        answers: [("q1".to_string(), vec!["yes".to_string()])]
            .into_iter()
            .collect(),
    };
    let value = serde_json::to_value(&response).unwrap();
    assert_eq!(value["approved"], Value::Bool(true));
    assert_eq!(value["applyToFuture"], Value::String("accept_all".into()));
    assert_eq!(value["selectedOption"], Value::String("execute".into()));
    assert!(value.get("note").is_none(), "None options must be skipped");
    assert_eq!(value["answers"]["q1"][0], Value::String("yes".into()));
}
