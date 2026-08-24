#[cfg(feature = "test-provider")]
mod harness;

#[cfg(feature = "test-provider")]
use std::{
    path::{Path, PathBuf},
    time::Duration,
};

#[cfg(feature = "test-provider")]
use anyhow::{Context, Result};
#[cfg(feature = "test-provider")]
use harness::{CodeSession, CodeSessionOptions, Scenario};
#[cfg(feature = "test-provider")]
use reqwest::StatusCode;
#[cfg(feature = "test-provider")]
use serde_json::{Value, json};
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
            .expect_transcript_contains("fake assistant: hello from the Web harness")?
            .expect_status_eq("idle")?;
    }

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
fn oversized_message_is_rejected_before_reaching_runtime() -> Result<()> {
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
            && transcript_contains(snapshot, "fake assistant: hello from the Web harness")
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
            && transcript_contains(snapshot, "fake assistant: hello from the Web harness")
    })?;

    session.shutdown()
}

/// W5-06: the Web launch defaults `--browser-control` to `loopback`, so this
/// scenario pins `off` explicitly — an attach must come back with
/// `BROWSER_CONTROL_DISABLED` and no browser controller may be published.
#[cfg(feature = "test-provider")]
#[test]
#[serial]
fn browser_attach_rejected_when_control_disabled() -> Result<()> {
    let mut session = CodeSession::spawn(
        CodeSessionOptions::new("browser-disabled", fixture("basic_chat"))
            .push_extra_cli_arg("--browser-control")
            .push_extra_cli_arg("off"),
    )?;

    let (http_status, body) = session.attach_browser_expect_error("scenario-browser-disabled")?;
    assert_eq!(http_status, StatusCode::FORBIDDEN);
    assert_eq!(error_code(&body), Some("BROWSER_CONTROL_DISABLED"));

    let snapshot = session.snapshot()?;
    assert_ne!(controller_kind(&snapshot), Some("browser"));

    session.shutdown()
}

/// Once a browser lease expires, the next attempted browser write should
/// reject the stale token and publish the reclaimed controller state.
#[cfg(feature = "test-provider")]
#[test]
#[serial]
fn browser_expired_controller_token_is_rejected_and_releases_snapshot() -> Result<()> {
    let mut session = CodeSession::spawn(
        CodeSessionOptions::new("browser-expired-token", fixture("basic_chat"))
            .with_browser_control_loopback(),
    )?;

    let token = session.attach_browser("scenario-browser-expired-token")?;
    session.wait_for_snapshot(Duration::from_secs(10), |snapshot| {
        controller_kind(snapshot) == Some("browser")
    })?;

    session.expire_browser_controller_lease_for_test(&token)?;
    let (status, body) = session.browser_submit_message(&token, "/chat hello")?;
    assert_eq!(status, StatusCode::CONFLICT);
    assert_eq!(error_code(&body), Some("CONTROLLER_CONFLICT"));

    // Web-only headless starts Unclaimed; expiry returns control to `none`.
    session.wait_for_snapshot(Duration::from_secs(10), |snapshot| {
        controller_kind(snapshot) == Some("none")
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

/// Once a browser holds the lease, a second browser attempting to attach
/// with a different `clientId` must trip `CONTROLLER_CONFLICT` instead of
/// kicking the first writer out — the lease must be released or expire
/// first. Mirrors the multi-tab scenario the frontend has to defend against.
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
/// Pins IntentSpec review + post-plan choice labels owned by the runtime so
/// later Web harness work retains these contracts instead of deleting them.
#[test]
fn plan_workflow_baseline_pins_intent_and_post_plan_choices() {
    use libra::internal::ai::workflow_baseline::{INTENT_REVIEW_CHOICES, POST_PLAN_CHOICES};

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

/// Drive `/plan` through risk profile + `submit_intent_draft` until an
/// `intent_review_choice` interaction is pending, then respond with
/// `selected_option` and wait until the session leaves `awaiting_interaction`.
///
/// W5-06: these scenarios now run against the default headless Web launch —
/// `HeadlessCodeRuntime` routes plain messages through Phase 0 and owns the
/// IntentSpec review gate, so no terminal UI is involved.
#[cfg(feature = "test-provider")]
fn plan_workflow_intent_review_respond(name: &str, selected_option: &str) -> Result<()> {
    let mut session = CodeSession::spawn(
        CodeSessionOptions::new(name, fixture("plan_intent_review")).with_context("dev"),
    )?;
    session.attach_automation(&format!("scenario-{name}"))?;

    // A plain (non-slash) message routes into the Phase 0 plan workflow
    // (`should_route_plain_message_to_plan`), matching the fixture's
    // "You are running /plan mode." rule.
    session.submit_message("Add a Usage section to the README documenting the CLI commands.")?;

    // Phase 0's tool-loop policy requires a risk-profile question before the
    // draft can be submitted; answer it through the same `answers` shape
    // `respond_pending_user_input_from_code_ui` expects.
    session.wait_for_snapshot(Duration::from_secs(10), |snapshot| {
        interaction_status(snapshot, "call_request_user_input_1") == Some("pending")
    })?;
    let (http_status, body) = session.respond_interaction(
        "call_request_user_input_1",
        &json!({ "answers": { "risk_profile": ["Low"] } }),
    )?;
    assert_eq!(
        http_status,
        StatusCode::OK,
        "risk profile answer rejected: {body}"
    );

    // Once `submit_intent_draft` lands, the review must be projectable over
    // the wire as `intent_review_choice` (AC2) rather than staying purely
    // adapter-internal state.
    let snapshot = session.wait_for_snapshot(Duration::from_secs(10), |snapshot| {
        find_interaction_by_kind(snapshot, "intent_review_choice").is_some()
    })?;
    assert_eq!(
        status(&snapshot),
        Some("awaiting_interaction"),
        "IntentSpec review must hold the session in awaiting_interaction: {snapshot}"
    );
    let interaction = find_interaction_by_kind(&snapshot, "intent_review_choice")
        .expect("intent_review_choice interaction must be present");
    let interaction_id = interaction
        .get("id")
        .and_then(Value::as_str)
        .expect("intent_review_choice interaction must carry an id")
        .to_string();
    let option_ids: Vec<&str> = interaction
        .get("options")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(|option| option.get("id").and_then(Value::as_str))
        .collect();
    assert_eq!(
        option_ids,
        vec!["confirm", "modify", "cancel"],
        "intent_review_choice options must stay stable for automation clients"
    );

    let (http_status, body) = session.respond_interaction(
        &interaction_id,
        &json!({ "selectedOption": selected_option }),
    )?;
    assert_eq!(
        http_status,
        StatusCode::OK,
        "{selected_option} should be accepted: {body}"
    );

    // `resolve_interaction` retains the wire item and flips status to
    // `resolved` (it does not delete the entry), so wait for that rather
    // than absence of the interaction.
    session.wait_for_snapshot(Duration::from_secs(10), |snapshot| {
        status(snapshot) != Some("awaiting_interaction")
            && find_interaction_by_kind(snapshot, "intent_review_choice")
                .and_then(|interaction| interaction.get("status"))
                .and_then(Value::as_str)
                == Some("resolved")
    })?;

    session.shutdown()
}

/// W2-02 AC2/AC4 (cargo filter: `plan_workflow`).
///
/// Confirming via `/api/code/interactions/{id}` drains the session out of
/// `awaiting_interaction` and releases the mutation fence (worker-owned).
#[cfg(feature = "test-provider")]
#[test]
#[serial]
fn plan_workflow_intent_review_confirm_transitions_session_past_review() -> Result<()> {
    plan_workflow_intent_review_respond("plan-intent-review-confirm", "confirm")
}

/// W2-02: `modify` takes the `CompletedDiscardQueued` path and clears the
/// review interaction so a follow-up revise prompt can be admitted.
#[cfg(feature = "test-provider")]
#[test]
#[serial]
fn plan_workflow_intent_review_modify_clears_review_interaction() -> Result<()> {
    plan_workflow_intent_review_respond("plan-intent-review-modify", "modify")
}

/// W2-02: `cancel` takes the `CompletedDiscardQueued` path and leaves the
/// session idle without an open IntentSpec review interaction.
#[cfg(feature = "test-provider")]
#[test]
#[serial]
fn plan_workflow_intent_review_cancel_clears_review_interaction() -> Result<()> {
    plan_workflow_intent_review_respond("plan-intent-review-cancel", "cancel")
}

/// W2-02 recovery: leave an unresolved IntentSpec review, `--resume` the
/// session, and confirm the restored gate still blocks progression until
/// resolved through `/api/code/interactions/{id}`.
#[cfg(feature = "test-provider")]
#[test]
#[serial]
fn plan_workflow_intent_review_survives_resume_and_can_be_confirmed() -> Result<()> {
    use std::process::Command;

    let case_name = "plan-intent-review-resume";
    let repo_root = tempfile::Builder::new()
        .prefix(&format!("{case_name}-"))
        .tempdir()
        .context("failed to create IntentSpec review resume tempdir")?;
    let repo_dir = repo_root.path().join("repo");
    std::fs::create_dir_all(&repo_dir).context("failed to create resume repo subdir")?;
    let init = Command::new(env!("CARGO_BIN_EXE_libra"))
        .args(["init", "--vault=false", "--quiet"])
        .arg(&repo_dir)
        .output()
        .context("failed to run libra init for IntentSpec review resume")?;
    if !init.status.success() {
        anyhow::bail!(
            "libra init failed\nstdout: {}\nstderr: {}",
            String::from_utf8_lossy(&init.stdout),
            String::from_utf8_lossy(&init.stderr)
        );
    }

    let session_id = {
        let mut session = CodeSession::spawn(
            CodeSessionOptions::new(format!("{case_name}-spawn"), fixture("plan_intent_review"))
                .with_existing_repo_dir(repo_dir.clone())
                .with_context("dev"),
        )?;
        session.attach_automation(&format!("{case_name}-spawn"))?;
        session
            .submit_message("Add a Usage section to the README documenting the CLI commands.")?;
        session.wait_for_snapshot(Duration::from_secs(10), |snapshot| {
            interaction_status(snapshot, "call_request_user_input_1") == Some("pending")
        })?;
        let (http_status, body) = session.respond_interaction(
            "call_request_user_input_1",
            &json!({ "answers": { "risk_profile": ["Low"] } }),
        )?;
        assert_eq!(
            http_status,
            StatusCode::OK,
            "risk profile answer rejected: {body}"
        );
        let snapshot = session.wait_for_snapshot(Duration::from_secs(10), |snapshot| {
            find_interaction_by_kind(snapshot, "intent_review_choice")
                .and_then(|interaction| interaction.get("status"))
                .and_then(Value::as_str)
                == Some("pending")
        })?;
        let id = snapshot
            .get("sessionId")
            .and_then(Value::as_str)
            .map(str::to_string)
            .ok_or_else(|| anyhow::anyhow!("snapshot missing sessionId: {snapshot}"))?;
        // Hard-kill without resolving the review. Clean shutdown /
        // SIGTERM can cancel the pending dialog and append
        // `InteractionResolved`, which is exactly what resume must not see.
        session.kill_without_cleanup()?;
        id
    };

    let mut resumed = CodeSession::spawn(
        CodeSessionOptions::new(format!("{case_name}-resume"), fixture("plan_intent_review"))
            .with_existing_repo_dir(repo_dir)
            .with_resume_thread(&session_id)
            .with_context("dev"),
    )?;
    resumed.attach_automation(&format!("{case_name}-resume"))?;
    let snapshot = resumed.wait_for_snapshot(Duration::from_secs(10), |snapshot| {
        find_interaction_by_kind(snapshot, "intent_review_choice")
            .and_then(|interaction| interaction.get("status"))
            .and_then(Value::as_str)
            == Some("pending")
    })?;
    assert_eq!(
        status(&snapshot),
        Some("awaiting_interaction"),
        "resumed session must reopen the IntentSpec review gate: {snapshot}"
    );
    let interaction_id = find_interaction_by_kind(&snapshot, "intent_review_choice")
        .and_then(|interaction| interaction.get("id"))
        .and_then(Value::as_str)
        .ok_or_else(|| anyhow::anyhow!("restored intent_review_choice missing id"))?
        .to_string();
    let (http_status, body) =
        resumed.respond_interaction(&interaction_id, &json!({ "selectedOption": "confirm" }))?;
    assert_eq!(
        http_status,
        StatusCode::OK,
        "confirm on restored review should be accepted: {body}"
    );
    resumed.wait_for_snapshot(Duration::from_secs(10), |snapshot| {
        status(snapshot) != Some("awaiting_interaction")
            && find_interaction_by_kind(snapshot, "intent_review_choice")
                .and_then(|interaction| interaction.get("status"))
                .and_then(Value::as_str)
                == Some("resolved")
    })?;
    resumed.shutdown()
}

#[cfg(feature = "test-provider")]
const PLAN_REVIEW_REQUEST: &str = "Add a Usage section to the README documenting the CLI commands.";
#[cfg(feature = "test-provider")]
const PLAN_REVIEW_README: &str = "# Fixture\n\nPlaceholder for plan-review Web process.\n";
#[cfg(feature = "test-provider")]
const PLAN_REVIEW_REVISION_NOTE: &str =
    "Keep the command list concise and add an explicit rollback note.";
#[cfg(feature = "test-provider")]
const PLAN_REVIEW_DRIFTED_README: &str =
    "# Fixture\n\nUser edit made while the Plan gate was pending.\n";
#[cfg(feature = "test-provider")]
const PLAN_REVIEW_REVISED_SUMMARY: &str =
    "Revised Phase 1 plan with concise commands and rollback guidance";

#[cfg(feature = "test-provider")]
fn run_plan_review_libra(repo_dir: &Path, args: &[&str], action: &str) -> Result<()> {
    let output = std::process::Command::new(env!("CARGO_BIN_EXE_libra"))
        .args(args)
        .current_dir(repo_dir)
        .output()
        .with_context(|| format!("failed to run {action}"))?;
    if !output.status.success() {
        anyhow::bail!(
            "{action} failed\nstdout: {}\nstderr: {}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
    }
    Ok(())
}

/// Phase 1 binds every reviewed plan to an immutable checkout commit. Keep the
/// Web-process fixtures realistic by creating a born HEAD before runtime start.
#[cfg(feature = "test-provider")]
fn initialize_plan_review_repo(case_name: &str) -> Result<tempfile::TempDir> {
    let repo_root = tempfile::Builder::new()
        .prefix(&format!("{case_name}-"))
        .tempdir()
        .context("failed to create plan-review repo tempdir")?;
    let repo_dir = repo_root.path().join("repo");
    std::fs::create_dir_all(&repo_dir).context("failed to create plan-review repo subdir")?;
    run_plan_review_libra(
        &repo_dir,
        &["init", "--vault=false", "--quiet"],
        "libra init for plan-review fixture",
    )?;
    run_plan_review_libra(
        &repo_dir,
        &["config", "user.name", "Libra Plan Review Test"],
        "libra config user.name for plan-review fixture",
    )?;
    run_plan_review_libra(
        &repo_dir,
        &["config", "user.email", "plan-review-test@example.com"],
        "libra config user.email for plan-review fixture",
    )?;
    std::fs::write(repo_dir.join("README.md"), PLAN_REVIEW_README)
        .context("failed to seed README.md for plan-review fixture")?;
    run_plan_review_libra(
        &repo_dir,
        &["add", "README.md"],
        "libra add for plan-review fixture",
    )?;
    run_plan_review_libra(
        &repo_dir,
        &["commit", "-m", "plan review fixture base", "--no-verify"],
        "libra commit for plan-review fixture",
    )?;
    Ok(repo_root)
}

/// Drive a real `libra code` process through its HTTP write surface until the
/// initial post-Plan Execute/Modify/Cancel gate is pending.
#[cfg(feature = "test-provider")]
fn drive_to_plan_review_gate(session: &mut CodeSession, client_id: &str) -> Result<Value> {
    session.attach_automation(client_id)?;
    session.submit_message(PLAN_REVIEW_REQUEST)?;
    session.wait_for_snapshot(Duration::from_secs(20), |snapshot| {
        interaction_status(snapshot, "call_request_user_input_1") == Some("pending")
    })?;
    let (http_status, body) = session.respond_interaction(
        "call_request_user_input_1",
        &json!({ "answers": { "risk_profile": ["Low"] } }),
    )?;
    assert_eq!(
        http_status,
        StatusCode::OK,
        "risk profile answer rejected: {body}"
    );

    let snapshot = session.wait_for_snapshot(Duration::from_secs(20), |snapshot| {
        find_interaction_by_kind(snapshot, "intent_review_choice")
            .and_then(|interaction| interaction.get("status"))
            .and_then(Value::as_str)
            == Some("pending")
    })?;
    let intent_id = find_interaction_by_kind(&snapshot, "intent_review_choice")
        .and_then(|interaction| interaction.get("id"))
        .and_then(Value::as_str)
        .ok_or_else(|| anyhow::anyhow!("intent_review_choice missing id: {snapshot}"))?
        .to_string();
    let (http_status, body) =
        session.respond_interaction(&intent_id, &json!({ "selectedOption": "confirm" }))?;
    assert_eq!(
        http_status,
        StatusCode::OK,
        "intent confirm rejected: {body}"
    );

    session.wait_for_snapshot(Duration::from_secs(30), |snapshot| {
        find_post_plan_execute_interaction(snapshot)
            .and_then(|interaction| interaction.get("status"))
            .and_then(Value::as_str)
            == Some("pending")
    })
}

#[cfg(feature = "test-provider")]
fn plan_ids(snapshot: &Value) -> Vec<&str> {
    snapshot
        .get("plans")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(|plan| plan.get("id").and_then(Value::as_str))
        .collect()
}

#[cfg(feature = "test-provider")]
fn assert_plan_review_has_no_execution_side_effects(
    session: &CodeSession,
    snapshot: &Value,
) -> Result<()> {
    assert_eq!(
        session.read_repo_file("README.md")?.as_deref(),
        Some(PLAN_REVIEW_README),
        "Plan review must not modify the requested file before W2-04 execution"
    );
    assert!(
        snapshot
            .get("patchsets")
            .and_then(Value::as_array)
            .is_none_or(Vec::is_empty),
        "Plan review must not project a workspace mutation: {snapshot}"
    );
    assert!(
        snapshot
            .get("toolCalls")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
            .all(|tool| {
                !matches!(
                    tool.get("toolName").and_then(Value::as_str),
                    Some("apply_patch" | "shell" | "blocking_mutation")
                )
            }),
        "Plan review must not invoke a workspace mutation tool: {snapshot}"
    );
    Ok(())
}

/// W2-03: Modify closes the source Plan gate, but no replacement exists until
/// the next plain HTTP message supplies the revision note. That note is
/// consumed once and produces a fresh Plan generation without entering the
/// network or execution paths.
#[cfg(feature = "test-provider")]
#[test]
#[serial]
fn plan_review_modify_next_plain_text_opens_replacement_plan_gate() -> Result<()> {
    let repo_root = initialize_plan_review_repo("plan-review-modify")?;
    let repo_dir = repo_root.path().join("repo");
    let mut session = CodeSession::spawn(
        CodeSessionOptions::new("plan-review-modify", fixture("plan_review"))
            .with_existing_repo_dir(repo_dir)
            .with_context("dev"),
    )?;
    let source_snapshot = drive_to_plan_review_gate(&mut session, "scenario-plan-review-modify")?;
    let source_interaction = find_post_plan_execute_interaction(&source_snapshot)
        .ok_or_else(|| anyhow::anyhow!("initial Plan review gate missing: {source_snapshot}"))?;
    let source_interaction_id = source_interaction
        .get("id")
        .and_then(Value::as_str)
        .ok_or_else(|| anyhow::anyhow!("initial Plan review gate missing id: {source_snapshot}"))?
        .to_string();
    let source_plan_id = source_interaction
        .get("metadata")
        .and_then(|metadata| metadata.get("planId"))
        .and_then(Value::as_str)
        .ok_or_else(|| {
            anyhow::anyhow!("initial Plan review gate missing planId: {source_snapshot}")
        })?
        .to_string();
    let source_plan_ids = plan_ids(&source_snapshot)
        .into_iter()
        .map(str::to_string)
        .collect::<Vec<_>>();

    let (http_status, body) = session.respond_interaction(
        &source_interaction_id,
        &json!({ "selectedOption": "modify" }),
    )?;
    assert_eq!(http_status, StatusCode::OK, "Plan Modify rejected: {body}");
    let before_note = session.wait_for_snapshot(Duration::from_secs(20), |snapshot| {
        status_eq(snapshot, "idle")
            && interaction_status(snapshot, &source_interaction_id) == Some("resolved")
    })?;
    assert!(
        before_note
            .get("interactions")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
            .all(|interaction| {
                interaction.get("kind").and_then(Value::as_str) != Some("post_plan_choice")
                    || interaction.get("status").and_then(Value::as_str) != Some("pending")
            }),
        "Modify must not synthesize a replacement Plan gate before the next plain message: {before_note}"
    );
    assert_eq!(
        plan_ids(&before_note),
        source_plan_ids
            .iter()
            .map(String::as_str)
            .collect::<Vec<_>>(),
        "Modify alone must not project a replacement plan"
    );
    assert!(
        find_network_policy_interaction(&before_note).is_none(),
        "Modify must not enter the network-policy phase: {before_note}"
    );
    assert_plan_review_has_no_execution_side_effects(&session, &before_note)?;

    session.submit_message(PLAN_REVIEW_REVISION_NOTE)?;
    let replacement_snapshot = session.wait_for_snapshot(Duration::from_secs(30), |snapshot| {
        find_post_plan_execute_interaction(snapshot).is_some_and(|interaction| {
            interaction.get("status").and_then(Value::as_str) == Some("pending")
                && interaction.get("id").and_then(Value::as_str)
                    != Some(source_interaction_id.as_str())
        })
    })?;
    assert_eq!(
        interaction_status(&replacement_snapshot, &source_interaction_id),
        Some("resolved"),
        "the source Plan gate must remain terminal after its replacement opens"
    );
    let replacement_interaction = find_post_plan_execute_interaction(&replacement_snapshot)
        .ok_or_else(|| anyhow::anyhow!("replacement Plan review gate missing"))?;
    let replacement_interaction_id = replacement_interaction
        .get("id")
        .and_then(Value::as_str)
        .ok_or_else(|| anyhow::anyhow!("replacement Plan review gate missing id"))?;
    assert_ne!(replacement_interaction_id, source_interaction_id);
    let replacement_plan_id = replacement_interaction
        .get("metadata")
        .and_then(|metadata| metadata.get("planId"))
        .and_then(Value::as_str)
        .ok_or_else(|| anyhow::anyhow!("replacement Plan review gate missing planId"))?;
    assert_ne!(
        replacement_plan_id, source_plan_id,
        "a revision must persist a fresh execution-plan revision"
    );
    let replacement_plan = replacement_snapshot
        .get("plans")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .find(|plan| plan.get("id").and_then(Value::as_str) == Some(replacement_interaction_id))
        .ok_or_else(|| anyhow::anyhow!("replacement gate has no matching plan projection"))?;
    assert_eq!(
        replacement_plan.get("summary").and_then(Value::as_str),
        Some(PLAN_REVIEW_REVISED_SUMMARY),
        "the distinct revision provider response must drive the replacement plan"
    );
    assert_eq!(
        replacement_snapshot
            .get("transcript")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
            .filter(|entry| {
                entry.get("content").and_then(Value::as_str) == Some(PLAN_REVIEW_REVISION_NOTE)
            })
            .count(),
        1,
        "the plain-text revision note must be durably projected exactly once"
    );
    assert!(
        find_network_policy_interaction(&replacement_snapshot).is_none(),
        "Plan revision must not enter the network-policy phase: {replacement_snapshot}"
    );
    assert_plan_review_has_no_execution_side_effects(&session, &replacement_snapshot)?;
    session.shutdown()
}

/// W2-03: an empty message cannot consume Plan Modify authority or fall
/// through to the generic direct-turn error. The HTTP contract is a typed 400,
/// and a later valid note must still produce the replacement Plan.
#[cfg(feature = "test-provider")]
#[test]
#[serial]
fn plan_review_empty_revision_note_is_typed_and_preserves_authority() -> Result<()> {
    let repo_root = initialize_plan_review_repo("plan-review-empty-revision")?;
    let repo_dir = repo_root.path().join("repo");
    let mut session = CodeSession::spawn(
        CodeSessionOptions::new("plan-review-empty-revision", fixture("plan_review"))
            .with_existing_repo_dir(repo_dir)
            .with_context("dev"),
    )?;
    let source_snapshot =
        drive_to_plan_review_gate(&mut session, "scenario-plan-review-empty-revision")?;
    let source_interaction_id = find_post_plan_execute_interaction(&source_snapshot)
        .and_then(|interaction| interaction.get("id"))
        .and_then(Value::as_str)
        .ok_or_else(|| anyhow::anyhow!("initial Plan gate missing: {source_snapshot}"))?
        .to_string();

    let (http_status, body) = session.respond_interaction(
        &source_interaction_id,
        &json!({ "selectedOption": "modify" }),
    )?;
    assert_eq!(http_status, StatusCode::OK, "Plan Modify rejected: {body}");
    session.wait_for_snapshot(Duration::from_secs(20), |snapshot| {
        status_eq(snapshot, "idle")
            && interaction_status(snapshot, &source_interaction_id) == Some("resolved")
    })?;

    let (http_status, body) = session.submit_message_expect_error("   \n\t")?;
    assert_eq!(
        http_status,
        StatusCode::BAD_REQUEST,
        "unexpected body: {body}"
    );
    assert_eq!(error_code(&body), Some("PLAN_REVISION_NOTE_REQUIRED"));
    let after_empty = session.snapshot()?;
    assert_eq!(
        interaction_status(&after_empty, &source_interaction_id),
        Some("resolved"),
        "the source gate must remain resolved while revision authority waits"
    );
    assert!(
        after_empty
            .get("interactions")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
            .all(|interaction| {
                interaction.get("kind").and_then(Value::as_str) != Some("post_plan_choice")
                    || interaction.get("status").and_then(Value::as_str) != Some("pending")
            }),
        "an empty note must not create a replacement Plan: {after_empty}"
    );
    assert_plan_review_has_no_execution_side_effects(&session, &after_empty)?;

    session.submit_message(PLAN_REVIEW_REVISION_NOTE)?;
    let replacement = session.wait_for_snapshot(Duration::from_secs(30), |snapshot| {
        find_post_plan_execute_interaction(snapshot).is_some_and(|interaction| {
            interaction.get("status").and_then(Value::as_str) == Some("pending")
                && interaction.get("id").and_then(Value::as_str)
                    != Some(source_interaction_id.as_str())
        })
    })?;
    assert_eq!(
        find_post_plan_execute_interaction(&replacement)
            .and_then(|interaction| interaction.get("metadata"))
            .and_then(|metadata| metadata.get("workspaceDrifted"))
            .and_then(Value::as_bool),
        Some(false),
        "the retained revision authority must bind a fresh checkout: {replacement}"
    );
    assert_plan_review_has_no_execution_side_effects(&session, &replacement)?;
    session.shutdown()
}

/// W2-03: if repository identity changes after Modify is selected, submitting
/// the note must fail before consumption. Restoring the reviewed repository
/// lets the exact same note retry once and open the replacement Plan without a
/// workspace mutation.
#[cfg(feature = "test-provider")]
#[test]
#[serial]
fn plan_review_repository_replacement_after_modify_keeps_revision_note_retryable() -> Result<()> {
    let case_name = "plan-review-repository-replaced-after-modify";
    let repo_root = initialize_plan_review_repo(case_name)?;
    let repo_dir = repo_root.path().join("repo");
    run_plan_review_libra(
        &repo_dir,
        &["config", "libra.repoid", "reviewed-repository-id"],
        "pin reviewed repository identity",
    )?;
    let mut session = CodeSession::spawn(
        CodeSessionOptions::new(case_name, fixture("plan_review"))
            .with_existing_repo_dir(repo_dir.clone())
            .with_context("dev"),
    )?;
    let source_snapshot = drive_to_plan_review_gate(&mut session, case_name)?;
    let source_interaction_id = find_post_plan_execute_interaction(&source_snapshot)
        .and_then(|interaction| interaction.get("id"))
        .and_then(Value::as_str)
        .ok_or_else(|| anyhow::anyhow!("initial Plan gate missing: {source_snapshot}"))?
        .to_string();

    let (http_status, body) = session.respond_interaction(
        &source_interaction_id,
        &json!({ "selectedOption": "modify" }),
    )?;
    assert_eq!(http_status, StatusCode::OK, "Plan Modify rejected: {body}");
    session.wait_for_snapshot(Duration::from_secs(20), |snapshot| {
        status_eq(snapshot, "idle")
            && interaction_status(snapshot, &source_interaction_id) == Some("resolved")
    })?;

    run_plan_review_libra(
        &repo_dir,
        &["config", "libra.repoid", "replacement-repository-id"],
        "replace repository identity after Plan Modify",
    )?;
    let (http_status, body) = session.submit_message_expect_error(PLAN_REVIEW_REVISION_NOTE)?;
    assert_eq!(http_status, StatusCode::CONFLICT, "unexpected body: {body}");
    assert_eq!(error_code(&body), Some("PHASE1_WORKSPACE_CHANGED"));
    let message = body
        .pointer("/error/message")
        .or_else(|| body.get("message"))
        .and_then(Value::as_str)
        .ok_or_else(|| anyhow::anyhow!("post-Modify repository error missing message: {body}"))?;
    assert!(
        message.contains("not consumed") && message.contains("original repository"),
        "post-Modify refusal must preserve a retryable note: {message}"
    );
    let after_refusal = session.snapshot()?;
    assert!(
        after_refusal
            .get("interactions")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
            .all(|interaction| {
                interaction.get("kind").and_then(Value::as_str) != Some("post_plan_choice")
                    || interaction.get("status").and_then(Value::as_str) != Some("pending")
            }),
        "repository replacement must not create a replacement Plan: {after_refusal}"
    );
    assert_plan_review_has_no_execution_side_effects(&session, &after_refusal)?;

    run_plan_review_libra(
        &repo_dir,
        &["config", "libra.repoid", "reviewed-repository-id"],
        "restore reviewed repository identity",
    )?;
    session.submit_message(PLAN_REVIEW_REVISION_NOTE)?;
    let replacement = session.wait_for_snapshot(Duration::from_secs(30), |snapshot| {
        find_post_plan_execute_interaction(snapshot).is_some_and(|interaction| {
            interaction.get("status").and_then(Value::as_str) == Some("pending")
                && interaction.get("id").and_then(Value::as_str)
                    != Some(source_interaction_id.as_str())
        })
    })?;
    assert_eq!(
        replacement
            .get("transcript")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
            .filter(|entry| {
                entry.get("content").and_then(Value::as_str) == Some(PLAN_REVIEW_REVISION_NOTE)
            })
            .count(),
        1,
        "the refused note must be projected only after its successful retry"
    );
    assert_plan_review_has_no_execution_side_effects(&session, &replacement)?;
    session.shutdown()
}

/// W2-03: ordinary workspace drift is recoverable gate state, not an
/// indeterminate side effect. Resume must retain the Plan authority, block a
/// stale Execute with a typed 409, and let Modify regenerate against the
/// current checkout without mutating the user's edit.
#[cfg(feature = "test-provider")]
#[test]
#[serial]
fn plan_review_workspace_drift_survives_resume_and_modify_rearms_current_checkout() -> Result<()> {
    let case_name = "plan-review-workspace-drift";
    let repo_root = initialize_plan_review_repo(case_name)?;
    let repo_dir = repo_root.path().join("repo");

    let (session_id, source_interaction_id) = {
        let mut session = CodeSession::spawn(
            CodeSessionOptions::new(format!("{case_name}-spawn"), fixture("plan_review"))
                .with_existing_repo_dir(repo_dir.clone())
                .with_context("dev"),
        )?;
        let snapshot = drive_to_plan_review_gate(&mut session, &format!("{case_name}-spawn"))?;
        let session_id = snapshot
            .get("sessionId")
            .and_then(Value::as_str)
            .ok_or_else(|| anyhow::anyhow!("Plan snapshot missing sessionId: {snapshot}"))?
            .to_string();
        let interaction_id = find_post_plan_execute_interaction(&snapshot)
            .and_then(|interaction| interaction.get("id"))
            .and_then(Value::as_str)
            .ok_or_else(|| anyhow::anyhow!("Plan snapshot missing gate id: {snapshot}"))?
            .to_string();
        session.kill_without_cleanup()?;
        (session_id, interaction_id)
    };

    std::fs::write(repo_dir.join("README.md"), PLAN_REVIEW_DRIFTED_README)
        .context("failed to edit README while Plan gate was offline")?;

    let mut resumed = CodeSession::spawn(
        CodeSessionOptions::new(format!("{case_name}-resume"), fixture("plan_review"))
            .with_existing_repo_dir(repo_dir)
            .with_resume_thread(&session_id)
            .with_context("dev"),
    )?;
    resumed.attach_automation(&format!("{case_name}-resume"))?;
    let restored = resumed.wait_for_snapshot(Duration::from_secs(20), |snapshot| {
        find_post_plan_execute_interaction(snapshot).is_some_and(|interaction| {
            interaction.get("status").and_then(Value::as_str) == Some("pending")
                && interaction
                    .get("metadata")
                    .and_then(|metadata| metadata.get("workspaceDrifted"))
                    .and_then(Value::as_bool)
                    == Some(true)
        })
    })?;
    let restored_gate = find_post_plan_execute_interaction(&restored)
        .ok_or_else(|| anyhow::anyhow!("restored Plan gate missing: {restored}"))?;
    assert_eq!(
        restored_gate.get("id").and_then(Value::as_str),
        Some(source_interaction_id.as_str()),
        "resume must retain the exact pending Plan generation"
    );
    assert!(
        restored_gate
            .get("metadata")
            .and_then(|metadata| metadata.get("workspaceWarning"))
            .and_then(Value::as_str)
            .is_some_and(|warning| warning.contains("exact identity and content recheck")),
        "restored gate must explain that Execute uses exact authority: {restored_gate}"
    );

    let (http_status, body) = resumed.respond_interaction(
        &source_interaction_id,
        &json!({ "selectedOption": "execute" }),
    )?;
    assert_eq!(http_status, StatusCode::CONFLICT, "unexpected body: {body}");
    assert_eq!(error_code(&body), Some("PHASE1_WORKSPACE_CHANGED"));
    let after_execute = resumed.snapshot()?;
    assert_eq!(
        interaction_status(&after_execute, &source_interaction_id),
        Some("pending"),
        "stale Execute must preserve the same Plan gate"
    );

    let (http_status, body) = resumed.respond_interaction(
        &source_interaction_id,
        &json!({ "selectedOption": "modify" }),
    )?;
    assert_eq!(http_status, StatusCode::OK, "Plan Modify rejected: {body}");
    resumed.wait_for_snapshot(Duration::from_secs(20), |snapshot| {
        status_eq(snapshot, "idle")
            && interaction_status(snapshot, &source_interaction_id) == Some("resolved")
    })?;
    resumed.submit_message(PLAN_REVIEW_REVISION_NOTE)?;
    let replacement = resumed.wait_for_snapshot(Duration::from_secs(30), |snapshot| {
        find_post_plan_execute_interaction(snapshot).is_some_and(|interaction| {
            interaction.get("status").and_then(Value::as_str) == Some("pending")
                && interaction.get("id").and_then(Value::as_str)
                    != Some(source_interaction_id.as_str())
        })
    })?;
    assert!(
        find_post_plan_execute_interaction(&replacement)
            .and_then(|interaction| interaction.get("metadata"))
            .and_then(|metadata| metadata.get("workspaceDrifted"))
            .and_then(Value::as_bool)
            == Some(false),
        "replacement Plan must bind the current workspace: {replacement}"
    );
    assert_eq!(
        resumed.read_repo_file("README.md")?.as_deref(),
        Some(PLAN_REVIEW_DRIFTED_README),
        "Plan regeneration must preserve the user's workspace edit"
    );
    resumed.shutdown()
}

/// W2-03: the additive metadata token is an advisory resume/pre-write signal,
/// not Execute authority. A timestamp-only change projects a warning, while
/// Execute's exact content recheck may still admit the unchanged Plan into the
/// network-policy gate.
#[cfg(feature = "test-provider")]
#[test]
#[serial]
fn plan_review_metadata_only_drift_warns_but_exact_execute_recheck_succeeds() -> Result<()> {
    let case_name = "plan-review-metadata-only-drift";
    let repo_root = initialize_plan_review_repo(case_name)?;
    let repo_dir = repo_root.path().join("repo");

    let (session_id, source_interaction_id) = {
        let mut session = CodeSession::spawn(
            CodeSessionOptions::new(format!("{case_name}-spawn"), fixture("plan_review"))
                .with_existing_repo_dir(repo_dir.clone())
                .with_context("dev"),
        )?;
        let snapshot = drive_to_plan_review_gate(&mut session, &format!("{case_name}-spawn"))?;
        let session_id = snapshot
            .get("sessionId")
            .and_then(Value::as_str)
            .ok_or_else(|| anyhow::anyhow!("Plan snapshot missing sessionId: {snapshot}"))?
            .to_string();
        let interaction_id = find_post_plan_execute_interaction(&snapshot)
            .and_then(|interaction| interaction.get("id"))
            .and_then(Value::as_str)
            .ok_or_else(|| anyhow::anyhow!("Plan snapshot missing gate id: {snapshot}"))?
            .to_string();
        session.kill_without_cleanup()?;
        (session_id, interaction_id)
    };

    let readme_path = repo_dir.join("README.md");
    let original =
        std::fs::read(&readme_path).context("failed to read README before touching it")?;
    let modified = std::fs::metadata(&readme_path)
        .context("failed to stat README before touching it")?
        .modified()
        .context("README has no modification timestamp")?;
    std::fs::File::options()
        .write(true)
        .open(&readme_path)
        .context("failed to open README for a metadata-only change")?
        .set_times(std::fs::FileTimes::new().set_modified(modified + Duration::from_secs(60)))
        .context("failed to change only the README timestamp")?;
    assert_eq!(
        std::fs::read(&readme_path).context("failed to reread touched README")?,
        original,
        "metadata-only fixture must not change file content"
    );

    let mut resumed = CodeSession::spawn(
        CodeSessionOptions::new(format!("{case_name}-resume"), fixture("plan_review"))
            .with_existing_repo_dir(repo_dir)
            .with_resume_thread(&session_id)
            .with_context("dev"),
    )?;
    resumed.attach_automation(&format!("{case_name}-resume"))?;
    let restored = resumed.wait_for_snapshot(Duration::from_secs(20), |snapshot| {
        find_post_plan_execute_interaction(snapshot).is_some_and(|interaction| {
            interaction.get("status").and_then(Value::as_str) == Some("pending")
                && interaction
                    .get("metadata")
                    .and_then(|metadata| metadata.get("workspaceDrifted"))
                    .and_then(Value::as_bool)
                    == Some(true)
        })
    })?;
    let restored_gate = find_post_plan_execute_interaction(&restored)
        .ok_or_else(|| anyhow::anyhow!("restored Plan gate missing: {restored}"))?;
    assert_eq!(
        restored_gate.get("id").and_then(Value::as_str),
        Some(source_interaction_id.as_str())
    );
    assert!(
        restored_gate
            .get("metadata")
            .and_then(|metadata| metadata.get("workspaceWarning"))
            .and_then(Value::as_str)
            .is_some_and(|warning| warning.contains("exact identity and content recheck")),
        "metadata-only warning must explain Execute's exact authority: {restored_gate}"
    );

    let (http_status, body) = resumed.respond_interaction(
        &source_interaction_id,
        &json!({ "selectedOption": "execute" }),
    )?;
    assert_eq!(
        http_status,
        StatusCode::OK,
        "exact content recheck should admit metadata-only drift: {body}"
    );
    let network_snapshot = resumed.wait_for_snapshot(Duration::from_secs(20), |snapshot| {
        find_network_policy_interaction(snapshot)
            .and_then(|interaction| interaction.get("status"))
            .and_then(Value::as_str)
            == Some("pending")
    })?;
    let network_interaction_id = find_network_policy_interaction(&network_snapshot)
        .and_then(|interaction| interaction.get("id"))
        .and_then(Value::as_str)
        .ok_or_else(|| anyhow::anyhow!("network-policy gate missing id: {network_snapshot}"))?
        .to_string();
    assert_plan_review_has_no_execution_side_effects(&resumed, &network_snapshot)?;

    let (http_status, body) = resumed.respond_interaction(
        &network_interaction_id,
        &json!({ "selectedOption": "network-deny" }),
    )?;
    assert_eq!(http_status, StatusCode::OK, "Network Deny rejected: {body}");
    resumed.wait_for_snapshot(Duration::from_secs(30), |snapshot| {
        status_eq(snapshot, "idle")
            && find_network_policy_interaction(snapshot)
                .and_then(|interaction| interaction.get("status"))
                .and_then(Value::as_str)
                == Some("resolved")
    })?;
    resumed.shutdown()
}

/// W2-03: a HEAD move while the process is offline follows the same recoverable
/// gate policy as an online move. Resume must project the original authority,
/// Execute must fail closed, and an explicit Modify may rebind only because the
/// repository identity is unchanged.
#[cfg(feature = "test-provider")]
#[test]
#[serial]
fn plan_review_head_drift_survives_resume_and_requires_explicit_modify() -> Result<()> {
    let case_name = "plan-review-head-drift";
    let repo_root = initialize_plan_review_repo(case_name)?;
    let repo_dir = repo_root.path().join("repo");

    let (session_id, source_interaction_id) = {
        let mut session = CodeSession::spawn(
            CodeSessionOptions::new(format!("{case_name}-spawn"), fixture("plan_review"))
                .with_existing_repo_dir(repo_dir.clone())
                .with_context("dev"),
        )?;
        let snapshot = drive_to_plan_review_gate(&mut session, &format!("{case_name}-spawn"))?;
        let session_id = snapshot
            .get("sessionId")
            .and_then(Value::as_str)
            .ok_or_else(|| anyhow::anyhow!("Plan snapshot missing sessionId: {snapshot}"))?
            .to_string();
        let interaction_id = find_post_plan_execute_interaction(&snapshot)
            .and_then(|interaction| interaction.get("id"))
            .and_then(Value::as_str)
            .ok_or_else(|| anyhow::anyhow!("Plan snapshot missing gate id: {snapshot}"))?
            .to_string();
        session.kill_without_cleanup()?;
        (session_id, interaction_id)
    };

    std::fs::write(repo_dir.join("README.md"), PLAN_REVIEW_DRIFTED_README)
        .context("failed to edit README before moving HEAD")?;
    run_plan_review_libra(&repo_dir, &["add", "README.md"], "stage HEAD-drift fixture")?;
    run_plan_review_libra(
        &repo_dir,
        &[
            "commit",
            "-m",
            "move HEAD while review is offline",
            "--no-verify",
        ],
        "commit HEAD-drift fixture",
    )?;

    let mut resumed = CodeSession::spawn(
        CodeSessionOptions::new(format!("{case_name}-resume"), fixture("plan_review"))
            .with_existing_repo_dir(repo_dir)
            .with_resume_thread(&session_id)
            .with_context("dev"),
    )?;
    resumed.attach_automation(&format!("{case_name}-resume"))?;
    let restored = resumed.wait_for_snapshot(Duration::from_secs(20), |snapshot| {
        find_post_plan_execute_interaction(snapshot).is_some_and(|interaction| {
            interaction.get("status").and_then(Value::as_str) == Some("pending")
                && interaction
                    .get("metadata")
                    .and_then(|metadata| metadata.get("workspaceDrifted"))
                    .and_then(Value::as_bool)
                    == Some(true)
        })
    })?;
    let restored_gate = find_post_plan_execute_interaction(&restored)
        .ok_or_else(|| anyhow::anyhow!("restored Plan gate missing: {restored}"))?;
    assert_eq!(
        restored_gate.get("id").and_then(Value::as_str),
        Some(source_interaction_id.as_str())
    );
    assert!(
        restored_gate
            .get("metadata")
            .and_then(|metadata| metadata.get("workspaceWarning"))
            .and_then(Value::as_str)
            .is_some_and(|warning| warning.contains("checkout identity changed")),
        "resume must explain the HEAD mismatch: {restored_gate}"
    );

    let (http_status, body) = resumed.respond_interaction(
        &source_interaction_id,
        &json!({ "selectedOption": "execute" }),
    )?;
    assert_eq!(http_status, StatusCode::CONFLICT, "unexpected body: {body}");
    assert_eq!(error_code(&body), Some("PHASE1_WORKSPACE_CHANGED"));
    assert_eq!(
        interaction_status(&resumed.snapshot()?, &source_interaction_id),
        Some("pending")
    );

    let (http_status, body) = resumed.respond_interaction(
        &source_interaction_id,
        &json!({ "selectedOption": "modify" }),
    )?;
    assert_eq!(http_status, StatusCode::OK, "Plan Modify rejected: {body}");
    resumed.wait_for_snapshot(Duration::from_secs(20), |snapshot| {
        status_eq(snapshot, "idle")
            && interaction_status(snapshot, &source_interaction_id) == Some("resolved")
    })?;
    resumed.submit_message(PLAN_REVIEW_REVISION_NOTE)?;
    let replacement = resumed.wait_for_snapshot(Duration::from_secs(30), |snapshot| {
        find_post_plan_execute_interaction(snapshot).is_some_and(|interaction| {
            interaction.get("status").and_then(Value::as_str) == Some("pending")
                && interaction.get("id").and_then(Value::as_str)
                    != Some(source_interaction_id.as_str())
        })
    })?;
    assert_eq!(
        find_post_plan_execute_interaction(&replacement)
            .and_then(|interaction| interaction.get("metadata"))
            .and_then(|metadata| metadata.get("workspaceDrifted"))
            .and_then(Value::as_bool),
        Some(false),
        "Modify must bind the replacement Plan to the new HEAD: {replacement}"
    );
    assert_eq!(
        resumed.read_repo_file("README.md")?.as_deref(),
        Some(PLAN_REVIEW_DRIFTED_README)
    );
    resumed.shutdown()
}

/// W2-03: Plan Modify may rebind a moved HEAD, but it must never reuse an old
/// IntentSpec after the repository identity itself is replaced. The refusal is
/// typed and leaves the original gate pending so the user can Cancel safely.
#[cfg(feature = "test-provider")]
#[test]
#[serial]
fn plan_review_repository_replacement_blocks_modify_and_preserves_gate() -> Result<()> {
    let case_name = "plan-review-repository-replaced";
    let repo_root = initialize_plan_review_repo(case_name)?;
    let repo_dir = repo_root.path().join("repo");
    let mut session = CodeSession::spawn(
        CodeSessionOptions::new(case_name, fixture("plan_review"))
            .with_existing_repo_dir(repo_dir.clone())
            .with_context("dev"),
    )?;
    let snapshot = drive_to_plan_review_gate(&mut session, case_name)?;
    let interaction_id = find_post_plan_execute_interaction(&snapshot)
        .and_then(|interaction| interaction.get("id"))
        .and_then(Value::as_str)
        .ok_or_else(|| anyhow::anyhow!("Plan snapshot missing gate id: {snapshot}"))?
        .to_string();

    run_plan_review_libra(
        &repo_dir,
        &["config", "libra.repoid", "replacement-repository-id"],
        "replace repository identity while Plan gate is pending",
    )?;

    let (http_status, body) =
        session.respond_interaction(&interaction_id, &json!({ "selectedOption": "execute" }))?;
    assert_eq!(http_status, StatusCode::CONFLICT, "unexpected body: {body}");
    assert_eq!(error_code(&body), Some("PHASE1_WORKSPACE_CHANGED"));
    let message = body
        .pointer("/error/message")
        .or_else(|| body.get("message"))
        .and_then(Value::as_str)
        .ok_or_else(|| anyhow::anyhow!("repository-replacement error missing message: {body}"))?;
    assert!(
        message.contains("Cancel") && message.contains("IntentSpec"),
        "repository replacement must give the only valid recovery path: {message}"
    );
    assert!(
        !message.contains("Choose Modify"),
        "repository replacement must not recommend a Modify path that will be refused: {message}"
    );

    let (http_status, body) =
        session.respond_interaction(&interaction_id, &json!({ "selectedOption": "modify" }))?;
    assert_eq!(http_status, StatusCode::CONFLICT, "unexpected body: {body}");
    assert_eq!(error_code(&body), Some("PHASE1_WORKSPACE_CHANGED"));
    assert_eq!(
        interaction_status(&session.snapshot()?, &interaction_id),
        Some("pending"),
        "repository replacement must not consume the Plan gate"
    );
    assert_plan_review_has_no_execution_side_effects(&session, &session.snapshot()?)?;
    session.shutdown()
}

/// W2-04: Network Allow admits confirmed plan execution onto the runtime
/// serialized queue. The network gate is consumed, execution starts, and
/// mutating tools still pass through approval/sandbox/ACL (the fake fixture
/// completes without apply_patch/shell).
#[cfg(feature = "test-provider")]
#[test]
#[serial]
fn plan_review_network_allow_enters_runtime_queue() -> Result<()> {
    let repo_root = initialize_plan_review_repo("plan-review-network-allow")?;
    let repo_dir = repo_root.path().join("repo");
    let mut session = CodeSession::spawn(
        CodeSessionOptions::new("plan-review-network-allow", fixture("plan_review"))
            .with_existing_repo_dir(repo_dir)
            .with_context("dev"),
    )?;
    let plan_snapshot =
        drive_to_plan_review_gate(&mut session, "scenario-plan-review-network-allow")?;
    let plan_interaction_id = find_post_plan_execute_interaction(&plan_snapshot)
        .and_then(|interaction| interaction.get("id"))
        .and_then(Value::as_str)
        .ok_or_else(|| anyhow::anyhow!("post_plan_choice missing id: {plan_snapshot}"))?
        .to_string();
    let (http_status, body) = session.respond_interaction(
        &plan_interaction_id,
        &json!({ "selectedOption": "execute" }),
    )?;
    assert_eq!(http_status, StatusCode::OK, "Plan Execute rejected: {body}");

    let network_snapshot = session.wait_for_snapshot(Duration::from_secs(20), |snapshot| {
        find_network_policy_interaction(snapshot)
            .and_then(|interaction| interaction.get("status"))
            .and_then(Value::as_str)
            == Some("pending")
    })?;
    let network_interaction_id = find_network_policy_interaction(&network_snapshot)
        .and_then(|interaction| interaction.get("id"))
        .and_then(Value::as_str)
        .ok_or_else(|| anyhow::anyhow!("network-policy gate missing id: {network_snapshot}"))?
        .to_string();
    assert_plan_review_has_no_execution_side_effects(&session, &network_snapshot)?;

    let (http_status, body) = session.respond_interaction(
        &network_interaction_id,
        &json!({ "selectedOption": "network-allow" }),
    )?;
    assert_eq!(
        http_status,
        StatusCode::OK,
        "Network Allow should admit confirmed plan execution: {body}"
    );
    assert_ne!(error_code(&body), Some("PLAN_EXECUTION_NOT_AVAILABLE"));

    let after = session.wait_for_snapshot(Duration::from_secs(30), |snapshot| {
        let network_resolved = find_network_policy_interaction(snapshot)
            .and_then(|interaction| interaction.get("status"))
            .and_then(Value::as_str)
            != Some("pending");
        let executing = status(snapshot) == Some("thinking")
            || snapshot
                .get("transcript")
                .and_then(Value::as_array)
                .into_iter()
                .flatten()
                .any(|entry| {
                    entry
                        .get("metadata")
                        .and_then(|metadata| metadata.get("phase"))
                        .and_then(Value::as_str)
                        == Some("plan_execution")
                        || entry.get("title").and_then(Value::as_str) == Some("Plan execution")
                });
        let settled = matches!(
            status(snapshot),
            Some("idle" | "awaiting_interaction" | "error")
        );
        network_resolved && (executing || settled)
    })?;
    assert!(
        find_network_policy_interaction(&after)
            .and_then(|interaction| interaction.get("status"))
            .and_then(Value::as_str)
            != Some("pending"),
        "Network Allow must consume the pending network gate: {after}"
    );
    session.shutdown()
}

/// Historical W2-03 leftover name: the 409 PLAN_EXECUTION_NOT_AVAILABLE
/// boundary was replaced by W2-04 Web confirmed-plan admission.
#[cfg(feature = "test-provider")]
#[test]
#[serial]
fn plan_review_network_allow_returns_conflict_and_preserves_pending_gate() -> Result<()> {
    plan_review_network_allow_enters_runtime_queue()
}

/// Drive the default Web process through IntentSpec confirm → Phase 1
/// `post_plan_choice` → Execute → network-policy gate, then hard-kill and
/// `--resume` so the network gate is restored before Deny.
#[cfg(feature = "test-provider")]
#[test]
#[serial]
fn plan_review_network_policy_survives_web_resume_and_can_be_denied() -> Result<()> {
    let case_name = "plan-review-network-resume";
    let repo_root = initialize_plan_review_repo(case_name)?;
    let repo_dir = repo_root.path().join("repo");

    let session_id = {
        let mut session = CodeSession::spawn(
            CodeSessionOptions::new(format!("{case_name}-spawn"), fixture("plan_review"))
                .with_existing_repo_dir(repo_dir.clone())
                .with_context("dev"),
        )?;
        let snapshot = drive_to_plan_review_gate(&mut session, &format!("{case_name}-spawn"))?;
        let plan_id = find_post_plan_execute_interaction(&snapshot)
            .and_then(|interaction| interaction.get("id"))
            .and_then(Value::as_str)
            .ok_or_else(|| anyhow::anyhow!("post_plan_choice missing id: {snapshot}"))?
            .to_string();
        let (http_status, body) =
            session.respond_interaction(&plan_id, &json!({ "selectedOption": "execute" }))?;
        assert_eq!(http_status, StatusCode::OK, "plan Execute rejected: {body}");

        session.wait_for_snapshot(Duration::from_secs(20), |snapshot| {
            find_network_policy_interaction(snapshot)
                .and_then(|interaction| interaction.get("status"))
                .and_then(Value::as_str)
                == Some("pending")
        })?;
        let id = session
            .wait_for_snapshot(Duration::from_secs(5), |snapshot| {
                snapshot.get("sessionId").and_then(Value::as_str).is_some()
            })?
            .get("sessionId")
            .and_then(Value::as_str)
            .map(str::to_string)
            .ok_or_else(|| anyhow::anyhow!("snapshot missing sessionId"))?;
        // A clean shutdown resolves pending dialogs. Hard-kill preserves the
        // durable pending marker so resume must restore the runtime-owned gate.
        session.kill_without_cleanup()?;
        id
    };

    let mut resumed = CodeSession::spawn(
        CodeSessionOptions::new(format!("{case_name}-resume"), fixture("plan_review"))
            .with_existing_repo_dir(repo_dir)
            .with_resume_thread(&session_id)
            .with_context("dev"),
    )?;
    resumed.attach_automation(&format!("{case_name}-resume"))?;
    let snapshot = resumed.wait_for_snapshot(Duration::from_secs(20), |snapshot| {
        find_network_policy_interaction(snapshot)
            .and_then(|interaction| interaction.get("status"))
            .and_then(Value::as_str)
            == Some("pending")
    })?;
    assert_eq!(
        status(&snapshot),
        Some("awaiting_interaction"),
        "resumed session must reopen the network-policy gate: {snapshot}"
    );
    let interaction_id = find_network_policy_interaction(&snapshot)
        .and_then(|interaction| interaction.get("id"))
        .and_then(Value::as_str)
        .ok_or_else(|| anyhow::anyhow!("restored network gate missing id: {snapshot}"))?
        .to_string();
    let (http_status, body) = resumed.respond_interaction(
        &interaction_id,
        &json!({ "selectedOption": "network-deny" }),
    )?;
    assert_eq!(
        http_status,
        StatusCode::OK,
        "network-deny on restored gate should be accepted: {body}"
    );
    resumed.wait_for_snapshot(Duration::from_secs(30), |snapshot| {
        status_eq(snapshot, "idle")
            && find_network_policy_interaction(snapshot)
                .and_then(|interaction| interaction.get("status"))
                .and_then(Value::as_str)
                == Some("resolved")
    })?;
    resumed.shutdown()
}

/// W2-03 Plan review / network-policy recovery contract (cargo filter: `plan_review`).
///
/// Pins the network-policy human-gate labels and exercises the same
/// `open_plan_review_from_workflow` / `open_network_policy_from_workflow`
/// scans that the Web resume path performs after crash/resume — including
/// the Execute→network marker ordering and the Back demote window. The
/// Web-process counterpart above pins the full HTTP projection/response and
/// resume path; this focused replay test keeps the state-machine edges local.
#[test]
fn plan_review_baseline_pins_network_policy_choices() {
    use libra::internal::ai::{
        runtime::phase1::{
            network_policy_interaction_id, open_network_policy_from_workflow,
            open_plan_review_from_workflow,
        },
        session::{CodeWorkflowEventKind, SessionJsonlStore},
        workflow_baseline::NETWORK_POLICY_CHOICES,
    };

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

    let temp = tempfile::tempdir().expect("temp dir");
    let store = SessionJsonlStore::new(temp.path().to_path_buf());
    let events = |store: &SessionJsonlStore| {
        store
            .load_code_workflow_replay()
            .expect("replay")
            .events
            .into_iter()
            .map(|event| event.event)
            .collect::<Vec<_>>()
    };

    store
        .append_code_workflow_durable(CodeWorkflowEventKind::PlanReviewRequested {
            interaction_id: "review-scenario".to_string(),
            plan_id: "plan-scenario".to_string(),
            turn_id: "plan-review-turn".to_string(),
            phase1_turn_id: "phase1-turn".to_string(),
            context_id: "review-scenario".to_string(),
            revision_of: None,
            prepared_from_network: None,
        })
        .expect("plan review marker");
    store
        .append_code_workflow_durable(CodeWorkflowEventKind::NetworkPolicyRequested {
            interaction_id: network_policy_interaction_id(Some("plan-scenario")),
            plan_id: "plan-scenario".to_string(),
            turn_id: "network-policy-turn".to_string(),
            default_allow: false,
        })
        .expect("App writes network marker before Execute");
    assert!(
        open_network_policy_from_workflow(events(&store).iter()).is_none(),
        "network gate must not restore while plan review is still open"
    );

    store
        .append_code_workflow_durable(CodeWorkflowEventKind::InteractionResolved {
            interaction_id: "review-scenario".to_string(),
            resolution: "execute".to_string(),
            command: None,
            prior_interaction_resolutions: Vec::new(),
            intent_revision_consumption: None,
        })
        .expect("Execute resolves plan review");
    assert_eq!(
        open_plan_review_from_workflow(events(&store).iter()),
        None,
        "Execute closes the plan review restore scan"
    );
    assert_eq!(
        open_network_policy_from_workflow(events(&store).iter()).map(|(id, ..)| id),
        Some(network_policy_interaction_id(Some("plan-scenario"))),
        "after Execute, App restore must reopen the network gate"
    );

    // Back crash window: replacement PlanReviewRequested before network resolve.
    store
        .append_code_workflow_durable(CodeWorkflowEventKind::PlanReviewRequested {
            interaction_id: "review-scenario".to_string(),
            plan_id: "plan-scenario".to_string(),
            turn_id: "plan-review-turn-2".to_string(),
            phase1_turn_id: String::new(),
            context_id: "review-scenario".to_string(),
            revision_of: None,
            prepared_from_network: None,
        })
        .expect("Back re-opens plan review");
    assert!(
        open_network_policy_from_workflow(events(&store).iter()).is_none(),
        "Back must demote the network gate so Allow cannot skip renewed plan review"
    );
    assert_eq!(
        open_plan_review_from_workflow(events(&store).iter()).map(|(id, ..)| id),
        Some("review-scenario".to_string()),
        "Back leaves plan review as the restorable gate"
    );
}

/// W0-02 baseline for automatic plan-repair threshold copy (filter: `repair`).
#[test]
fn repair_loop_baseline_threshold_keeps_plan_continue_affordance() {
    use libra::internal::ai::workflow_baseline::plan_repair_threshold_baseline_message;

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
    // Keep the migrated interaction kind stable for Code UI wire consumers.
    // Full multi-question coverage remains on the approval/interaction path;
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
/// `ai_goal_state_test`; this filter keeps the Code UI-facing wire tag discoverable
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

/// Status (`pending` / `resolved` / `cancelled`) of the interaction with the
/// given id, or `None` if no interaction with that id is present yet.
#[cfg(feature = "test-provider")]
fn interaction_status<'a>(snapshot: &'a Value, interaction_id: &str) -> Option<&'a str> {
    snapshot
        .get("interactions")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .find(|interaction| interaction.get("id").and_then(Value::as_str) == Some(interaction_id))
        .and_then(|interaction| interaction.get("status"))
        .and_then(Value::as_str)
}

/// First interaction of the given wire `kind` (e.g. `"intent_review_choice"`)
/// regardless of status, or `None` if none has been projected yet.
#[cfg(feature = "test-provider")]
fn find_interaction_by_kind<'a>(snapshot: &'a Value, kind: &str) -> Option<&'a Value> {
    snapshot
        .get("interactions")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .find(|interaction| interaction.get("kind").and_then(Value::as_str) == Some(kind))
}

/// Plan-review Execute gate (`post_plan_choice` without `phase=networkPolicy`).
/// Prefer a pending item when both Execute and network gates are present.
#[cfg(feature = "test-provider")]
fn find_post_plan_execute_interaction(snapshot: &Value) -> Option<&Value> {
    let interactions = snapshot.get("interactions").and_then(Value::as_array)?;
    let mut fallback = None;
    for interaction in interactions {
        if interaction.get("kind").and_then(Value::as_str) != Some("post_plan_choice") {
            continue;
        }
        if interaction
            .get("metadata")
            .and_then(|metadata| metadata.get("phase"))
            .and_then(Value::as_str)
            == Some("networkPolicy")
        {
            continue;
        }
        if interaction.get("status").and_then(Value::as_str) == Some("pending") {
            return Some(interaction);
        }
        fallback = Some(interaction);
    }
    fallback
}

/// Network-policy gate projected as `post_plan_choice` with
/// `phase=networkPolicy`.
#[cfg(feature = "test-provider")]
fn find_network_policy_interaction(snapshot: &Value) -> Option<&Value> {
    snapshot
        .get("interactions")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .find(|interaction| {
            interaction.get("kind").and_then(Value::as_str) == Some("post_plan_choice")
                && interaction
                    .get("metadata")
                    .and_then(|metadata| metadata.get("phase"))
                    .and_then(Value::as_str)
                    == Some("networkPolicy")
        })
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
