use std::{
    fs::{self, OpenOptions},
    io::Write,
    sync::{Arc, Barrier},
    thread,
};

use chrono::Utc;
#[cfg(feature = "test-provider")]
use libra::internal::ai::web::{
    code_ui::{CodeUiProviderInfo, initial_snapshot},
    code_ui_projection::fold_code_ui_snapshot,
    headless::headless_capabilities,
    sse_wire::CodeUiWireV2Event,
};
use libra::internal::ai::{
    intentspec::{
        ResolveContext,
        draft::{DraftAcceptance, DraftIntent, DraftRisk, IntentDraft},
        resolve_intentspec,
        types::{ChangeType, Check, CheckKind, Objective, ObjectiveKind, RiskLevel},
    },
    orchestrator::types::ExecutionPlanSpec,
    runtime::{
        event::Event,
        phase1::{
            Phase1CheckoutBinding, Phase1PersistedPlan, Phase1RetryIntentReviewState,
            Phase1ReviewContext, Phase1StartSeed, compile_submitted_plan,
            gc_unreachable_phase1_review_contexts, load_phase1_review_context,
            load_phase1_start_seed, open_network_policy_from_workflow,
            open_plan_review_from_workflow, pending_plan_revision_from_workflow,
            persist_phase1_review_context, persist_phase1_start_seed,
            phase1_retry_intent_review_state, phase1_review_context_path, phase1_turn_id_from_seed,
            preserve_unchanged_revision_steps, validate_phase1_context_session_budget,
            validate_single_open_gate_authority,
        },
    },
    session::{
        INTENT_REVISION_CONSUMER_COMMAND_KIND, INTENT_REVISION_CONSUMPTION_SCHEMA_VERSION,
        IntentRevisionConsumption, IntentRevisionConsumptionClaim, IntentRevisionRecovery,
        SessionState, SessionStore,
        jsonl::{
            CodeCommandAdmission, CodeCommandIdentity, CodeCommandIntent, CodeCommandRecovery,
            CodeCommandStatus, CodeCommandStoreError, CodeWorkflowEvent, CodeWorkflowEventKind,
            Phase1RetryIntentReview, SessionEvent, SessionJsonlStore,
        },
    },
    tools::context::{PlanDraftStep, SubmitPlanDraftArgs},
};

#[test]
fn ai_session_jsonl_save_load_roundtrip_and_event_contract() {
    let tmp = tempfile::TempDir::new().unwrap();
    let store = SessionStore::from_storage_path(tmp.path());

    let mut session = SessionState::new("/repo/main");
    session.summary = "JSONL session".to_string();
    session.context_mode = Some("dev".to_string());
    session.add_user_message("hello");
    session.add_assistant_message("hi");
    session
        .metadata
        .insert("thread_id".to_string(), serde_json::json!(session.id));

    store.save(&session).unwrap();

    let legacy_blob = tmp
        .path()
        .join("sessions")
        .join(format!("{}.json", session.id));
    let events_path = tmp
        .path()
        .join("sessions")
        .join(&session.id)
        .join("events.jsonl");
    assert!(!legacy_blob.exists(), "new saves must not write JSON blobs");
    assert!(events_path.exists(), "new saves must write events.jsonl");

    let loaded = store.load(&session.id).unwrap();
    assert_eq!(loaded.id, session.id);
    assert_eq!(loaded.summary, "JSONL session");
    assert_eq!(loaded.context_mode.as_deref(), Some("dev"));
    assert_eq!(loaded.message_count(), 2);

    let jsonl = SessionJsonlStore::new(store.session_root(&session.id));
    let events = jsonl.load_events().unwrap();
    assert_eq!(events.len(), 1);
    assert_event_trait(&events[0]);

    let line = fs::read_to_string(events_path).unwrap();
    let value: serde_json::Value = serde_json::from_str(line.trim_end()).unwrap();
    assert_eq!(value["kind"], "session_snapshot");
    assert!(
        value.get("payload").is_some(),
        "event must use envelope payload"
    );
}

#[test]
fn ai_session_jsonl_reader_skips_unknown_events_and_recovers_truncated_tail() {
    let tmp = tempfile::TempDir::new().unwrap();
    let store = SessionStore::from_storage_path(tmp.path());

    let mut session = SessionState::new("/repo/main");
    session.summary = "valid prefix".to_string();
    session.add_user_message("keep me");
    store.save(&session).unwrap();

    let events_path = tmp
        .path()
        .join("sessions")
        .join(&session.id)
        .join("events.jsonl");
    let mut file = OpenOptions::new().append(true).open(&events_path).unwrap();
    writeln!(
        file,
        "{{\"kind\":\"future_session_event\",\"payload\":{{\"ignored\":true}}}}"
    )
    .unwrap();
    write!(
        file,
        "{{\"kind\":\"session_snapshot\",\"payload\":{{\"event_id\":\""
    )
    .unwrap();

    let loaded = store.load(&session.id).unwrap();
    assert_eq!(loaded.summary, "valid prefix");
    assert_eq!(loaded.message_count(), 1);
}

#[test]
fn ai_session_jsonl_reader_rejects_complete_malformed_lines() {
    let tmp = tempfile::TempDir::new().unwrap();
    let store = SessionStore::from_storage_path(tmp.path());

    let mut session = SessionState::new("/repo/main");
    session.summary = "valid prefix".to_string();
    store.save(&session).unwrap();

    let events_path = tmp
        .path()
        .join("sessions")
        .join(&session.id)
        .join("events.jsonl");
    let mut file = OpenOptions::new().append(true).open(&events_path).unwrap();
    writeln!(file, "{{\"kind\":\"session_snapshot\",\"payload\":").unwrap();

    let error = store.load(&session.id).unwrap_err();
    assert_eq!(error.kind(), std::io::ErrorKind::InvalidData);
    assert!(error.to_string().contains("malformed complete line"));
}

#[test]
fn ai_session_jsonl_legacy_json_migration_is_concurrency_safe() {
    let tmp = tempfile::TempDir::new().unwrap();
    let sessions_dir = tmp.path().join("sessions");
    fs::create_dir_all(&sessions_dir).unwrap();

    let mut legacy = SessionState::new("/repo/main");
    legacy.id = "legacy-session".to_string();
    legacy.created_at = Utc::now();
    legacy.updated_at = legacy.created_at;
    legacy.summary = "legacy migrated once".to_string();
    legacy.add_user_message("from json");

    fs::write(
        sessions_dir.join("legacy-session.json"),
        serde_json::to_vec_pretty(&legacy).unwrap(),
    )
    .unwrap();

    let barrier = Arc::new(Barrier::new(2));
    let mut handles = Vec::new();
    for _ in 0..2 {
        let root = tmp.path().to_path_buf();
        let barrier = Arc::clone(&barrier);
        handles.push(thread::spawn(move || {
            let store = SessionStore::from_storage_path(&root);
            barrier.wait();
            store.load("legacy-session").unwrap()
        }));
    }

    let loaded_a = handles.pop().unwrap().join().unwrap();
    let loaded_b = handles.pop().unwrap().join().unwrap();
    assert_eq!(loaded_a.summary, "legacy migrated once");
    assert_eq!(loaded_b.summary, "legacy migrated once");

    let jsonl = SessionJsonlStore::new(sessions_dir.join("legacy-session"));
    let events = jsonl.load_events().unwrap();
    assert_eq!(
        events.len(),
        1,
        "concurrent legacy migration must append exactly one snapshot"
    );
    assert!(sessions_dir.join("legacy-session.json").exists());
}

#[test]
fn code_session_event_additive_variants_and_sequence() {
    let tmp = tempfile::TempDir::new().unwrap();
    let jsonl = SessionJsonlStore::new(tmp.path().join("session"));

    let accepted = jsonl
        .append_code_workflow(CodeWorkflowEventKind::CommandAccepted {
            command_id: "cmd-1".to_string(),
            workflow: "implement".to_string(),
        })
        .unwrap();
    let succeeded = jsonl
        .append_code_workflow(CodeWorkflowEventKind::TerminalSuccess {
            command_id: "cmd-1".to_string(),
            summary: "completed".to_string(),
        })
        .unwrap();
    assert_eq!(accepted.sequence, 1);
    assert_eq!(succeeded.sequence, 2);
    assert_ne!(accepted.event_id, succeeded.event_id);

    let events_path = jsonl.events_path();
    let failed = CodeWorkflowEvent::new(
        4,
        CodeWorkflowEventKind::TerminalFailure {
            command_id: "cmd-2".to_string(),
            reason: "provider unavailable".to_string(),
        },
    );
    let mut file = OpenOptions::new().append(true).open(&events_path).unwrap();
    writeln!(
        file,
        "{}",
        serde_json::to_string(&SessionEvent::code_workflow(accepted.clone())).unwrap()
    )
    .unwrap();
    writeln!(
        file,
        "{}",
        serde_json::to_string(&SessionEvent::code_workflow(failed)).unwrap()
    )
    .unwrap();
    writeln!(
        file,
        "{{\"kind\":\"code_workflow\",\"payload\":{{\"event_id\":\"{}\",\"sequence\":3,\"recorded_at\":\"{}\",\"event\":\"future_code_event\"}}}}",
        uuid::Uuid::new_v4(),
        Utc::now().to_rfc3339(),
    )
    .unwrap();
    write!(
        file,
        "{{\"kind\":\"code_workflow\",\"payload\":{{\"event_id\":\""
    )
    .unwrap();
    drop(file);

    // The current reader skips future nested variants and the interrupted tail.
    // It keeps the cursor discontinuity explicit instead of guessing a state.
    let before_recovery = jsonl.load_code_workflow_replay().unwrap();
    assert_eq!(
        before_recovery.events.len(),
        3,
        "duplicate event id must deduplicate"
    );
    assert_eq!(before_recovery.gaps.len(), 1);
    assert_eq!(before_recovery.gaps[0].after, 2);
    assert_eq!(before_recovery.gaps[0].before, 4);

    // Appending after a torn tail must first discard only that tail. The next
    // sequence follows the last complete Code event; W1-05 adds the lock/fsync
    // durability boundary around this same schema primitive.
    let indeterminate = jsonl
        .append_code_workflow(CodeWorkflowEventKind::IndeterminateSideEffect {
            command_id: "cmd-3".to_string(),
            effect: "write_file".to_string(),
            reason: "process stopped after dispatch".to_string(),
        })
        .unwrap();
    assert_eq!(indeterminate.sequence, 5);

    let replay = jsonl.load_code_workflow_replay().unwrap();
    assert_eq!(replay.events.len(), 4);
    assert!(matches!(
        &replay.events[0].event,
        CodeWorkflowEventKind::CommandAccepted { .. }
    ));
    assert!(matches!(
        &replay.events[1].event,
        CodeWorkflowEventKind::TerminalSuccess { .. }
    ));
    assert!(matches!(
        &replay.events[2].event,
        CodeWorkflowEventKind::TerminalFailure { .. }
    ));
    assert!(matches!(
        &replay.events[3].event,
        CodeWorkflowEventKind::IndeterminateSideEffect { .. }
    ));
    assert_eq!(replay.gaps, before_recovery.gaps);

    let direct_append = jsonl.append(&SessionEvent::code_workflow(CodeWorkflowEvent::new(
        6,
        CodeWorkflowEventKind::CommandAccepted {
            command_id: "cmd-4".to_string(),
            workflow: "implement".to_string(),
        },
    )));
    assert_eq!(
        direct_append.unwrap_err().kind(),
        std::io::ErrorKind::InvalidInput,
        "Code workflow rows must use the locked sequence allocator",
    );

    let persisted = fs::read_to_string(events_path).unwrap();
    assert!(persisted.ends_with('\n'));
}

#[test]
fn code_workflow_bounded_replay_starts_after_durable_cursor() {
    let tmp = tempfile::TempDir::new().unwrap();
    let jsonl = SessionJsonlStore::new(tmp.path().join("session"));
    for command_id in ["cmd-1", "cmd-2", "cmd-3"] {
        jsonl
            .append_code_workflow(CodeWorkflowEventKind::CommandAccepted {
                command_id: command_id.to_string(),
                workflow: "direct_chat".to_string(),
            })
            .unwrap();
    }

    let suffix = jsonl
        .load_code_workflow_replay_since(2, 2, 1024 * 1024)
        .unwrap();
    assert_eq!(suffix.events.len(), 1);
    assert_eq!(suffix.events[0].sequence, 3);
    assert!(suffix.gaps.is_empty());

    let error = jsonl
        .load_code_workflow_replay_since(0, 2, 1024 * 1024)
        .expect_err("an uncheckpointed suffix beyond the fixed event budget must fail closed");
    assert_eq!(error.kind(), std::io::ErrorKind::InvalidData);
    assert!(error.to_string().contains("bounded limit"));
}

#[test]
fn code_workflow_bounded_replay_rejects_a_truncated_tail_without_workflow_rows() {
    let tmp = tempfile::TempDir::new().unwrap();
    let jsonl = SessionJsonlStore::new(tmp.path().join("session"));
    jsonl
        .append_code_workflow(CodeWorkflowEventKind::CommandAccepted {
            command_id: "cmd-1".to_string(),
            workflow: "direct_chat".to_string(),
        })
        .unwrap();

    // Put a much larger legacy snapshot after the workflow row. A tail-only
    // read cannot prove that an omitted workflow row did not occur before this
    // snapshot, so resume must fail closed rather than treating it as empty.
    let mut snapshot = SessionState::new("/repo/main");
    snapshot.summary = "x".repeat(8 * 1024);
    jsonl.append(&SessionEvent::snapshot(snapshot)).unwrap();

    let error = jsonl
        .load_code_workflow_replay_since(1, 16, 1024)
        .expect_err("a truncated tail without workflow rows must not be trusted");
    assert_eq!(error.kind(), std::io::ErrorKind::InvalidData);
    assert!(error.to_string().contains("cannot prove"));
}

#[test]
fn code_workflow_append_serializes_concurrent_sequence_allocation() {
    let tmp = tempfile::TempDir::new().unwrap();
    let jsonl = Arc::new(SessionJsonlStore::new(tmp.path().join("session")));
    let barrier = Arc::new(Barrier::new(4));
    let mut handles = Vec::new();

    for command_number in 0..4 {
        let jsonl = Arc::clone(&jsonl);
        let barrier = Arc::clone(&barrier);
        handles.push(thread::spawn(move || {
            barrier.wait();
            jsonl
                .append_code_workflow(CodeWorkflowEventKind::CommandAccepted {
                    command_id: format!("cmd-{command_number}"),
                    workflow: "implement".to_string(),
                })
                .unwrap()
                .sequence
        }));
    }

    let mut sequences: Vec<_> = handles
        .into_iter()
        .map(|handle| handle.join().unwrap())
        .collect();
    sequences.sort_unstable();
    assert_eq!(sequences, vec![1, 2, 3, 4]);

    let replay = jsonl.load_code_workflow_replay().unwrap();
    assert_eq!(replay.events.len(), 4);
    assert!(replay.gaps.is_empty());
}

#[test]
fn command_idempotency_and_indeterminate_recovery() {
    let tmp = tempfile::TempDir::new().unwrap();
    let jsonl = SessionJsonlStore::new(tmp.path().join("session"));
    let mutating_identity = CodeCommandIdentity::new("repo-a", "session-a", "alice", "cmd-1");
    let mutating_intent = CodeCommandIntent::new(
        mutating_identity.clone(),
        "apply_patch",
        "sha256:request-a",
        true,
    );

    assert!(matches!(
        jsonl.admit_code_command(mutating_intent.clone()).unwrap(),
        CodeCommandAdmission::Execute { .. }
    ));
    assert!(matches!(
        jsonl.admit_code_command(mutating_intent.clone()).unwrap(),
        CodeCommandAdmission::Existing {
            status: CodeCommandStatus::Pending
        }
    ));

    let conflict = jsonl
        .admit_code_command(CodeCommandIntent::new(
            mutating_identity.clone(),
            "apply_patch",
            "sha256:other-payload",
            true,
        ))
        .unwrap_err();
    assert!(matches!(
        conflict,
        CodeCommandStoreError::PayloadConflict { .. }
    ));

    assert!(matches!(
        jsonl.recover_code_command(&mutating_identity).unwrap(),
        CodeCommandRecovery::Existing {
            status: CodeCommandStatus::Indeterminate { .. }
        }
    ));
    assert!(matches!(
        jsonl.admit_code_command(mutating_intent).unwrap(),
        CodeCommandAdmission::Existing {
            status: CodeCommandStatus::Indeterminate { .. }
        }
    ));

    let read_only_identity = CodeCommandIdentity::new("repo-a", "session-a", "alice", "cmd-2");
    let read_only_intent = CodeCommandIntent::new(
        read_only_identity.clone(),
        "search_files",
        "sha256:request-b",
        false,
    );
    assert!(matches!(
        jsonl.admit_code_command(read_only_intent.clone()).unwrap(),
        CodeCommandAdmission::Execute { .. }
    ));
    assert!(matches!(
        jsonl.recover_code_command(&read_only_identity).unwrap(),
        CodeCommandRecovery::RetryReadOnly { intent } if intent == read_only_intent
    ));
    assert!(matches!(
        jsonl
            .complete_code_command_success(&read_only_identity, "found 3 matches")
            .unwrap(),
        CodeCommandStatus::Succeeded { .. }
    ));
    assert!(matches!(
        jsonl.admit_code_command(read_only_intent).unwrap(),
        CodeCommandAdmission::Existing {
            status: CodeCommandStatus::Succeeded { .. }
        }
    ));

    let durable_events = jsonl.load_code_workflow_replay().unwrap();
    assert!(durable_events.events.iter().any(|event| matches!(
        event.event,
        CodeWorkflowEventKind::CommandIntentPersisted { .. }
    )));
    assert!(durable_events.events.iter().any(|event| matches!(
        event.event,
        CodeWorkflowEventKind::CommandIndeterminateSideEffect { .. }
    )));
    assert!(durable_events.events.iter().any(|event| matches!(
        event.event,
        CodeWorkflowEventKind::CommandTerminalSuccess { .. }
    )));
}

#[test]
fn intent_review_recovery_completes_only_phase0_pending_mutation() {
    let tmp = tempfile::TempDir::new().unwrap();
    let jsonl = SessionJsonlStore::new(tmp.path().join("session"));
    let phase0_identity = CodeCommandIdentity::new("repo-a", "session-a", "alice", "phase0-1");
    let queued_identity = CodeCommandIdentity::new("repo-a", "session-a", "alice", "queued-1");

    // Crash window: IntentReviewRequested is durable while Phase 0 and a
    // queued mutation are still Pending (no live owner after restart).
    jsonl
        .append_code_workflow_durable(CodeWorkflowEventKind::CommandIntentPersisted {
            command: CodeCommandIntent::new(
                phase0_identity.clone(),
                "tui_local_turn",
                "sha256:phase0",
                true,
            ),
        })
        .unwrap();
    jsonl
        .append_code_workflow_durable(CodeWorkflowEventKind::IntentReviewRequested {
            interaction_id: "intent-1".to_string(),
            intent_id: "spec-1".to_string(),
            turn_id: "gate-1".to_string(),
            phase0_turn_id: "phase0-1".to_string(),
        })
        .unwrap();
    jsonl
        .append_code_workflow_durable(CodeWorkflowEventKind::CommandIntentPersisted {
            command: CodeCommandIntent::new(
                queued_identity.clone(),
                "tui_local_turn",
                "sha256:queued",
                true,
            ),
        })
        .unwrap();

    let fenced = jsonl
        .recover_pending_mutating_code_commands_for_intent_review(Some("phase0-1"))
        .unwrap();
    assert_eq!(
        fenced,
        vec![queued_identity.clone()],
        "only the non-Phase-0 pending mutation must remain fenced"
    );

    assert!(matches!(
        jsonl.recover_code_command(&phase0_identity).unwrap(),
        CodeCommandRecovery::Existing {
            status: CodeCommandStatus::Succeeded { .. }
        }
    ));
    assert!(matches!(
        jsonl.recover_code_command(&queued_identity).unwrap(),
        CodeCommandRecovery::Existing {
            status: CodeCommandStatus::Indeterminate { .. }
        }
    ));
}

#[test]
fn terminal_before_intent_fails_closed_on_admit() {
    let tmp = tempfile::TempDir::new().unwrap();
    let jsonl = SessionJsonlStore::new(tmp.path().join("session"));
    let identity = CodeCommandIdentity::new("repo-a", "session-a", "alice", "cmd-orphan");
    let intent = CodeCommandIntent::new(
        identity.clone(),
        "apply_patch",
        "sha256:request-orphan",
        true,
    );

    jsonl
        .append_code_workflow(CodeWorkflowEventKind::CommandTerminalSuccess {
            command: identity.clone(),
            summary: "orphaned success".to_string(),
        })
        .unwrap();

    let orphan = jsonl.recover_code_command(&identity).unwrap_err();
    assert!(matches!(
        orphan,
        CodeCommandStoreError::TerminalWithoutIntent { .. }
    ));

    // A later intent must not clear the orphan terminal into a re-dispatchable
    // Pending state.
    jsonl
        .append_code_workflow(CodeWorkflowEventKind::CommandIntentPersisted {
            command: intent.clone(),
        })
        .unwrap();
    let conflict = jsonl.admit_code_command(intent).unwrap_err();
    assert!(matches!(
        conflict,
        CodeCommandStoreError::TerminalConflict { .. }
    ));
}

#[test]
fn command_status_cache_refreshes_under_append_lock() {
    let tmp = tempfile::TempDir::new().unwrap();
    let session_root = tmp.path().join("session");
    let writer = SessionJsonlStore::new(session_root.clone());
    let stale_reader = SessionJsonlStore::new(session_root);
    let identity = CodeCommandIdentity::new("repo-a", "session-a", "alice", "cmd-cache");
    let intent = CodeCommandIntent::new(
        identity.clone(),
        "apply_patch",
        "sha256:request-cache",
        true,
    );

    assert!(matches!(
        writer.admit_code_command(intent.clone()).unwrap(),
        CodeCommandAdmission::Execute { .. }
    ));
    // Populate the second store's cache while the command is still Pending.
    assert!(matches!(
        stale_reader.admit_code_command(intent.clone()).unwrap(),
        CodeCommandAdmission::Existing {
            status: CodeCommandStatus::Pending
        }
    ));
    assert!(matches!(
        writer
            .complete_code_command_success(&identity, "mutation applied")
            .unwrap(),
        CodeCommandStatus::Succeeded { .. }
    ));

    // After the durable terminal append, the stale store must refresh under the
    // lock and refuse a conflicting failure terminal instead of appending it.
    let conflict = stale_reader
        .complete_code_command_failure(&identity, "stale failure")
        .unwrap_err();
    assert!(matches!(
        conflict,
        CodeCommandStoreError::TerminalConflict { .. }
    ));
    assert!(matches!(
        stale_reader.admit_code_command(intent).unwrap(),
        CodeCommandAdmission::Existing {
            status: CodeCommandStatus::Succeeded { .. }
        }
    ));
}

#[test]
fn terminal_success_interaction_resolution_retry_is_idempotent() {
    let tmp = tempfile::TempDir::new().unwrap();
    let session_root = tmp.path().join("session");
    let writer = SessionJsonlStore::new(session_root.clone());
    let retrying_writer = SessionJsonlStore::new(session_root);
    let identity = CodeCommandIdentity::new("repo-a", "session-a", "alice", "cmd-review");
    let intent = CodeCommandIntent::new(
        identity.clone(),
        "respond_interaction",
        "sha256:review-modify",
        false,
    );
    let resolution = vec![
        ("risk-profile-a".to_string(), "answered".to_string()),
        ("plan-review-a".to_string(), "modify".to_string()),
    ];

    assert!(matches!(
        writer.admit_code_command(intent).unwrap(),
        CodeCommandAdmission::Execute { .. }
    ));
    writer
        .append_code_workflow_durable(CodeWorkflowEventKind::PlanReviewRequested {
            interaction_id: "plan-review-a".to_string(),
            plan_id: "plan-a".to_string(),
            turn_id: "review-turn-a".to_string(),
            phase1_turn_id: "phase1-turn-a".to_string(),
            context_id: "plan-context-a".to_string(),
            revision_of: None,
            prepared_from_network: None,
        })
        .unwrap();
    writer
        .complete_code_command_success_with_interaction_resolutions(
            &identity,
            "plan review modified",
            &resolution,
        )
        .unwrap();
    let before_retry = writer.load_code_workflow_replay().unwrap();

    assert!(matches!(
        retrying_writer
            .complete_code_command_success_with_interaction_resolutions(
                &identity,
                "plan review modified",
                &resolution,
            )
            .unwrap(),
        CodeCommandStatus::Succeeded { .. }
    ));
    let after_retry = writer.load_code_workflow_replay().unwrap();
    assert_eq!(
        after_retry.events, before_retry.events,
        "an identical retry of terminal success plus resolution must be a no-op"
    );
}

#[cfg(feature = "test-provider")]
#[test]
fn exact_pending_resolution_retry_resyncs_visible_post_write_row() {
    let tmp = tempfile::TempDir::new().unwrap();
    let store = SessionJsonlStore::new(tmp.path().join("session"));
    let identity = CodeCommandIdentity::new(
        "repo-a",
        "session-a",
        "alice",
        "checkpoint-post-write-retry",
    );
    let intent = CodeCommandIntent::new(
        identity.clone(),
        "agent_turn",
        "sha256:checkpoint-post-write-retry",
        false,
    );
    let resolutions = vec![("user-input-a".to_string(), "answered".to_string())];
    assert!(matches!(
        store.admit_code_command(intent).unwrap(),
        CodeCommandAdmission::Execute { .. }
    ));

    store.fail_next_durable_sync_after_write_for_test();
    store
        .checkpoint_pending_interaction_resolutions(&identity, &resolutions)
        .expect_err("the first checkpoint must expose the post-write sync ambiguity");
    assert_eq!(
        store
            .load_code_workflow_replay()
            .unwrap()
            .events
            .iter()
            .filter(|event| matches!(
                &event.event,
                CodeWorkflowEventKind::InteractionResolved {
                    command: Some(command),
                    ..
                } if command == &identity
            ))
            .count(),
        1,
        "the complete checkpoint row is visible after the injected sync failure"
    );

    store.fail_next_events_log_resync_for_test();
    store
        .checkpoint_pending_interaction_resolutions(&identity, &resolutions)
        .expect_err("an exact retry must attempt an event-log re-sync before ACK");
    let before_final_retry = store.load_code_workflow_replay().unwrap();
    store
        .checkpoint_pending_interaction_resolutions(&identity, &resolutions)
        .expect("the final exact retry must re-sync and ACK the existing row");
    assert_eq!(
        store.load_code_workflow_replay().unwrap().events,
        before_final_retry.events,
        "an exact checkpoint retry must never append a duplicate row"
    );
}

#[cfg(feature = "test-provider")]
#[test]
fn exact_combined_terminal_retry_resyncs_and_rejects_payload_aliases() {
    let tmp = tempfile::TempDir::new().unwrap();
    let store = SessionJsonlStore::new(tmp.path().join("session"));
    let identity =
        CodeCommandIdentity::new("repo-a", "session-a", "alice", "combined-post-write-retry");
    let intent = CodeCommandIntent::new(
        identity.clone(),
        "respond_interaction",
        "sha256:combined-post-write-retry",
        false,
    );
    let resolutions = vec![
        ("risk-profile-a".to_string(), "answered".to_string()),
        ("plan-review-a".to_string(), "confirm".to_string()),
    ];
    assert!(matches!(
        store.admit_code_command(intent).unwrap(),
        CodeCommandAdmission::Execute { .. }
    ));

    store.fail_next_durable_sync_after_write_for_test();
    store
        .complete_code_command_success_with_interaction_resolutions(
            &identity,
            "plan confirmed",
            &resolutions,
        )
        .expect_err("the first combined terminal must expose the post-write sync ambiguity");
    store.fail_next_events_log_resync_for_test();
    store
        .complete_code_command_success_with_interaction_resolutions(
            &identity,
            "plan confirmed",
            &resolutions,
        )
        .expect_err("an exact terminal retry must attempt an event-log re-sync before ACK");
    assert_eq!(
        store
            .complete_code_command_success_with_interaction_resolutions(
                &identity,
                "plan confirmed",
                &resolutions,
            )
            .expect("the final exact retry must re-sync and ACK the existing terminal row"),
        CodeCommandStatus::Succeeded {
            summary: "plan confirmed".to_string()
        }
    );

    let replay = store.load_code_workflow_replay().unwrap();
    assert_eq!(
        replay
            .events
            .iter()
            .filter(|event| matches!(
                &event.event,
                CodeWorkflowEventKind::CommandTerminalSuccessWithInteractionResolved {
                    command,
                    ..
                } if command == &identity
            ))
            .count(),
        1,
        "the exact terminal retry must never append a duplicate combined row"
    );

    for conflicting in [
        resolutions[1..].to_vec(),
        vec![resolutions[1].clone(), resolutions[0].clone()],
    ] {
        assert!(matches!(
            store.complete_code_command_success_with_interaction_resolutions(
                &identity,
                "plan confirmed",
                &conflicting,
            ),
            Err(CodeCommandStoreError::TerminalConflict { .. })
        ));
    }
}

#[cfg(feature = "test-provider")]
#[test]
fn exact_admission_and_status_ack_resync_visible_post_write_intents() {
    fn intent(command_id: &str) -> CodeCommandIntent {
        CodeCommandIntent::new(
            CodeCommandIdentity::new("repo-a", "session-a", "alice", command_id),
            "agent_turn",
            format!("sha256:{command_id}"),
            false,
        )
    }

    let tmp = tempfile::TempDir::new().unwrap();
    let admission_store = SessionJsonlStore::new(tmp.path().join("admission-session"));
    let admission_intent = intent("admission-post-write-retry");
    admission_store.fail_next_durable_sync_after_write_for_test();
    admission_store
        .admit_code_command(admission_intent.clone())
        .expect_err("the first admission must expose the post-write sync ambiguity");
    admission_store.fail_next_events_log_resync_for_test();
    admission_store
        .admit_code_command(admission_intent.clone())
        .expect_err("an exact admission retry must re-sync before returning Existing");
    assert_eq!(
        admission_store
            .admit_code_command(admission_intent.clone())
            .expect("the final admission retry must re-sync the existing intent"),
        CodeCommandAdmission::Existing {
            status: CodeCommandStatus::Pending
        }
    );
    assert_eq!(
        admission_store
            .load_code_workflow_replay()
            .unwrap()
            .events
            .iter()
            .filter(|event| matches!(
                &event.event,
                CodeWorkflowEventKind::CommandIntentPersisted { command }
                    if command == &admission_intent
            ))
            .count(),
        1
    );

    let status_store = SessionJsonlStore::new(tmp.path().join("status-session"));
    let status_intent = intent("status-post-write-retry");
    status_store.fail_next_durable_sync_after_write_for_test();
    status_store
        .admit_code_command(status_intent.clone())
        .expect_err("the status fixture must leave one visible but unsynced intent row");
    status_store.fail_next_events_log_resync_for_test();
    status_store
        .code_command_intent_status(&status_intent.identity)
        .expect_err("status ACK must re-sync a visible post-write intent");
    assert_eq!(
        status_store
            .code_command_intent_status(&status_intent.identity)
            .expect("the final status read must re-sync and ACK the intent"),
        Some((status_intent, CodeCommandStatus::Pending))
    );
}

#[test]
fn combined_retry_cannot_adopt_unrelated_resolution_after_plain_success() {
    let tmp = tempfile::TempDir::new().unwrap();
    let store = SessionJsonlStore::new(tmp.path().join("session"));
    let identity = CodeCommandIdentity::new(
        "repo-a",
        "session-a",
        "alice",
        "plain-success-unrelated-resolution",
    );
    let intent = CodeCommandIntent::new(
        identity.clone(),
        "respond_interaction",
        "sha256:plain-success-unrelated-resolution",
        false,
    );
    assert!(matches!(
        store.admit_code_command(intent).unwrap(),
        CodeCommandAdmission::Execute { .. }
    ));
    store
        .complete_code_command_success(&identity, "done")
        .unwrap();
    store
        .append_code_workflow_durable(CodeWorkflowEventKind::InteractionResolved {
            interaction_id: "plan-review-a".to_string(),
            resolution: "confirm".to_string(),
            command: None,
            prior_interaction_resolutions: Vec::new(),
            intent_revision_consumption: None,
        })
        .unwrap();

    assert!(matches!(
        store.complete_code_command_success_with_interaction_resolutions(
            &identity,
            "done",
            &[("plan-review-a".to_string(), "confirm".to_string())],
        ),
        Err(CodeCommandStoreError::TerminalConflict { .. })
    ));
}

#[test]
fn legacy_combined_terminal_defaults_prior_interaction_resolutions_to_empty() {
    let identity = CodeCommandIdentity::new(
        "repo-legacy",
        "session-legacy",
        "principal-legacy",
        "combined-command-legacy",
    );
    let decoded: CodeWorkflowEventKind = serde_json::from_value(serde_json::json!({
        "event": "command_terminal_success_with_interaction_resolved",
        "command": identity,
        "summary": "legacy review accepted",
        "interaction_id": "legacy-intent",
        "resolution": "confirm"
    }))
    .expect("legacy combined terminal without prior resolutions must remain readable");
    match decoded {
        CodeWorkflowEventKind::CommandTerminalSuccessWithInteractionResolved {
            interaction_id,
            resolution,
            prior_interaction_resolutions,
            intent_revision,
            ..
        } => {
            assert_eq!(interaction_id, "legacy-intent");
            assert_eq!(resolution, "confirm");
            assert!(prior_interaction_resolutions.is_empty());
            assert!(intent_revision.is_none());
        }
        other => panic!("decoded the wrong legacy combined variant: {other:?}"),
    }
}

fn intent_revision_test_command(
    command_id: &str,
    command_kind: &str,
    mutating: bool,
) -> CodeCommandIntent {
    CodeCommandIntent::new(
        CodeCommandIdentity::new("repo-a", "session-a", "alice", command_id),
        command_kind,
        format!("sha256:{command_id}"),
        mutating,
    )
}

fn append_synthetic_intent_revision_terminal(
    store: &SessionJsonlStore,
    source: &CodeCommandIntent,
    interaction_id: &str,
    intent_id: &str,
    digest_byte: char,
) -> (IntentRevisionRecovery, CodeWorkflowEvent) {
    store
        .append_code_workflow_durable(CodeWorkflowEventKind::IntentReviewRequested {
            interaction_id: interaction_id.to_string(),
            intent_id: intent_id.to_string(),
            turn_id: format!("{interaction_id}-gate"),
            phase0_turn_id: source.identity.command_id.clone(),
        })
        .unwrap();
    let recovery = IntentRevisionRecovery {
        interaction_id: interaction_id.to_string(),
        sidecar_digest: format!("hmac-sha256:{}", digest_byte.to_string().repeat(64)),
    };
    let terminal = store
        .append_code_workflow_durable(
            CodeWorkflowEventKind::CommandTerminalSuccessWithInteractionResolved {
                command: source.identity.clone(),
                summary: "IntentSpec revision requested".to_string(),
                interaction_id: interaction_id.to_string(),
                resolution: "modify".to_string(),
                prior_interaction_resolutions: Vec::new(),
                intent_revision: Some(recovery.clone()),
            },
        )
        .unwrap();
    (recovery, terminal)
}

fn append_raw_json_line(store: &SessionJsonlStore, value: &serde_json::Value) {
    let mut file = OpenOptions::new()
        .append(true)
        .open(store.events_path())
        .expect("open workflow JSONL for a malformed replay fixture");
    serde_json::to_writer(&mut file, value).expect("serialize malformed replay fixture row");
    writeln!(file).expect("terminate malformed replay fixture row");
    file.sync_all()
        .expect("durably persist malformed replay fixture row");
}

fn durable_intent_revision_source(
    store: &SessionJsonlStore,
    command_id: &str,
    interaction_id: &str,
    intent_id: &str,
    digest_byte: char,
) -> (CodeCommandIntent, IntentRevisionRecovery, CodeWorkflowEvent) {
    let source =
        intent_revision_test_command(command_id, INTENT_REVISION_CONSUMER_COMMAND_KIND, true);
    assert!(matches!(
        store.admit_code_command(source.clone()).unwrap(),
        CodeCommandAdmission::Execute { .. }
    ));
    store
        .append_code_workflow_durable(CodeWorkflowEventKind::IntentReviewRequested {
            interaction_id: interaction_id.to_string(),
            intent_id: intent_id.to_string(),
            turn_id: format!("{interaction_id}-gate"),
            phase0_turn_id: source.identity.command_id.clone(),
        })
        .unwrap();
    let recovery = IntentRevisionRecovery {
        interaction_id: interaction_id.to_string(),
        sidecar_digest: format!("hmac-sha256:{}", digest_byte.to_string().repeat(64)),
    };
    store
        .complete_code_command_success_with_interaction_resolutions_and_intent_revision(
            &source.identity,
            "IntentSpec revision requested",
            &[(interaction_id.to_string(), "modify".to_string())],
            Some(&recovery),
        )
        .unwrap();
    let terminal = store
        .load_code_workflow_replay()
        .unwrap()
        .events
        .into_iter()
        .find(|event| {
            matches!(
                &event.event,
                CodeWorkflowEventKind::CommandTerminalSuccessWithInteractionResolved {
                    command,
                    interaction_id: candidate,
                    ..
                } if command == &source.identity && candidate == interaction_id
            )
        })
        .expect("the durable revision source must have one combined terminal");
    (source, recovery, terminal)
}

fn intent_revision_consumption_claim(
    source: &CodeCommandIntent,
    recovery: &IntentRevisionRecovery,
    terminal: &CodeWorkflowEvent,
    intent_id: &str,
    consumer_intent: CodeCommandIntent,
) -> IntentRevisionConsumptionClaim {
    IntentRevisionConsumptionClaim {
        schema_version: INTENT_REVISION_CONSUMPTION_SCHEMA_VERSION,
        interaction_id: recovery.interaction_id.clone(),
        source_command: source.identity.clone(),
        consumer_intent,
        terminal_event_id: terminal.event_id,
        terminal_sequence: terminal.sequence,
        intent_id: intent_id.to_string(),
        sidecar_digest: Some(recovery.sidecar_digest.clone()),
    }
}

#[cfg(feature = "test-provider")]
#[test]
fn intent_revision_receipt_orders_source_consumer_and_resyncs_exact_postwrite_retry() {
    let tmp = tempfile::TempDir::new().unwrap();
    let store = SessionJsonlStore::new(tmp.path().join("session"));
    let raw_note_sentinel = "raw-note-must-remain-sidecar-only";
    let (source, recovery, terminal) = durable_intent_revision_source(
        &store,
        "revision-source",
        "revision-review",
        "intent-revision",
        'a',
    );
    let consumer = intent_revision_test_command(
        "revision-consumer",
        INTENT_REVISION_CONSUMER_COMMAND_KIND,
        true,
    );
    assert!(matches!(
        store.admit_code_command(consumer.clone()).unwrap(),
        CodeCommandAdmission::Execute { .. }
    ));
    let claim = intent_revision_consumption_claim(
        &source,
        &recovery,
        &terminal,
        "intent-revision",
        consumer.clone(),
    );
    let consumption = store
        .prepare_intent_revision_consumption(&consumer, &claim)
        .expect("the exact pending consumer intent must resolve to its durable event");

    store.fail_next_durable_sync_after_write_for_test();
    store.fail_next_events_log_resync_for_test();
    let self_resync = store
        .record_intent_revision_consumption(&consumption)
        .expect_err("the append path must self-resync its visible post-write receipt before ACK");
    assert!(
        self_resync
            .to_string()
            .contains("re-syncing the durable session event log"),
        "the visible post-write receipt must surface the distinct injected self-resync failure: {self_resync}"
    );
    store
        .record_intent_revision_consumption(&consumption)
        .expect("the final exact receipt retry must durably re-sync and ACK the one existing row");

    let replay = store.load_code_workflow_replay().unwrap();
    let receipt = replay
        .events
        .iter()
        .filter(|event| {
            matches!(
                &event.event,
                CodeWorkflowEventKind::InteractionResolved {
                    intent_revision_consumption: Some(actual),
                    ..
                } if actual == &consumption
            )
        })
        .collect::<Vec<_>>();
    assert_eq!(
        receipt.len(),
        1,
        "receipt retries must never append a duplicate"
    );
    let receipt = receipt[0];
    assert!(
        terminal.sequence < consumption.consumer_intent_sequence
            && consumption.consumer_intent_sequence < receipt.sequence,
        "the durable lineage must be source terminal < consumer intent < consume receipt"
    );
    assert!(matches!(
        &receipt.event,
        CodeWorkflowEventKind::InteractionResolved {
            interaction_id,
            resolution,
            command: None,
            prior_interaction_resolutions,
            intent_revision_consumption: Some(actual),
        } if interaction_id == &recovery.interaction_id
            && resolution == "modify"
            && prior_interaction_resolutions.is_empty()
            && actual == &consumption
    ));

    let terminal_wire = serde_json::to_string(&SessionEvent::code_workflow(terminal)).unwrap();
    let receipt_wire =
        serde_json::to_string(&SessionEvent::code_workflow((*receipt).clone())).unwrap();
    for workflow_wire in [&terminal_wire, &receipt_wire] {
        assert!(!workflow_wire.contains(raw_note_sentinel));
        assert!(
            !workflow_wire.contains("\"note\""),
            "workflow terminal/receipt payloads must remain digest-only"
        );
        assert!(workflow_wire.contains(&recovery.sidecar_digest));
    }
    let sse = CodeUiWireV2Event::from_workflow_event(receipt);
    assert_eq!(sse.kind, "intent_revision_consumed");
    let sse_wire = serde_json::to_string(&sse).unwrap();
    assert!(!sse_wire.contains(raw_note_sentinel));
    assert!(sse_wire.contains(&recovery.sidecar_digest));

    let receipt_index = replay
        .events
        .iter()
        .position(|event| event.event_id == receipt.event_id)
        .unwrap();
    let mut before_receipt = replay.clone();
    before_receipt.events.truncate(receipt_index);
    let mut through_receipt = replay.clone();
    through_receipt.events.truncate(receipt_index + 1);
    let bootstrap = initial_snapshot(
        "/repo/main".to_string(),
        CodeUiProviderInfo {
            provider: "receipt-projection-test".to_string(),
            model: Some("deterministic".to_string()),
            mode: Some("headless".to_string()),
            managed: false,
        },
        headless_capabilities(),
    );
    let before_snapshot = fold_code_ui_snapshot(bootstrap.clone(), &before_receipt)
        .expect("fold workflow immediately before the receipt")
        .snapshot;
    let through_snapshot = fold_code_ui_snapshot(bootstrap, &through_receipt)
        .expect("fold workflow including the dedicated receipt")
        .snapshot;
    assert_eq!(
        serde_json::to_value(through_snapshot).unwrap(),
        serde_json::to_value(before_snapshot).unwrap(),
        "the consume receipt must not change gate state, resolvedAt, or session status"
    );

    store
        .complete_code_command_success(&consumer.identity, "revision consumer completed")
        .unwrap();
    store.fail_next_events_log_resync_for_test();
    let terminal_resync = store
        .record_intent_revision_consumption(&consumption)
        .expect_err("a terminal consumer still requires an exact receipt re-sync");
    assert!(
        terminal_resync
            .to_string()
            .contains("re-syncing the durable session event log"),
        "terminal exact retry must expose the injected re-sync failure: {terminal_resync}"
    );
    store
        .record_intent_revision_consumption(&consumption)
        .expect("the exact receipt remains idempotent after consumer terminalization");
}

#[test]
fn intent_revision_receipt_is_bidirectional_first_writer_and_rejects_half_matches() {
    let tmp = tempfile::TempDir::new().unwrap();
    let store = SessionJsonlStore::new(tmp.path().join("session"));
    let (source_a, recovery_a, terminal_a) = durable_intent_revision_source(
        &store,
        "revision-source-a",
        "revision-review-a",
        "intent-revision-a",
        'a',
    );
    let (source_b, recovery_b, terminal_b) = durable_intent_revision_source(
        &store,
        "revision-source-b",
        "revision-review-b",
        "intent-revision-b",
        'b',
    );
    let consumer_a = intent_revision_test_command(
        "revision-consumer-a",
        INTENT_REVISION_CONSUMER_COMMAND_KIND,
        true,
    );
    store.admit_code_command(consumer_a.clone()).unwrap();
    let claim_a = intent_revision_consumption_claim(
        &source_a,
        &recovery_a,
        &terminal_a,
        "intent-revision-a",
        consumer_a.clone(),
    );
    let consumption_a = store
        .prepare_intent_revision_consumption(&consumer_a, &claim_a)
        .unwrap();
    store
        .record_intent_revision_consumption(&consumption_a)
        .unwrap();

    let claim_same_consumer = intent_revision_consumption_claim(
        &source_b,
        &recovery_b,
        &terminal_b,
        "intent-revision-b",
        consumer_a.clone(),
    );
    assert!(matches!(
        store.prepare_intent_revision_consumption(&consumer_a, &claim_same_consumer),
        Err(CodeCommandStoreError::InvalidIntent)
    ));
    store
        .complete_code_command_success(&consumer_a.identity, "consumer A complete")
        .unwrap();

    let consumer_b = intent_revision_test_command(
        "revision-consumer-b",
        INTENT_REVISION_CONSUMER_COMMAND_KIND,
        true,
    );
    store.admit_code_command(consumer_b.clone()).unwrap();
    let claim_b = intent_revision_consumption_claim(
        &source_b,
        &recovery_b,
        &terminal_b,
        "intent-revision-b",
        consumer_b.clone(),
    );
    let consumption_b = store
        .prepare_intent_revision_consumption(&consumer_b, &claim_b)
        .unwrap();
    for half_match in [
        IntentRevisionConsumption {
            consumer_intent_event_id: consumption_a.consumer_intent_event_id,
            ..consumption_b.clone()
        },
        IntentRevisionConsumption {
            consumer_intent_sequence: consumption_a.consumer_intent_sequence,
            ..consumption_b.clone()
        },
    ] {
        assert!(matches!(
            store.record_intent_revision_consumption(&half_match),
            Err(CodeCommandStoreError::InvalidIntent)
        ));
    }
    store
        .record_intent_revision_consumption(&consumption_b)
        .unwrap();
    store
        .complete_code_command_success(&consumer_b.identity, "consumer B complete")
        .unwrap();

    let consumer_c = intent_revision_test_command(
        "revision-consumer-c",
        INTENT_REVISION_CONSUMER_COMMAND_KIND,
        true,
    );
    store.admit_code_command(consumer_c.clone()).unwrap();
    let claim_reused_source = intent_revision_consumption_claim(
        &source_a,
        &recovery_a,
        &terminal_a,
        "intent-revision-a",
        consumer_c.clone(),
    );
    assert!(matches!(
        store.prepare_intent_revision_consumption(&consumer_c, &claim_reused_source),
        Err(CodeCommandStoreError::InvalidIntent)
    ));
}

#[test]
fn intent_revision_consumption_requires_one_prior_source_intent() {
    for source_shape in ["missing", "duplicate"] {
        let tmp = tempfile::TempDir::new().unwrap();
        let session_root = tmp.path().join(source_shape);
        let store = SessionJsonlStore::new(session_root.clone());
        let source = intent_revision_test_command(
            &format!("{source_shape}-source"),
            INTENT_REVISION_CONSUMER_COMMAND_KIND,
            true,
        );
        if source_shape == "duplicate" {
            assert!(matches!(
                store.admit_code_command(source.clone()).unwrap(),
                CodeCommandAdmission::Execute { .. }
            ));
            store
                .append_code_workflow_durable(CodeWorkflowEventKind::CommandIntentPersisted {
                    command: source.clone(),
                })
                .expect("append a duplicate identical source intent fixture");
        }
        let interaction_id = format!("{source_shape}-source-review");
        let intent_id = format!("{source_shape}-source-intent");
        let (recovery, terminal) = append_synthetic_intent_revision_terminal(
            &store,
            &source,
            &interaction_id,
            &intent_id,
            if source_shape == "missing" { '4' } else { '5' },
        );
        drop(store);
        let store = SessionJsonlStore::new(session_root);
        let consumer = intent_revision_test_command(
            &format!("{source_shape}-source-consumer"),
            INTENT_REVISION_CONSUMER_COMMAND_KIND,
            true,
        );
        assert!(matches!(
            store.admit_code_command(consumer.clone()).unwrap(),
            CodeCommandAdmission::Execute { .. }
        ));
        let claim = intent_revision_consumption_claim(
            &source,
            &recovery,
            &terminal,
            &intent_id,
            consumer.clone(),
        );
        assert!(matches!(
            store.prepare_intent_revision_consumption(&consumer, &claim),
            Err(CodeCommandStoreError::InvalidIntent)
        ));
        assert!(
            store
                .load_code_workflow_replay()
                .unwrap()
                .events
                .iter()
                .all(|event| !matches!(
                    &event.event,
                    CodeWorkflowEventKind::InteractionResolved {
                        intent_revision_consumption: Some(_),
                        ..
                    }
                ))
        );
    }
}

#[test]
fn intent_revision_consumption_rejects_incomplete_or_unknown_workflow_replay() {
    for corruption in [
        "sequence-gap",
        "duplicate-event-id",
        "unknown-code-workflow",
    ] {
        let tmp = tempfile::TempDir::new().unwrap();
        let store = SessionJsonlStore::new(tmp.path().join(corruption));
        let (source, recovery, terminal) = durable_intent_revision_source(
            &store,
            &format!("{corruption}-source"),
            &format!("{corruption}-review"),
            &format!("{corruption}-intent"),
            if corruption == "sequence-gap" {
                '2'
            } else {
                '3'
            },
        );
        let consumer = intent_revision_test_command(
            &format!("{corruption}-consumer"),
            INTENT_REVISION_CONSUMER_COMMAND_KIND,
            true,
        );
        assert!(matches!(
            store.admit_code_command(consumer.clone()).unwrap(),
            CodeCommandAdmission::Execute { .. }
        ));
        let claim = intent_revision_consumption_claim(
            &source,
            &recovery,
            &terminal,
            &format!("{corruption}-intent"),
            consumer.clone(),
        );
        let consumption = store
            .prepare_intent_revision_consumption(&consumer, &claim)
            .expect("capture the exact consumer before corrupting replay continuity");
        let replay = store.load_code_workflow_replay().unwrap();
        let tip = replay.events.last().expect("consumer intent is durable");
        let malformed = match corruption {
            "sequence-gap" => {
                serde_json::to_value(SessionEvent::code_workflow(CodeWorkflowEvent::new(
                    tip.sequence + 2,
                    CodeWorkflowEventKind::CodeUiProjectionDelta {
                        projection: "test_gap".to_string(),
                        summary: "malformed replay gap fixture".to_string(),
                        payload: serde_json::Value::Null,
                    },
                )))
                .unwrap()
            }
            "duplicate-event-id" => {
                let mut duplicate = (*tip).clone();
                duplicate.sequence += 1;
                serde_json::to_value(SessionEvent::code_workflow(duplicate)).unwrap()
            }
            "unknown-code-workflow" => {
                let mut unknown =
                    serde_json::to_value(SessionEvent::code_workflow(CodeWorkflowEvent::new(
                        tip.sequence + 1,
                        CodeWorkflowEventKind::CodeUiProjectionDelta {
                            projection: "test_unknown".to_string(),
                            summary: "unknown workflow fixture".to_string(),
                            payload: serde_json::Value::Null,
                        },
                    )))
                    .unwrap();
                unknown["payload"]["event"] =
                    serde_json::Value::String("future_intent_revision_protocol".to_string());
                unknown
            }
            _ => unreachable!(),
        };
        append_raw_json_line(&store, &malformed);
        let malformed_log = fs::read(store.events_path()).unwrap();

        assert!(
            store
                .prepare_intent_revision_consumption(&consumer, &claim)
                .is_err(),
            "{corruption} must fail closed before consumption admission"
        );
        assert!(
            store
                .recover_pending_mutating_code_commands_for_review_and_phase1_prewrite(
                    None,
                    None,
                    Some(&consumption),
                )
                .is_err(),
            "{corruption} must fail closed again under the recovery append lock"
        );
        assert_eq!(
            fs::read(store.events_path()).unwrap(),
            malformed_log,
            "{corruption} rejection must not append a terminal, receipt, or repair row"
        );
        let raw = fs::read_to_string(store.events_path()).unwrap();
        assert!(
            !raw.contains("\"intent_revision_consumption\""),
            "{corruption} must not append a consume receipt"
        );
    }
}

#[test]
fn aborted_intent_revision_claim_does_not_block_a_later_consumer() {
    let tmp = tempfile::TempDir::new().unwrap();
    let store = SessionJsonlStore::new(tmp.path().join("session"));
    let (source, recovery, terminal) = durable_intent_revision_source(
        &store,
        "aborted-claim-source",
        "aborted-claim-review",
        "aborted-claim-intent",
        'f',
    );
    let abandoned = intent_revision_test_command(
        "aborted-claim-consumer",
        INTENT_REVISION_CONSUMER_COMMAND_KIND,
        true,
    );
    store.admit_code_command(abandoned.clone()).unwrap();
    let abandoned_claim = intent_revision_consumption_claim(
        &source,
        &recovery,
        &terminal,
        "aborted-claim-intent",
        abandoned.clone(),
    );
    store
        .prepare_intent_revision_consumption(&abandoned, &abandoned_claim)
        .expect("a prepared in-memory claim is not yet a durable receipt");
    store
        .complete_code_command_failure(
            &abandoned.identity,
            "runtime turn cancelled before a mutating side effect began",
        )
        .unwrap();

    let replacement = intent_revision_test_command(
        "replacement-claim-consumer",
        INTENT_REVISION_CONSUMER_COMMAND_KIND,
        true,
    );
    store.admit_code_command(replacement.clone()).unwrap();
    let replacement_claim = intent_revision_consumption_claim(
        &source,
        &recovery,
        &terminal,
        "aborted-claim-intent",
        replacement.clone(),
    );
    let replacement_consumption = store
        .prepare_intent_revision_consumption(&replacement, &replacement_claim)
        .expect("the abandoned non-durable claim must not reserve the source revision");
    store
        .record_intent_revision_consumption(&replacement_consumption)
        .expect("the later legal consumer must commit the unique receipt");
    let replay = store.load_code_workflow_replay().unwrap();
    assert_eq!(
        replay
            .events
            .iter()
            .filter(|event| matches!(
                &event.event,
                CodeWorkflowEventKind::InteractionResolved {
                    intent_revision_consumption: Some(actual),
                    ..
                } if actual == &replacement_consumption
            ))
            .count(),
        1
    );
    assert!(replay.events.iter().all(|event| !matches!(
        &event.event,
        CodeWorkflowEventKind::InteractionResolved {
            intent_revision_consumption: Some(actual),
            ..
        } if actual.claim.consumer_intent.identity == abandoned.identity
    )));
}

#[test]
fn intent_revision_recovery_rejects_arbitrary_failure_and_indeterminate_terminals() {
    for terminal_kind in ["failure", "indeterminate", "dynamic-indeterminate"] {
        let tmp = tempfile::TempDir::new().unwrap();
        let store = SessionJsonlStore::new(tmp.path().join(terminal_kind));
        let (source, recovery, terminal) = durable_intent_revision_source(
            &store,
            &format!("{terminal_kind}-source"),
            &format!("{terminal_kind}-review"),
            &format!("{terminal_kind}-intent"),
            match terminal_kind {
                "failure" => 'd',
                "indeterminate" => 'e',
                "dynamic-indeterminate" => 'f',
                _ => unreachable!(),
            },
        );
        let consumer = intent_revision_test_command(
            &format!("{terminal_kind}-consumer"),
            INTENT_REVISION_CONSUMER_COMMAND_KIND,
            true,
        );
        assert!(matches!(
            store.admit_code_command(consumer.clone()).unwrap(),
            CodeCommandAdmission::Execute { .. }
        ));
        let claim = intent_revision_consumption_claim(
            &source,
            &recovery,
            &terminal,
            &format!("{terminal_kind}-intent"),
            consumer.clone(),
        );
        let consumption = store
            .prepare_intent_revision_consumption(&consumer, &claim)
            .expect("resolve the exact durable consumer before injecting a wrong terminal");
        match terminal_kind {
            "failure" => {
                store
                    .complete_code_command_failure(
                        &consumer.identity,
                        "arbitrary failure must not impersonate startup recovery",
                    )
                    .unwrap();
            }
            "indeterminate" => {
                store
                    .mark_code_command_indeterminate(
                        &consumer.identity,
                        "wrong_recovery_effect",
                        "wrong recovery reason",
                    )
                    .unwrap();
            }
            "dynamic-indeterminate" => {
                store
                    .mark_code_command_indeterminate(
                        &consumer.identity,
                        "mutating_runtime_turn",
                        "failed to persist the IntentSpec revision consume boundary: injected test failure",
                    )
                    .unwrap();
            }
            _ => unreachable!(),
        }

        assert!(matches!(
            store.recover_pending_mutating_code_commands_for_review_and_phase1_prewrite(
                None,
                None,
                Some(&consumption),
            ),
            Err(CodeCommandStoreError::InvalidIntent)
        ));
    }
}

#[test]
fn intent_revision_recovery_accepts_exact_legacy_indeterminate_without_fencing() {
    const LEGACY_EFFECT: &str = "unknown_mutating_dispatch";
    const LEGACY_REASON: &str =
        "runtime stopped after durable intent; manual reconciliation is required";

    let tmp = tempfile::TempDir::new().unwrap();
    let store = SessionJsonlStore::new(tmp.path().join("legacy-indeterminate"));
    let (source, recovery, terminal) = durable_intent_revision_source(
        &store,
        "legacy-indeterminate-source",
        "legacy-indeterminate-review",
        "legacy-indeterminate-intent",
        '9',
    );
    let consumer = intent_revision_test_command(
        "legacy-indeterminate-consumer",
        INTENT_REVISION_CONSUMER_COMMAND_KIND,
        true,
    );
    assert!(matches!(
        store.admit_code_command(consumer.clone()).unwrap(),
        CodeCommandAdmission::Execute { .. }
    ));
    let claim = intent_revision_consumption_claim(
        &source,
        &recovery,
        &terminal,
        "legacy-indeterminate-intent",
        consumer.clone(),
    );
    let consumption = store
        .prepare_intent_revision_consumption(&consumer, &claim)
        .expect("resolve the exact historical revision consumer");
    store
        .mark_code_command_indeterminate(&consumer.identity, LEGACY_EFFECT, LEGACY_REASON)
        .expect("persist the exact historical generic recovery terminal");

    let outcome = store
        .recover_pending_mutating_code_commands_for_review_and_phase1_prewrite(
            None,
            None,
            Some(&consumption),
        )
        .expect("the exact historical generic recovery terminal remains compatible");
    assert!(outcome.fenced.is_empty());
    assert!(!outcome.phase1_prewrite_reattached);
    assert!(outcome.intent_revision_consumer_healed);
    assert!(matches!(
        store.recover_code_command(&consumer.identity).unwrap(),
        CodeCommandRecovery::Existing {
            status: CodeCommandStatus::Indeterminate { effect, reason }
        } if effect == LEGACY_EFFECT && reason == LEGACY_REASON
    ));
}

#[test]
fn intent_revision_recovery_accepts_exact_pre_receipt_failure_without_fencing() {
    const FAILURE_REASON: &str = "IntentSpec revision consumer stopped before its durable consumption receipt; the revision remains available for retry";

    let tmp = tempfile::TempDir::new().unwrap();
    let store = SessionJsonlStore::new(tmp.path().join("current-worker-failure"));
    let (source, recovery, terminal) = durable_intent_revision_source(
        &store,
        "current-worker-source",
        "current-worker-review",
        "current-worker-intent",
        '8',
    );
    let consumer = intent_revision_test_command(
        "current-worker-consumer",
        INTENT_REVISION_CONSUMER_COMMAND_KIND,
        true,
    );
    assert!(matches!(
        store.admit_code_command(consumer.clone()).unwrap(),
        CodeCommandAdmission::Execute { .. }
    ));
    let claim = intent_revision_consumption_claim(
        &source,
        &recovery,
        &terminal,
        "current-worker-intent",
        consumer.clone(),
    );
    let consumption = store
        .prepare_intent_revision_consumption(&consumer, &claim)
        .expect("resolve the exact current-worker revision consumer");
    store
        .complete_code_command_failure(&consumer.identity, FAILURE_REASON)
        .expect("persist the canonical current-worker pre-receipt failure");

    let outcome = store
        .recover_pending_mutating_code_commands_for_review_and_phase1_prewrite(
            None,
            None,
            Some(&consumption),
        )
        .expect("the exact current-worker pre-receipt failure remains retryable");
    assert!(outcome.fenced.is_empty());
    assert!(!outcome.phase1_prewrite_reattached);
    assert!(outcome.intent_revision_consumer_healed);
    assert!(matches!(
        store.recover_code_command(&consumer.identity).unwrap(),
        CodeCommandRecovery::Existing {
            status: CodeCommandStatus::Failed { reason }
        } if reason == FAILURE_REASON
    ));
    assert!(
        store
            .load_code_workflow_replay()
            .unwrap()
            .events
            .iter()
            .all(|event| !matches!(
                &event.event,
                CodeWorkflowEventKind::InteractionResolved {
                    intent_revision_consumption: Some(_),
                    ..
                }
            ))
    );
}

#[test]
fn intent_revision_recovery_accepts_exact_pre_receipt_indeterminate_without_fencing() {
    const EFFECT: &str = "mutating_runtime_turn";
    const REASON: &str = "IntentSpec revision consumption stopped before its durable receipt; the revision remains available for retry";

    let tmp = tempfile::TempDir::new().unwrap();
    let store = SessionJsonlStore::new(tmp.path().join("current-worker-indeterminate"));
    let (source, recovery, terminal) = durable_intent_revision_source(
        &store,
        "current-worker-indeterminate-source",
        "current-worker-indeterminate-review",
        "current-worker-indeterminate-intent",
        '7',
    );
    let consumer = intent_revision_test_command(
        "current-worker-indeterminate-consumer",
        INTENT_REVISION_CONSUMER_COMMAND_KIND,
        true,
    );
    assert!(matches!(
        store.admit_code_command(consumer.clone()).unwrap(),
        CodeCommandAdmission::Execute { .. }
    ));
    let claim = intent_revision_consumption_claim(
        &source,
        &recovery,
        &terminal,
        "current-worker-indeterminate-intent",
        consumer.clone(),
    );
    let consumption = store
        .prepare_intent_revision_consumption(&consumer, &claim)
        .expect("resolve the exact current-worker pre-receipt consumer");
    store
        .mark_code_command_indeterminate(&consumer.identity, EFFECT, REASON)
        .expect("persist the canonical current-worker pre-receipt Indeterminate terminal");

    let outcome = store
        .recover_pending_mutating_code_commands_for_review_and_phase1_prewrite(
            None,
            None,
            Some(&consumption),
        )
        .expect("the exact current-worker pre-receipt Indeterminate remains retryable");
    assert!(outcome.fenced.is_empty());
    assert!(!outcome.phase1_prewrite_reattached);
    assert!(outcome.intent_revision_consumer_healed);
    assert!(matches!(
        store.recover_code_command(&consumer.identity).unwrap(),
        CodeCommandRecovery::Existing {
            status: CodeCommandStatus::Indeterminate { effect, reason }
        } if effect == EFFECT && reason == REASON
    ));
    assert!(
        store
            .load_code_workflow_replay()
            .unwrap()
            .events
            .iter()
            .all(|event| !matches!(
                &event.event,
                CodeWorkflowEventKind::InteractionResolved {
                    intent_revision_consumption: Some(_),
                    ..
                }
            ))
    );
}

#[test]
fn intent_revision_recovery_rejects_a_later_web_command_after_the_consuming_owner() {
    const FAILURE_REASON: &str = "IntentSpec revision consumer stopped before its durable consumption receipt; the revision remains available for retry";

    for later_status in ["pending", "failed", "succeeded"] {
        let tmp = tempfile::TempDir::new().unwrap();
        let store = SessionJsonlStore::new(tmp.path().join(later_status));
        let (source, recovery, terminal) = durable_intent_revision_source(
            &store,
            &format!("{later_status}-later-source"),
            &format!("{later_status}-later-review"),
            &format!("{later_status}-later-intent"),
            match later_status {
                "pending" => 'a',
                "failed" => 'b',
                "succeeded" => 'c',
                _ => unreachable!(),
            },
        );
        let consuming_owner = intent_revision_test_command(
            &format!("{later_status}-consuming-owner"),
            INTENT_REVISION_CONSUMER_COMMAND_KIND,
            true,
        );
        store
            .admit_code_command(consuming_owner.clone())
            .expect("admit the original Consuming owner");
        let owner_claim = intent_revision_consumption_claim(
            &source,
            &recovery,
            &terminal,
            &format!("{later_status}-later-intent"),
            consuming_owner.clone(),
        );
        let owner_consumption = store
            .prepare_intent_revision_consumption(&consuming_owner, &owner_claim)
            .expect("prepare the original Consuming owner");
        store
            .complete_code_command_failure(&consuming_owner.identity, FAILURE_REASON)
            .expect("terminalize the original owner with the canonical retryable failure");

        let later = intent_revision_test_command(
            &format!("{later_status}-later-command"),
            INTENT_REVISION_CONSUMER_COMMAND_KIND,
            true,
        );
        store
            .admit_code_command(later.clone())
            .expect("admit the later durable Web command");
        match later_status {
            "pending" => {}
            "failed" => {
                store
                    .complete_code_command_failure(&later.identity, "later unrelated failure")
                    .unwrap();
            }
            "succeeded" => {
                store
                    .complete_code_command_success(&later.identity, "later unrelated success")
                    .unwrap();
            }
            _ => unreachable!(),
        }

        assert!(
            store
                .recover_pending_mutating_code_commands_for_review_and_phase1_prewrite(
                    None,
                    None,
                    Some(&owner_consumption),
                )
                .is_err(),
            "a {later_status} later Web command must invalidate stale Consuming authority"
        );
    }
}

#[test]
fn intent_revision_recovery_allows_a_second_canonical_aborted_consumer() {
    const FAILURE_REASON: &str = "IntentSpec revision consumer stopped before its durable consumption receipt; the revision remains available for retry";

    let tmp = tempfile::TempDir::new().unwrap();
    let store = SessionJsonlStore::new(tmp.path().join("second-aborted-consumer"));
    let (source, recovery, terminal) = durable_intent_revision_source(
        &store,
        "replacement-source",
        "replacement-review",
        "replacement-intent",
        '6',
    );
    let consumer_a = intent_revision_test_command(
        "replacement-consumer-a",
        INTENT_REVISION_CONSUMER_COMMAND_KIND,
        true,
    );
    store
        .admit_code_command(consumer_a.clone())
        .expect("admit first aborted consumer");
    let claim_a = intent_revision_consumption_claim(
        &source,
        &recovery,
        &terminal,
        "replacement-intent",
        consumer_a.clone(),
    );
    let consumption_a = store
        .prepare_intent_revision_consumption(&consumer_a, &claim_a)
        .expect("prepare first aborted consumer");
    store
        .complete_code_command_failure(&consumer_a.identity, FAILURE_REASON)
        .expect("persist first canonical aborted-consumer tombstone");

    let consumer_b = intent_revision_test_command(
        "replacement-consumer-b",
        INTENT_REVISION_CONSUMER_COMMAND_KIND,
        true,
    );
    store
        .admit_code_command(consumer_b.clone())
        .expect("admit replacement consumer B");
    let claim_b = intent_revision_consumption_claim(
        &source,
        &recovery,
        &terminal,
        "replacement-intent",
        consumer_b.clone(),
    );
    let consumption_b = store
        .prepare_intent_revision_consumption(&consumer_b, &claim_b)
        .expect("prepare replacement Consuming owner B");
    store
        .complete_code_command_failure(&consumer_b.identity, FAILURE_REASON)
        .expect("persist replacement B's canonical pre-receipt failure");

    let outcome = store
        .recover_pending_mutating_code_commands_for_review_and_phase1_prewrite(
            None,
            None,
            Some(&consumption_b),
        )
        .expect("the latest replacement consumer must survive its second pre-receipt crash");
    assert!(outcome.fenced.is_empty());
    assert!(!outcome.phase1_prewrite_reattached);
    assert!(outcome.intent_revision_consumer_healed);
    assert!(
        store
            .recover_pending_mutating_code_commands_for_review_and_phase1_prewrite(
                None,
                None,
                Some(&consumption_a),
            )
            .is_err(),
        "the earlier aborted consumer must not become current again"
    );
    for consumer in [&consumer_a, &consumer_b] {
        assert!(matches!(
            store.recover_code_command(&consumer.identity).unwrap(),
            CodeCommandRecovery::Existing {
                status: CodeCommandStatus::Failed { reason }
            } if reason == FAILURE_REASON
        ));
    }
}

#[test]
fn intent_revision_recovery_distinguishes_prior_prestart_cancel_from_current_owner() {
    const CANCEL_REASON: &str = "runtime turn cancelled before a mutating side effect began";
    const RECOVERY_REASON: &str = "IntentSpec revision consumer stopped before its durable consumption receipt; the revision remains available for retry";

    let tmp = tempfile::TempDir::new().unwrap();
    let store = SessionJsonlStore::new(tmp.path().join("prior-prestart-cancel"));
    let (source, recovery, terminal) = durable_intent_revision_source(
        &store,
        "prestart-source",
        "prestart-review",
        "prestart-intent",
        '1',
    );
    let cancelled_a = intent_revision_test_command(
        "prestart-cancelled-a",
        INTENT_REVISION_CONSUMER_COMMAND_KIND,
        true,
    );
    store
        .admit_code_command(cancelled_a.clone())
        .expect("admit the consumer cancelled before start");
    store
        .complete_code_command_failure(&cancelled_a.identity, CANCEL_REASON)
        .expect("persist the exact pre-start cancellation terminal");

    let consumer_b = intent_revision_test_command(
        "prestart-replacement-b",
        INTENT_REVISION_CONSUMER_COMMAND_KIND,
        true,
    );
    store
        .admit_code_command(consumer_b.clone())
        .expect("admit replacement Consuming owner B");
    let claim_b = intent_revision_consumption_claim(
        &source,
        &recovery,
        &terminal,
        "prestart-intent",
        consumer_b.clone(),
    );
    let consumption_b = store
        .prepare_intent_revision_consumption(&consumer_b, &claim_b)
        .expect("prepare replacement B after A's pre-start cancellation");
    let outcome = store
        .recover_pending_mutating_code_commands_for_review_and_phase1_prewrite(
            None,
            None,
            Some(&consumption_b),
        )
        .expect("prior pre-start cancellation must not invalidate current Consuming B");
    assert!(outcome.fenced.is_empty());
    assert!(outcome.intent_revision_consumer_healed);
    assert!(matches!(
        store.recover_code_command(&consumer_b.identity).unwrap(),
        CodeCommandRecovery::Existing {
            status: CodeCommandStatus::Failed { reason }
        } if reason == RECOVERY_REASON
    ));

    let current_tmp = tempfile::TempDir::new().unwrap();
    let current_store = SessionJsonlStore::new(current_tmp.path().join("current-cancel"));
    let (source, recovery, terminal) = durable_intent_revision_source(
        &current_store,
        "current-cancel-source",
        "current-cancel-review",
        "current-cancel-intent",
        '0',
    );
    let current = intent_revision_test_command(
        "current-cancel-consumer",
        INTENT_REVISION_CONSUMER_COMMAND_KIND,
        true,
    );
    current_store
        .admit_code_command(current.clone())
        .expect("admit the current Consuming owner");
    let current_claim = intent_revision_consumption_claim(
        &source,
        &recovery,
        &terminal,
        "current-cancel-intent",
        current.clone(),
    );
    let current_consumption = current_store
        .prepare_intent_revision_consumption(&current, &current_claim)
        .expect("prepare the current cancellation-negative fixture");
    current_store
        .complete_code_command_failure(&current.identity, CANCEL_REASON)
        .expect("persist cancellation terminal on the current Consuming owner");
    let outcome = current_store
        .recover_pending_mutating_code_commands_for_review_and_phase1_prewrite(
            None,
            None,
            Some(&current_consumption),
        )
        .expect("the exact current pre-start cancellation remains safely retryable");
    assert!(outcome.fenced.is_empty());
    assert!(outcome.intent_revision_consumer_healed);
    assert!(matches!(
        current_store.recover_code_command(&current.identity).unwrap(),
        CodeCommandRecovery::Existing {
            status: CodeCommandStatus::Failed { reason }
        } if reason == CANCEL_REASON
    ));
}

#[test]
fn intent_revision_terminal_exact_retry_rejects_marker_appended_after_terminal() {
    let tmp = tempfile::TempDir::new().unwrap();
    let store = SessionJsonlStore::new(tmp.path().join("session"));
    let source = intent_revision_test_command(
        "terminal-before-marker",
        INTENT_REVISION_CONSUMER_COMMAND_KIND,
        true,
    );
    store.admit_code_command(source.clone()).unwrap();
    let recovery = IntentRevisionRecovery {
        interaction_id: "late-review".to_string(),
        sidecar_digest: format!("hmac-sha256:{}", "c".repeat(64)),
    };
    store
        .append_code_workflow_durable(
            CodeWorkflowEventKind::CommandTerminalSuccessWithInteractionResolved {
                command: source.identity.clone(),
                summary: "revision requested".to_string(),
                interaction_id: recovery.interaction_id.clone(),
                resolution: "modify".to_string(),
                prior_interaction_resolutions: Vec::new(),
                intent_revision: Some(recovery.clone()),
            },
        )
        .unwrap();
    store
        .append_code_workflow_durable(CodeWorkflowEventKind::IntentReviewRequested {
            interaction_id: recovery.interaction_id.clone(),
            intent_id: "late-intent".to_string(),
            turn_id: "late-review-gate".to_string(),
            phase0_turn_id: source.identity.command_id.clone(),
        })
        .unwrap();

    assert!(matches!(
        store.complete_code_command_success_with_interaction_resolutions_and_intent_revision(
            &source.identity,
            "revision requested",
            &[(recovery.interaction_id.clone(), "modify".to_string())],
            Some(&recovery),
        ),
        Err(CodeCommandStoreError::InvalidIntent)
    ));
}

#[test]
fn pending_resolution_checkpoint_reuses_legacy_event_with_command_bound_history() {
    let decoded: CodeWorkflowEventKind = serde_json::from_value(serde_json::json!({
        "event": "interaction_resolved",
        "interaction_id": "legacy-risk",
        "resolution": "answered"
    }))
    .expect("legacy InteractionResolved rows must keep decoding");
    match decoded {
        CodeWorkflowEventKind::InteractionResolved {
            interaction_id,
            resolution,
            command,
            prior_interaction_resolutions,
            intent_revision_consumption,
        } => {
            assert_eq!(interaction_id, "legacy-risk");
            assert_eq!(resolution, "answered");
            assert_eq!(command, None);
            assert!(prior_interaction_resolutions.is_empty());
            assert!(intent_revision_consumption.is_none());
        }
        other => panic!("decoded the wrong legacy workflow variant: {other:?}"),
    }

    let tmp = tempfile::TempDir::new().unwrap();
    let jsonl = SessionJsonlStore::new(tmp.path().join("session"));
    let identity = CodeCommandIdentity::new("repo-a", "session-a", "alice", "pending-checkpoint");
    let intent = CodeCommandIntent::new(
        identity.clone(),
        "agent_turn",
        "sha256:pending-checkpoint",
        false,
    );
    assert!(matches!(
        jsonl.admit_code_command(intent.clone()).unwrap(),
        CodeCommandAdmission::Execute { .. }
    ));
    let legacy = jsonl
        .append_code_workflow_durable(CodeWorkflowEventKind::InteractionResolved {
            interaction_id: "legacy-input".to_string(),
            resolution: "answered".to_string(),
            command: None,
            prior_interaction_resolutions: Vec::new(),
            intent_revision_consumption: None,
        })
        .unwrap();
    let legacy_wire = serde_json::to_value(&legacy.event).unwrap();
    assert_eq!(legacy_wire["event"], "interaction_resolved");
    assert!(legacy_wire.get("command").is_none());
    assert!(legacy_wire.get("prior_interaction_resolutions").is_none());

    let resolutions = vec![
        ("risk-profile".to_string(), "answered".to_string()),
        ("user-input".to_string(), "answered".to_string()),
    ];
    jsonl
        .checkpoint_pending_interaction_resolutions(&identity, &resolutions)
        .unwrap();
    jsonl
        .checkpoint_pending_interaction_resolutions(&identity, &resolutions)
        .expect("an identical graceful-shutdown checkpoint retry must be idempotent");

    let replay = jsonl.load_code_workflow_replay().unwrap();
    assert!(
        replay.gaps.is_empty(),
        "the legacy row must advance sequence"
    );
    let checkpoints = replay
        .events
        .iter()
        .filter_map(|event| match &event.event {
            CodeWorkflowEventKind::InteractionResolved {
                interaction_id,
                resolution,
                command: Some(command),
                prior_interaction_resolutions,
                ..
            } if command == &identity => Some((
                event.sequence,
                interaction_id.clone(),
                resolution.clone(),
                prior_interaction_resolutions.clone(),
            )),
            _ => None,
        })
        .collect::<Vec<_>>();
    assert_eq!(
        checkpoints,
        vec![(
            legacy.sequence + 1,
            "user-input".to_string(),
            "answered".to_string(),
            vec![("risk-profile".to_string(), "answered".to_string())],
        )],
        "the checkpoint must be one compatible command-bound row with the last response primary"
    );
    assert!(matches!(
        jsonl.admit_code_command(intent).unwrap(),
        CodeCommandAdmission::Existing {
            status: CodeCommandStatus::Pending
        }
    ));
}

#[test]
fn pending_resolution_checkpoint_extends_long_history_and_exact_retry_is_a_noop() {
    let tmp = tempfile::TempDir::new().unwrap();
    let jsonl = SessionJsonlStore::new(tmp.path().join("session"));
    let identity =
        CodeCommandIdentity::new("repo-a", "session-a", "alice", "long-checkpoint-history");
    let intent = CodeCommandIntent::new(
        identity.clone(),
        "agent_turn",
        "sha256:long-checkpoint-history",
        false,
    );
    assert!(matches!(
        jsonl.admit_code_command(intent.clone()).unwrap(),
        CodeCommandAdmission::Execute { .. }
    ));

    let extended = (0..257)
        .map(|index| (format!("interaction-{index}"), "answered".to_string()))
        .collect::<Vec<_>>();
    let prefix = extended[..128].to_vec();
    jsonl
        .checkpoint_pending_interaction_resolutions(&identity, &prefix)
        .unwrap();
    jsonl
        .checkpoint_pending_interaction_resolutions(&identity, &extended)
        .unwrap();
    let before_retry = jsonl.load_code_workflow_replay().unwrap();
    jsonl
        .checkpoint_pending_interaction_resolutions(&identity, &extended)
        .expect("an exact retry of the extended checkpoint must be a no-op");
    let after_retry = jsonl.load_code_workflow_replay().unwrap();
    assert_eq!(after_retry.events, before_retry.events);

    let checkpoints = after_retry
        .events
        .iter()
        .filter_map(|event| match &event.event {
            CodeWorkflowEventKind::InteractionResolved {
                interaction_id,
                resolution,
                command: Some(command),
                prior_interaction_resolutions,
                ..
            } if command == &identity => Some((
                event.sequence,
                interaction_id.clone(),
                resolution.clone(),
                prior_interaction_resolutions.clone(),
            )),
            _ => None,
        })
        .collect::<Vec<_>>();
    assert_eq!(checkpoints.len(), 2);
    assert_eq!(checkpoints[0].1.as_str(), "interaction-127");
    assert_eq!(checkpoints[0].2.as_str(), "answered");
    assert_eq!(checkpoints[0].3.as_slice(), &prefix[..127]);
    assert_eq!(checkpoints[1].0, checkpoints[0].0 + 1);
    assert_eq!(checkpoints[1].1.as_str(), "interaction-256");
    assert_eq!(checkpoints[1].2.as_str(), "answered");
    assert_eq!(checkpoints[1].3.as_slice(), &extended[..256]);
    assert!(matches!(
        jsonl.admit_code_command(intent).unwrap(),
        CodeCommandAdmission::Existing {
            status: CodeCommandStatus::Pending
        }
    ));
}

#[test]
fn combined_terminal_keeps_primary_gate_and_prior_resolution_in_one_replay_row() {
    let tmp = tempfile::TempDir::new().unwrap();
    let session_root = tmp.path().join("session");
    let writer = SessionJsonlStore::new(session_root.clone());
    let identity = CodeCommandIdentity::new("repo-a", "session-a", "alice", "cmd-intent-confirm");
    let intent = CodeCommandIntent::new(
        identity.clone(),
        "headless_direct_turn",
        "sha256:intent-confirm-with-risk",
        true,
    );
    assert!(matches!(
        writer.admit_code_command(intent).unwrap(),
        CodeCommandAdmission::Execute { .. }
    ));
    writer
        .append_code_workflow_durable(CodeWorkflowEventKind::IntentReviewRequested {
            interaction_id: "intent-review-primary".to_string(),
            intent_id: "intent-primary".to_string(),
            turn_id: "intent-review-gate-turn".to_string(),
            phase0_turn_id: identity.command_id.clone(),
        })
        .unwrap();
    let resolutions = vec![
        ("risk-profile".to_string(), "answered".to_string()),
        ("intent-review-primary".to_string(), "confirm".to_string()),
    ];
    writer
        .complete_code_command_success_with_interaction_resolutions(
            &identity,
            "Intent confirmed",
            &resolutions,
        )
        .unwrap();

    let replay = SessionJsonlStore::new(session_root)
        .load_code_workflow_replay()
        .unwrap();
    let combined = replay
        .events
        .iter()
        .filter_map(|event| match &event.event {
            CodeWorkflowEventKind::CommandTerminalSuccessWithInteractionResolved {
                command,
                interaction_id,
                resolution,
                prior_interaction_resolutions,
                ..
            } if command == &identity => Some((
                interaction_id.clone(),
                resolution.clone(),
                prior_interaction_resolutions.clone(),
            )),
            _ => None,
        })
        .collect::<Vec<_>>();
    assert_eq!(
        combined,
        vec![(
            "intent-review-primary".to_string(),
            "confirm".to_string(),
            vec![("risk-profile".to_string(), "answered".to_string())],
        )]
    );
    assert!(replay.events.iter().all(|event| !matches!(
        &event.event,
        CodeWorkflowEventKind::InteractionResolved { .. }
    )));
    assert!(
        libra::internal::ai::runtime::phase0::open_intent_review_from_workflow(
            replay.events.iter().map(|event| &event.event)
        )
        .is_none(),
        "the legacy primary fields must close the Intent authority"
    );
}

#[test]
fn legacy_command_terminal_failure_defaults_interaction_resolutions_to_empty() {
    let identity = CodeCommandIdentity::new(
        "repo-legacy",
        "session-legacy",
        "principal-legacy",
        "command-legacy",
    );
    let decoded: CodeWorkflowEventKind = serde_json::from_value(serde_json::json!({
        "event": "command_terminal_failure",
        "command": identity,
        "reason": "legacy cancellation"
    }))
    .expect("legacy failure row without interaction_resolutions must remain readable");
    match decoded {
        CodeWorkflowEventKind::CommandTerminalFailure {
            interaction_resolutions,
            retry_intent_review,
            ..
        } => {
            assert!(interaction_resolutions.is_empty());
            assert!(
                retry_intent_review.is_none(),
                "legacy failure rows must default the additive retry authority to absent"
            );
        }
        other => panic!("decoded the wrong legacy workflow variant: {other:?}"),
    }
}

#[test]
fn command_terminal_failure_atomically_roundtrips_phase1_retry_authority() {
    let tmp = tempfile::TempDir::new().unwrap();
    let store = SessionJsonlStore::new(tmp.path().join("session"));
    let identity =
        CodeCommandIdentity::new("repo-a", "session-a", "alice", "phase1-retry-terminal");
    let intent = CodeCommandIntent::new(
        identity.clone(),
        "headless_direct_turn",
        "sha256:phase1-retry-terminal",
        true,
    );
    assert!(matches!(
        store.admit_code_command(intent).unwrap(),
        CodeCommandAdmission::Execute { .. }
    ));
    let retry = Phase1RetryIntentReview {
        interaction_id: "intent-review-retry-a".to_string(),
        intent_id: "intent-a".to_string(),
        intent_spec_id: "intent-spec-a".to_string(),
        source_interaction_id: "intent-review-source-a".to_string(),
        source_resolution: "confirm".to_string(),
        source_phase1_turn_id: identity.command_id.clone(),
        start_seed_digest: "a".repeat(64),
    };

    store
        .complete_code_command_failure_with_interaction_resolutions_and_retry_intent_review(
            &identity,
            "pre-formal planning failed",
            &[],
            Some(&retry),
        )
        .unwrap();
    let before_retry = store.load_code_workflow_replay().unwrap();
    assert_eq!(
        before_retry
            .events
            .iter()
            .filter(|event| matches!(
                &event.event,
                CodeWorkflowEventKind::CommandTerminalFailure {
                    command,
                    retry_intent_review: Some(persisted),
                    ..
                } if command == &identity && persisted == &retry
            ))
            .count(),
        1,
        "the failed command and replacement review authority must share one row"
    );
    assert_eq!(
        phase1_retry_intent_review_state(
            before_retry.events.iter().map(|event| &event.event),
            &identity.command_id,
        )
        .unwrap(),
        Phase1RetryIntentReviewState::Open(retry.clone())
    );
    assert_eq!(
        libra::internal::ai::runtime::phase0::open_intent_review_from_workflow(
            before_retry.events.iter().map(|event| &event.event)
        )
        .map(|(interaction_id, intent_id, ..)| (interaction_id, intent_id)),
        Some((retry.interaction_id.clone(), retry.intent_id.clone()))
    );

    store
        .complete_code_command_failure_with_interaction_resolutions_and_retry_intent_review(
            &identity,
            "pre-formal planning failed",
            &[],
            Some(&retry),
        )
        .expect("an exact retry must ACK the same atomic terminal payload");
    assert_eq!(
        store.load_code_workflow_replay().unwrap().events,
        before_retry.events,
        "an exact retry must not append a second failed terminal or retry authority"
    );

    store
        .append_code_workflow_durable(CodeWorkflowEventKind::InteractionResolved {
            interaction_id: retry.interaction_id.clone(),
            resolution: "cancel".to_string(),
            command: None,
            prior_interaction_resolutions: Vec::new(),
            intent_revision_consumption: None,
        })
        .unwrap();
    let resolved = store.load_code_workflow_replay().unwrap();
    assert_eq!(
        phase1_retry_intent_review_state(
            resolved.events.iter().map(|event| &event.event),
            &identity.command_id,
        )
        .unwrap(),
        Phase1RetryIntentReviewState::Resolved {
            review: retry,
            resolution: "cancel".to_string(),
        }
    );
}

#[test]
fn command_terminal_failure_roundtrips_one_and_two_interaction_resolutions() {
    let tmp = tempfile::TempDir::new().unwrap();
    let session_root = tmp.path().join("session");
    let writer = SessionJsonlStore::new(session_root.clone());

    for count in 1..=2 {
        let identity = CodeCommandIdentity::new(
            "repo-a",
            "session-a",
            "alice",
            format!("cmd-failed-after-{count}-interactions"),
        );
        let intent = CodeCommandIntent::new(
            identity.clone(),
            "agent_turn",
            format!("sha256:failed-after-{count}-interactions"),
            false,
        );
        assert!(matches!(
            writer.admit_code_command(intent).unwrap(),
            CodeCommandAdmission::Execute { .. }
        ));
        let expected = (1..=count)
            .map(|index| {
                (
                    format!("user-input-{count}-{index}"),
                    "answered".to_string(),
                )
            })
            .collect::<Vec<_>>();
        assert!(matches!(
            writer
                .complete_code_command_failure_with_interaction_resolutions(
                    &identity,
                    "turn cancelled",
                    &expected,
                )
                .unwrap(),
            CodeCommandStatus::Failed { .. }
        ));

        let reopened = SessionJsonlStore::new(session_root.clone());
        let replay = reopened.load_code_workflow_replay().unwrap();
        let persisted = replay.events.iter().find_map(|event| match &event.event {
            CodeWorkflowEventKind::CommandTerminalFailure {
                command,
                interaction_resolutions,
                ..
            } if command == &identity => Some(interaction_resolutions.clone()),
            _ => None,
        });
        assert_eq!(
            persisted,
            Some(expected),
            "failure terminal must replay all {count} delivered interaction resolution(s)"
        );
    }
}

#[test]
fn phase1_context_budget_rejects_the_65th_file_in_the_actual_session_directory() {
    let tmp = tempfile::TempDir::new().unwrap();
    let session_store = SessionStore::from_storage_path(tmp.path());
    let session = SessionState::new("/repo/main");
    let jsonl = SessionJsonlStore::new(session_store.session_root(&session.id));

    for index in 0..64 {
        let path = phase1_review_context_path(&jsonl, &format!("review-{index}"));
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, []).unwrap();
    }
    fs::write(
        jsonl.session_root().join("phase1/pending-start.json"),
        b"seed",
    )
    .unwrap();
    fs::write(jsonl.session_root().join("phase1-writer.lock"), b"lock").unwrap();

    let error = validate_phase1_context_session_budget(&jsonl, &phase1_budget_context("review-65"))
        .expect_err("the 65th context sidecar must fail closed in the real session layout");
    assert_eq!(error.kind(), std::io::ErrorKind::StorageFull);
    assert!(error.to_string().contains("budget is exhausted"));
}

#[test]
fn phase1_context_budget_rejects_aggregate_bytes_in_the_actual_session_directory() {
    let tmp = tempfile::TempDir::new().unwrap();
    let session_store = SessionStore::from_storage_path(tmp.path());
    let session = SessionState::new("/repo/main");
    let jsonl = SessionJsonlStore::new(session_store.session_root(&session.id));
    let existing = phase1_review_context_path(&jsonl, "large-review");
    fs::create_dir_all(existing.parent().unwrap()).unwrap();
    let file = fs::File::create(existing).unwrap();
    file.set_len(32 * 1024 * 1024).unwrap();

    let error =
        validate_phase1_context_session_budget(&jsonl, &phase1_budget_context("next-review"))
            .expect_err("aggregate context bytes beyond 32 MiB must fail closed");
    assert_eq!(error.kind(), std::io::ErrorKind::StorageFull);
    assert!(error.to_string().contains("budget is exhausted"));
}

#[test]
fn startup_gc_preserves_reachable_phase1_context_and_removes_orphan() {
    let tmp = tempfile::TempDir::new().unwrap();
    let jsonl = SessionJsonlStore::new(tmp.path().join("session"));
    let reachable = phase1_budget_context("reachable-context");
    let orphan = phase1_budget_context("orphan-context");
    persist_phase1_review_context(&jsonl, &reachable).unwrap();
    persist_phase1_review_context(&jsonl, &orphan).unwrap();
    jsonl
        .append_code_workflow_durable(CodeWorkflowEventKind::PlanReviewRequested {
            interaction_id: "reachable-plan-gate".to_string(),
            plan_id: "reachable-plan".to_string(),
            turn_id: "reachable-review-turn".to_string(),
            phase1_turn_id: "reachable-phase1-turn".to_string(),
            context_id: reachable.interaction_id.clone(),
            revision_of: None,
            prepared_from_network: None,
        })
        .unwrap();

    assert_eq!(gc_unreachable_phase1_review_contexts(&jsonl).unwrap(), 1);
    assert_eq!(
        load_phase1_review_context(&jsonl, &reachable.interaction_id)
            .unwrap()
            .interaction_id,
        reachable.interaction_id
    );
    assert!(
        !phase1_review_context_path(&jsonl, &orphan.interaction_id).exists(),
        "startup GC must remove a context with no durable gate or revision authority"
    );
}

#[test]
fn startup_gc_defers_orphan_collection_while_phase1_start_seed_is_pending() {
    let tmp = tempfile::TempDir::new().unwrap();
    let jsonl = SessionJsonlStore::new(tmp.path().join("session"));
    let orphan = phase1_budget_context("seed-straddled-context");
    persist_phase1_review_context(&jsonl, &orphan).unwrap();
    let seed = phase1_start_seed("source-intent-review", Some("browser-command"));
    persist_phase1_start_seed(&jsonl, &seed).unwrap();

    assert_eq!(
        gc_unreachable_phase1_review_contexts(&jsonl).unwrap(),
        0,
        "a seed may straddle formal context persistence and its review marker"
    );
    assert!(phase1_review_context_path(&jsonl, &orphan.interaction_id).exists());
    assert_eq!(
        load_phase1_start_seed(&jsonl)
            .unwrap()
            .expect("pending seed survives GC")
            .browser_command_id
            .as_deref(),
        Some("browser-command")
    );
}

#[test]
fn startup_gc_preserves_context_reachable_from_promoted_network_gate() {
    let tmp = tempfile::TempDir::new().unwrap();
    let jsonl = SessionJsonlStore::new(tmp.path().join("session"));
    let reachable = phase1_budget_context("network-context");
    let orphan = phase1_budget_context("network-orphan");
    persist_phase1_review_context(&jsonl, &reachable).unwrap();
    persist_phase1_review_context(&jsonl, &orphan).unwrap();
    jsonl
        .append_code_workflow_durable(CodeWorkflowEventKind::PlanReviewRequested {
            interaction_id: "network-plan-review".to_string(),
            plan_id: "network-plan".to_string(),
            turn_id: "network-plan-turn".to_string(),
            phase1_turn_id: "network-phase1-turn".to_string(),
            context_id: reachable.interaction_id.clone(),
            revision_of: None,
            prepared_from_network: None,
        })
        .unwrap();
    jsonl
        .append_code_workflow_durable(CodeWorkflowEventKind::NetworkPolicyRequested {
            interaction_id: "network-gate".to_string(),
            plan_id: "network-plan".to_string(),
            turn_id: "network-turn".to_string(),
            default_allow: false,
        })
        .unwrap();
    jsonl
        .append_code_workflow_durable(CodeWorkflowEventKind::InteractionResolved {
            interaction_id: "network-plan-review".to_string(),
            resolution: "execute".to_string(),
            command: None,
            prior_interaction_resolutions: Vec::new(),
            intent_revision_consumption: None,
        })
        .unwrap();

    assert_eq!(gc_unreachable_phase1_review_contexts(&jsonl).unwrap(), 1);
    assert!(phase1_review_context_path(&jsonl, &reachable.interaction_id).exists());
    assert!(!phase1_review_context_path(&jsonl, &orphan.interaction_id).exists());
}

#[test]
fn startup_gc_preserves_context_reachable_from_pending_modify_revision() {
    let tmp = tempfile::TempDir::new().unwrap();
    let jsonl = SessionJsonlStore::new(tmp.path().join("session"));
    let reachable = phase1_budget_context("modify-context");
    let orphan = phase1_budget_context("modify-orphan");
    persist_phase1_review_context(&jsonl, &reachable).unwrap();
    persist_phase1_review_context(&jsonl, &orphan).unwrap();
    jsonl
        .append_code_workflow_durable(CodeWorkflowEventKind::PlanReviewRequested {
            interaction_id: "modify-plan-review".to_string(),
            plan_id: "modify-plan".to_string(),
            turn_id: "modify-plan-turn".to_string(),
            phase1_turn_id: "modify-phase1-turn".to_string(),
            context_id: reachable.interaction_id.clone(),
            revision_of: None,
            prepared_from_network: None,
        })
        .unwrap();
    jsonl
        .append_code_workflow_durable(CodeWorkflowEventKind::InteractionResolved {
            interaction_id: "modify-plan-review".to_string(),
            resolution: "modify".to_string(),
            command: None,
            prior_interaction_resolutions: Vec::new(),
            intent_revision_consumption: None,
        })
        .unwrap();

    assert_eq!(gc_unreachable_phase1_review_contexts(&jsonl).unwrap(), 1);
    assert!(phase1_review_context_path(&jsonl, &reachable.interaction_id).exists());
    assert!(!phase1_review_context_path(&jsonl, &orphan.interaction_id).exists());
}

#[cfg(unix)]
#[test]
fn startup_gc_rejects_matching_shape_context_symlink() {
    use std::os::unix::fs::symlink;

    let tmp = tempfile::TempDir::new().unwrap();
    let jsonl = SessionJsonlStore::new(tmp.path().join("session"));
    let target = tmp.path().join("outside-context.json");
    fs::write(
        &target,
        serde_json::to_vec(&phase1_budget_context("outside")).unwrap(),
    )
    .unwrap();
    let link = phase1_review_context_path(&jsonl, "symlink-context");
    fs::create_dir_all(link.parent().unwrap()).unwrap();
    symlink(&target, &link).unwrap();

    let error = gc_unreachable_phase1_review_contexts(&jsonl)
        .expect_err("a matching-shape symlink must fail closed instead of being followed");
    assert_eq!(error.kind(), std::io::ErrorKind::InvalidData);
    assert!(error.to_string().contains("not a regular file"));
    assert!(link.is_symlink());
    assert!(target.exists());
}

#[test]
fn startup_gc_rejects_context_whose_body_identity_does_not_match_path() {
    let tmp = tempfile::TempDir::new().unwrap();
    let jsonl = SessionJsonlStore::new(tmp.path().join("session"));
    let path = phase1_review_context_path(&jsonl, "path-context");
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    fs::write(
        &path,
        serde_json::to_vec(&phase1_budget_context("body-context")).unwrap(),
    )
    .unwrap();

    let error = gc_unreachable_phase1_review_contexts(&jsonl)
        .expect_err("GC must not delete or adopt a context stored under another identity");
    assert_eq!(error.kind(), std::io::ErrorKind::InvalidData);
    assert!(error.to_string().contains("mismatched immutable identity"));
    assert!(
        path.exists(),
        "identity mismatch must preserve evidence for repair"
    );
}

#[test]
fn plan_revision_preserves_parent_reason_and_only_unchanged_step_identity() {
    let context = phase1_budget_context("revision-context");
    let fresh_plan = || compile_submitted_plan(&context.intent_spec, &context.plan_draft).unwrap();
    let prior = fresh_plan();
    let prior_step_id = prior.tasks[0].step_id();

    let mut revised = fresh_plan();
    let fresh_unchanged_step_id = revised.tasks[0].step_id();
    assert_ne!(fresh_unchanged_step_id, prior_step_id);
    revised.parent_revision = Some(prior.revision);
    revised.revision = prior.revision + 1;
    revised.replan_reason = Some("Keep the behavior but clarify verification".to_string());
    preserve_unchanged_revision_steps(&mut revised, &prior);
    assert_eq!(revised.parent_revision, Some(prior.revision));
    assert_eq!(revised.revision, prior.revision + 1);
    assert_eq!(
        revised.replan_reason.as_deref(),
        Some("Keep the behavior but clarify verification")
    );
    assert_eq!(revised.tasks[0].step_id(), prior_step_id);

    let changed_draft = SubmitPlanDraftArgs {
        explanation: context.plan_draft.explanation.clone(),
        steps: vec![PlanDraftStep {
            title: "Change the aggregate Phase 1 context policy".to_string(),
        }],
    };
    let mut changed = compile_submitted_plan(&context.intent_spec, &changed_draft).unwrap();
    let fresh_changed_step_id = changed.tasks[0].step_id();
    preserve_unchanged_revision_steps(&mut changed, &prior);
    assert_eq!(
        changed.tasks[0].step_id(),
        fresh_changed_step_id,
        "semantically changed work must keep its newly allocated step identity"
    );
    assert_ne!(changed.tasks[0].step_id(), prior_step_id);

    let mut description_changed = fresh_plan();
    let fresh_description_step_id = description_changed.tasks[0].step_id();
    description_changed.tasks[0]
        .task
        .set_description(Some("Use a different durability algorithm".to_string()));
    preserve_unchanged_revision_steps(&mut description_changed, &prior);
    assert_eq!(
        description_changed.tasks[0].step_id(),
        fresh_description_step_id,
        "a changed task description must not inherit the prior step provenance"
    );
    assert_ne!(description_changed.tasks[0].step_id(), prior_step_id);

    let mut checks_changed = fresh_plan();
    let fresh_checks_step_id = checks_changed.tasks[0].step_id();
    checks_changed.tasks[0].checks.push(Check {
        id: "revision-durability-check".to_string(),
        kind: CheckKind::Command,
        command: Some("cargo test --test ai_session_jsonl_test".to_string()),
        timeout_seconds: Some(30),
        expected_exit_code: Some(0),
        required: true,
        artifacts_produced: Vec::new(),
    });
    preserve_unchanged_revision_steps(&mut checks_changed, &prior);
    assert_eq!(
        checks_changed.tasks[0].step_id(),
        fresh_checks_step_id,
        "changed verification checks must not inherit the prior step provenance"
    );
    assert_ne!(checks_changed.tasks[0].step_id(), prior_step_id);

    let mut dependency_changed = fresh_plan();
    let dependency_draft = SubmitPlanDraftArgs {
        explanation: Some("Prepare a prerequisite".to_string()),
        steps: vec![PlanDraftStep {
            title: "Prepare the durability prerequisite".to_string(),
        }],
    };
    let dependency = compile_submitted_plan(&context.intent_spec, &dependency_draft)
        .unwrap()
        .tasks
        .into_iter()
        .next()
        .unwrap();
    let dependency_id = dependency.id();
    dependency_changed.tasks.push(dependency);
    let fresh_dependency_step_id = dependency_changed.tasks[0].step_id();
    dependency_changed.tasks[0]
        .task
        .add_dependency(dependency_id);
    preserve_unchanged_revision_steps(&mut dependency_changed, &prior);
    assert_eq!(
        dependency_changed.tasks[0].step_id(),
        fresh_dependency_step_id,
        "a changed dependency edge must not inherit the prior step provenance"
    );
    assert_ne!(dependency_changed.tasks[0].step_id(), prior_step_id);

    let mut duplicate_prior = fresh_plan();
    duplicate_prior
        .tasks
        .push(fresh_plan().tasks.into_iter().next().unwrap());
    let duplicate_prior_step_ids = duplicate_prior
        .tasks
        .iter()
        .map(|task| task.step_id())
        .collect::<Vec<_>>();
    assert_ne!(duplicate_prior_step_ids[0], duplicate_prior_step_ids[1]);
    let mut duplicate_revised = fresh_plan();
    duplicate_revised
        .tasks
        .push(fresh_plan().tasks.into_iter().next().unwrap());
    let duplicate_fresh_step_ids = duplicate_revised
        .tasks
        .iter()
        .map(|task| task.step_id())
        .collect::<Vec<_>>();
    preserve_unchanged_revision_steps(&mut duplicate_revised, &duplicate_prior);
    assert_eq!(
        duplicate_revised
            .tasks
            .iter()
            .map(|task| task.step_id())
            .collect::<Vec<_>>(),
        duplicate_prior_step_ids,
        "duplicate-title semantic matches must consume prior steps one-to-one"
    );
    assert_ne!(duplicate_fresh_step_ids, duplicate_prior_step_ids);
}

#[test]
fn phase1_start_seed_roundtrip_preserves_revision_parent_pair_reason_and_step_id() {
    let context = phase1_budget_context("seed-revision-context");
    let mut prior_plan = compile_submitted_plan(&context.intent_spec, &context.plan_draft).unwrap();
    prior_plan.revision = 7;
    let prior_step_id = prior_plan.tasks[0].step_id();
    let mut seed = phase1_start_seed("source-plan-review", Some("browser-revision-command"));
    seed.revision_note = Some("Split verification from implementation".to_string());
    seed.prior_plan = Some(prior_plan);
    seed.prior_plan_id = Some("prior-plan-summary-id".to_string());
    seed.prior_persisted_plan = Phase1PersistedPlan::Persisted {
        execution_plan_id: "parent-execution-plan".to_string(),
        test_plan_id: "parent-test-plan".to_string(),
    };
    let expected_digest = seed.durable_digest().unwrap();

    let tmp = tempfile::TempDir::new().unwrap();
    let jsonl = SessionJsonlStore::new(tmp.path().join("session"));
    persist_phase1_start_seed(&jsonl, &seed).unwrap();
    let restored = load_phase1_start_seed(&jsonl)
        .unwrap()
        .expect("revision seed must round-trip");

    assert_eq!(restored.durable_digest().unwrap(), expected_digest);
    assert_eq!(restored.attempt_id, "ai-session-jsonl-attempt");
    assert_eq!(
        restored.revision_note.as_deref(),
        Some("Split verification from implementation")
    );
    assert_eq!(
        restored.prior_plan_id.as_deref(),
        Some("prior-plan-summary-id")
    );
    assert!(matches!(
        restored.prior_persisted_plan,
        Phase1PersistedPlan::Persisted {
            ref execution_plan_id,
            ref test_plan_id,
        } if execution_plan_id == "parent-execution-plan" && test_plan_id == "parent-test-plan"
    ));
    let restored_prior = restored.prior_plan.expect("prior normalized plan");
    assert_eq!(restored_prior.revision, 7);
    assert_eq!(restored_prior.tasks[0].step_id(), prior_step_id);

    let mut original_attempt = seed.clone();
    original_attempt.browser_command_id = None;
    let mut next_attempt = original_attempt.clone();
    next_attempt.attempt_id = "ai-session-jsonl-next-attempt".to_string();
    assert_ne!(
        phase1_turn_id_from_seed(&original_attempt).unwrap(),
        phase1_turn_id_from_seed(&next_attempt).unwrap(),
        "server-generated retries must receive a new durable turn identity from attempt_id"
    );
}

#[test]
fn gate_authority_validator_rejects_two_open_plan_generations() {
    let tmp = tempfile::TempDir::new().unwrap();
    let jsonl = SessionJsonlStore::new(tmp.path().join("session"));
    for suffix in ["a", "b"] {
        jsonl
            .append_code_workflow_durable(CodeWorkflowEventKind::PlanReviewRequested {
                interaction_id: format!("plan-review-{suffix}"),
                plan_id: format!("plan-{suffix}"),
                turn_id: format!("review-turn-{suffix}"),
                phase1_turn_id: format!("phase1-turn-{suffix}"),
                context_id: format!("plan-context-{suffix}"),
                revision_of: None,
                prepared_from_network: None,
            })
            .unwrap();
    }

    let error = validate_single_open_gate_authority(&jsonl)
        .expect_err("two distinct open Plan gates must require reconciliation");
    assert_eq!(error.kind(), std::io::ErrorKind::InvalidData);
    assert!(
        error.to_string().contains("intent=0, plan=2, network=0"),
        "validator must report the conflicting Plan authorities: {error}"
    );
}

#[test]
fn gate_authority_validator_rejects_open_intent_and_plan() {
    let tmp = tempfile::TempDir::new().unwrap();
    let jsonl = SessionJsonlStore::new(tmp.path().join("session"));
    jsonl
        .append_code_workflow_durable(CodeWorkflowEventKind::IntentReviewRequested {
            interaction_id: "intent-review".to_string(),
            intent_id: "intent-spec".to_string(),
            turn_id: "intent-review-turn".to_string(),
            phase0_turn_id: "phase0-turn".to_string(),
        })
        .unwrap();
    jsonl
        .append_code_workflow_durable(CodeWorkflowEventKind::PlanReviewRequested {
            interaction_id: "plan-review".to_string(),
            plan_id: "plan".to_string(),
            turn_id: "plan-review-turn".to_string(),
            phase1_turn_id: "phase1-turn".to_string(),
            context_id: "plan-context".to_string(),
            revision_of: None,
            prepared_from_network: None,
        })
        .unwrap();

    let error = validate_single_open_gate_authority(&jsonl)
        .expect_err("simultaneous Intent and Plan gates must require reconciliation");
    assert_eq!(error.kind(), std::io::ErrorKind::InvalidData);
    assert!(
        error.to_string().contains("intent=1, plan=1, network=0"),
        "validator must report the cross-phase authority conflict: {error}"
    );
}

#[test]
fn gate_authority_validator_rejects_two_promoted_network_gates() {
    let tmp = tempfile::TempDir::new().unwrap();
    let jsonl = SessionJsonlStore::new(tmp.path().join("session"));
    for suffix in ["a", "b"] {
        let review_id = format!("plan-review-{suffix}");
        let plan_id = format!("plan-{suffix}");
        jsonl
            .append_code_workflow_durable(CodeWorkflowEventKind::PlanReviewRequested {
                interaction_id: review_id.clone(),
                plan_id: plan_id.clone(),
                turn_id: format!("review-turn-{suffix}"),
                phase1_turn_id: format!("phase1-turn-{suffix}"),
                context_id: format!("plan-context-{suffix}"),
                revision_of: None,
                prepared_from_network: None,
            })
            .unwrap();
        jsonl
            .append_code_workflow_durable(CodeWorkflowEventKind::NetworkPolicyRequested {
                interaction_id: format!("network-policy-{suffix}"),
                plan_id,
                turn_id: format!("network-turn-{suffix}"),
                default_allow: false,
            })
            .unwrap();
        jsonl
            .append_code_workflow_durable(CodeWorkflowEventKind::InteractionResolved {
                interaction_id: review_id,
                resolution: "execute".to_string(),
                command: None,
                prior_interaction_resolutions: Vec::new(),
                intent_revision_consumption: None,
            })
            .unwrap();
    }

    let error = validate_single_open_gate_authority(&jsonl)
        .expect_err("two promoted Network gates must require reconciliation");
    assert_eq!(error.kind(), std::io::ErrorKind::InvalidData);
    assert!(
        error.to_string().contains("intent=0, plan=0, network=2"),
        "validator must count only the promoted Network authorities: {error}"
    );
}

#[test]
fn back_prepare_drop_or_rollback_before_network_resolution_preserves_network_authority() {
    let tmp = tempfile::TempDir::new().unwrap();
    let jsonl = SessionJsonlStore::new(tmp.path().join("session"));
    let mut reachable = phase1_budget_context("back-source-context");
    reachable.persisted_plan = Phase1PersistedPlan::Persisted {
        execution_plan_id: "back-plan".to_string(),
        test_plan_id: "back-test-plan".to_string(),
    };
    let orphan = phase1_budget_context("back-drop-orphan");
    persist_phase1_review_context(&jsonl, &reachable).unwrap();
    persist_phase1_review_context(&jsonl, &orphan).unwrap();
    append_back_provisional_workflow(&jsonl, &reachable.interaction_id);

    let replay = jsonl.load_code_workflow_replay().unwrap();
    assert!(
        open_plan_review_from_workflow(replay.events.iter().map(|event| &event.event)).is_none(),
        "a prepared Back marker is provisional until the Network response is durable"
    );
    assert_eq!(
        open_network_policy_from_workflow(replay.events.iter().map(|event| &event.event))
            .map(|(interaction_id, ..)| interaction_id),
        Some("back-network-review".to_string()),
        "a process drop after prepare must restore the still-authoritative Network gate"
    );
    validate_single_open_gate_authority(&jsonl).unwrap();
    assert_eq!(gc_unreachable_phase1_review_contexts(&jsonl).unwrap(), 1);
    assert!(phase1_review_context_path(&jsonl, &reachable.interaction_id).exists());
    assert!(!phase1_review_context_path(&jsonl, &orphan.interaction_id).exists());

    jsonl
        .append_code_workflow_durable(CodeWorkflowEventKind::InteractionResolved {
            interaction_id: "back-plan-review".to_string(),
            resolution: "back-prepare-rollback".to_string(),
            command: None,
            prior_interaction_resolutions: Vec::new(),
            intent_revision_consumption: None,
        })
        .unwrap();
    let replay = jsonl.load_code_workflow_replay().unwrap();
    assert!(
        open_plan_review_from_workflow(replay.events.iter().map(|event| &event.event)).is_none(),
        "rollback before the Network Back is consumed must not activate the prepared Plan"
    );
    assert_eq!(
        open_network_policy_from_workflow(replay.events.iter().map(|event| &event.event))
            .map(|(interaction_id, ..)| interaction_id),
        Some("back-network-review".to_string())
    );
    validate_single_open_gate_authority(&jsonl).unwrap();
    assert_eq!(gc_unreachable_phase1_review_contexts(&jsonl).unwrap(), 0);
}

#[test]
fn durable_back_activates_prepared_plan_and_late_rollback_cannot_close_it() {
    let tmp = tempfile::TempDir::new().unwrap();
    let jsonl = SessionJsonlStore::new(tmp.path().join("session"));
    let mut reachable = phase1_budget_context("back-activated-context");
    reachable.persisted_plan = Phase1PersistedPlan::Persisted {
        execution_plan_id: "back-plan".to_string(),
        test_plan_id: "back-test-plan".to_string(),
    };
    persist_phase1_review_context(&jsonl, &reachable).unwrap();
    append_back_provisional_workflow(&jsonl, &reachable.interaction_id);
    jsonl
        .append_code_workflow_durable(CodeWorkflowEventKind::InteractionResolved {
            interaction_id: "back-network-review".to_string(),
            resolution: "back".to_string(),
            command: None,
            prior_interaction_resolutions: Vec::new(),
            intent_revision_consumption: None,
        })
        .unwrap();

    let replay = jsonl.load_code_workflow_replay().unwrap();
    assert_eq!(
        open_plan_review_from_workflow(replay.events.iter().map(|event| &event.event))
            .map(|(interaction_id, ..)| interaction_id),
        Some("back-plan-review".to_string()),
        "durable Network Back must activate the prepared replacement Plan"
    );
    assert!(
        open_network_policy_from_workflow(replay.events.iter().map(|event| &event.event)).is_none(),
        "the consumed Network gate must be demoted after Back"
    );
    validate_single_open_gate_authority(&jsonl).unwrap();
    assert_eq!(gc_unreachable_phase1_review_contexts(&jsonl).unwrap(), 0);
    assert!(phase1_review_context_path(&jsonl, &reachable.interaction_id).exists());

    // A retry can append the same prepare marker and then discover that the
    // Network response was already consumed. Its synthetic rollback is stale:
    // it must not close the Plan generation activated by the first attempt.
    jsonl
        .append_code_workflow_durable(CodeWorkflowEventKind::PlanReviewRequested {
            interaction_id: "back-plan-review".to_string(),
            plan_id: "back-plan".to_string(),
            turn_id: "back-plan-turn".to_string(),
            phase1_turn_id: String::new(),
            context_id: reachable.interaction_id.clone(),
            revision_of: None,
            prepared_from_network: Some("back-network-review".to_string()),
        })
        .unwrap();
    jsonl
        .append_code_workflow_durable(CodeWorkflowEventKind::InteractionResolved {
            interaction_id: "back-plan-review".to_string(),
            resolution: "back-prepare-rollback".to_string(),
            command: None,
            prior_interaction_resolutions: Vec::new(),
            intent_revision_consumption: None,
        })
        .unwrap();
    let replay = jsonl.load_code_workflow_replay().unwrap();
    assert_eq!(
        open_plan_review_from_workflow(replay.events.iter().map(|event| &event.event))
            .map(|(interaction_id, ..)| interaction_id),
        Some("back-plan-review".to_string())
    );
    assert!(
        open_network_policy_from_workflow(replay.events.iter().map(|event| &event.event)).is_none()
    );
    validate_single_open_gate_authority(&jsonl).unwrap();
}

#[test]
fn late_or_conflicting_plan_resolution_retry_cannot_rearm_replaced_generation() {
    let tmp = tempfile::TempDir::new().unwrap();
    let session_root = tmp.path().join("session");
    let writer = SessionJsonlStore::new(session_root.clone());
    let stale_writer = SessionJsonlStore::new(session_root);
    let identity = CodeCommandIdentity::new("repo-a", "session-a", "alice", "cmd-source");
    let intent = CodeCommandIntent::new(
        identity.clone(),
        "respond_interaction",
        "sha256:source-modify",
        false,
    );
    let source_modify = vec![("plan-review-source".to_string(), "modify".to_string())];

    assert!(matches!(
        writer.admit_code_command(intent).unwrap(),
        CodeCommandAdmission::Execute { .. }
    ));
    writer
        .append_code_workflow_durable(CodeWorkflowEventKind::PlanReviewRequested {
            interaction_id: "plan-review-source".to_string(),
            plan_id: "plan-source".to_string(),
            turn_id: "review-turn-source".to_string(),
            phase1_turn_id: "phase1-turn-source".to_string(),
            context_id: "plan-context-source".to_string(),
            revision_of: None,
            prepared_from_network: None,
        })
        .unwrap();
    writer
        .complete_code_command_success_with_interaction_resolutions(
            &identity,
            "plan review modified",
            &source_modify,
        )
        .unwrap();
    writer
        .append_code_workflow_durable(CodeWorkflowEventKind::PlanReviewRequested {
            interaction_id: "plan-review-replacement".to_string(),
            plan_id: "plan-replacement".to_string(),
            turn_id: "review-turn-replacement".to_string(),
            phase1_turn_id: "phase1-turn-replacement".to_string(),
            context_id: "plan-context-replacement".to_string(),
            revision_of: Some("plan-review-source".to_string()),
            prepared_from_network: None,
        })
        .unwrap();
    let events_before_retry = writer.load_code_workflow_replay().unwrap();
    assert_eq!(
        pending_plan_revision_from_workflow(
            events_before_retry.events.iter().map(|event| &event.event)
        ),
        None,
        "replacement gate consumes the source Modify revision request"
    );

    stale_writer
        .complete_code_command_success_with_interaction_resolutions(
            &identity,
            "plan review modified",
            &source_modify,
        )
        .expect("an identical late retry is an idempotent success");
    let events_after_retry = writer.load_code_workflow_replay().unwrap();
    assert_eq!(
        events_after_retry.events, events_before_retry.events,
        "a late identical retry must not append a source-generation resolution"
    );
    assert_eq!(
        pending_plan_revision_from_workflow(
            events_after_retry.events.iter().map(|event| &event.event)
        ),
        None,
        "a late source retry must not re-arm the already-replaced revision"
    );

    let conflict = stale_writer
        .complete_code_command_success_with_interaction_resolutions(
            &identity,
            "plan review modified",
            &[("plan-review-source".to_string(), "execute".to_string())],
        )
        .unwrap_err();
    assert!(matches!(
        conflict,
        CodeCommandStoreError::TerminalConflict { .. }
    ));
    assert_eq!(
        writer.load_code_workflow_replay().unwrap().events,
        events_before_retry.events,
        "a conflicting stale resolution must not mutate the durable log"
    );
}

#[test]
fn failed_phase1_revision_attempt_preserves_modify_authority_for_new_attempt_id() {
    let tmp = tempfile::TempDir::new().unwrap();
    let jsonl = SessionJsonlStore::new(tmp.path().join("session"));
    jsonl
        .append_code_workflow_durable(CodeWorkflowEventKind::PlanReviewRequested {
            interaction_id: "revision-source".to_string(),
            plan_id: "revision-plan-v1".to_string(),
            turn_id: "revision-review-turn-v1".to_string(),
            phase1_turn_id: "revision-phase1-turn-v1".to_string(),
            context_id: "revision-context-v1".to_string(),
            revision_of: None,
            prepared_from_network: None,
        })
        .unwrap();
    jsonl
        .append_code_workflow_durable(CodeWorkflowEventKind::InteractionResolved {
            interaction_id: "revision-source".to_string(),
            resolution: "modify".to_string(),
            command: None,
            prior_interaction_resolutions: Vec::new(),
            intent_revision_consumption: None,
        })
        .unwrap();

    let first_identity =
        CodeCommandIdentity::new("repo-a", "session-a", "alice", "revision-attempt-1");
    let first = CodeCommandIntent::new(
        first_identity.clone(),
        "headless_direct_turn",
        "sha256:same-revision-note",
        true,
    );
    assert!(matches!(
        jsonl.admit_code_command(first.clone()).unwrap(),
        CodeCommandAdmission::Execute { .. }
    ));
    jsonl
        .complete_code_command_failure(&first_identity, "oversized plan draft")
        .unwrap();
    let replay = jsonl.load_code_workflow_replay().unwrap();
    assert_eq!(
        pending_plan_revision_from_workflow(replay.events.iter().map(|event| &event.event))
            .as_deref(),
        Some("revision-source"),
        "a determinate pre-write failure must not consume the durable Modify authority"
    );

    let second_identity =
        CodeCommandIdentity::new("repo-a", "session-a", "alice", "revision-attempt-2");
    let second = CodeCommandIntent::new(
        second_identity.clone(),
        first.command_kind.clone(),
        first.canonical_request_hash.clone(),
        true,
    );
    assert!(matches!(
        jsonl.admit_code_command(second).unwrap(),
        CodeCommandAdmission::Execute { .. }
    ));
    jsonl
        .append_code_workflow_durable(CodeWorkflowEventKind::PlanReviewRequested {
            interaction_id: "revision-replacement".to_string(),
            plan_id: "revision-plan-v2".to_string(),
            turn_id: "revision-review-turn-v2".to_string(),
            phase1_turn_id: second_identity.command_id.clone(),
            context_id: "revision-context-v2".to_string(),
            revision_of: Some("revision-source".to_string()),
            prepared_from_network: None,
        })
        .unwrap();
    jsonl
        .complete_code_command_success(&second_identity, "revision plan durable")
        .unwrap();

    let replay = jsonl.load_code_workflow_replay().unwrap();
    assert_eq!(
        pending_plan_revision_from_workflow(replay.events.iter().map(|event| &event.event)),
        None,
        "only the durable replacement Plan generation consumes Modify authority"
    );
    assert_eq!(
        replay
            .events
            .iter()
            .filter(|event| matches!(
                &event.event,
                CodeWorkflowEventKind::PlanReviewRequested {
                    revision_of: Some(source),
                    ..
                } if source == "revision-source"
            ))
            .count(),
        1
    );
    assert_ne!(first_identity.command_id, second_identity.command_id);
}

fn append_back_provisional_workflow(jsonl: &SessionJsonlStore, context_id: &str) {
    jsonl
        .append_code_workflow_durable(CodeWorkflowEventKind::PlanReviewRequested {
            interaction_id: "back-source-review".to_string(),
            plan_id: "back-plan".to_string(),
            turn_id: "back-source-plan-turn".to_string(),
            phase1_turn_id: "back-source-phase1-turn".to_string(),
            context_id: context_id.to_string(),
            revision_of: None,
            prepared_from_network: None,
        })
        .unwrap();
    jsonl
        .append_code_workflow_durable(CodeWorkflowEventKind::NetworkPolicyRequested {
            interaction_id: "back-network-review".to_string(),
            plan_id: "back-plan".to_string(),
            turn_id: "back-network-turn".to_string(),
            default_allow: false,
        })
        .unwrap();
    jsonl
        .append_code_workflow_durable(CodeWorkflowEventKind::InteractionResolved {
            interaction_id: "back-source-review".to_string(),
            resolution: "execute".to_string(),
            command: None,
            prior_interaction_resolutions: Vec::new(),
            intent_revision_consumption: None,
        })
        .unwrap();
    jsonl
        .append_code_workflow_durable(CodeWorkflowEventKind::PlanReviewRequested {
            interaction_id: "back-plan-review".to_string(),
            plan_id: "back-plan".to_string(),
            turn_id: "back-plan-turn".to_string(),
            phase1_turn_id: String::new(),
            context_id: context_id.to_string(),
            revision_of: None,
            prepared_from_network: Some("back-network-review".to_string()),
        })
        .unwrap();
}

fn phase1_budget_context(interaction_id: &str) -> Phase1ReviewContext {
    let intent_spec = resolve_intentspec(
        IntentDraft {
            intent: DraftIntent {
                summary: "Exercise Phase 1 context budget".to_string(),
                problem_statement: "Phase 1 context sidecars need an aggregate bound".to_string(),
                change_type: ChangeType::Test,
                objectives: vec![Objective {
                    title: "Reject an over-budget context".to_string(),
                    kind: ObjectiveKind::Analysis,
                }],
                in_scope: vec!["src/internal/ai/runtime/phase1.rs".to_string()],
                out_of_scope: vec![],
                touch_hints: None,
            },
            acceptance: DraftAcceptance {
                success_criteria: vec!["The context budget fails closed".to_string()],
                fast_checks: vec![],
                integration_checks: vec![],
                security_checks: vec![],
                release_checks: vec![],
            },
            risk: DraftRisk {
                rationale: "test fixture".to_string(),
                factors: vec![],
                level: Some(RiskLevel::Low),
            },
        },
        RiskLevel::Low,
        ResolveContext {
            working_dir: "/repo/main".to_string(),
            base_ref: "HEAD".to_string(),
            created_by_id: "ai-session-jsonl-test".to_string(),
        },
    );
    let intent_spec_id = intent_spec.metadata.id.clone();
    Phase1ReviewContext {
        schema_version: Phase1ReviewContext::SCHEMA_VERSION,
        interaction_id: interaction_id.to_string(),
        intent_id: "persisted-intent-revision".to_string(),
        intent_spec_id: intent_spec_id.clone(),
        persisted_plan: Phase1PersistedPlan::Unavailable,
        intent_spec,
        plan_draft: SubmitPlanDraftArgs {
            explanation: None,
            steps: vec![PlanDraftStep {
                title: "Keep aggregate Phase 1 context bounded".to_string(),
            }],
        },
        execution_plan: ExecutionPlanSpec {
            intent_spec_id,
            revision: 1,
            parent_revision: None,
            replan_reason: None,
            tasks: vec![],
            max_parallel: 1,
            checkpoints: vec![],
        },
        default_allow_network: false,
        checkout: Phase1CheckoutBinding {
            canonical_working_dir: "/repo/main".to_string(),
            repo_locator: "/repo/main".to_string(),
            repo_id: "repo-id".to_string(),
            workspace_fingerprint: "0".repeat(64),
            workspace_change_token: String::new(),
            base_ref: "HEAD".to_string(),
            head_oid: Some("0".repeat(40)),
            branch_label: "main".to_string(),
            worktree_id: None,
        },
    }
}

fn phase1_start_seed(
    source_interaction_id: &str,
    browser_command_id: Option<&str>,
) -> Phase1StartSeed {
    let context = phase1_budget_context("seed-context-template");
    Phase1StartSeed {
        schema_version: Phase1StartSeed::SCHEMA_VERSION,
        source_interaction_id: source_interaction_id.to_string(),
        intent_id: context.intent_id,
        intent_spec_id: context.intent_spec_id,
        intent_spec_json: serde_json::to_string(&context.intent_spec).unwrap(),
        source_resolution: "confirm".to_string(),
        revision_note: None,
        checkout: context.checkout,
        prior_plan: None,
        prior_plan_id: None,
        prior_persisted_plan: Phase1PersistedPlan::Unavailable,
        browser_command_id: browser_command_id.map(str::to_string),
        attempt_id: "ai-session-jsonl-attempt".to_string(),
    }
}

fn assert_event_trait(event: &dyn Event) {
    assert_eq!(event.event_kind(), "session_snapshot");
    assert_ne!(event.event_id(), uuid::Uuid::nil());
    assert!(event.event_summary().contains("session"));
}
