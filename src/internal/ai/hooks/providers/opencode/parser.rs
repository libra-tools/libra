//! Validate the pinned OpenCode hook contract before lifecycle lowering.
//!
//! Native 2.0.26 `data` payloads are minimized by the plugin. Registry names
//! without a lifecycle disposition are skipped by capture ingress. Known but
//! malformed frames fail closed instead of ending an active turn.

use anyhow::{Result, bail};

use super::events::{
    OPENCODE_IDLE_STATUS, OPENCODE_TERMINAL_INTERRUPTION_REASONS, OpenCodeForwarding,
    OpenCodePayloadPolicy, event_spec,
};
use crate::internal::ai::hooks::lifecycle::{
    LifecycleEvent, SessionHookEnvelope, build_lifecycle_event,
};

pub(super) fn parse_opencode_hook_event(
    hook_event_name: &str,
    envelope: &SessionHookEnvelope,
) -> Result<LifecycleEvent> {
    let Some(spec) = event_spec(hook_event_name) else {
        bail!("unknown OpenCode hook event; update the Libra-managed OpenCode plugin");
    };
    let Some(kind) = spec.kind else {
        bail!("OpenCode event is not a standalone lifecycle observation");
    };
    match spec.payload_policy {
        OpenCodePayloadPolicy::IdleStatus => {
            let status = envelope
                .extra
                .get("status")
                .and_then(|value| value.get("type"))
                .and_then(serde_json::Value::as_str);
            if status != Some(OPENCODE_IDLE_STATUS) {
                bail!(
                    "OpenCode session.status requires status.type=idle; busy or retry cannot end a turn"
                );
            }
        }
        OpenCodePayloadPolicy::ExecutionInterrupted => {
            let reason = envelope
                .extra
                .get("reason")
                .and_then(serde_json::Value::as_str);
            if !reason
                .is_some_and(|reason| OPENCODE_TERMINAL_INTERRUPTION_REASONS.contains(&reason))
            {
                bail!(
                    "OpenCode execution interruption requires a terminal reason; shutdown preserves restart continuity"
                );
            }
        }
        OpenCodePayloadPolicy::UserDelivery => {
            if envelope
                .extra
                .get("role")
                .and_then(serde_json::Value::as_str)
                != Some("user")
                || envelope
                    .extra
                    .get("prompt")
                    .and_then(serde_json::Value::as_str)
                    .is_none()
            {
                bail!(
                    "OpenCode inbox delivery requires a user role and a prompt string; refresh the Libra-managed plugin"
                );
            }
        }
        OpenCodePayloadPolicy::LegacyPrompt => {
            let prompt_is_string = envelope
                .extra
                .get("prompt")
                .is_some_and(serde_json::Value::is_string);
            let role_is_user_or_absent = envelope
                .extra
                .get("role")
                .is_none_or(|role| role.as_str() == Some("user"));
            if !prompt_is_string || !role_is_user_or_absent {
                bail!(
                    "OpenCode message.updated requires a prompt string and accepts only a user role when provided; refresh the Libra-managed plugin"
                );
            }
        }
        OpenCodePayloadPolicy::Identity | OpenCodePayloadPolicy::ToolObservation => {}
        OpenCodePayloadPolicy::NotForwarded => {
            bail!("OpenCode event must be merged by the Libra-managed plugin before forwarding");
        }
    }
    if spec.forwarding == OpenCodeForwarding::DeprecatedAlias {
        tracing::warn!(
            provider = "opencode",
            reason = "legacy_event_alias",
            "deprecated OpenCode hook alias accepted; refresh the Libra-managed plugin"
        );
    }
    Ok(build_lifecycle_event(kind, envelope))
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;
    use crate::internal::ai::hooks::{
        LifecycleEventKind,
        providers::opencode::events::{
            OPENCODE_DEPRECATED_ALIASES, OPENCODE_HOOK_EVENT_SPECS, OPENCODE_LEGACY_EVENT_SPECS,
        },
    };

    fn envelope() -> SessionHookEnvelope {
        serde_json::from_value(json!({
            "hook_event_name": "session.status", "session_id": "ses_synthetic_001",
            "cwd": "/tmp/synthetic-project", "role": "user", "prompt": "",
            "status": {"type": "idle"}, "reason": "user"
        }))
        .expect("synthetic envelope")
    }

    #[test]
    fn every_advertised_event_name_parses() {
        let envelope = envelope();
        for spec in OPENCODE_HOOK_EVENT_SPECS
            .iter()
            .chain(OPENCODE_LEGACY_EVENT_SPECS)
            .chain(OPENCODE_DEPRECATED_ALIASES)
        {
            let result = parse_opencode_hook_event(spec.name, &envelope);
            if let Some(kind) = spec.kind {
                assert_eq!(result.expect("advertised event").kind, kind);
            } else {
                assert!(result.is_err());
            }
        }
    }

    #[test]
    fn interruption_reason_must_be_terminal_and_diagnostics_are_fixed() {
        for reason in ["user", "superseded", "inactivity"] {
            let mut frame = envelope();
            frame.extra.insert("reason".into(), json!(reason));
            assert_eq!(
                parse_opencode_hook_event("session.execution.interrupted", &frame)
                    .unwrap()
                    .kind,
                LifecycleEventKind::TurnEnd
            );
        }
        for reason in [
            None,
            Some(json!(null)),
            Some(json!(42)),
            Some(json!("shutdown")),
            Some(json!("PRIVATE-REASON-CANARY")),
        ] {
            let mut frame = envelope();
            frame.extra.remove("reason");
            if let Some(reason) = reason {
                frame.extra.insert("reason".into(), reason);
            }
            let error =
                parse_opencode_hook_event("session.execution.interrupted", &frame).unwrap_err();
            assert_eq!(
                error.to_string(),
                "OpenCode execution interruption requires a terminal reason; shutdown preserves restart continuity"
            );
        }
    }

    #[test]
    fn parser_rejects_unknown_event() {
        let envelope = envelope();
        for name in [
            "message.part.updated",
            "session.error",
            "UnknownHook",
            "session.inbox.enqueued",
        ] {
            assert!(parse_opencode_hook_event(name, &envelope).is_err());
        }
    }

    #[test]
    fn parser_requires_idle_status_without_reflecting_payload() {
        for status in [
            json!({"type": "busy"}),
            json!({"type": "retry"}),
            json!("idle"),
            json!(null),
            json!({"type": "PRIVATE-STATUS-CANARY"}),
        ] {
            let mut frame = envelope();
            frame.extra.insert("status".into(), status);
            let error = parse_opencode_hook_event("session.status", &frame)
                .expect_err("invalid idle frame");
            assert!(!error.to_string().contains("PRIVATE-STATUS-CANARY"));
        }
        assert_eq!(
            parse_opencode_hook_event("session.status", &envelope())
                .expect("idle")
                .kind,
            LifecycleEventKind::TurnEnd
        );
    }
    #[test]
    fn legacy_prompt_requires_string_but_allows_empty_and_missing_role() {
        const DIAGNOSTIC: &str = "OpenCode message.updated requires a prompt string and accepts only a user role when provided; refresh the Libra-managed plugin";
        for extra in [
            json!({"prompt":""}),
            json!({"prompt":"synthetic", "role":"user"}),
            json!({"prompt":"", "message":"PRIVATE-PROMPT-CANARY"}),
        ] {
            let mut frame = envelope();
            frame.extra = extra.as_object().expect("object").clone();
            let event =
                parse_opencode_hook_event("message.updated", &frame).expect("valid legacy prompt");
            assert_eq!(event.kind, LifecycleEventKind::TurnStart);
            assert_eq!(event.prompt.as_deref(), extra["prompt"].as_str());
        }
        for extra in [
            json!({}),
            json!({"role":"user"}),
            json!({"message":"PRIVATE-PROMPT-CANARY"}),
            json!({"user_prompt":"PRIVATE-PROMPT-CANARY"}),
            json!({"role":"user", "message":"PRIVATE-PROMPT-CANARY"}),
            json!({"prompt":null}),
            json!({"prompt":42}),
            json!({"prompt":false}),
            json!({"prompt":{}}),
            json!({"prompt":[]}),
            json!({"prompt":"", "role":null}),
            json!({"prompt":"", "role":42}),
            json!({"prompt":"", "role":{}}),
            json!({"prompt":"", "role":"assistant"}),
            json!({"prompt":"", "role":"PRIVATE-ROLE-CANARY"}),
        ] {
            let mut frame = envelope();
            frame.extra = extra.as_object().expect("object").clone();
            assert_eq!(
                parse_opencode_hook_event("message.updated", &frame)
                    .expect_err("invalid legacy prompt")
                    .to_string(),
                DIAGNOSTIC
            );
        }
    }

    #[test]
    fn delivery_requires_explicit_user_and_prompt_without_fallback() {
        for extra in [
            json!({"prompt":""}),
            json!({"role":"user"}),
            json!({"role":"user","message":"PRIVATE-CANARY"}),
            json!({"role":"assistant","prompt":""}),
            json!({"role":"user","prompt":null}),
        ] {
            let mut frame = envelope();
            frame.extra = extra.as_object().expect("object").clone();
            assert_eq!(
                parse_opencode_hook_event("session.inbox.delivered", &frame)
                    .expect_err("invalid delivery")
                    .to_string(),
                "OpenCode inbox delivery requires a user role and a prompt string; refresh the Libra-managed plugin"
            );
        }
        let event = parse_opencode_hook_event("session.inbox.delivered", &envelope())
            .expect("empty user prompt valid");
        assert_eq!(event.prompt.as_deref(), Some(""));
    }

    #[test]
    fn flattened_status_and_reason_are_authoritative_over_data() {
        let mut frame = envelope();
        frame.extra.insert(
            "data".into(),
            json!({"status":{"type":"busy"},"reason":"shutdown"}),
        );
        assert_eq!(
            parse_opencode_hook_event("session.status", &frame)
                .expect("root idle")
                .kind,
            LifecycleEventKind::TurnEnd
        );
        assert_eq!(
            parse_opencode_hook_event("session.execution.interrupted", &frame)
                .expect("root user")
                .kind,
            LifecycleEventKind::TurnEnd
        );
        frame.extra.insert("status".into(), json!({"type":"busy"}));
        frame.extra.insert("reason".into(), json!("shutdown"));
        frame.extra.insert(
            "data".into(),
            json!({"status":{"type":"idle"},"reason":"user"}),
        );
        assert!(parse_opencode_hook_event("session.status", &frame).is_err());
        assert!(parse_opencode_hook_event("session.execution.interrupted", &frame).is_err());
    }
}
