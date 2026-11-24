//! GK-02: synthetic Grok `updates.jsonl` keeps each thought chunk in order.
//!
//! Fixtures are written under a temp session directory. Nothing here opens
//! `GROK_HOME` or starts `grok`.

use std::{
    fs::{self, File, OpenOptions},
    io::Write,
    path::Path,
};

use libra::internal::ai::{
    hooks::{
        lifecycle::SessionHookEnvelope,
        providers::{
            grok::{
                encode_grok_cwd, install_marked_grok_hooks, observe_grok_hook,
                uninstall_marked_grok_hooks,
            },
            grok_provider,
        },
    },
    observed_agents::{
        AgentKind, AgentStability, agent_for,
        grok_reasoning::{
            GrokMergedEvent, GrokReasoningError, export_grok_ordinary, import_grok_reasoning,
            sink_bundle,
        },
        grok_updates::{
            GROK_LINE_MAX_BYTES, GrokReadCursor, GrokTranscriptEvent, GrokUpdatesError,
            read_grok_updates, session_file,
        },
        live_capture_matches_hook_installable, registration_for,
        transcript_source::TRANSCRIPT_READ_HARD_CAP_BYTES,
    },
};
use sha2::{Digest, Sha256};

fn thought_line(event_id: &str, prompt_id: &str, text: &str, timestamp: &str) -> String {
    serde_json::json!({
        "timestamp": timestamp,
        "method": "session/update",
        "params": {
            "sessionId": "sess-synthetic",
            "update": {
                "sessionUpdate": "agent_thought_chunk",
                "promptId": prompt_id,
                "content": { "type": "text", "text": text }
            },
            "_meta": { "eventId": event_id }
        }
    })
    .to_string()
}

fn tool_line(event_id: &str, title: &str) -> String {
    serde_json::json!({
        "timestamp": "2026-10-18T00:00:00Z",
        "method": "session/update",
        "params": {
            "sessionId": "sess-synthetic",
            "update": {
                "sessionUpdate": "tool_call",
                "title": title,
                "toolCallId": "call-1"
            },
            "_meta": { "eventId": event_id }
        }
    })
    .to_string()
}

fn turn_line(event_id: &str, reasoning_tokens: u64) -> String {
    serde_json::json!({
        "timestamp": "2026-10-18T00:00:00Z",
        "method": "session/update",
        "params": {
            "sessionId": "sess-synthetic",
            "update": {
                "sessionUpdate": "turn_completed",
                "usage": { "reasoningTokens": reasoning_tokens }
            },
            "_meta": { "eventId": event_id }
        }
    })
    .to_string()
}

fn write_jsonl(dir: &Path, lines: &[String]) -> std::path::PathBuf {
    let path = session_file(dir, "updates.jsonl");
    let mut body = String::new();
    for line in lines {
        body.push_str(line);
        body.push('\n');
    }
    fs::write(&path, body).expect("write synthetic updates.jsonl");
    path
}

fn thought_texts(events: &[GrokTranscriptEvent]) -> Vec<&str> {
    events
        .iter()
        .filter_map(|event| match event {
            GrokTranscriptEvent::Thought { text, .. } => Some(text.as_str()),
            _ => None,
        })
        .collect()
}

#[test]
fn grok_updates_keeps_three_thoughts_in_order() {
    let session = tempfile::tempdir().expect("session dir");
    let path = write_jsonl(
        session.path(),
        &[
            thought_line("e1", "p1", "alpha", "2026-10-18T00:00:00Z"),
            tool_line("e2", "shell"),
            thought_line("e3", "p2", "beta", "2026-10-18T00:00:00Z"),
            turn_line("e4", 17),
            thought_line("e5", "p3", "gamma", "2026-10-18T00:00:00Z"),
        ],
    );
    let read = read_grok_updates(session.path(), &path, &GrokReadCursor::default())
        .expect("read synthetic updates");
    assert!(read.is_complete());
    assert_eq!(thought_texts(&read.events), ["alpha", "beta", "gamma"]);
    assert_eq!(
        read.events,
        vec![
            GrokTranscriptEvent::Thought {
                text: "alpha".to_string(),
                prompt_id: "p1".to_string(),
                event_id: "e1".to_string(),
            },
            GrokTranscriptEvent::Tool {
                name: "shell".to_string(),
                event_id: "e2".to_string(),
            },
            GrokTranscriptEvent::Thought {
                text: "beta".to_string(),
                prompt_id: "p2".to_string(),
                event_id: "e3".to_string(),
            },
            GrokTranscriptEvent::Turn {
                reasoning_tokens: 17,
                event_id: "e4".to_string(),
            },
            GrokTranscriptEvent::Thought {
                text: "gamma".to_string(),
                prompt_id: "p3".to_string(),
                event_id: "e5".to_string(),
            },
        ]
    );
    assert!(
        thought_texts(&read.events).iter().all(|text| *text != "17"),
        "reasoningTokens is a count, not thought text"
    );
}

#[test]
fn grok_updates_string_chunks_are_not_concatenated() {
    let session = tempfile::tempdir().expect("session dir");
    let path = write_jsonl(
        session.path(),
        &[
            thought_line("c1", "same-prompt", "left-", "2026-10-18T00:00:01Z"),
            thought_line("c2", "same-prompt", "right", "2026-10-18T00:00:02Z"),
        ],
    );
    let read = read_grok_updates(session.path(), &path, &GrokReadCursor::default())
        .expect("read synthetic updates");
    assert!(read.is_complete());
    assert_eq!(read.events.len(), 2);
    match &read.events[0] {
        GrokTranscriptEvent::Thought {
            text, prompt_id, ..
        } => {
            assert_eq!(text, "left-");
            assert_eq!(prompt_id, "same-prompt");
        }
        other => panic!("first chunk must stay its own thought, got {other:?}"),
    }
    match &read.events[1] {
        GrokTranscriptEvent::Thought {
            text, prompt_id, ..
        } => {
            assert_eq!(text, "right");
            assert_eq!(prompt_id, "same-prompt");
        }
        other => panic!("second chunk must stay its own thought, got {other:?}"),
    }
    assert!(
        read.events.iter().all(|event| match event {
            GrokTranscriptEvent::Thought { text, .. } => text != "left-right",
            _ => true,
        }),
        "same promptId chunks must not be joined"
    );
}

#[test]
fn grok_updates_decoded_utf8_escape_and_unicode() {
    let session = tempfile::tempdir().expect("session dir");
    // Rust escapes below are the JSON spelling: \" and \u754c. `café` is raw UTF-8.
    let line = "{\"timestamp\":\"t\",\"method\":\"session/update\",\"params\":{\"sessionId\":\"s\",\"update\":{\"sessionUpdate\":\"agent_thought_chunk\",\"promptId\":\"p\",\"content\":{\"type\":\"text\",\"text\":\"say \\\"\\u754c\\\" café\"}},\"_meta\":{\"eventId\":\"e-utf\"}}}";
    assert!(line.contains(r#"\u754c"#));
    assert!(line.contains(r#"\""#));
    assert!(line.contains("café"));
    let path = session_file(session.path(), "updates.jsonl");
    fs::write(&path, format!("{line}\n")).expect("write escaped line");
    let read = read_grok_updates(session.path(), &path, &GrokReadCursor::default())
        .expect("read escaped line");
    assert!(read.is_complete());
    assert_eq!(
        thought_texts(&read.events),
        ["say \"界\" café"],
        "JSON escapes and non-ASCII must be decoded UTF-8"
    );
}

#[test]
fn grok_updates_repeat_event_id_is_idempotent() {
    let session = tempfile::tempdir().expect("session dir");
    let path = write_jsonl(
        session.path(),
        &[
            thought_line("e1", "p", "alpha", "2026-10-18T00:00:00Z"),
            thought_line("e1", "p", "alpha-duplicate", "2026-10-18T00:00:09Z"),
            thought_line("e2", "p", "beta", "2026-10-18T00:00:00Z"),
            thought_line("e3", "p", "gamma", "2026-10-18T00:00:00Z"),
        ],
    );
    let first =
        read_grok_updates(session.path(), &path, &GrokReadCursor::default()).expect("first read");
    assert!(first.is_complete());
    assert_eq!(
        thought_texts(&first.events),
        ["alpha", "beta", "gamma"],
        "duplicate eventId is one thought; equal timestamps stay separate"
    );
    let cursor = GrokReadCursor {
        offset: first.next_offset,
        seen_event_ids: first.seen_event_ids.clone(),
    };
    let second = read_grok_updates(session.path(), &path, &cursor).expect("reread");
    assert!(
        second.events.is_empty(),
        "reread from the recorded offset adds nothing"
    );

    let appended = thought_line("e4", "p", "delta", "2026-10-17T00:00:00Z");
    let mut file = OpenOptions::new().append(true).open(&path).expect("append");
    writeln!(file, "{appended}").expect("write appended thought");
    drop(file);
    let third = read_grok_updates(session.path(), &path, &cursor).expect("resume");
    assert_eq!(thought_texts(&third.events), ["delta"]);
    assert!(
        third
            .events
            .iter()
            .all(|event| !matches!(event, GrokTranscriptEvent::Thought { text, .. } if text == "alpha" || text == "beta" || text == "gamma")),
        "resume must not merge the new row into earlier rows by timestamp"
    );

    let from_start = GrokReadCursor {
        offset: 0,
        seen_event_ids: first.seen_event_ids,
    };
    let replay = read_grok_updates(session.path(), &path, &from_start).expect("replay");
    assert_eq!(
        thought_texts(&replay.events),
        ["delta"],
        "a saved eventId set does not add the same thought again"
    );
}

#[test]
fn grok_updates_non_string_text_marks_partial() {
    let session = tempfile::tempdir().expect("session dir");
    let non_string = serde_json::json!({
        "timestamp": "t",
        "method": "session/update",
        "params": {
            "sessionId": "s",
            "update": {
                "sessionUpdate": "agent_thought_chunk",
                "promptId": "p",
                "content": { "type": "text", "text": { "nested": true } }
            },
            "_meta": { "eventId": "bad" }
        }
    })
    .to_string();
    let wrong_type = serde_json::json!({
        "timestamp": "t",
        "method": "session/update",
        "params": {
            "sessionId": "s",
            "update": {
                "sessionUpdate": "agent_thought_chunk",
                "promptId": "p",
                "content": { "type": "image", "text": "hidden" }
            },
            "_meta": { "eventId": "img" }
        }
    })
    .to_string();
    let path = write_jsonl(
        session.path(),
        &[
            thought_line("ok1", "p", "kept-before", "t"),
            non_string,
            wrong_type,
            thought_line("ok2", "p", "kept-after", "t"),
        ],
    );
    let read =
        read_grok_updates(session.path(), &path, &GrokReadCursor::default()).expect("partial read");
    assert!(read.partial);
    assert!(!read.is_complete());
    assert_eq!(thought_texts(&read.events), ["kept-before", "kept-after"]);
}

#[test]
fn grok_updates_unknown_session_update_keeps_type_and_hash() {
    let session = tempfile::tempdir().expect("session dir");
    let unknown = serde_json::json!({
        "timestamp": "t",
        "method": "session/update",
        "params": {
            "sessionId": "s",
            "update": { "sessionUpdate": "agent_custom_widget", "payload": 1 },
            "_meta": { "eventId": "u1" }
        }
    })
    .to_string();
    let expected_hash = format!("{:x}", Sha256::digest(unknown.as_bytes()));
    let path = write_jsonl(
        session.path(),
        &[thought_line("ok", "p", "real-thought", "t"), unknown],
    );
    let read = read_grok_updates(session.path(), &path, &GrokReadCursor::default())
        .expect("read unknown update");
    assert!(read.is_complete());
    assert_eq!(thought_texts(&read.events), ["real-thought"]);
    assert!(read.events.iter().any(|event| matches!(
        event,
        GrokTranscriptEvent::Unknown {
            session_update,
            line_sha256,
        } if session_update == "agent_custom_widget" && line_sha256 == &expected_hash
    )));
    assert!(
        read.events
            .iter()
            .all(|event| !matches!(event, GrokTranscriptEvent::Thought { text, .. } if text.contains("agent_custom_widget")))
    );
}

#[test]
fn grok_updates_oversized_line_is_not_complete() {
    let session = tempfile::tempdir().expect("session dir");
    let path = session_file(session.path(), "updates.jsonl");
    let mut file = File::create(&path).expect("create");
    let huge = "x".repeat((GROK_LINE_MAX_BYTES as usize) + 1);
    writeln!(file, "{huge}").expect("write oversized line");
    let kept = thought_line("after", "p", "still-here", "t");
    writeln!(file, "{kept}").expect("write following thought");
    drop(file);
    let read = read_grok_updates(session.path(), &path, &GrokReadCursor::default())
        .expect("read oversized line");
    assert!(read.partial);
    assert!(!read.is_complete());
    assert_eq!(thought_texts(&read.events), ["still-here"]);
    assert!(
        read.events.iter().all(|event| match event {
            GrokTranscriptEvent::Thought { text, .. } => text.len() < 64,
            _ => true,
        }),
        "the oversized line must not be stored"
    );
}

#[test]
fn grok_updates_oversized_source_is_refused() {
    let session = tempfile::tempdir().expect("session dir");
    let path = session_file(session.path(), "updates.jsonl");
    let file = File::create(&path).expect("create");
    file.set_len(TRANSCRIPT_READ_HARD_CAP_BYTES + 1)
        .expect("set logical size");
    drop(file);
    let error = read_grok_updates(session.path(), &path, &GrokReadCursor::default())
        .expect_err("source above 16 MiB is refused");
    match error {
        GrokUpdatesError::Refused { reason, .. } => {
            assert!(
                {
                    let needle = TRANSCRIPT_READ_HARD_CAP_BYTES.to_string();
                    reason.contains(&needle)
                },
                "reason should name the cap, got {reason}"
            );
        }
        other => panic!("expected refusal, got {other}"),
    }
}

#[test]
fn grok_updates_symlink_directory_and_outside_path_are_refused() {
    let session = tempfile::tempdir().expect("session dir");
    let real = write_jsonl(session.path(), &[thought_line("e", "p", "inside", "t")]);
    let link = session.path().join("link.jsonl");
    std::os::unix::fs::symlink(&real, &link).expect("symlink");
    let link_error = read_grok_updates(session.path(), &link, &GrokReadCursor::default())
        .expect_err("symlink is refused");
    assert!(
        matches!(link_error, GrokUpdatesError::Refused { ref reason, .. } if reason.contains("symlink")),
        "got {link_error}"
    );

    let directory = session.path().join("not-a-file");
    fs::create_dir(&directory).expect("dir");
    let dir_error = read_grok_updates(session.path(), &directory, &GrokReadCursor::default())
        .expect_err("directory is refused");
    assert!(
        matches!(dir_error, GrokUpdatesError::Refused { ref reason, .. } if reason.contains("regular file")),
        "got {dir_error}"
    );

    let outside = tempfile::tempdir().expect("outside dir");
    let outside_file = write_jsonl(outside.path(), &[thought_line("e", "p", "outside", "t")]);
    let outside_error =
        read_grok_updates(session.path(), &outside_file, &GrokReadCursor::default())
            .expect_err("path outside the session dir is refused");
    assert!(
        matches!(outside_error, GrokUpdatesError::Refused { ref reason, .. } if reason.contains("outside")),
        "got {outside_error}"
    );
}

fn reasoning_line(id: &str, status: &str, summaries: &[&str], encrypted: &str) -> String {
    let summary = summaries
        .iter()
        .map(|text| serde_json::json!({"type": "summary_text", "text": text}))
        .collect::<Vec<_>>();
    serde_json::json!({
        "type": "reasoning",
        "id": id,
        "status": status,
        "summary": summary,
        "encrypted_content": encrypted,
    })
    .to_string()
}

fn thought(event_id: &str, prompt_id: &str, text: &str) -> GrokTranscriptEvent {
    GrokTranscriptEvent::Thought {
        text: text.to_string(),
        prompt_id: prompt_id.to_string(),
        event_id: event_id.to_string(),
    }
}

fn write_history(dir: &Path, lines: &[String]) -> std::path::PathBuf {
    let path = session_file(dir, "chat_history.jsonl");
    let mut body = String::new();
    for line in lines {
        body.push_str(line);
        body.push('\n');
    }
    fs::write(&path, body).expect("write chat history");
    path
}

#[test]
fn grok_unproven_ciphertext_is_in_transcript_and_absent_from_projection() {
    let session = tempfile::tempdir().expect("session dir");
    let secret = "sk-ant-api03-AAAAAAAAAAAAAAAAAA";
    let canary = "GROK-CANARY-7c1e";
    let exact_cipher = "CIPHER-EXACT-7c1e";
    let join_cipher = "CIPHER-JOIN-7c1e";
    let orphan_cipher = "CIPHER-ORPHAN-7c1e";
    let reasoning_id = "rid-7c1e";
    let updates = vec![
        thought("e1", "p-join", "W1"),
        thought("e2", "p-join", "W2"),
        thought("e3", "p-join", "W3"),
        thought("e4", "p-join", "W4"),
        GrokTranscriptEvent::Tool {
            name: "shell".to_string(),
            event_id: "e-tool".to_string(),
        },
        thought("e-exact", "p-exact", canary),
        thought("e-secret", "p-secret", secret),
    ];
    let history = write_history(
        session.path(),
        &[
            reasoning_line("rid-join", "completed", &["W1W2W3W4"], join_cipher),
            reasoning_line(reasoning_id, "completed", &[canary], exact_cipher),
            reasoning_line(
                "rid-orphan",
                "completed",
                &["orphan-summary"],
                orphan_cipher,
            ),
        ],
    );
    let import = import_grok_reasoning(session.path(), &history, &updates).expect("import");
    assert!(import.is_complete());
    let document = import.transcript_document();
    let transcript = document.to_string();
    assert!(transcript.contains(canary));
    assert!(transcript.contains(exact_cipher));
    assert!(transcript.contains(join_cipher));
    assert!(transcript.contains(orphan_cipher));
    assert!(transcript.contains(reasoning_id));
    assert!(transcript.contains("completed"));
    assert!(transcript.contains("orphan-summary"));
    assert!(transcript.contains("encrypted_content"));
    assert!(
        !transcript.contains("W1W2W3W4"),
        "a four-chunk join is not stored as a second body"
    );
    assert_eq!(
        transcript.matches(canary).count(),
        1,
        "exact summary is not duplicated"
    );
    assert!(transcript.contains("<REDACTED:anthropic-api-key>"));
    assert!(!transcript.contains(secret));

    let mut attached_join = 0;
    let mut attached_exact = 0;
    for event in &import.events {
        match event {
            GrokMergedEvent::Thought {
                event_id,
                text,
                reasoning,
                ..
            } => {
                if event_id == "e1" {
                    let reasoning = reasoning
                        .as_ref()
                        .expect("join ciphertext hangs on the first chunk");
                    assert_eq!(reasoning.encrypted_content, join_cipher);
                    attached_join += 1;
                    assert_eq!(text, "W1");
                }
                if event_id == "e-exact" {
                    let reasoning = reasoning
                        .as_ref()
                        .expect("exact ciphertext hangs on that thought");
                    assert_eq!(reasoning.encrypted_content, exact_cipher);
                    assert_eq!(reasoning.id, reasoning_id);
                    assert_eq!(reasoning.status, "completed");
                    attached_exact += 1;
                }
                assert_ne!(text, "W1W2W3W4");
            }
            GrokMergedEvent::Reasoning {
                summary_text,
                encrypted_content,
                ..
            } => {
                assert_eq!(summary_text.as_deref(), Some("orphan-summary"));
                assert_eq!(encrypted_content, orphan_cipher);
            }
            GrokMergedEvent::Tool { name, .. } => assert_eq!(name, "shell"),
            other => panic!("unexpected event {other:?}"),
        }
    }
    assert_eq!(attached_join, 1);
    assert_eq!(attached_exact, 1);
    let tool_at = import
        .events
        .iter()
        .position(|event| matches!(event, GrokMergedEvent::Tool { .. }))
        .expect("tool");
    assert!(matches!(
        &import.events[tool_at - 1],
        GrokMergedEvent::Thought { event_id, .. } if event_id == "e4"
    ));
    assert!(matches!(
        &import.events[tool_at + 1],
        GrokMergedEvent::Thought { text, .. } if text == canary
    ));

    let sinks = sink_bundle(&import);
    for (name, body) in [
        ("metadata", sinks.metadata_json.as_str()),
        ("coverage", sinks.typed_coverage_json.as_str()),
        ("debug", sinks.debug_text.as_str()),
        ("projection", sinks.safe_projection_json.as_str()),
        ("doctor", sinks.doctor_json.as_str()),
        ("digest", sinks.coverage_digest.as_str()),
        ("log", sinks.log_line.as_str()),
    ] {
        for secret_text in [
            canary,
            exact_cipher,
            join_cipher,
            orphan_cipher,
            reasoning_id,
            secret,
            "orphan-summary",
        ] {
            assert!(
                !body.contains(secret_text),
                "{name} contains {secret_text}: {body}"
            );
        }
    }
    assert!(sinks.doctor_json.contains("<REDACTED:anthropic-api-key>"));

    let mut with_metadata = document;
    with_metadata["metadata"] = serde_json::json!({
        "openai": {"reasoningEncryptedContent": "OPENAI-CANARY-7c1e", "itemId": "keep-id"},
        "anthropic": {"signature": "ANTHROPIC-CANARY-7c1e"}
    });
    let exported = export_grok_ordinary(&with_metadata).expect("export");
    let export_text = String::from_utf8(exported).expect("utf8 export");
    assert!(!export_text.contains("encrypted_content"));
    assert!(!export_text.contains("reasoningEncryptedContent"));
    assert!(!export_text.contains("\"signature\""));
    assert!(!export_text.contains(exact_cipher));
    assert!(export_text.contains("keep-id"));
    assert!(transcript.contains(exact_cipher));
}

#[test]
fn grok_reasoning_empty_summary_keeps_ciphertext() {
    let session = tempfile::tempdir().expect("session dir");
    let history = write_history(
        session.path(),
        &[reasoning_line(
            "rid-empty",
            "completed",
            &[],
            "CIPHER-EMPTY-7c1e",
        )],
    );
    let import = import_grok_reasoning(
        session.path(),
        &history,
        &[thought("e", "p", "visible-thought")],
    )
    .expect("import");
    assert!(import.is_complete());
    match &import.events[..] {
        [
            GrokMergedEvent::Thought {
                text, reasoning, ..
            },
            GrokMergedEvent::Reasoning {
                id,
                status,
                summary_text,
                encrypted_content,
            },
        ] => {
            assert_eq!(text, "visible-thought");
            assert!(reasoning.is_none());
            assert_eq!(id, "rid-empty");
            assert_eq!(status, "completed");
            assert_eq!(summary_text, &None);
            assert_eq!(encrypted_content, "CIPHER-EMPTY-7c1e");
        }
        other => panic!("expected thought plus ciphertext-only reasoning, got {other:?}"),
    }
    let transcript = import.transcript_document().to_string();
    assert!(transcript.contains("CIPHER-EMPTY-7c1e"));
    assert!(!transcript.contains("summary_text\":\""));
}

#[test]
fn grok_reasoning_non_string_ciphertext_is_partial() {
    let session = tempfile::tempdir().expect("session dir");
    let bad = serde_json::json!({
        "type": "reasoning",
        "id": "rid-bad",
        "status": "completed",
        "summary": [{"type": "summary_text", "text": "kept-out"}],
        "encrypted_content": {"blob": true}
    })
    .to_string();
    let history = write_history(
        session.path(),
        &[
            reasoning_line("rid-good", "completed", &["visible-thought"], "CIPHER-GOOD"),
            bad,
        ],
    );
    let import = import_grok_reasoning(
        session.path(),
        &history,
        &[thought("e", "p", "visible-thought")],
    )
    .expect("partial import");
    assert!(import.partial);
    assert!(!import.is_complete());
    let transcript = import.transcript_document().to_string();
    assert!(transcript.contains("CIPHER-GOOD"));
    assert!(transcript.contains("visible-thought"));
    assert!(!transcript.contains("rid-bad"));
    assert!(!transcript.contains("kept-out"));
    assert!(!transcript.contains("blob"));
}

#[test]
fn grok_reasoning_outside_matrix_is_refused() {
    let session = tempfile::tempdir().expect("session dir");
    let history = write_history(
        session.path(),
        &[
            reasoning_line("rid-good", "completed", &["visible-thought"], "CIPHER-GOOD"),
            r#"{"type":"reasoning","id":"rid-x","status":"completed","summary":[],"cipher":"dropped-thinking"}"#.to_string(),
        ],
    );
    let error = import_grok_reasoning(
        session.path(),
        &history,
        &[thought("e", "p", "visible-thought")],
    )
    .expect_err("shape outside the matrix is refused");
    assert!(
        matches!(error, GrokReasoningError::Malformed { ref reason } if reason.contains("encrypted_content")),
        "got {error}"
    );
    let rendered = error.to_string();
    assert!(rendered.contains("malformed transcript"));
    assert!(!rendered.contains("dropped-thinking"));
}

const LIBRA_HOOK_COMMAND: &str = "libra agent hooks grok";

fn hook_event(
    name: &str,
    session_id: &str,
    cwd: &str,
    transcript: Option<&str>,
) -> serde_json::Value {
    let mut event = serde_json::json!({
        "hook_event_name": name,
        "sessionId": session_id,
        "cwd": cwd,
    });
    if let Some(transcript) = transcript {
        event["transcript_path"] = serde_json::json!(transcript);
    }
    event
}

fn write_session_updates(
    root: &Path,
    cwd: &str,
    session_id: &str,
    body: &str,
) -> std::path::PathBuf {
    let path = root
        .join(encode_grok_cwd(cwd))
        .join(session_id)
        .join("updates.jsonl");
    fs::create_dir_all(path.parent().expect("session dir")).expect("mkdir");
    fs::write(&path, body).expect("write updates");
    path
}

fn stored_thoughts(state: &Path, session_id: &str) -> Vec<String> {
    let bytes = fs::read(state.join(session_id).join("store.json")).expect("store");
    let value: serde_json::Value = serde_json::from_slice(&bytes).expect("json");
    value["thoughts"]
        .as_array()
        .expect("thoughts")
        .iter()
        .map(|item| item.as_str().expect("text").to_string())
        .collect()
}

#[test]
fn grok_hook_marker_round_trip_keeps_user_bytes() {
    let hooks = tempfile::tempdir().expect("hooks");
    let user_path = hooks.path().join("user.json");
    let user_bytes = b"{\n  \"hooks\": {\n    \"Stop\": [{\"matcher\": \"user\", \"hooks\": [{\"type\": \"command\", \"command\": \"echo keep-me\"}]}]\n  }\n}\n";
    fs::write(&user_path, user_bytes).expect("user file");
    let mixed_path = hooks.path().join("mixed.json");
    fs::write(
        &mixed_path,
        serde_json::to_vec_pretty(&serde_json::json!({
            "note": "leave-me",
            "hooks": {
                "Stop": [
                    {"matcher": "user", "hooks": [{"type": "command", "command": "echo keep-user"}]},
                    {"statusMessage": "libra capture", "hooks": [{"type": "command", "command": "echo old-mark", "timeout": 10}]}
                ]
            }
        }))
        .expect("mixed"),
    )
    .expect("write mixed");
    install_marked_grok_hooks(hooks.path(), LIBRA_HOOK_COMMAND).expect("install");
    assert_eq!(fs::read(&user_path).expect("reread user"), user_bytes);
    let mixed: serde_json::Value =
        serde_json::from_slice(&fs::read(&mixed_path).expect("mixed")).expect("parse mixed");
    let groups = mixed["hooks"]["Stop"].as_array().expect("groups");
    assert_eq!(groups[0]["hooks"][0]["command"], "echo keep-user");
    assert_eq!(groups[1]["hooks"][0]["command"], LIBRA_HOOK_COMMAND);
    assert_eq!(mixed["note"], "leave-me");
    let owned = fs::read_to_string(hooks.path().join("libra-grok-capture.json")).expect("owned");
    assert!(owned.contains(LIBRA_HOOK_COMMAND));
    assert!(owned.contains("libra capture"));
    assert!(!owned.contains("\"command\": \"grok"));
    uninstall_marked_grok_hooks(hooks.path()).expect("uninstall");
    assert_eq!(fs::read(&user_path).expect("user after"), user_bytes);
    assert!(!hooks.path().join("libra-grok-capture.json").exists());
    let mixed: serde_json::Value =
        serde_json::from_slice(&fs::read(&mixed_path).expect("mixed after")).expect("parse");
    let groups = mixed["hooks"]["Stop"].as_array().expect("groups");
    assert_eq!(groups.len(), 1);
    assert_eq!(groups[0]["hooks"][0]["command"], "echo keep-user");
    assert_eq!(mixed["note"], "leave-me");
    assert!(
        !serde_json::to_string(&mixed)
            .expect("text")
            .contains("libra capture")
    );
}

#[test]
fn grok_hook_does_not_spawn_grok() {
    let hooks = tempfile::tempdir().expect("hooks");
    let rejected = install_marked_grok_hooks(hooks.path(), "grok --serve");
    assert!(rejected.is_err(), "install must refuse a grok program");
    assert!(
        fs::read_dir(hooks.path())
            .map(|entries| entries.count())
            .unwrap_or(0)
            == 0
    );
    install_marked_grok_hooks(hooks.path(), LIBRA_HOOK_COMMAND).expect("install libra");
    for entry in fs::read_dir(hooks.path()).expect("list") {
        let path = entry.expect("entry").path();
        let text = fs::read_to_string(&path).expect("hook file");
        let value: serde_json::Value = serde_json::from_str(&text).expect("json");
        let hooks_obj = value.get("hooks").and_then(|item| item.as_object());
        let Some(hooks_obj) = hooks_obj else {
            continue;
        };
        for groups in hooks_obj.values() {
            for group in groups.as_array().into_iter().flatten() {
                for hook in group
                    .get("hooks")
                    .and_then(|item| item.as_array())
                    .into_iter()
                    .flatten()
                {
                    let command = hook
                        .get("command")
                        .and_then(|item| item.as_str())
                        .unwrap_or("");
                    let program = command.split_whitespace().next().unwrap_or("");
                    assert_ne!(
                        Path::new(program)
                            .file_name()
                            .and_then(|name| name.to_str()),
                        Some("grok"),
                        "installed command must not be grok: {command}"
                    );
                }
            }
        }
    }
    let source = Path::new(env!("CARGO_MANIFEST_DIR")).join("src/internal/ai/hooks/providers/grok");
    for name in ["mod.rs", "parser.rs", "session_files.rs", "settings.rs"] {
        let text = fs::read_to_string(source.join(name)).expect("source");
        assert!(
            !text.contains("std::process") && !text.contains("Command::new"),
            "{name} must not spawn a process"
        );
    }
    let sessions = tempfile::tempdir().expect("sessions");
    let state = tempfile::tempdir().expect("state");
    let cwd = sessions.path().join("work");
    fs::create_dir_all(&cwd).expect("cwd");
    let cwd_text = cwd.to_string_lossy().to_string();
    let encoded = encode_grok_cwd(&cwd_text);
    assert!(encoded.len() <= 255);
    assert!(encoded.contains("%2F") || encoded.contains('-'));
    let updates = write_session_updates(
        sessions.path(),
        &cwd_text,
        "sess-1",
        &format!(
            "{}\n",
            thought_line("e1", "p", "alpha", "2026-10-18T00:00:00Z")
        ),
    );
    let outcome = observe_grok_hook(
        sessions.path(),
        state.path(),
        &hook_event("Stop", "sess-1", &cwd_text, None),
        None,
    );
    assert_eq!(outcome.exit_code, 0);
    assert_eq!(outcome.stdout, "{}");
    assert!(!outcome.stdout.contains("deny"));
    assert!(!outcome.stdout.contains("block"));
    assert!(outcome.complete);
    assert_eq!(stored_thoughts(state.path(), "sess-1"), ["alpha"]);
    let link = updates.with_file_name("link.jsonl");
    std::os::unix::fs::symlink(&updates, &link).expect("symlink");
    let via_link = observe_grok_hook(
        sessions.path(),
        state.path(),
        &hook_event("Stop", "sess-1", &cwd_text, Some(&link.to_string_lossy())),
        None,
    );
    assert!(via_link.refused);
    assert_eq!(stored_thoughts(state.path(), "sess-1"), ["alpha"]);
    let outside = tempfile::tempdir().expect("outside");
    let outside_file = outside.path().join("updates.jsonl");
    fs::write(&outside_file, "{}\n").expect("outside");
    let outside_outcome = observe_grok_hook(
        sessions.path(),
        state.path(),
        &hook_event(
            "Stop",
            "sess-1",
            &cwd_text,
            Some(&outside_file.to_string_lossy()),
        ),
        None,
    );
    assert!(outside_outcome.refused);
    let missing = observe_grok_hook(
        sessions.path(),
        state.path(),
        &serde_json::json!({"hook_event_name": "Stop", "sessionId": "sess-1"}),
        None,
    );
    assert!(missing.lifecycle_only);
    assert!(!missing.complete);
    let short = observe_grok_hook(
        sessions.path(),
        state.path(),
        &hook_event("SessionEnd", "sess-1", &cwd_text, None),
        Some(1_500),
    );
    assert_eq!(short.exit_code, 0);
    assert!(short.pending);
    assert!(!short.complete);
    assert!(state.path().join("sess-1").join("pending.json").exists());
}

#[test]
fn grok_hook_precompact_and_shrink_keep_captured_thoughts() {
    let sessions = tempfile::tempdir().expect("sessions");
    let state = tempfile::tempdir().expect("state");
    let cwd = "/var/lib/grok-hook-synthetic";
    let updates = write_session_updates(
        sessions.path(),
        cwd,
        "sess-keep",
        &format!(
            "{}\n{}\n",
            thought_line("e1", "p", "alpha", "t"),
            thought_line("e2", "p", "gamma", "t")
        ),
    );
    let stop = observe_grok_hook(
        sessions.path(),
        state.path(),
        &hook_event("Stop", "sess-keep", cwd, None),
        None,
    );
    assert!(stop.complete);
    assert_eq!(
        stored_thoughts(state.path(), "sess-keep"),
        ["alpha", "gamma"]
    );
    let compact = observe_grok_hook(
        sessions.path(),
        state.path(),
        &hook_event("PreCompact", "sess-keep", cwd, None),
        None,
    );
    assert_eq!(compact.exit_code, 0);
    let checkpoint = fs::read_to_string(state.path().join("sess-keep").join("checkpoint.json"))
        .expect("checkpoint");
    assert!(checkpoint.contains("alpha"));
    assert!(checkpoint.contains("gamma"));
    fs::write(
        &updates,
        format!("{}\n", thought_line("e3", "p", "beta", "t")),
    )
    .expect("shorter file");
    let again = observe_grok_hook(
        sessions.path(),
        state.path(),
        &hook_event("StopFailure", "sess-keep", cwd, None),
        None,
    );
    assert!(again.complete);
    let thoughts = stored_thoughts(state.path(), "sess-keep");
    assert!(
        thoughts
            .iter()
            .any(|candidate| candidate.as_str() == "alpha")
    );
    assert!(
        thoughts
            .iter()
            .any(|candidate| candidate.as_str() == "gamma")
    );
    assert!(
        thoughts
            .iter()
            .any(|candidate| candidate.as_str() == "beta")
    );
    let before = thoughts.len();
    let mut repeated = fs::read_to_string(&updates).expect("current");
    repeated.push_str(&thought_line("e3", "p", "beta-again", "t"));
    repeated.push('\n');
    fs::write(&updates, repeated).expect("repeat id");
    observe_grok_hook(
        sessions.path(),
        state.path(),
        &hook_event("StopCancelled", "sess-keep", cwd, None),
        None,
    );
    let after = stored_thoughts(state.path(), "sess-keep");
    assert!(after.starts_with(&thoughts));
    assert_eq!(
        after.iter().filter(|text| text.as_str() == "beta").count(),
        1
    );
    assert!(!after.iter().any(|text| text.as_str() == "beta-again"));
    assert!(after.len() >= before);

    let child_cwd = "/var/lib/grok-hook-child";
    write_session_updates(
        sessions.path(),
        child_cwd,
        "child-1",
        &format!("{}\n", thought_line("c1", "p", "child-thought", "t")),
    );
    let mut child_event = hook_event("Stop", "child-1", child_cwd, None);
    child_event["parentSessionId"] = serde_json::json!("parent-1");
    observe_grok_hook(sessions.path(), state.path(), &child_event, None);
    assert_eq!(stored_thoughts(state.path(), "child-1"), ["child-thought"]);
    let parent =
        fs::read_to_string(state.path().join("parent-1").join("store.json")).expect("parent");
    assert!(parent.contains("child-1"));
    assert!(!parent.contains("child-thought"));

    let huge = "x".repeat((GROK_LINE_MAX_BYTES as usize) + 8);
    write_session_updates(sessions.path(), cwd, "sess-partial", &format!("{huge}\n"));
    let partial = observe_grok_hook(
        sessions.path(),
        state.path(),
        &hook_event("Stop", "sess-partial", cwd, None),
        None,
    );
    assert!(partial.pending);
    assert!(!partial.complete);
    assert!(
        state
            .path()
            .join("sess-partial")
            .join("pending.json")
            .exists()
    );
}

#[test]
fn grok_live_capture_matches_hook_installable() {
    let row = registration_for(AgentKind::Grok);
    assert!(live_capture_matches_hook_installable(AgentKind::Grok));
    assert!(row.supported);
    assert!(row.hook_installable);
    assert_eq!(row.support_wave, Some("grok"));
    assert!(!row.launchable_review);
    assert!(!row.launchable_investigate);
    let agent = agent_for(AgentKind::Grok);
    assert!(agent.as_hooks().is_some());
    assert_eq!(agent.stability(), AgentStability::Stable);
    for kind in AgentKind::all() {
        assert!(live_capture_matches_hook_installable(*kind));
    }
}

#[test]
fn grok_updates_foreign_method_is_refused() {
    let session = tempfile::tempdir().expect("session");
    let foreign = write_jsonl(
        session.path(),
        &[r#"{"method":"session/other","params":{"update":{"sessionUpdate":"agent_thought_chunk","content":{"type":"text","text":"dropped-thought"}}}}"#
            .to_string()],
    );
    let error = read_grok_updates(session.path(), &foreign, &GrokReadCursor::default())
        .expect_err("foreign method is refused");
    let rendered = error.to_string();
    assert!(
        rendered.contains("outside the grok field matrix"),
        "{rendered}"
    );
    assert!(!rendered.contains("dropped-thought"));
}

#[test]
fn grok_debug_omits_plaintext() {
    let thought = GrokTranscriptEvent::Thought {
        text: "plain-thought-canary".to_string(),
        prompt_id: "prompt-canary".to_string(),
        event_id: "event-canary".to_string(),
    };
    let thought_debug = format!("{thought:?}");
    assert!(!thought_debug.contains("plain-thought-canary"));
    assert!(!thought_debug.contains("prompt-canary"));
    let reasoning = GrokMergedEvent::Reasoning {
        id: "id-canary".to_string(),
        status: "completed".to_string(),
        summary_text: Some("summary-canary".to_string()),
        encrypted_content: "cipher-canary".to_string(),
    };
    let reasoning_debug = format!("{reasoning:?}");
    assert!(!reasoning_debug.contains("summary-canary"));
    assert!(!reasoning_debug.contains("cipher-canary"));
    assert!(!reasoning_debug.contains("id-canary"));
}

#[test]
fn grok_provider_parse_is_lifecycle_only() {
    use libra::internal::ai::hooks::lifecycle::LifecycleEventKind;

    // These paths deliberately do not exist. Decoding needs neither source
    // access nor any host HOME/env mutation; AST guards pin the delegate too.
    let temp = tempfile::tempdir().expect("isolated paths");
    let missing_cwd = temp.path().join("missing-cwd");
    let missing_source = temp.path().join("missing-updates.jsonl");
    for (event_name, expected) in [
        ("Stop", LifecycleEventKind::TurnEnd),
        ("StopCancelled", LifecycleEventKind::TurnEnd),
        ("StopFailure", LifecycleEventKind::TurnEnd),
        ("PreCompact", LifecycleEventKind::Compaction),
        ("SessionEnd", LifecycleEventKind::SessionEnd),
    ] {
        let envelope = SessionHookEnvelope {
            hook_event_name: event_name.to_string(),
            session_id: "synthetic-session".to_string(),
            cwd: missing_cwd.to_string_lossy().into_owned(),
            transcript_path: Some(missing_source.to_string_lossy().into_owned()),
            extra: serde_json::Map::new(),
        };
        let event = grok_provider()
            .parse_hook_event(event_name, &envelope)
            .expect("pure parse");
        assert_eq!(event.kind, expected);
        assert_eq!(event.session_id, "synthetic-session");
        assert!(event.assistant_message.is_none());
        assert!(event.prompt.is_none());
        assert_eq!(
            fs::read_dir(temp.path())
                .expect("unchanged temp dir")
                .count(),
            0
        );
    }
    let unsafe_envelope = SessionHookEnvelope {
        hook_event_name: "Stop".to_string(),
        session_id: "../unsafe".to_string(),
        cwd: String::new(),
        transcript_path: None,
        extra: serde_json::Map::new(),
    };
    assert!(
        grok_provider()
            .parse_hook_event("Stop", &unsafe_envelope)
            .is_err()
    );
    let safe_envelope = SessionHookEnvelope {
        session_id: "synthetic-session".to_string(),
        ..unsafe_envelope
    };
    assert!(
        grok_provider()
            .parse_hook_event("Unknown", &safe_envelope)
            .is_err()
    );
}

// GK-07 pure snapshot APIs. No real provider home, process or runtime wiring.
mod pure_snapshot {
    use std::time::{Duration, Instant};

    use libra::internal::ai::observed_agents::{
        grok_reasoning::{
            GROK_NORMALIZED_MAX_BYTES, GROK_THOUGHT_MAX_BYTES, GrokReasoningError,
            normalize_grok_reasoning_bytes,
        },
        grok_updates::{GrokUpdatesError, parse_grok_updates_bytes},
    };

    use super::*;

    fn bytes(lines: &[String]) -> Vec<u8> {
        format!("{}\n", lines.join("\n")).into_bytes()
    }

    fn updates(
        lines: &[String],
    ) -> libra::internal::ai::observed_agents::grok_updates::GrokUpdatesRead {
        parse_grok_updates_bytes(&bytes(lines), &GrokReadCursor::default(), None)
            .expect("bounded synthetic updates")
    }

    #[test]
    fn grok_pure_updates_matches_file_and_preserves_order_cursor_and_replay() {
        let lines = [
            thought_line("e1", "p", "left-", "t"),
            thought_line("e1", "p", "duplicate", "t"),
            tool_line("tool", "shell"),
            thought_line("e2", "p", "middle-", "t"),
            thought_line("e3", "p", "right", "t"),
            turn_line("turn", 7),
        ];
        let snapshot = bytes(&lines);
        let before = snapshot.clone();
        let pure = parse_grok_updates_bytes(&snapshot, &GrokReadCursor::default(), None)
            .expect("pure parse");
        let temp = tempfile::tempdir().expect("synthetic session");
        let path = write_jsonl(temp.path(), &lines);
        let file = read_grok_updates(temp.path(), &path, &GrokReadCursor::default())
            .expect("legacy file API");
        assert_eq!(pure, file);
        assert_eq!(thought_texts(&pure.events), ["left-", "middle-", "right"]);
        assert!(!thought_texts(&pure.events).contains(&"left-middle-right"));
        let cursor = GrokReadCursor {
            offset: pure.next_offset,
            seen_event_ids: pure.seen_event_ids.clone(),
        };
        assert!(
            parse_grok_updates_bytes(&snapshot, &cursor, None)
                .expect("resume")
                .events
                .is_empty()
        );
        let replay = GrokReadCursor {
            offset: 0,
            seen_event_ids: pure.seen_event_ids,
        };
        assert!(
            parse_grok_updates_bytes(&snapshot, &replay, None)
                .expect("replay")
                .events
                .is_empty()
        );
        assert_eq!(snapshot, before);
    }

    #[test]
    fn grok_pure_summary_exact_four_chunk_unmatched_and_opaque_only() {
        let read = updates(&[
            thought_line("a", "p", "W1", "t"),
            thought_line("b", "p", "W2", "t"),
            thought_line("c", "p", "W3", "t"),
            thought_line("d", "p", "W4", "t"),
            thought_line("e", "p", "W5", "t"),
            thought_line("x", "q", "界 café", "t"),
        ]);
        let history = bytes(&[
            reasoning_line("four", "completed", &["W1W2W3W4"], "OPAQUE-FOUR"),
            reasoning_line("exact", "completed", &["界 café"], "OPAQUE-EXACT"),
            reasoning_line("five", "completed", &["W1W2W3W4W5"], "OPAQUE-FIVE"),
            reasoning_line("unmatched", "completed", &["orphan"], "OPAQUE-ORPHAN"),
            reasoning_line("empty", "completed", &[], "OPAQUE-ONLY"),
        ]);
        let before = history.clone();
        let normalized = normalize_grok_reasoning_bytes(&history, &read, None).expect("normalize");
        assert_eq!(normalized.unattributed_event_count, 9);
        assert!(normalized.transcript.partial);
        let document = normalized.transcript_document();
        assert_eq!(document["source"], "unproven");
        assert_eq!(document["completeness"], "partial");
        assert_eq!(document["unattributed_event_count"], 9);
        assert!(document.get("messages").is_none());
        let rendered = document.to_string();
        assert!(!rendered.contains("\"text\":\"W1W2W3W4\""));
        assert_eq!(rendered.matches("界 café").count(), 1);
        assert!(
            matches!(&normalized.transcript.events[0], GrokMergedEvent::Thought { text, reasoning: Some(reasoning), .. } if text == "W1" && reasoning.id == "four")
        );
        assert!(
            matches!(&normalized.transcript.events[5], GrokMergedEvent::Thought { reasoning: Some(reasoning), .. } if reasoning.id == "exact")
        );
        assert!(
            matches!(&normalized.transcript.events[8], GrokMergedEvent::Reasoning { summary_text: None, encrypted_content, .. } if encrypted_content == "OPAQUE-ONLY")
        );
        assert!(normalized.transcript.events.iter().any(|event| matches!(event, GrokMergedEvent::Reasoning { summary_text: Some(summary), .. } if summary == "W1W2W3W4W5")));
        let temp = tempfile::tempdir().expect("session");
        let path = temp.path().join("chat_history.jsonl");
        fs::write(&path, &history).expect("synthetic history");
        let old = import_grok_reasoning(temp.path(), &path, &read.events).expect("file merge");
        assert_eq!(old.events, normalized.transcript.events);
        assert!(
            old.is_complete(),
            "legacy field completeness remains distinct"
        );
        assert_eq!(history, before);
    }

    #[test]
    fn grok_pure_matching_does_not_cross_prompt_or_drop_occupied_reasoning() {
        let read = updates(&[
            thought_line("a", "p", "left", "t"),
            thought_line("b", "q", "right", "t"),
        ]);
        let normalized = normalize_grok_reasoning_bytes(
            &bytes(&[
                reasoning_line("one", "completed", &["left"], "OPAQUE-1"),
                reasoning_line("two", "completed", &["left"], "OPAQUE-2"),
                reasoning_line("cross", "completed", &["leftright"], "OPAQUE-CROSS"),
            ]),
            &read,
            None,
        )
        .expect("normalize");
        assert_eq!(normalized.transcript.events.len(), 4);
        assert!(
            matches!(&normalized.transcript.events[0], GrokMergedEvent::Thought { reasoning: Some(reasoning), .. } if reasoning.id == "one")
        );
        assert!(
            matches!(&normalized.transcript.events[2], GrokMergedEvent::Reasoning { id, summary_text: Some(text), .. } if id == "two" && text == "left")
        );
        assert!(
            matches!(&normalized.transcript.events[3], GrokMergedEvent::Reasoning { id, .. } if id == "cross")
        );
    }

    #[test]
    fn grok_pure_decoded_unicode_and_credentials_are_scanned_before_retention() {
        let secret = "sk-ant-api03-AAAAAAAAAAAAAAAAAA";
        let escaped = thought_line(
            "e",
            "p",
            &format!("界 BODY-CANARY <REDACTED:FORGED-CANARY> {secret}"),
            "t",
        )
        .replace("sk-ant", r"\u0073k-ant");
        let read = updates(&[escaped]);
        assert!(
            thought_texts(&read.events)[0].contains(secret),
            "raw typed events are only an intermediate for exact matching"
        );
        let history = bytes(&[reasoning_line(
            "rid",
            "completed",
            &[&format!("café SUMMARY-CANARY {secret}")],
            "OPAQUE-CANARY",
        )]);
        let normalized =
            normalize_grok_reasoning_bytes(&history, &read, None).expect("scanned ordinary body");
        let body = normalized.transcript_document().to_string();
        assert!(body.contains("界 BODY-CANARY"));
        assert!(body.contains("café SUMMARY-CANARY"));
        assert!(body.contains("OPAQUE-CANARY"));
        assert!(!body.contains(secret));
        assert!(body.contains("<REDACTED:anthropic-api-key>"));
        let sinks = sink_bundle(&normalized.transcript);
        for text in [
            &sinks.metadata_json,
            &sinks.typed_coverage_json,
            &sinks.debug_text,
            &sinks.safe_projection_json,
            &sinks.doctor_json,
            &sinks.coverage_digest,
            &sinks.log_line,
        ] {
            for canary in [
                secret,
                "BODY-CANARY",
                "SUMMARY-CANARY",
                "OPAQUE-CANARY",
                "FORGED-CANARY",
            ] {
                assert!(
                    !text.contains(canary),
                    "sink must not contain retained body"
                );
            }
        }
        assert!(!format!("{normalized:?}").contains("BODY-CANARY"));
    }

    #[test]
    fn grok_pure_missing_or_bad_fields_propagate_partial_without_invented_turns() {
        let mut line: serde_json::Value =
            serde_json::from_str(&thought_line("bad", "p", "ignored", "t")).expect("JSON");
        line["params"]["update"]["content"]["text"] = serde_json::json!(true);
        let read = updates(&[line.to_string(), turn_line("usage-only", 42)]);
        assert!(read.partial);
        let normalized =
            normalize_grok_reasoning_bytes(b"", &read, None).expect("partial typed input");
        assert!(normalized.transcript.partial);
        assert_eq!(normalized.unattributed_event_count, 1);
        assert!(matches!(
            normalized.transcript.events[0],
            GrokMergedEvent::Turn {
                reasoning_tokens: 42,
                ..
            }
        ));
        assert!(
            !normalized
                .transcript_document()
                .to_string()
                .contains("assistant")
        );
        let empty =
            normalize_grok_reasoning_bytes(b"", &updates(&[]), None).expect("empty snapshots");
        assert_eq!(empty.unattributed_event_count, 0);
        assert_eq!(empty.transcript_document()["completeness"], "partial");
    }

    #[test]
    fn grok_pure_malformed_truncated_and_foreign_lines_refuse_without_body_in_errors() {
        let read = updates(&[]);
        for input in [
            b"{\"canary\":\"BROKEN-BODY".as_slice(),
            b"[1,2]",
            b"{\"method\":\"foreign\",\"canary\":\"BROKEN-BODY\"}",
        ] {
            let error = parse_grok_updates_bytes(input, &GrokReadCursor::default(), None)
                .expect_err("refuse malformed input");
            assert!(!error.to_string().contains("BROKEN-BODY"));
        }
        for input in [
            b"{\"canary\":\"BROKEN-BODY".as_slice(),
            b"[]",
            b"{\"type\":\"BROKEN-BODY\"}",
        ] {
            let error = normalize_grok_reasoning_bytes(input, &read, None)
                .expect_err("refuse malformed history");
            assert!(!error.to_string().contains("BROKEN-BODY"));
        }
    }

    #[test]
    fn grok_pure_source_line_and_deadline_limits_are_explicit() {
        let past = Instant::now()
            .checked_sub(Duration::from_secs(1))
            .expect("past deadline");
        assert!(matches!(
            parse_grok_updates_bytes(b"", &GrokReadCursor::default(), Some(past)),
            Err(GrokUpdatesError::Deadline)
        ));
        assert!(matches!(
            normalize_grok_reasoning_bytes(b"", &updates(&[]), Some(past)),
            Err(GrokReasoningError::Deadline)
        ));
        let oversized = vec![b' '; TRANSCRIPT_READ_HARD_CAP_BYTES as usize + 1];
        assert!(matches!(
            parse_grok_updates_bytes(&oversized, &GrokReadCursor::default(), None),
            Err(GrokUpdatesError::Refused { .. })
        ));
        assert!(matches!(
            normalize_grok_reasoning_bytes(&oversized, &updates(&[]), None),
            Err(GrokReasoningError::Read(_))
        ));
        let mut long = vec![b'x'; GROK_LINE_MAX_BYTES as usize + 1];
        long.push(b'\n');
        long.extend_from_slice(thought_line("kept", "p", "after", "t").as_bytes());
        let read = parse_grok_updates_bytes(&long, &GrokReadCursor::default(), None)
            .expect("oversized update line is partial");
        assert!(read.partial);
        assert_eq!(thought_texts(&read.events), ["after"]);
        assert!(matches!(
            normalize_grok_reasoning_bytes(&long, &updates(&[]), None),
            Err(GrokReasoningError::Malformed { .. })
        ));
    }

    #[test]
    fn grok_pure_retained_body_budget_refuses_instead_of_silently_dropping_text() {
        let piece = "x".repeat(900_000);
        let lines = (0..10)
            .map(|n| thought_line(&format!("e{n}"), "p", &piece, "t"))
            .collect::<Vec<_>>();
        let read = updates(&lines);
        assert!(
            matches!(normalize_grok_reasoning_bytes(b"", &read, None), Err(GrokReasoningError::Budget { resource: "retained body", limit }) if limit == GROK_THOUGHT_MAX_BYTES)
        );
    }

    #[test]
    fn grok_pure_repeated_summary_cannot_expand_opaque_output_without_bound() {
        let read = updates(&[]);
        let summaries = vec!["s"; 50];
        let row = reasoning_line("rid", "completed", &summaries, &"o".repeat(800_000));
        assert!(row.len() < GROK_LINE_MAX_BYTES as usize);
        assert!(
            matches!(normalize_grok_reasoning_bytes(&bytes(&[row]), &read, None), Err(GrokReasoningError::Budget { resource: "normalized output", limit }) if limit == GROK_NORMALIZED_MAX_BYTES)
        );
    }

    #[test]
    fn grok_pure_non_string_reasoning_fields_stay_partial() {
        let read = updates(&[thought_line("e", "p", "thought", "t")]);
        let invalid_cipher = serde_json::json!({"type":"reasoning","id":"r","status":"completed","summary":[],"encrypted_content":false});
        let invalid_summary = serde_json::json!({"type":"reasoning","id":"r2","status":"completed","summary":[{"type":"summary_text","text":7}],"encrypted_content":"OPAQUE"});
        let normalized = normalize_grok_reasoning_bytes(
            &bytes(&[invalid_cipher.to_string(), invalid_summary.to_string()]),
            &read,
            None,
        )
        .expect("retain permitted fields");
        assert!(normalized.transcript.partial);
        assert_eq!(normalized.transcript.events.len(), 2);
        assert!(
            matches!(&normalized.transcript.events[1], GrokMergedEvent::Reasoning { summary_text: None, encrypted_content, .. } if encrypted_content == "OPAQUE")
        );
    }

    #[test]
    fn grok_pure_typed_input_and_error_messages_are_bounded() {
        let mut read = updates(&[]);
        read.events.push(thought(
            "e",
            "p",
            &"x".repeat(GROK_LINE_MAX_BYTES as usize + 1),
        ));
        let error = normalize_grok_reasoning_bytes(b"", &read, None)
            .expect_err("oversized caller-built typed event");
        assert_eq!(
            error.to_string(),
            "Grok reasoning normalization exceeded its 1048576 byte typed update limit"
        );
        assert_eq!(
            GrokUpdatesError::Deadline.to_string(),
            "Grok updates parsing exceeded its deadline; retry with a fresh source snapshot"
        );
        assert_eq!(
            GrokReasoningError::Deadline.to_string(),
            "Grok reasoning normalization exceeded its deadline; retry with a fresh source snapshot"
        );
        let future = Instant::now()
            .checked_add(Duration::from_secs(60))
            .expect("future deadline");
        let empty = parse_grok_updates_bytes(b"", &GrokReadCursor::default(), Some(future))
            .expect("available deadline");
        assert!(normalize_grok_reasoning_bytes(b"", &empty, Some(future)).is_ok());
    }
}
