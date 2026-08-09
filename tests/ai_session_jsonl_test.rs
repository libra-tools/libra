use std::{
    fs::{self, OpenOptions},
    io::Write,
    sync::{Arc, Barrier},
    thread,
};

use chrono::Utc;
use libra::internal::ai::{
    runtime::event::Event,
    session::{
        SessionState, SessionStore,
        jsonl::{
            CodeCommandAdmission, CodeCommandIdentity, CodeCommandIntent, CodeCommandRecovery,
            CodeCommandStatus, CodeCommandStoreError, CodeWorkflowEvent, CodeWorkflowEventKind,
            SessionEvent, SessionJsonlStore,
        },
    },
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

fn assert_event_trait(event: &dyn Event) {
    assert_eq!(event.event_kind(), "session_snapshot");
    assert_ne!(event.event_id(), uuid::Uuid::nil());
    assert!(event.event_summary().contains("session"));
}
