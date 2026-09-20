//! KEEP session JSONL pins after RC-23 deleted Code workflow / phase1 SCC.
//! Only session-store roundtrip, unknown-event skip, malformed-line reject,
//! and concurrent legacy-JSON migration remain.

use std::{
    fs::{self, OpenOptions},
    io::Write,
    sync::{Arc, Barrier},
    thread,
};

use chrono::Utc;
use libra::internal::ai::{
    event::Event,
    session::{SessionState, SessionStore, jsonl::SessionJsonlStore},
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

fn assert_event_trait(event: &dyn Event) {
    assert_eq!(event.event_kind(), "session_snapshot");
    assert_ne!(event.event_id(), uuid::Uuid::nil());
    assert!(event.event_summary().contains("session"));
}
