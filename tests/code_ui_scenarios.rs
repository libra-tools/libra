#[cfg(feature = "test-provider")]
mod harness;

#[cfg(feature = "test-provider")]
use std::{path::PathBuf, time::Duration};

#[cfg(feature = "test-provider")]
use anyhow::Result;
#[cfg(feature = "test-provider")]
use harness::{CodeSession, CodeSessionOptions, Scenario};
#[cfg(feature = "test-provider")]
use reqwest::StatusCode;
#[cfg(feature = "test-provider")]
use serde_json::Value;
#[cfg(feature = "test-provider")]
use serial_test::serial;

#[cfg(feature = "test-provider")]
#[test]
#[serial]
fn basic_chat_submit_updates_transcript() -> Result<()> {
    let mut session = CodeSession::spawn(CodeSessionOptions::new("basic", fixture("basic_chat")))?;
    {
        let mut scenario = Scenario::new("basic_chat", &mut session);
        scenario
            .step("attach automation")
            .attach_automation("scenario-basic")?
            .expect_controller_kind("automation")?;
        scenario
            .step("submit direct chat")
            .submit("/chat hello")?
            .expect_transcript_contains("fake assistant: hello from the PTY harness")?
            .expect_status_eq("idle")?;
    }

    session.shutdown()
}

#[cfg(feature = "test-provider")]
#[test]
#[serial]
fn automation_reclaim_returns_control_to_tui() -> Result<()> {
    let mut session =
        CodeSession::spawn(CodeSessionOptions::new("reclaim", fixture("basic_chat")))?;
    session.attach_automation("scenario-reclaim")?;
    session.wait_for_snapshot(Duration::from_secs(10), |snapshot| {
        controller_kind(snapshot) == Some("automation")
    })?;

    session.write_tui_line("/control reclaim")?;
    session.wait_for_snapshot(Duration::from_secs(10), |snapshot| {
        controller_kind(snapshot) == Some("tui")
    })?;

    let (status, body) = session.submit_message_expect_error("/chat hello")?;
    assert!(!status.is_success());
    assert!(matches!(
        error_code(&body),
        Some("INVALID_CONTROLLER_TOKEN" | "CONTROLLER_CONFLICT")
    ));

    session.shutdown()
}

#[cfg(feature = "test-provider")]
#[test]
#[serial]
fn cancel_running_turn_returns_session_to_idle() -> Result<()> {
    let mut session =
        CodeSession::spawn(CodeSessionOptions::new("cancel", fixture("delayed_chat")))?;
    session.attach_automation("scenario-cancel")?;
    session.submit_message("/chat slow")?;
    session.wait_for_snapshot(Duration::from_secs(10), |snapshot| {
        status(snapshot) == Some("thinking")
    })?;

    session.cancel_turn()?;
    session.wait_for_snapshot(Duration::from_secs(10), |snapshot| {
        status(snapshot) == Some("idle")
    })?;

    session.shutdown()
}

#[cfg(feature = "test-provider")]
#[test]
#[serial]
fn oversized_message_is_rejected_before_reaching_tui() -> Result<()> {
    let mut session =
        CodeSession::spawn(CodeSessionOptions::new("oversize", fixture("basic_chat")))?;
    session.attach_automation("scenario-oversize")?;
    let (status, body) = session.submit_large_message(300 * 1024)?;
    assert_eq!(status, StatusCode::PAYLOAD_TOO_LARGE);
    assert_eq!(error_code(&body), Some("PAYLOAD_TOO_LARGE"));
    session.shutdown()
}

#[cfg(feature = "test-provider")]
#[test]
#[serial]
fn unknown_interaction_id_is_rejected_without_state_change() -> Result<()> {
    let mut session = CodeSession::spawn(CodeSessionOptions::new(
        "unknown-interaction",
        fixture("basic_chat"),
    ))?;
    session.attach_automation("scenario-unknown-interaction")?;
    let before = session.snapshot()?;

    let (http_status, body) = session.respond_interaction_expect_error("missing-interaction")?;

    assert_eq!(http_status, StatusCode::CONFLICT);
    assert_eq!(error_code(&body), Some("INTERACTION_NOT_ACTIVE"));
    let after = session.snapshot()?;
    assert_eq!(status(&before), status(&after));
    assert_eq!(controller_kind(&after), Some("automation"));
    session.shutdown()
}

#[cfg(feature = "test-provider")]
#[test]
#[serial]
fn default_control_paths_reject_second_live_instance() -> Result<()> {
    let mut session = CodeSession::spawn(
        CodeSessionOptions::new("multi-instance", fixture("basic_chat"))
            .with_default_control_paths(),
    )?;
    let output = session.run_default_control_conflict()?;
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    let combined = format!("{stdout}\n{stderr}");

    assert!(!output.status.success());
    assert!(combined.contains("CONTROL_INSTANCE_CONFLICT"));
    assert!(combined.contains("baseUrl") || combined.contains("http://127.0.0.1:"));

    session.shutdown()
}

#[cfg(feature = "test-provider")]
#[test]
#[serial]
fn default_control_paths_restart_after_stale_pid_takeover() -> Result<()> {
    let mut first = CodeSession::spawn(
        CodeSessionOptions::new("stale-pid-first", fixture("basic_chat"))
            .with_default_control_paths(),
    )?;
    let repo_dir = first.repo_dir().to_path_buf();
    let token_path = first.token_path().to_path_buf();
    let info_path = first.info_path().to_path_buf();
    let first_token = first.control_token_value().to_string();

    assert!(token_path.exists());
    assert!(info_path.exists());

    first.kill_without_cleanup()?;
    assert!(
        token_path.exists(),
        "SIGKILL fixture should leave stale token file for takeover"
    );
    assert!(
        info_path.exists(),
        "SIGKILL fixture should leave stale control.json for takeover"
    );

    let mut second = CodeSession::spawn(
        CodeSessionOptions::new("stale-pid-second", fixture("basic_chat"))
            .with_default_control_paths()
            .with_existing_repo_dir(repo_dir),
    )?;
    assert_eq!(second.token_path(), token_path.as_path());
    assert_eq!(second.info_path(), info_path.as_path());
    assert_ne!(
        second.control_token_value(),
        first_token,
        "restart should replace the stale process control token"
    );
    let snapshot = second.snapshot()?;
    assert_eq!(snapshot["provider"]["provider"], "fake");

    second.shutdown()
}

/// Browser-controller end-to-end smoke. Spawns `libra code` with
/// `--browser-control loopback`, verifies the loopback browser receives the
/// embedded Web app rather than stale mock content, attaches as a browser (no
/// automation control token), submits a chat through the browser write surface,
/// and confirms the snapshot reflects the browser ownership + transcript turn.
/// Ends with a clean detach.
#[cfg(feature = "test-provider")]
#[test]
#[serial]
fn browser_static_app_loads_and_submit_updates_snapshot() -> Result<()> {
    let mut session = CodeSession::spawn(
        CodeSessionOptions::new("browser-static-submit", fixture("basic_chat"))
            .with_browser_control_loopback(),
    )?;

    let (page_status, page_html) = session.get_web_path("/")?;
    assert!(
        page_status.is_success(),
        "loopback Web app must load, got {page_status}",
    );
    assert!(
        page_html.contains("Libra — Agent Workspace"),
        "loopback Web app should serve the embedded Next.js page",
    );
    for stale_text in ["src/lib/query.ts", "useMutation", "optimistic mutation"] {
        assert!(
            !page_html.contains(stale_text),
            "loopback Web app should not contain stale mock text '{stale_text}'",
        );
    }

    let token = session.attach_browser("scenario-browser-roundtrip")?;
    session.wait_for_snapshot(Duration::from_secs(10), |snapshot| {
        controller_kind(snapshot) == Some("browser")
    })?;

    let (status, body) = session.browser_submit_message(&token, "/chat hello")?;
    assert!(
        status.is_success(),
        "browser submit must succeed, got {status}: {body}",
    );

    session.wait_for_snapshot(Duration::from_secs(10), |snapshot| {
        status_eq(snapshot, "idle")
            && transcript_contains(snapshot, "fake assistant: hello from the PTY harness")
    })?;

    let (detach_status, _) = session.browser_detach(&token, "scenario-browser-roundtrip")?;
    assert!(detach_status.is_success());

    session.shutdown()
}

/// Browser reloads re-attach with the same `clientId`. That path should renew
/// the existing lease and keep the same writer token instead of treating the
/// tab as a conflicting second browser.
#[cfg(feature = "test-provider")]
#[test]
#[serial]
fn browser_same_client_reconnect_renews_existing_lease() -> Result<()> {
    let mut session = CodeSession::spawn(
        CodeSessionOptions::new("browser-reconnect", fixture("basic_chat"))
            .with_browser_control_loopback(),
    )?;

    let first_token = session.attach_browser("scenario-browser-reconnect")?;
    session.wait_for_snapshot(Duration::from_secs(10), |snapshot| {
        controller_kind(snapshot) == Some("browser")
    })?;

    let second_token = session.attach_browser("scenario-browser-reconnect")?;
    assert_eq!(
        first_token, second_token,
        "same-client browser reconnect should renew the existing lease",
    );

    let (status, body) = session.browser_submit_message(&second_token, "/chat hello")?;
    assert!(
        status.is_success(),
        "renewed browser token must stay writable, got {status}: {body}",
    );
    session.wait_for_snapshot(Duration::from_secs(10), |snapshot| {
        status_eq(snapshot, "idle")
            && transcript_contains(snapshot, "fake assistant: hello from the PTY harness")
    })?;

    session.shutdown()
}

/// `--browser-control` defaults to `off` for the harness's TUI fixture, so
/// without `with_browser_control_loopback()` an attach must come back with
/// `BROWSER_CONTROL_DISABLED` and the runtime stays controlled by the TUI.
#[cfg(feature = "test-provider")]
#[test]
#[serial]
fn browser_attach_rejected_when_control_disabled() -> Result<()> {
    let mut session = CodeSession::spawn(CodeSessionOptions::new(
        "browser-disabled",
        fixture("basic_chat"),
    ))?;

    let (http_status, body) = session.attach_browser_expect_error("scenario-browser-disabled")?;
    assert_eq!(http_status, StatusCode::FORBIDDEN);
    assert_eq!(error_code(&body), Some("BROWSER_CONTROL_DISABLED"));

    let snapshot = session.snapshot()?;
    assert_ne!(controller_kind(&snapshot), Some("browser"));

    session.shutdown()
}

/// Once a browser lease expires, the next attempted browser write should
/// reject the stale token and publish the reclaimed TUI controller state.
#[cfg(feature = "test-provider")]
#[test]
#[serial]
fn browser_expired_controller_token_is_rejected_and_releases_snapshot() -> Result<()> {
    let mut session = CodeSession::spawn(
        CodeSessionOptions::new("browser-expired-token", fixture("basic_chat"))
            .with_browser_control_loopback()
            .with_lease_duration_ms(50),
    )?;

    let token = session.attach_browser("scenario-browser-expired-token")?;
    session.wait_for_snapshot(Duration::from_secs(10), |snapshot| {
        controller_kind(snapshot) == Some("browser")
    })?;

    std::thread::sleep(Duration::from_millis(100));
    let (status, body) = session.browser_submit_message(&token, "/chat hello")?;
    assert_eq!(status, StatusCode::CONFLICT);
    assert_eq!(error_code(&body), Some("CONTROLLER_CONFLICT"));

    session.wait_for_snapshot(Duration::from_secs(10), |snapshot| {
        controller_kind(snapshot) == Some("tui")
    })?;

    session.shutdown()
}

/// Browser-side oversized payload must be rejected by the
/// `enforce_code_write_body_limit` middleware before the runtime sees it.
/// Confirms the 256 KiB cap applies uniformly to browser leases (not only
/// automation), so a malicious or buggy browser cannot starve the agent.
#[cfg(feature = "test-provider")]
#[test]
#[serial]
fn browser_oversized_message_returns_payload_too_large() -> Result<()> {
    let mut session = CodeSession::spawn(
        CodeSessionOptions::new("browser-oversize", fixture("basic_chat"))
            .with_browser_control_loopback(),
    )?;

    let token = session.attach_browser("scenario-browser-oversize")?;
    session.wait_for_snapshot(Duration::from_secs(10), |snapshot| {
        controller_kind(snapshot) == Some("browser")
    })?;

    let (status, body) = session.browser_submit_large_message(&token, 300 * 1024)?;
    assert_eq!(status, StatusCode::PAYLOAD_TOO_LARGE);
    assert_eq!(error_code(&body), Some("PAYLOAD_TOO_LARGE"));

    session.shutdown()
}

/// Browser-issued cancel must reach `code_cancel_handler` with only the
/// lease token (no `X-Libra-Control-Token`) and successfully abort an
/// in-flight turn — this is the surface the chat header's "Cancel turn"
/// button drives. The `delayed_chat` fixture gives us a deterministic
/// 10-second window to fire the cancel mid-stream.
#[cfg(feature = "test-provider")]
#[test]
#[serial]
fn browser_cancel_turn_aborts_in_flight_turn_without_automation_token() -> Result<()> {
    let mut session = CodeSession::spawn(
        CodeSessionOptions::new("browser-cancel", fixture("delayed_chat"))
            .with_browser_control_loopback(),
    )?;

    let token = session.attach_browser("scenario-browser-cancel")?;
    session.wait_for_snapshot(Duration::from_secs(10), |snapshot| {
        controller_kind(snapshot) == Some("browser")
    })?;

    let (submit_status, submit_body) = session.browser_submit_message(&token, "/chat slow")?;
    assert!(
        submit_status.is_success(),
        "submit must accept the prompt, got {submit_status}: {submit_body}",
    );

    // Wait for the turn to enter `thinking` so the cancel hits a live turn,
    // not an idle session.
    session.wait_for_snapshot(Duration::from_secs(10), |snapshot| {
        status(snapshot) == Some("thinking")
    })?;

    // Anchor the post-cancel "no resurrection" window to the moment the
    // provider task is observed running. Anchoring earlier (e.g. before
    // submit) would let Axum routing + queuing latency eat into the safety
    // margin on slow CI; the fixture's `delayMs` (10 s) starts ticking
    // when the provider task begins, which is exactly here.
    let provider_started_at = std::time::Instant::now();

    let (cancel_status, cancel_body) = session.browser_cancel_turn(&token)?;
    assert!(
        cancel_status.is_success(),
        "browser cancel must succeed with only the lease token, got {cancel_status}: {cancel_body}",
    );

    // Tighter than the fixture's 10 s response delay so we cannot pass by
    // letting the provider settle naturally — a real cancel has to be the
    // reason the snapshot returned to idle.
    session.wait_for_snapshot(Duration::from_secs(3), |snapshot| {
        status(snapshot) == Some("idle")
    })?;

    // Sleep until past the fixture's natural completion window measured
    // from the moment the provider task started. If cancel only marked the
    // session idle but left the provider task running, the delayed
    // response would land here and the assertion below would catch it.
    let elapsed = provider_started_at.elapsed();
    let provider_delay = Duration::from_millis(10_000);
    let safety_margin = Duration::from_millis(1_500);
    if elapsed < provider_delay + safety_margin {
        std::thread::sleep(provider_delay + safety_margin - elapsed);
    }

    let final_snapshot = session.snapshot()?;
    assert!(
        !transcript_contains(&final_snapshot, "fake assistant: delayed response"),
        "cancel must abort the provider before its delayed response lands; transcript: {final_snapshot}",
    );

    session.shutdown()
}

/// Posting to `/interactions/{id}` for an interaction that is not currently
/// pending must surface `INTERACTION_NOT_ACTIVE` regardless of whether the
/// caller is a browser or an automation client. Mirrors the automation
/// scenario `unknown_interaction_id_is_rejected_without_state_change`.
#[cfg(feature = "test-provider")]
#[test]
#[serial]
fn browser_unknown_interaction_id_is_rejected_without_state_change() -> Result<()> {
    let mut session = CodeSession::spawn(
        CodeSessionOptions::new("browser-unknown-interaction", fixture("basic_chat"))
            .with_browser_control_loopback(),
    )?;

    let token = session.attach_browser("scenario-browser-unknown-interaction")?;
    session.wait_for_snapshot(Duration::from_secs(10), |snapshot| {
        controller_kind(snapshot) == Some("browser")
    })?;
    let before = session.snapshot()?;

    let (http_status, body) = session.browser_respond_interaction(&token, "missing-interaction")?;
    assert_eq!(http_status, StatusCode::CONFLICT);
    assert_eq!(error_code(&body), Some("INTERACTION_NOT_ACTIVE"));

    let after = session.snapshot()?;
    assert_eq!(status(&before), status(&after));
    assert_eq!(controller_kind(&after), Some("browser"));

    session.shutdown()
}

/// Browser write paths must leave an audit trail without exposing the raw
/// browser `clientId`. This covers the browser-only write surface called out
/// in the web improvement plan: interaction responses, message submit, and
/// turn cancel all use the lease token without an automation control token.
#[cfg(feature = "test-provider")]
#[test]
#[serial]
fn browser_write_appends_redacted_control_audit() -> Result<()> {
    let mut session = CodeSession::spawn(
        CodeSessionOptions::new("browser-write-audit", fixture("delayed_chat"))
            .with_browser_control_loopback(),
    )?;

    let token = session.attach_browser("scenario-browser-write token:super-secret-149")?;
    session.wait_for_snapshot(Duration::from_secs(10), |snapshot| {
        controller_kind(snapshot) == Some("browser")
    })?;

    let (interaction_status, interaction_body) =
        session.browser_respond_interaction(&token, "missing-interaction")?;
    assert_eq!(interaction_status, StatusCode::CONFLICT);
    assert_eq!(
        error_code(&interaction_body),
        Some("INTERACTION_NOT_ACTIVE")
    );

    let (submit_status, submit_body) = session.browser_submit_message(&token, "/chat slow")?;
    assert!(
        submit_status.is_success(),
        "browser submit must succeed, got {submit_status}: {submit_body}",
    );
    session.wait_for_snapshot(Duration::from_secs(10), |snapshot| {
        status(snapshot) == Some("thinking")
    })?;

    let (cancel_status, cancel_body) = session.browser_cancel_turn(&token)?;
    assert!(
        cancel_status.is_success(),
        "browser cancel must succeed, got {cancel_status}: {cancel_body}",
    );
    session.wait_for_snapshot(Duration::from_secs(10), |snapshot| {
        status(snapshot) == Some("idle")
    })?;

    let log = session.libra_log_text()?;
    for action in ["interaction.respond", "message.submit", "turn.cancel"] {
        assert!(
            log.contains(action),
            "browser write audit log must contain '{action}'; full log:\n{log}",
        );
    }
    assert!(
        !log.contains("super-secret-149"),
        "browser write audit log leaked the raw client id secret suffix:\n{log}",
    );

    session.shutdown()
}

/// `/control reclaim` from the TUI must clear an active browser lease and
/// flip the controller back to `tui`. Subsequent writes from the browser's
/// (now stale) lease token must be rejected. Browser counterpart of
/// `automation_reclaim_returns_control_to_tui`.
#[cfg(feature = "test-provider")]
#[test]
#[serial]
fn local_tui_reclaim_invalidates_browser_lease() -> Result<()> {
    let mut session = CodeSession::spawn(
        CodeSessionOptions::new("browser-reclaim", fixture("basic_chat"))
            .with_browser_control_loopback(),
    )?;

    let token = session.attach_browser("scenario-browser-reclaim")?;
    session.wait_for_snapshot(Duration::from_secs(10), |snapshot| {
        controller_kind(snapshot) == Some("browser")
    })?;

    session.write_tui_line("/control reclaim")?;
    session.wait_for_snapshot(Duration::from_secs(10), |snapshot| {
        controller_kind(snapshot) == Some("tui")
    })?;

    let (status, body) = session.browser_submit_message(&token, "/chat hello")?;
    assert!(
        !status.is_success(),
        "stale browser lease must be rejected after TUI reclaim, got {status}: {body}",
    );
    assert!(
        matches!(
            error_code(&body),
            Some("INVALID_CONTROLLER_TOKEN" | "CONTROLLER_CONFLICT")
        ),
        "expected INVALID_CONTROLLER_TOKEN or CONTROLLER_CONFLICT, got: {body}",
    );

    session.shutdown()
}

/// Once a browser holds the lease, a second browser attempting to attach
/// with a different `clientId` must trip `CONTROLLER_CONFLICT` instead of
/// kicking the first writer out — the lease must be released or expire
/// first. Mirrors the multi-tab scenario the frontend has to defend against.
#[cfg(feature = "test-provider")]
#[test]
#[serial]
fn second_browser_attach_with_different_client_returns_conflict() -> Result<()> {
    let mut session = CodeSession::spawn(
        CodeSessionOptions::new("browser-conflict", fixture("basic_chat"))
            .with_browser_control_loopback(),
    )?;

    let _first_token = session.attach_browser("scenario-browser-first")?;
    session.wait_for_snapshot(Duration::from_secs(10), |snapshot| {
        controller_kind(snapshot) == Some("browser")
    })?;

    let (http_status, body) = session.attach_browser_expect_error("scenario-browser-second")?;
    assert_eq!(http_status, StatusCode::CONFLICT);
    assert_eq!(error_code(&body), Some("CONTROLLER_CONFLICT"));

    session.shutdown()
}

#[cfg(not(feature = "test-provider"))]
#[test]
fn code_ui_scenarios_require_test_provider_feature() {
    eprintln!("skipping code UI scenarios; enable --features test-provider");
}

/// W0-02 baseline skeleton (cargo filter: `plan_workflow`).
///
/// Pins IntentSpec review + post-plan choice labels owned by the TUI today so
/// later Web harness work can retarget these contracts instead of deleting them.
#[test]
fn plan_workflow_baseline_pins_intent_and_post_plan_choices() {
    use libra::internal::tui::{INTENT_REVIEW_CHOICES, POST_PLAN_CHOICES};

    assert_eq!(
        INTENT_REVIEW_CHOICES,
        &["Confirm Intent", "Modify Intent", "Cancel"]
    );
    assert_eq!(
        POST_PLAN_CHOICES,
        &["Execute Plan", "Modify Plan", "Cancel"]
    );
    assert!(
        !INTENT_REVIEW_CHOICES
            .iter()
            .any(|choice| choice.contains("Execute")),
        "IntentSpec review must stay phase-specific"
    );
}

/// W0-02 baseline skeleton (cargo filter: `plan_review`).
///
/// Pins the network-policy human gate that follows Plan approval.
#[test]
fn plan_review_baseline_pins_network_policy_choices() {
    use libra::internal::tui::NETWORK_POLICY_CHOICES;

    assert_eq!(
        NETWORK_POLICY_CHOICES,
        &["Network: Deny", "Network: Allow", "Back"]
    );
    assert!(
        !NETWORK_POLICY_CHOICES
            .iter()
            .any(|choice| choice.contains("Execute")),
        "network policy must not reuse post-plan Execute labels"
    );
}

/// W0-02 baseline for automatic plan-repair threshold copy (filter: `repair`).
#[test]
fn repair_loop_baseline_threshold_keeps_plan_continue_affordance() {
    use libra::internal::tui::plan_repair_threshold_baseline_message;

    let message = plan_repair_threshold_baseline_message("task failed: missing fixture", 3, 3);
    assert!(message.contains(
        "Automatic plan repair stopped after 3 failed repair attempts (automatic threshold: 3)."
    ));
    assert!(message.contains("/plan continue"));
    assert!(message.contains("task failed: missing fixture"));
}

/// W0-02 baseline for request_user_input wire kind (filter: `user_input`).
#[test]
fn user_input_baseline_interaction_kind_is_request_user_input() {
    // Keep the TUI-owned interaction kind stable for Code UI wire consumers.
    // Full multi-question PTY coverage remains on the approval/interaction path;
    // this pin prevents silent rename during runtime migration.
    let value = serde_json::to_value(
        libra::internal::ai::web::code_ui::CodeUiInteractionKind::RequestUserInput,
    )
    .expect("RequestUserInput must serialize");
    assert_eq!(
        value,
        serde_json::Value::String("request_user_input".into())
    );
}

/// W0-02 goal/task control-surface baseline (filter: `goal_task`).
///
/// Pins the SessionEvent kind tag that `/goal` slash commands and Code Control
/// `goal.*` methods both project. Full Goal state-machine coverage remains in
/// `ai_goal_state_test`; this filter keeps the TUI-facing wire tag discoverable
/// from the plan's `code_ui_scenarios` verification entry.
#[test]
fn goal_task_control_baseline_session_event_kind_tag_is_goal() {
    use chrono::Utc;
    use libra::internal::ai::{
        goal::{
            GoalActor, GoalCriterion, GoalEvent, GoalEventEnvelope, GoalEvidencePolicy, GoalSpec,
        },
        session::jsonl::SessionEvent,
    };
    use uuid::Uuid;

    let goal_id = Uuid::nil();
    let spec = GoalSpec::new(
        goal_id,
        "thread-w0-02",
        "session-w0-02",
        "freeze goal/task control baseline",
        vec![GoalCriterion {
            id: "baseline".into(),
            description: "goal/task control remains addressable".into(),
            required: true,
            verifier_hint: None,
            requires_workspace_change: false,
        }],
        Vec::new(),
        GoalEvidencePolicy::Standard,
        Default::default(),
        Utc::now(),
        GoalActor::User {
            id: Some("w0-02".into()),
        },
    )
    .expect("baseline GoalSpec must construct");
    let event = SessionEvent::Goal(GoalEventEnvelope::new(
        goal_id,
        Utc::now(),
        GoalEvent::Created(spec),
    ));
    let encoded = serde_json::to_value(&event).expect("SessionEvent::Goal must serialize");
    assert_eq!(
        encoded.get("kind").and_then(|v| v.as_str()),
        Some("goal"),
        "goal/task control must keep SessionEvent kind tag `goal`: {encoded}"
    );
}

#[cfg(feature = "test-provider")]
fn fixture(name: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests")
        .join("fixtures")
        .join("code_ui")
        .join(format!("{name}.json"))
}

#[cfg(feature = "test-provider")]
fn status(snapshot: &Value) -> Option<&str> {
    snapshot.get("status").and_then(Value::as_str)
}

#[cfg(feature = "test-provider")]
fn controller_kind(snapshot: &Value) -> Option<&str> {
    snapshot
        .get("controller")
        .and_then(|controller| controller.get("kind"))
        .and_then(Value::as_str)
}

#[cfg(feature = "test-provider")]
fn error_code(body: &Value) -> Option<&str> {
    body.get("error")
        .and_then(|error| error.get("code"))
        .or_else(|| body.get("code"))
        .and_then(Value::as_str)
}

#[cfg(feature = "test-provider")]
fn status_eq(snapshot: &Value, expected: &str) -> bool {
    status(snapshot) == Some(expected)
}

#[cfg(feature = "test-provider")]
fn transcript_contains(snapshot: &Value, needle: &str) -> bool {
    let Some(transcript) = snapshot.get("transcript").and_then(Value::as_array) else {
        return false;
    };
    transcript.iter().any(|entry| {
        entry
            .get("content")
            .and_then(Value::as_str)
            .is_some_and(|content| content.contains(needle))
    })
}
