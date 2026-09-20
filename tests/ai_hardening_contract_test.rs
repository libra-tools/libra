//! Phase E hardening contract tests.
//!
//! Pin the security/policy contracts that the AI runtime exposes to integrators:
//! - `ToolBoundaryPolicy` decisions for observers vs humans, mutating vs read-only,
//!   network vs local tools, and `list_`/`create_` prefix classification.
//! - `SecretRedactor::redact` removes the four common token shapes used in our log
//!   summaries.
//! - `InMemoryAuditSink` round-trips an `AuditEvent` with redaction applied.
//!
//! **Layer:** L1 — pure unit tests, no I/O, no external services.

use chrono::Utc;
use libra::internal::ai::hardening::{
    AuditEvent, AuditSink, InMemoryAuditSink, PrincipalContext, PrincipalRole, SecretRedactor,
    ToolBoundaryPolicy, ToolOperation, ToolOperationDetails,
};
use uuid::Uuid;

/// Scenario: confirm the default `ToolBoundaryPolicy` rejects two high-risk patterns
/// outright (no approval pathway) — observer principals attempting to mutate state,
/// and any principal (including system) attempting a tool that needs the network.
/// Acts as a regression guard for the policy's most important deny defaults.
#[test]
fn tool_boundary_blocks_observer_mutations_and_network_by_default() {
    let policy = ToolBoundaryPolicy::default_runtime();
    let observer = PrincipalContext {
        principal_id: "browser".to_string(),
        role: PrincipalRole::Observer,
    };

    let mutation = policy.decide(&observer, &ToolOperation::tool("apply_patch", true, false));
    assert!(!mutation.allowed);
    assert!(!mutation.approval_required);

    let network_read = policy.decide(
        &PrincipalContext::system(),
        &ToolOperation::tool("read_file", false, true),
    );
    assert!(!network_read.allowed);
}

/// Scenario: a human Owner running a mutating tool (`shell`) is allowed but flagged
/// `approval_required = true`, forcing the runtime to mediate the call rather than
/// auto-execute. Pins the policy's "trusted but not unconditional" middle tier.
#[test]
fn mutating_tool_requires_runtime_mediated_approval_for_humans() {
    let policy = ToolBoundaryPolicy::default_runtime();
    let human = PrincipalContext {
        principal_id: "alice".to_string(),
        role: PrincipalRole::Owner,
    };

    let decision = policy.decide(&human, &ToolOperation::tool("shell", true, false));

    assert!(decision.allowed);
    assert!(decision.approval_required);
}

/// Scenario: a sub-agent spawn is represented as the mutating `task`
/// operation with structured spawn details. Contributors may request it
/// only through the approval-mediated path; observers are denied before
/// any child session can be created.
#[test]
fn sub_agent_spawn_is_a_mutating_task_boundary_operation() {
    let policy = ToolBoundaryPolicy::default_runtime();
    let operation = ToolOperation::sub_agent_spawn("explore", "inspect src");
    assert_eq!(operation.tool_name, "task");
    assert!(operation.mutates_state);
    assert!(!operation.requires_network);
    assert_eq!(
        operation.details,
        ToolOperationDetails::SubAgentSpawn {
            name: "explore".to_string(),
            prompt_digest: "inspect src".to_string(),
        }
    );

    let contributor = PrincipalContext {
        principal_id: "alice".to_string(),
        role: PrincipalRole::Contributor,
    };
    let contributor_decision = policy.decide(&contributor, &operation);
    assert!(contributor_decision.allowed);
    assert!(contributor_decision.approval_required);

    let observer = PrincipalContext {
        principal_id: "readonly".to_string(),
        role: PrincipalRole::Observer,
    };
    let observer_decision = policy.decide(&observer, &operation);
    assert!(!observer_decision.allowed);
    assert!(!observer_decision.approval_required);
}

/// Scenario: `list_` / `create_` tool prefixes classify into the right policy
/// bucket. `list_decisions` (a read tool) is auto-allowed for system
/// principals; the `create_decision` write tool requires approval for an
/// Owner. Guards the prefix-based classifier that the policy uses to decide
/// approval for unknown tools at runtime.
#[test]
fn list_and_create_tool_prefixes_are_classified() {
    let policy = ToolBoundaryPolicy::default_runtime();

    let list_decisions = policy.decide(
        &PrincipalContext::system(),
        &ToolOperation::tool("list_decisions", false, false),
    );
    assert!(list_decisions.allowed);
    assert!(!list_decisions.approval_required);

    let create_decision = policy.decide(
        &PrincipalContext {
            principal_id: "alice".to_string(),
            role: PrincipalRole::Owner,
        },
        &ToolOperation::tool("create_decision", true, false),
    );
    assert!(create_decision.allowed);
    assert!(create_decision.approval_required);
}

/// Scenario: feed a sample log line containing four common credential shapes
/// (OPENAI api key, bearer token, password, generic token) and confirm all four are
/// replaced with `[REDACTED]`. The `matches("[REDACTED]").count() == 4` assertion
/// catches both under- and over-redaction in one shot.
#[test]
fn secret_redactor_removes_common_token_shapes() {
    let redactor = SecretRedactor::default_runtime();
    let redacted = redactor.redact(
        "OPENAI_API_KEY=sk-live Authorization: Bearer abc123 password: hunter2 token=secret",
    );

    assert!(!redacted.contains("sk-live"));
    assert!(!redacted.contains("abc123"));
    assert!(!redacted.contains("hunter2"));
    assert!(!redacted.contains("secret"));
    assert_eq!(redacted.matches("[REDACTED]").count(), 4);
}

/// Scenario: write a redacted `AuditEvent` into `InMemoryAuditSink`, flush, then read
/// it back. Asserts the event survives round-trip with the policy version stamp
/// (`tool-boundary:v1`) and that the redacted summary still does not leak the
/// pre-redaction "secret" substring. This is the contract integrators rely on for
/// compliance logging.
#[tokio::test]
async fn audit_sink_records_redacted_policy_events() {
    let sink = InMemoryAuditSink::default();
    let policy = ToolBoundaryPolicy::default_runtime();
    let redactor = SecretRedactor::default_runtime();

    sink.append(AuditEvent {
        trace_id: Uuid::new_v4(),
        principal_id: "alice".to_string(),
        action: "tool_call".to_string(),
        policy_version: policy.policy_version().to_string(),
        redacted_summary: redactor.redact("token=secret shell apply_patch"),
        at: Utc::now(),
    })
    .await
    .unwrap();
    sink.flush().await.unwrap();

    let events = sink.events().await;
    assert_eq!(events.len(), 1);
    assert_eq!(events[0].policy_version, "tool-boundary:v1");
    assert!(!events[0].redacted_summary.contains("secret"));
}

// ---------------------------------------------------------------------------
// CEX-00.5: AuditSink semantic helpers (record_decision / record_event)
// ---------------------------------------------------------------------------

mod cex_00_5 {
    use chrono::Utc;
    use libra::internal::ai::{
        hardening::{
            AuditEvent, AuditSink, BoundaryDecision, InMemoryAuditSink, PrincipalContext,
            SecretRedactor, ToolBoundaryPolicy, ToolOperation,
        },
        hooks::lifecycle::{LifecycleEvent, LifecycleEventKind},
    };
    use uuid::Uuid;

    fn redactor() -> SecretRedactor {
        SecretRedactor::default_runtime()
    }

    fn principal() -> PrincipalContext {
        PrincipalContext::system()
    }

    fn operation(name: &str, mutates: bool) -> ToolOperation {
        ToolOperation::tool(name, mutates, false)
    }

    fn lifecycle(kind: LifecycleEventKind, session: &str) -> LifecycleEvent {
        LifecycleEvent {
            kind,
            session_id: session.to_string(),
            session_ref: None,
            prompt: None,
            model: None,
            source: None,
            tool_name: None,
            tool_input: None,
            tool_response: None,
            assistant_message: None,
            timestamp: Utc::now(),
        }
    }

    /// Default `record_decision` impl on `AuditSink` must convert the
    /// (principal, operation, decision) tuple into a `boundary_decision`
    /// audit row with a deterministic redacted summary, then forward to
    /// the existing `append` channel. This pins the CEX-00.5 contract for
    /// pre-existing sinks that want the new helper without overriding it.
    #[tokio::test]
    async fn record_decision_default_impl_emits_boundary_decision_audit_row() {
        let sink = InMemoryAuditSink::default();
        let trace = Uuid::new_v4();
        let principal = principal();
        let policy = ToolBoundaryPolicy::default_runtime();
        let op = operation("apply_patch", true);
        let decision = BoundaryDecision {
            allowed: true,
            approval_required: false,
            reason: "default".to_string(),
        };

        AuditSink::record_decision(
            &sink,
            trace,
            &principal,
            policy.policy_version(),
            &op,
            &decision,
            &redactor(),
        )
        .await
        .expect("record_decision must succeed via default impl");

        let events = sink.events().await;
        assert_eq!(events.len(), 1, "single decision -> single audit row");
        let row = &events[0];
        assert_eq!(row.trace_id, trace);
        assert_eq!(row.principal_id, principal.principal_id);
        assert_eq!(row.action, "boundary_decision");
        assert_eq!(row.policy_version, policy.policy_version());
        assert!(row.redacted_summary.contains("tool=apply_patch"));
        assert!(row.redacted_summary.contains("allowed=true"));
        assert!(row.redacted_summary.contains("approval_required=false"));
    }

    /// CEX-00.5 P1-a fix (round 2 — direct regression guard): a custom
    /// `Event` impl whose `event_summary()` carries a secret must have that
    /// secret stripped by `record_event` before reaching the audit row.
    /// Without the redactor call in the default impl this test fails.
    #[tokio::test]
    async fn record_event_redacts_secrets_from_event_summary() {
        use libra::internal::ai::event::Event;
        use uuid::Uuid;

        struct LeakyEvent;

        impl Event for LeakyEvent {
            fn event_kind(&self) -> &'static str {
                "leaky"
            }
            fn event_id(&self) -> Uuid {
                Uuid::nil()
            }
            fn event_summary(&self) -> String {
                "user said password: hunter2 keep secret".to_string()
            }
        }

        let sink = InMemoryAuditSink::default();
        let policy = ToolBoundaryPolicy::default_runtime();

        AuditSink::record_event(
            &sink,
            Uuid::nil(),
            &principal(),
            policy.policy_version(),
            &LeakyEvent,
            &redactor(),
        )
        .await
        .expect("record_event");

        let row = &sink.events().await[0];
        assert!(
            !row.redacted_summary.contains("hunter2"),
            "redactor must strip password: hunter2 from event_summary"
        );
        assert!(
            row.redacted_summary.contains("[REDACTED]"),
            "expected at least one [REDACTED] marker in the summary"
        );
        assert_eq!(row.action, "event/leaky");
    }

    /// CEX-00.5 P1-a fix: secrets in `decision.reason` or `operation.tool_name`
    /// must be redacted before reaching `AuditEvent.redacted_summary`. Pin
    /// the contract: a `password=` token in the reason should never appear
    /// verbatim in the audit row.
    #[tokio::test]
    async fn record_decision_redacts_secrets_in_reason() {
        let sink = InMemoryAuditSink::default();
        let policy = ToolBoundaryPolicy::default_runtime();
        let decision = BoundaryDecision {
            allowed: false,
            approval_required: false,
            reason: "blocked because password: hunter2 leaked".to_string(),
        };

        AuditSink::record_decision(
            &sink,
            Uuid::nil(),
            &principal(),
            policy.policy_version(),
            &operation("shell", true),
            &decision,
            &redactor(),
        )
        .await
        .expect("record_decision");

        let events = sink.events().await;
        let row = &events[0];
        assert!(
            !row.redacted_summary.contains("hunter2"),
            "redactor must strip password: hunter2 from the summary"
        );
        assert!(row.redacted_summary.contains("[REDACTED]"));
    }

    /// A denied + approval-required decision must serialize all three
    /// fields into the summary so reviewers can read why the operation
    /// stopped.
    #[tokio::test]
    async fn record_decision_summary_carries_denied_and_reason() {
        let sink = InMemoryAuditSink::default();
        let policy = ToolBoundaryPolicy::default_runtime();
        let decision = BoundaryDecision {
            allowed: false,
            approval_required: true,
            reason: "policy".to_string(),
        };

        AuditSink::record_decision(
            &sink,
            Uuid::nil(),
            &principal(),
            policy.policy_version(),
            &operation("shell", true),
            &decision,
            &redactor(),
        )
        .await
        .expect("record_decision");

        let events = sink.events().await;
        assert_eq!(events.len(), 1);
        assert!(events[0].redacted_summary.contains("allowed=false"));
        assert!(
            events[0]
                .redacted_summary
                .contains("approval_required=true")
        );
        assert!(events[0].redacted_summary.contains("reason=policy"));
    }

    /// `record_event` flows a `&dyn Event` through the audit channel and
    /// produces an action of `event/<event_kind>`, using `event_summary()`
    /// as the redacted summary (the redactor still runs at `append` time).
    #[tokio::test]
    async fn record_event_default_impl_uses_event_kind_in_action() {
        let sink = InMemoryAuditSink::default();
        let policy = ToolBoundaryPolicy::default_runtime();
        let event = lifecycle(LifecycleEventKind::TurnStart, "session-A");

        AuditSink::record_event(
            &sink,
            Uuid::nil(),
            &principal(),
            policy.policy_version(),
            &event,
            &redactor(),
        )
        .await
        .expect("record_event");

        let rows = sink.events().await;
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].action, "event/turn_start");
        assert!(rows[0].redacted_summary.contains("kind=turn_start"));
        assert!(rows[0].redacted_summary.contains("session=session-A"));
    }

    /// `event/<kind>` must match the existing `LifecycleEventKind::Display`
    /// strings for every variant — drift here would split log readers.
    #[tokio::test]
    async fn record_event_action_string_is_stable_across_kinds() {
        let sink = InMemoryAuditSink::default();
        let policy = ToolBoundaryPolicy::default_runtime();
        let kinds = [
            (LifecycleEventKind::SessionStart, "event/session_start"),
            (LifecycleEventKind::TurnStart, "event/turn_start"),
            (LifecycleEventKind::ToolUse, "event/tool_use"),
            (LifecycleEventKind::ModelUpdate, "event/model_update"),
            (LifecycleEventKind::Compaction, "event/compaction"),
            (LifecycleEventKind::TurnEnd, "event/turn_end"),
            (LifecycleEventKind::SessionEnd, "event/session_end"),
        ];

        for (i, (kind, expected_action)) in kinds.iter().enumerate() {
            let event = lifecycle(*kind, &format!("session-{i}"));
            AuditSink::record_event(
                &sink,
                Uuid::nil(),
                &principal(),
                policy.policy_version(),
                &event,
                &redactor(),
            )
            .await
            .expect("record_event");
            let rows = sink.events().await;
            assert_eq!(rows.last().unwrap().action, *expected_action);
        }

        let rows = sink.events().await;
        assert_eq!(rows.len(), kinds.len());
    }

    /// Backward-compat regression guard: `append` is still a public,
    /// callable method. CEX-00.5 only added new methods with default impls.
    #[tokio::test]
    async fn append_remains_callable_post_cex_00_5() {
        let sink = InMemoryAuditSink::default();
        let trace = Uuid::new_v4();
        let event = AuditEvent {
            trace_id: trace,
            principal_id: "tester".to_string(),
            action: "custom_action".to_string(),
            policy_version: "v0".to_string(),
            redacted_summary: "manual append".to_string(),
            at: Utc::now(),
        };

        sink.append(event.clone())
            .await
            .expect("append must remain a public API");

        let events = sink.events().await;
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].action, "custom_action");
    }

    /// The default impls reach pre-existing concrete sinks via dyn dispatch.
    /// `Box<dyn AuditSink>` exists in `ToolBoundaryRuntime`, so we pin the
    /// dyn-compatibility of the new methods explicitly.
    #[tokio::test]
    async fn semantic_helpers_work_via_dyn_dispatch() {
        let sink: Box<dyn AuditSink> = Box::new(InMemoryAuditSink::default());
        let policy = ToolBoundaryPolicy::default_runtime();

        sink.record_decision(
            Uuid::nil(),
            &principal(),
            policy.policy_version(),
            &operation("read_file", false),
            &BoundaryDecision {
                allowed: true,
                approval_required: false,
                reason: "ro".to_string(),
            },
            &redactor(),
        )
        .await
        .expect("record_decision via dyn");

        let event = lifecycle(LifecycleEventKind::ToolUse, "session-dyn");
        sink.record_event(
            Uuid::nil(),
            &principal(),
            policy.policy_version(),
            &event,
            &redactor(),
        )
        .await
        .expect("record_event via dyn");

        sink.flush().await.unwrap();
    }
}
