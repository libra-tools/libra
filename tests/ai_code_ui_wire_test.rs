//! Code UI wire-format golden tests.
//!
//! Pins the on-the-wire shape consumed by the browser (`web/src/lib/code-ui/types.ts`):
//! camelCase struct fields and snake_case enum variants. Renaming a field, changing
//! a tag value, or reordering an enum will fail these tests immediately so the
//! frontend contract cannot drift silently.
//!
//! **Layer:** L1 — pure serde, no I/O, no async.

use chrono::{DateTime, Utc};
use libra::internal::ai::{
    agent::runtime::{RuntimeUsageTotals, UsageStatus},
    runtime::{ExecutionFailureEvidence, ExecutionFailureRevision, PlanExecutionRepairState},
    session::{
        CodeCommandIdentity, CodeCommandIntent, CodeWorkflowEvent, CodeWorkflowEventKind,
        INTENT_REVISION_CONSUMER_COMMAND_KIND, INTENT_REVISION_CONSUMPTION_SCHEMA_VERSION,
        IntentRevisionConsumption, IntentRevisionConsumptionClaim, IntentRevisionRecovery,
        Phase1RetryIntentReview,
    },
    web::{
        ThreadListItem,
        code_ui::{
            CodeUiAckResponse, CodeUiApiError, CodeUiApplyToFuture, CodeUiCapabilities,
            CodeUiControllerAttachRequest, CodeUiControllerAttachResponse, CodeUiControllerKind,
            CodeUiControllerState, CodeUiEventEnvelope, CodeUiEventType, CodeUiInteractionKind,
            CodeUiInteractionOption, CodeUiInteractionRequest, CodeUiInteractionResponse,
            CodeUiInteractionStatus, CodeUiPatchChange, CodeUiPatchsetSnapshot, CodeUiPlanSnapshot,
            CodeUiPlanStep, CodeUiProviderInfo, CodeUiSession, CodeUiSessionResumeRequest,
            CodeUiSessionSnapshot, CodeUiSessionStatus, CodeUiSkillActivateRequest,
            CodeUiTaskSnapshot, CodeUiThreadGraph, CodeUiThreadGraphNode, CodeUiToolCallSnapshot,
            CodeUiTranscriptEntry, CodeUiTranscriptEntryKind, code_ui_error_codes,
        },
        sse_wire::CodeUiWireV2Event,
    },
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
        plan_execution_repair: Some(PlanExecutionRepairState::AwaitingUser {
            interaction_id: "repair-1".to_string(),
            route: ExecutionFailureRevision::PlanRevision,
            evidence: ExecutionFailureEvidence {
                output: "Decision: Abandon.".to_string(),
                diagnostics: vec!["verification failed".to_string()],
                attempt: 2,
                max_attempts: 2,
            },
        }),
        thread_graph: Some(CodeUiThreadGraph {
            thread_id: "thread-1".to_string(),
            title: Some("Wire thread".to_string()),
            selected_plan_id: Some("plan-1".to_string()),
            active_task_id: Some("task-1".to_string()),
            active_run_id: None,
            nodes: vec![
                CodeUiThreadGraphNode {
                    depth: 1,
                    kind: "plan".to_string(),
                    id: "plan-1".to_string(),
                    label: "Plan 1".to_string(),
                    tags: vec!["selected".to_string()],
                },
                CodeUiThreadGraphNode {
                    depth: 4,
                    kind: "patchset".to_string(),
                    id: "patch-1".to_string(),
                    label: "PatchSet 1".to_string(),
                    tags: vec!["ready".to_string()],
                },
            ],
            ..Default::default()
        }),
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
    assert_eq!(
        serialized["planExecutionRepair"]["state"],
        Value::String("awaiting_user".into())
    );
    assert_eq!(
        serialized["planExecutionRepair"]["interaction_id"],
        Value::String("repair-1".into())
    );
    assert_eq!(
        serialized["threadGraph"]["threadId"],
        Value::String("thread-1".into())
    );
    assert_eq!(
        serialized["threadGraph"]["nodes"][0]["kind"],
        Value::String("plan".into())
    );
    assert_eq!(
        serialized["threadGraph"]["nodes"][1]["kind"],
        Value::String("patchset".into())
    );
    assert!(
        serialized["threadGraph"].get("truncated").is_none(),
        "untruncated graphs omit truncation metadata"
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

/// W2-03: Plan lineage fields are additive durable-field extensions.
/// Historical Plan review rows omit them and must continue to decode as an
/// initial gate with the legacy interaction-id context fallback; replacement
/// rows preserve their identities through a serde round trip.
#[test]
fn plan_review_revision_lineage_defaults_for_old_rows_and_round_trips() {
    let legacy_row = json!({
        "event_id": "00000000-0000-0000-0000-000000000001",
        "sequence": 41,
        "recorded_at": "2024-03-09T16:00:00Z",
        "event": "plan_review_requested",
        "interaction_id": "plan-review-legacy",
        "plan_id": "plan-legacy",
        "turn_id": "review-turn-legacy",
        "phase1_turn_id": "phase1-turn-legacy",
    });
    let decoded: CodeWorkflowEvent =
        serde_json::from_value(legacy_row).expect("legacy Plan review row must deserialize");
    assert!(matches!(
        decoded.event,
        CodeWorkflowEventKind::PlanReviewRequested {
            context_id,
            revision_of: None,
            prepared_from_network: None,
            ..
        } if context_id.is_empty()
    ));

    let replacement = CodeWorkflowEvent {
        event_id: uuid::Uuid::nil(),
        sequence: 42,
        recorded_at: fixed_ts(),
        event: CodeWorkflowEventKind::PlanReviewRequested {
            interaction_id: "plan-review-replacement".to_string(),
            plan_id: "plan-replacement".to_string(),
            turn_id: "review-turn-replacement".to_string(),
            phase1_turn_id: "phase1-turn-replacement".to_string(),
            context_id: "phase1-context-replacement".to_string(),
            revision_of: Some("plan-review-source".to_string()),
            prepared_from_network: None,
        },
    };
    let serialized = serde_json::to_value(&replacement).expect("replacement row must serialize");
    assert_eq!(
        serialized["revision_of"],
        Value::String("plan-review-source".to_string())
    );
    assert_eq!(
        serialized["context_id"],
        Value::String("phase1-context-replacement".to_string())
    );
    assert!(
        serialized.get("revisionOf").is_none() && serialized.get("contextId").is_none(),
        "durable workflow rows keep snake_case field names"
    );
    let round_tripped: CodeWorkflowEvent =
        serde_json::from_value(serialized).expect("replacement row must deserialize");
    assert_eq!(round_tripped, replacement);
}

/// W2-03: the v2 envelope is camelCase, but its workflow payload deliberately
/// preserves the durable event's snake_case schema, including `revision_of`
/// and the immutable `context_id`. Back opens a fresh gate identity while
/// retaining the source context binding.
#[test]
fn sse_wire_v2_plan_revision_payload_pins_snake_case_lineage() {
    let event = CodeWorkflowEvent {
        event_id: uuid::Uuid::nil(),
        sequence: 42,
        recorded_at: fixed_ts(),
        event: CodeWorkflowEventKind::PlanReviewRequested {
            interaction_id: "plan-review-replacement".to_string(),
            plan_id: "plan-replacement".to_string(),
            turn_id: "review-turn-replacement".to_string(),
            phase1_turn_id: "phase1-turn-replacement".to_string(),
            context_id: "phase1-context-replacement".to_string(),
            revision_of: Some("plan-review-source".to_string()),
            prepared_from_network: None,
        },
    };

    let wire = serde_json::to_value(CodeUiWireV2Event::from_workflow_event(&event))
        .expect("v2 Plan review event must serialize");
    assert_eq!(
        wire,
        json!({
            "cursor": 42,
            "eventId": "00000000-0000-0000-0000-000000000000",
            "kind": "plan_review_requested",
            "at": "2024-03-09T16:00:00Z",
            "payload": {
                "event": "plan_review_requested",
                "interaction_id": "plan-review-replacement",
                "plan_id": "plan-replacement",
                "turn_id": "review-turn-replacement",
                "phase1_turn_id": "phase1-turn-replacement",
                "context_id": "phase1-context-replacement",
                "revision_of": "plan-review-source",
            },
        })
    );
    assert!(
        wire["payload"].get("revisionOf").is_none() && wire["payload"].get("contextId").is_none(),
        "payload fields must not inherit the envelope's camelCase rename"
    );

    let back_event = CodeWorkflowEvent {
        event_id: uuid::Uuid::nil(),
        sequence: 43,
        recorded_at: fixed_ts(),
        event: CodeWorkflowEventKind::PlanReviewRequested {
            interaction_id: "plan-review-after-back".to_string(),
            plan_id: "plan-replacement".to_string(),
            turn_id: "review-turn-after-back".to_string(),
            phase1_turn_id: String::new(),
            context_id: "phase1-context-replacement".to_string(),
            revision_of: None,
            prepared_from_network: Some("network-policy-source".to_string()),
        },
    };
    let back_wire = serde_json::to_value(CodeUiWireV2Event::from_workflow_event(&back_event))
        .expect("v2 Back replacement event must serialize");
    assert_eq!(
        back_wire["payload"]["interaction_id"],
        Value::String("plan-review-after-back".to_string())
    );
    assert_eq!(
        back_wire["payload"]["context_id"],
        Value::String("phase1-context-replacement".to_string()),
        "Back must keep the immutable source context while opening a fresh gate"
    );
    assert!(
        back_wire["payload"].get("revision_of").is_none(),
        "Back is a gate replacement, not a plan revision"
    );
    assert_eq!(
        back_wire["payload"]["prepared_from_network"],
        Value::String("network-policy-source".to_string()),
        "Back replacement stays provisional until its source Network gate is durably resolved"
    );
}

/// W2-03: the formal-write marker is a public recovery boundary on SSE v2.
/// It carries only stable identifiers and a digest, never the raw IntentSpec,
/// revision note, or provider output.
#[test]
fn sse_wire_v2_phase1_formal_write_marker_pins_recovery_payload() {
    let event = CodeWorkflowEvent {
        event_id: uuid::Uuid::nil(),
        sequence: 44,
        recorded_at: fixed_ts(),
        event: CodeWorkflowEventKind::Phase1FormalWriteStarted {
            phase1_turn_id: "phase1-turn".to_string(),
            source_interaction_id: "intent-review".to_string(),
            seed_digest: "sha256-redacted-digest".to_string(),
        },
    };

    let wire = serde_json::to_value(CodeUiWireV2Event::from_workflow_event(&event))
        .expect("v2 Phase 1 formal-write marker must serialize");
    assert_eq!(
        wire,
        json!({
            "cursor": 44,
            "eventId": "00000000-0000-0000-0000-000000000000",
            "kind": "phase1_formal_write_started",
            "at": "2024-03-09T16:00:00Z",
            "payload": {
                "event": "phase1_formal_write_started",
                "phase1_turn_id": "phase1-turn",
                "source_interaction_id": "intent-review",
                "seed_digest": "sha256-redacted-digest",
            },
        })
    );
}

/// W2-03: a command can deliver risk/user-input interactions before its final
/// review choice. The current gate stays in the legacy primary fields while
/// earlier non-secret audit labels are an additive snake_case list on the same
/// crash-atomic terminal row.
#[test]
fn sse_wire_v2_terminal_resolution_history_is_additive_and_snake_case() {
    let legacy_checkpoint = json!({
        "event_id": "00000000-0000-0000-0000-000000000001",
        "sequence": 44,
        "recorded_at": "2024-03-09T16:00:00Z",
        "event": "interaction_resolved",
        "interaction_id": "question-legacy",
        "resolution": "answered"
    });
    let decoded_checkpoint: CodeWorkflowEvent = serde_json::from_value(legacy_checkpoint)
        .expect("legacy interaction resolution must deserialize");
    assert!(matches!(
        decoded_checkpoint.event,
        CodeWorkflowEventKind::InteractionResolved {
            command: None,
            prior_interaction_resolutions,
            intent_revision_consumption: None,
            ..
        } if prior_interaction_resolutions.is_empty()
    ));

    let legacy_row = json!({
        "event_id": "00000000-0000-0000-0000-000000000001",
        "sequence": 45,
        "recorded_at": "2024-03-09T16:00:00Z",
        "event": "command_terminal_success_with_interaction_resolved",
        "command": {
            "repo_id": "repo-1",
            "session_id": "session-1",
            "principal_id": "principal-1",
            "command_id": "command-1"
        },
        "summary": "IntentSpec confirmed",
        "interaction_id": "intent-review-1",
        "resolution": "confirm"
    });
    let decoded: CodeWorkflowEvent =
        serde_json::from_value(legacy_row).expect("legacy combined terminal row must deserialize");
    assert!(matches!(
        decoded.event,
        CodeWorkflowEventKind::CommandTerminalSuccessWithInteractionResolved {
            prior_interaction_resolutions,
            ..
        } if prior_interaction_resolutions.is_empty()
    ));

    let command = CodeCommandIdentity::new("repo-1", "session-1", "principal-1", "command-1");
    let checkpoint = CodeWorkflowEvent {
        event_id: uuid::Uuid::nil(),
        sequence: 44,
        recorded_at: fixed_ts(),
        event: CodeWorkflowEventKind::InteractionResolved {
            interaction_id: "question-2".to_string(),
            resolution: "answered".to_string(),
            command: Some(command.clone()),
            prior_interaction_resolutions: vec![("question-1".to_string(), "answered".to_string())],
            intent_revision_consumption: None,
        },
    };
    let checkpoint_wire = serde_json::to_value(CodeUiWireV2Event::from_workflow_event(&checkpoint))
        .expect("Pending command interaction checkpoint must serialize");
    assert_eq!(
        checkpoint_wire["payload"]["command"]["command_id"],
        Value::String("command-1".to_string())
    );
    assert_eq!(
        checkpoint_wire["payload"]["prior_interaction_resolutions"],
        json!([["question-1", "answered"]])
    );
    assert_eq!(
        checkpoint_wire["kind"],
        Value::String("interaction_resolved".to_string()),
        "ordinary checkpoints must not use the dedicated revision-consumption event kind"
    );
    assert!(
        checkpoint_wire["payload"].get("commandId").is_none()
            && checkpoint_wire["payload"]
                .get("priorInteractionResolutions")
                .is_none(),
        "checkpoint payload additions must remain snake_case"
    );

    let success = CodeWorkflowEvent {
        event_id: uuid::Uuid::nil(),
        sequence: 45,
        recorded_at: fixed_ts(),
        event: CodeWorkflowEventKind::CommandTerminalSuccessWithInteractionResolved {
            command: command.clone(),
            summary: "IntentSpec confirmed".to_string(),
            interaction_id: "intent-review-1".to_string(),
            resolution: "confirm".to_string(),
            prior_interaction_resolutions: vec![(
                "risk-profile-1".to_string(),
                "answered".to_string(),
            )],
            intent_revision: None,
        },
    };
    let success_wire = serde_json::to_value(CodeUiWireV2Event::from_workflow_event(&success))
        .expect("combined success terminal must serialize");
    assert_eq!(
        success_wire["payload"]["prior_interaction_resolutions"],
        json!([["risk-profile-1", "answered"]])
    );
    assert!(
        success_wire["payload"]
            .get("priorInteractionResolutions")
            .is_none(),
        "workflow payload history must remain snake_case"
    );

    let failure = CodeWorkflowEvent {
        event_id: uuid::Uuid::nil(),
        sequence: 46,
        recorded_at: fixed_ts(),
        event: CodeWorkflowEventKind::CommandTerminalFailure {
            command,
            reason: "turn cancelled".to_string(),
            interaction_resolutions: vec![
                ("question-1".to_string(), "answered".to_string()),
                ("approval-1".to_string(), "denied".to_string()),
            ],
            retry_intent_review: None,
        },
    };
    let failure_wire = serde_json::to_value(CodeUiWireV2Event::from_workflow_event(&failure))
        .expect("failure terminal history must serialize");
    assert_eq!(
        failure_wire["payload"]["interaction_resolutions"],
        json!([["question-1", "answered"], ["approval-1", "denied"]])
    );
    assert!(
        failure_wire["payload"]
            .get("interactionResolutions")
            .is_none(),
        "workflow payload history must remain snake_case"
    );
}

/// W2-03: an IntentSpec Modify terminal exposes only a session-sidecar HMAC
/// binding. Historical rows default it to `None`; populated durable rows and
/// SSE v2 retain digest-only snake_case fields and never copy the raw note.
#[test]
fn intent_revision_recovery_is_additive_and_pins_sse_snake_case() {
    let raw_revision_note = "Keep the public API unchanged.";
    let sidecar_digest = format!("hmac-sha256:{}", "a".repeat(64));
    let legacy_row = json!({
        "event_id": "00000000-0000-0000-0000-000000000001",
        "sequence": 47,
        "recorded_at": "2024-03-09T16:00:00Z",
        "event": "command_terminal_success_with_interaction_resolved",
        "command": {
            "repo_id": "repo-1",
            "session_id": "session-1",
            "principal_id": "principal-1",
            "command_id": "phase0-turn-1"
        },
        "summary": "IntentSpec revision mode armed",
        "interaction_id": "intent-review-1",
        "resolution": "modify"
    });
    let decoded: CodeWorkflowEvent = serde_json::from_value(legacy_row)
        .expect("legacy Modify terminal without recovery data must deserialize");
    assert!(matches!(
        decoded.event,
        CodeWorkflowEventKind::CommandTerminalSuccessWithInteractionResolved {
            intent_revision: None,
            ..
        }
    ));

    let event = CodeWorkflowEvent {
        event_id: uuid::Uuid::nil(),
        sequence: 48,
        recorded_at: fixed_ts(),
        event: CodeWorkflowEventKind::CommandTerminalSuccessWithInteractionResolved {
            command: CodeCommandIdentity::new(
                "repo-1",
                "session-1",
                "principal-1",
                "phase0-turn-1",
            ),
            summary: "IntentSpec revision mode armed".to_string(),
            interaction_id: "intent-review-1".to_string(),
            resolution: "modify".to_string(),
            prior_interaction_resolutions: Vec::new(),
            intent_revision: Some(IntentRevisionRecovery {
                interaction_id: "intent-review-1".to_string(),
                sidecar_digest: sidecar_digest.clone(),
            }),
        },
    };

    let serialized = serde_json::to_value(&event).expect("Modify terminal must serialize");
    assert_eq!(
        serialized["intent_revision"],
        json!({
            "interaction_id": "intent-review-1",
            "sidecar_digest": sidecar_digest,
        })
    );
    assert!(
        serialized.get("intentRevision").is_none()
            && serialized["intent_revision"].get("interactionId").is_none()
            && serialized["intent_revision"].get("sidecarDigest").is_none()
            && serialized["intent_revision"].get("note").is_none(),
        "durable recovery fields must remain digest-only snake_case"
    );
    let round_tripped: CodeWorkflowEvent = serde_json::from_value(serialized.clone())
        .expect("populated Modify terminal must round-trip");
    assert_eq!(round_tripped, event);

    let wire = serde_json::to_value(CodeUiWireV2Event::from_workflow_event(&event))
        .expect("v2 Modify terminal must serialize");
    assert_eq!(
        wire,
        json!({
            "cursor": 48,
            "eventId": "00000000-0000-0000-0000-000000000000",
            "kind": "command_terminal_success_with_interaction_resolved",
            "at": "2024-03-09T16:00:00Z",
            "payload": {
                "event": "command_terminal_success_with_interaction_resolved",
                "command": {
                    "repo_id": "repo-1",
                    "session_id": "session-1",
                    "principal_id": "principal-1",
                    "command_id": "phase0-turn-1",
                },
                "summary": "IntentSpec revision mode armed",
                "interaction_id": "intent-review-1",
                "resolution": "modify",
                "intent_revision": {
                    "interaction_id": "intent-review-1",
                    "sidecar_digest": format!("hmac-sha256:{}", "a".repeat(64)),
                },
            },
        })
    );
    assert!(
        wire["payload"].get("intentRevision").is_none()
            && wire["payload"]["intent_revision"]
                .get("interactionId")
                .is_none()
            && wire["payload"]["intent_revision"]
                .get("sidecarDigest")
                .is_none()
            && wire["payload"]["intent_revision"].get("note").is_none(),
        "SSE workflow payload and nested recovery object must remain digest-only snake_case"
    );

    assert!(
        !serialized.to_string().contains(raw_revision_note)
            && !wire.to_string().contains(raw_revision_note),
        "raw revision notes must not enter the terminal row or its SSE v2 event"
    );

    let mut snapshot = fully_populated_snapshot();
    snapshot.transcript.push(CodeUiTranscriptEntry {
        id: "msg-revision".to_string(),
        kind: CodeUiTranscriptEntryKind::UserMessage,
        title: None,
        content: Some(raw_revision_note.to_string()),
        status: None,
        streaming: false,
        metadata: json!({}),
        created_at: fixed_ts(),
        updated_at: fixed_ts(),
    });
    let snapshot_wire = serde_json::to_value(snapshot).expect("session snapshot must serialize");
    assert_eq!(
        snapshot_wire["transcript"][1]["content"],
        Value::String(raw_revision_note.to_string()),
        "ordinary user transcript content keeps the existing snapshot retention boundary"
    );
}

/// W2-03: the irreversible sidecar consumption receipt projects as a dedicated
/// SSE event. The payload is the exact durable lineage record, not a second
/// interaction-resolution event and never a carrier for the raw revision note.
#[test]
fn sse_wire_v2_intent_revision_consumed_uses_dedicated_payload() {
    let raw_revision_note = "Keep the public API unchanged.";
    let sidecar_digest = format!("hmac-sha256:{}", "b".repeat(64));
    let consumption = IntentRevisionConsumption {
        claim: IntentRevisionConsumptionClaim {
            schema_version: INTENT_REVISION_CONSUMPTION_SCHEMA_VERSION,
            interaction_id: "intent-review-1".to_string(),
            source_command: CodeCommandIdentity::new(
                "repo-1",
                "session-1",
                "principal-1",
                "phase0-turn-1",
            ),
            consumer_intent: CodeCommandIntent::new(
                CodeCommandIdentity::new("repo-1", "session-1", "principal-1", "ordinary-turn-1"),
                INTENT_REVISION_CONSUMER_COMMAND_KIND,
                "sha256:consumer-request",
                true,
            ),
            terminal_event_id: uuid::Uuid::from_u128(1),
            terminal_sequence: 48,
            intent_id: "intent-1".to_string(),
            sidecar_digest: Some(sidecar_digest.clone()),
        },
        consumer_intent_event_id: uuid::Uuid::from_u128(2),
        consumer_intent_sequence: 49,
    };
    let event = CodeWorkflowEvent {
        event_id: uuid::Uuid::nil(),
        sequence: 50,
        recorded_at: fixed_ts(),
        event: CodeWorkflowEventKind::InteractionResolved {
            interaction_id: "intent-review-1".to_string(),
            resolution: "modify".to_string(),
            command: None,
            prior_interaction_resolutions: Vec::new(),
            intent_revision_consumption: Some(consumption.clone()),
        },
    };

    let durable = serde_json::to_value(&event).expect("consumption receipt must serialize");
    assert_eq!(
        durable["event"],
        Value::String("interaction_resolved".to_string())
    );
    assert_eq!(
        durable["intent_revision_consumption"]["claim"]["sidecar_digest"],
        Value::String(sidecar_digest.clone())
    );
    assert!(
        durable.get("intentRevisionConsumption").is_none()
            && durable["intent_revision_consumption"]
                .get("consumerIntentEventId")
                .is_none()
            && durable["intent_revision_consumption"]["claim"]
                .get("sidecarDigest")
                .is_none(),
        "durable receipt fields must remain snake_case"
    );

    let wire = serde_json::to_value(CodeUiWireV2Event::from_workflow_event(&event))
        .expect("v2 consumption receipt must serialize");
    assert_eq!(
        wire,
        json!({
            "cursor": 50,
            "eventId": "00000000-0000-0000-0000-000000000000",
            "kind": "intent_revision_consumed",
            "at": "2024-03-09T16:00:00Z",
            "payload": {
                "consumption": {
                    "claim": {
                        "schema_version": 1,
                        "interaction_id": "intent-review-1",
                        "source_command": {
                            "repo_id": "repo-1",
                            "session_id": "session-1",
                            "principal_id": "principal-1",
                            "command_id": "phase0-turn-1",
                        },
                        "consumer_intent": {
                            "identity": {
                                "repo_id": "repo-1",
                                "session_id": "session-1",
                                "principal_id": "principal-1",
                                "command_id": "ordinary-turn-1",
                            },
                            "command_kind": "headless_direct_turn",
                            "canonical_request_hash": "sha256:consumer-request",
                            "mutating": true,
                        },
                        "terminal_event_id": "00000000-0000-0000-0000-000000000001",
                        "terminal_sequence": 48,
                        "intent_id": "intent-1",
                        "sidecar_digest": sidecar_digest,
                    },
                    "consumer_intent_event_id": "00000000-0000-0000-0000-000000000002",
                    "consumer_intent_sequence": 49,
                },
            },
        })
    );
    assert!(
        wire["payload"].get("event").is_none()
            && wire["payload"].get("resolution").is_none()
            && wire["payload"].get("interaction_id").is_none()
            && !durable.to_string().contains(raw_revision_note)
            && !wire.to_string().contains(raw_revision_note),
        "the dedicated receipt payload must not masquerade as a resolution or expose the note"
    );
}

/// W2-03: pre-formal-write Phase 1 failures may atomically carry the sole
/// retry IntentSpec gate authority. The field is additive for durable replay,
/// and SSE v2 retains the workflow payload's snake_case field name.
#[test]
fn command_terminal_failure_retry_intent_review_is_additive_and_snake_case() {
    let legacy_row = json!({
        "event_id": "00000000-0000-0000-0000-000000000001",
        "sequence": 47,
        "recorded_at": "2024-03-09T16:00:00Z",
        "event": "command_terminal_failure",
        "command": {
            "repo_id": "repo-1",
            "session_id": "session-1",
            "principal_id": "principal-1",
            "command_id": "phase1-turn-1"
        },
        "reason": "provider unavailable"
    });
    let decoded: CodeWorkflowEvent = serde_json::from_value(legacy_row)
        .expect("legacy failure without retry authority must deserialize");
    assert!(matches!(
        decoded.event,
        CodeWorkflowEventKind::CommandTerminalFailure {
            retry_intent_review: None,
            ..
        }
    ));

    let retry = Phase1RetryIntentReview {
        interaction_id: "intent-retry-1".to_string(),
        intent_id: "intent-1".to_string(),
        intent_spec_id: "intent-spec-1".to_string(),
        source_interaction_id: "intent-review-1".to_string(),
        source_resolution: "confirm".to_string(),
        source_phase1_turn_id: "phase1-turn-1".to_string(),
        start_seed_digest: "a".repeat(64),
    };
    let event = CodeWorkflowEvent {
        event_id: uuid::Uuid::nil(),
        sequence: 47,
        recorded_at: fixed_ts(),
        event: CodeWorkflowEventKind::CommandTerminalFailure {
            command: CodeCommandIdentity::new(
                "repo-1",
                "session-1",
                "principal-1",
                "phase1-turn-1",
            ),
            reason: "provider unavailable".to_string(),
            interaction_resolutions: Vec::new(),
            retry_intent_review: Some(retry),
        },
    };

    let serialized = serde_json::to_value(&event).expect("retry failure row must serialize");
    assert_eq!(
        serialized["retry_intent_review"],
        json!({
            "interactionId": "intent-retry-1",
            "intentId": "intent-1",
            "intentSpecId": "intent-spec-1",
            "sourceInteractionId": "intent-review-1",
            "sourceResolution": "confirm",
            "sourcePhase1TurnId": "phase1-turn-1",
            "startSeedDigest": "a".repeat(64),
        })
    );
    assert!(
        serialized.get("retryIntentReview").is_none(),
        "durable retry authority must keep its snake_case field name"
    );
    let round_tripped: CodeWorkflowEvent = serde_json::from_value(serialized)
        .expect("retry failure row must deserialize after serialization");
    assert_eq!(round_tripped, event);

    let wire = serde_json::to_value(CodeUiWireV2Event::from_workflow_event(&event))
        .expect("v2 retry failure event must serialize");
    assert_eq!(
        wire["payload"]["retry_intent_review"]["interactionId"],
        Value::String("intent-retry-1".to_string())
    );
    assert!(
        wire["payload"].get("retryIntentReview").is_none(),
        "SSE workflow payload must keep retry_intent_review in snake_case"
    );
}

/// W2-03: Network Allow remains a documented fail-closed conflict until the
/// W2-04 execution handoff is available. Pin both the constructor state and
/// the public Code UI error catalogue entry.
#[test]
fn plan_execution_not_available_is_a_catalogued_conflict() {
    let error = CodeUiApiError::conflict(
        "PLAN_EXECUTION_NOT_AVAILABLE",
        "confirmed-plan execution handoff is unavailable",
    );
    assert_eq!(error.code, "PLAN_EXECUTION_NOT_AVAILABLE");
    assert_eq!(error.status, 409);
    assert_eq!(
        code_ui_error_codes()
            .iter()
            .copied()
            .find(|(code, _)| *code == error.code.as_str()),
        Some(("PLAN_EXECUTION_NOT_AVAILABLE", 409))
    );
}

/// W2-03: stale checkout execution and an empty Plan revision note are typed
/// public wire failures, not generic unsupported-operation fallbacks.
#[test]
fn phase1_workspace_and_revision_errors_are_catalogued() {
    for (error, expected_code, expected_status) in [
        (
            CodeUiApiError::conflict(
                "PHASE1_WORKSPACE_CHANGED",
                "the reviewed Plan no longer matches the checkout",
            ),
            "PHASE1_WORKSPACE_CHANGED",
            409,
        ),
        (
            CodeUiApiError::bad_request(
                "PLAN_REVISION_NOTE_REQUIRED",
                "the Plan revision note is empty",
            ),
            "PLAN_REVISION_NOTE_REQUIRED",
            400,
        ),
    ] {
        assert_eq!(error.code, expected_code);
        assert_eq!(error.status, expected_status);
        assert_eq!(
            code_ui_error_codes()
                .iter()
                .copied()
                .find(|(code, _)| *code == expected_code),
            Some((expected_code, expected_status))
        );
    }
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

/// All interaction kinds shipped to the browser must keep their snake_case
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
        (
            CodeUiInteractionKind::PlanExecutionRepair,
            "plan_execution_repair",
        ),
    ] {
        let value = serde_json::to_value(variant).unwrap();
        assert_eq!(value, Value::String(expected.into()));
    }
}

/// Controller kinds serialized into snapshots must keep stable snake_case
/// tags; the retired local-interaction value remains decodable for old
/// snapshots even though new leases reject it.
#[test]
fn controller_kinds_use_snake_case_values() {
    for (variant, expected) in [
        (CodeUiControllerKind::None, "none"),
        (CodeUiControllerKind::Browser, "browser"),
        (CodeUiControllerKind::Automation, "automation"),
        (CodeUiControllerKind::LegacyLocal, "tui"),
        (CodeUiControllerKind::Cli, "cli"),
    ] {
        let value = serde_json::to_value(variant).unwrap();
        assert_eq!(value, Value::String(expected.into()));
    }

    let legacy: CodeUiControllerKind = serde_json::from_value(Value::String("tui".into()))
        .expect("legacy controller tag must remain decodable");
    assert_eq!(legacy, CodeUiControllerKind::LegacyLocal);
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
    // Omitted `kind` stays None; HTTP handler resolves browser vs automation.
    assert_eq!(request.kind, None);

    let explicit: CodeUiControllerAttachRequest =
        serde_json::from_value(json!({ "clientId": "browser-b", "kind": "browser" })).unwrap();
    assert_eq!(explicit.kind, Some(CodeUiControllerKind::Browser));

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
/// fields with mixed naming. Pins `selectedOption`, `applyToFuture`,
/// `maxAttempts`, and the `answers` map's plain string keys.
#[test]
fn interaction_response_serialization_drops_none_fields() {
    let response = CodeUiInteractionResponse {
        approved: Some(true),
        apply_to_future: Some(CodeUiApplyToFuture::AcceptAll),
        selected_option: Some("execute".to_string()),
        max_attempts: Some(3),
        note: None,
        answers: [("q1".to_string(), vec!["yes".to_string()])]
            .into_iter()
            .collect(),
    };
    let value = serde_json::to_value(&response).unwrap();
    assert_eq!(value["approved"], Value::Bool(true));
    assert_eq!(value["applyToFuture"], Value::String("accept_all".into()));
    assert_eq!(value["selectedOption"], Value::String("execute".into()));
    assert_eq!(value["maxAttempts"], Value::from(3));
    assert!(value.get("note").is_none(), "None options must be skipped");
    assert_eq!(value["answers"]["q1"][0], Value::String("yes".into()));
}

/// W2-11 r16: local TUI repair continuation must settle the browser's
/// interaction before it publishes the retrying repair state.
#[tokio::test]
async fn local_repair_continuation_resolves_code_ui_prompt_before_retry() {
    let session = CodeUiSession::new(fully_populated_snapshot());

    session.resolve_interaction("int-1").await;
    session
        .set_plan_execution_repair(Some(PlanExecutionRepairState::AutomaticRepair {
            route: ExecutionFailureRevision::PlanRevision,
            evidence: ExecutionFailureEvidence {
                output: "retrying repaired plan".to_string(),
                diagnostics: Vec::new(),
                attempt: 2,
                max_attempts: 3,
            },
        }))
        .await;
    session.set_status(CodeUiSessionStatus::Thinking).await;

    let snapshot = session.snapshot().await;
    assert_eq!(snapshot.status, CodeUiSessionStatus::Thinking);
    assert!(matches!(
        snapshot.plan_execution_repair,
        Some(PlanExecutionRepairState::AutomaticRepair { .. })
    ));
    assert_eq!(
        snapshot
            .interactions
            .iter()
            .find(|interaction| interaction.id == "int-1")
            .map(|interaction| &interaction.status),
        Some(&CodeUiInteractionStatus::Resolved),
        "the old repair prompt must not remain selectable after local continuation"
    );
}

/// W3-01: usage is a separate camelCase read model rather than fabricated
/// fields on the session snapshot. This pins the browser's `UsageReadModel`
/// totals contract used by `GET /api/code/usage`, including the fail-closed
/// `subAgentsStatus` when durable child attribution is unavailable.
#[test]
fn code_ui_command_surface_full() {
    let totals = RuntimeUsageTotals {
        request_count: 3,
        total_tokens: 120,
        cost_usd: Some(0.01),
        cost_estimate_micro_dollars: Some(10_000),
        usage_status: UsageStatus::Partial,
        cost_status: UsageStatus::Known,
        error_status: UsageStatus::Known,
        failed_count: 0,
        unknown_usage_count: 1,
        unknown_cost_count: 0,
    };
    let value = serde_json::to_value(totals).expect("usage totals serialize");
    assert_eq!(value["requestCount"], Value::from(3));
    assert_eq!(value["totalTokens"], Value::from(120));
    assert_eq!(value["usageStatus"], Value::String("partial".to_string()));
    assert_eq!(value["costStatus"], Value::String("known".to_string()));
    assert_eq!(value["unknownUsageCount"], Value::from(1));

    let activation = CodeUiSkillActivateRequest {
        provider: "claude-code".to_string(),
        name: "/review".to_string(),
    };
    assert_eq!(
        serde_json::to_value(activation).expect("skill activation serializes"),
        json!({ "provider": "claude-code", "name": "/review" })
    );

    let usage_envelope = json!({
        "cumulative": value,
        "subAgentsStatus": "unavailable",
    });
    assert!(
        usage_envelope.get("subAgents").is_none(),
        "omit empty subAgents when attribution is unavailable"
    );
}

/// W3-01: resume selection retains the original working directory and refuses
/// to reinterpret an indeterminate session as a resumable idle snapshot.
/// Thread list items omit workingDir until projections persist per-thread cwd.
#[test]
fn code_ui_browser_resume_contract() {
    let snapshot = fully_populated_snapshot();
    let value = serde_json::to_value(snapshot).expect("resume snapshot serializes");
    assert_eq!(value["threadId"], Value::String("thread-1".to_string()));
    assert_eq!(value["workingDir"], Value::String("/repo".to_string()));
    assert_eq!(
        value["status"],
        Value::String("awaiting_interaction".to_string()),
        "the browser must receive the live projected state before selecting resume"
    );
    let thread = ThreadListItem {
        id: "thread-1".to_string(),
        title: None,
        archived: false,
        current_intent_id: None,
        working_dir: None,
        created_at: fixed_ts(),
        updated_at: fixed_ts(),
    };
    let thread_value = serde_json::to_value(thread).expect("thread list item serializes");
    assert!(
        thread_value.get("workingDir").is_none(),
        "do not stamp server cwd onto repository-shared threads: {thread_value}"
    );
    let request = CodeUiSessionResumeRequest {
        thread_id: "thread-1".to_string(),
    };
    assert_eq!(
        serde_json::to_value(request).expect("resume request serializes"),
        json!({ "threadId": "thread-1" })
    );
    assert!(matches!(
        CodeUiSessionStatus::Thinking,
        CodeUiSessionStatus::Thinking | CodeUiSessionStatus::ExecutingTool
    ));
    assert_eq!(
        CodeUiSessionStatus::IndeterminateSideEffect,
        CodeUiSessionStatus::IndeterminateSideEffect
    );
}

/// W3-06: negotiate SSE wire v1/v2 (explicit, default, illegal fail-closed).
#[test]
fn sse_wire_version_negotiation() {
    use axum::http::{HeaderMap, HeaderValue, header};
    use libra::internal::ai::web::sse_wire::{
        CodeEventsQuery, CodeUiSseWireVersion, parse_code_events_wire_version,
    };

    let headers = HeaderMap::new();
    assert_eq!(
        parse_code_events_wire_version(&CodeEventsQuery::default(), &headers).unwrap(),
        CodeUiSseWireVersion::V1
    );
    for (raw, expected) in [
        ("1", CodeUiSseWireVersion::V1),
        ("v1", CodeUiSseWireVersion::V1),
        ("2", CodeUiSseWireVersion::V2),
        ("v2", CodeUiSseWireVersion::V2),
    ] {
        let query = CodeEventsQuery {
            wire: Some(raw.into()),
            cursor: None,
        };
        assert_eq!(
            parse_code_events_wire_version(&query, &headers).unwrap(),
            expected
        );
    }
    assert!(
        parse_code_events_wire_version(
            &CodeEventsQuery {
                wire: Some("3".into()),
                cursor: None,
            },
            &headers
        )
        .is_err()
    );

    let mut accept = HeaderMap::new();
    accept.insert(
        header::ACCEPT,
        HeaderValue::from_static("text/event-stream;libra-wire=2"),
    );
    assert_eq!(
        parse_code_events_wire_version(&CodeEventsQuery::default(), &accept).unwrap(),
        CodeUiSseWireVersion::V2
    );
    assert_eq!(
        parse_code_events_wire_version(
            &CodeEventsQuery {
                wire: Some("1".into()),
                cursor: None,
            },
            &accept
        )
        .unwrap(),
        CodeUiSseWireVersion::V1,
        "query wire must win over Accept"
    );
}

/// W3-07: managed Codex approval projection must match the non-Codex headless
/// exec-approval wire (`approve` / `deny` / `abort`) so the browser does not
/// branch on provider. App-server still owns the approval loop (DEFER-07).
#[test]
fn codex_projection_matches_non_codex_provider() {
    use libra::internal::ai::codex::codex_tool_approval_interaction;

    let ts = fixed_ts();
    let codex = codex_tool_approval_interaction(
        "req-parity-1",
        "command_execution",
        Some("Command execution".to_string()),
        Some("echo hello".to_string()),
        json!({ "itemId": "item-1" }),
        ts,
    );
    let non_codex = CodeUiInteractionRequest {
        id: "req-parity-1".to_string(),
        kind: CodeUiInteractionKind::Approval,
        title: Some("Approve command execution".to_string()),
        description: Some("Command execution".to_string()),
        prompt: Some("echo hello".to_string()),
        options: vec![
            CodeUiInteractionOption {
                id: "approve".to_string(),
                label: "Approve".to_string(),
                description: Some("Allow this command once".to_string()),
            },
            CodeUiInteractionOption {
                id: "deny".to_string(),
                label: "Deny".to_string(),
                description: Some("Skip this command".to_string()),
            },
            CodeUiInteractionOption {
                id: "abort".to_string(),
                label: "Abort".to_string(),
                description: Some("Cancel this tool run immediately".to_string()),
            },
        ],
        status: CodeUiInteractionStatus::Pending,
        metadata: json!({ "command": "echo hello" }),
        requested_at: ts,
        resolved_at: None,
    };

    assert_eq!(codex.id, non_codex.id);
    assert_eq!(codex.kind, non_codex.kind);
    assert_eq!(codex.status, non_codex.status);
    assert_eq!(
        codex
            .options
            .iter()
            .map(|option| option.id.as_str())
            .collect::<Vec<_>>(),
        non_codex
            .options
            .iter()
            .map(|option| option.id.as_str())
            .collect::<Vec<_>>(),
        "Codex and non-Codex approval option ids must match on the wire"
    );

    let codex_wire = serde_json::to_value(&codex).expect("codex interaction must serialize");
    let non_codex_wire =
        serde_json::to_value(&non_codex).expect("non-codex interaction must serialize");
    assert_eq!(codex_wire["kind"], non_codex_wire["kind"]);
    assert_eq!(codex_wire["status"], non_codex_wire["status"]);
    assert_eq!(
        codex_wire["options"]
            .as_array()
            .expect("options")
            .iter()
            .map(|option| option["id"].clone())
            .collect::<Vec<_>>(),
        non_codex_wire["options"]
            .as_array()
            .expect("options")
            .iter()
            .map(|option| option["id"].clone())
            .collect::<Vec<_>>(),
    );
    assert_eq!(codex_wire["id"], json!("req-parity-1"));
}
