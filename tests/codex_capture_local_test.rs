//! Operator-local Codex CX-15 capture contracts. Never scheduled in CI.
//! Run with --features test-live-agent --test codex_capture_local_test
//! -- --ignored --test-threads=1. Synthetic fixtures do not prove model capture.

#![recursion_limit = "256"]

#[cfg(unix)]
#[tokio::test]
#[ignore = "operator-local Codex acceptance; never run in CI"]
async fn codex_child_hint_cli_keeps_candidates_out_of_storage_and_enabled_trace() {
    use std::io::{Read, Write};

    use sea_orm::{ConnectionTrait, Statement};

    const CANARY: &str = "CX31-HINT-CANARY";
    for malformed in [false, true] {
        let fixture = LocalCodexSession::new();
        fixture.source(1, true);
        let start = fixture.cx01_hook("SessionStart", "session-start");
        assert!(start.status.success(), "{start:?}");
        for (event, verb) in [
            ("SubagentStop", "subagent-end"),
            ("Stop", "stop"),
            ("SessionEnd", "session-end"),
        ] {
            let mut frame = serde_json::json!({
                "hook_event_name": event,
                "session_id": "123e4567-e89b-12d3-a456-426614174031",
                "cwd": fixture.repo,
                "transcript_path": format!("/untrusted/{CANARY}/parent.jsonl"),
                "agent_transcript_path": format!("/untrusted/{CANARY}/child.jsonl"),
                "agent_id": format!("{CANARY}-child"),
                "turn_id": format!("{CANARY}-turn"),
                "unknown_extra": {"private": CANARY}
            });
            if malformed {
                frame["agent_transcript_path"] = serde_json::json!({"private": CANARY});
                frame["agent_id"] = serde_json::json!(42);
            }
            let mut child = std::process::Command::new(env!("CARGO_BIN_EXE_libra"))
                .current_dir(&fixture.repo)
                .env_clear()
                .env("PATH", "/usr/bin:/bin")
                .env("HOME", &fixture.home)
                .env("USERPROFILE", &fixture.home)
                .env("LIBRA_TEST_HOME", &fixture.home)
                .env("LIBRA_TEST", "1")
                .env("LIBRA_LOG", "trace")
                // Existing descriptor only: do not create a log artifact.
                .env("LIBRA_LOG_FILE", "/dev/fd/2")
                .args(["hooks", "codex", verb, "--capture-budget-ms", "5000"])
                .stdin(std::process::Stdio::piped())
                .stdout(std::process::Stdio::piped())
                .stderr(std::process::Stdio::piped())
                .spawn()
                .unwrap();
            child
                .stdin
                .take()
                .unwrap()
                .write_all(&serde_json::to_vec(&frame).unwrap())
                .unwrap();
            let out = child.wait_with_output().unwrap();
            assert!(out.status.success(), "{event}/{malformed}: {out:?}");
            assert!(out.stdout.is_empty());
            let trace = String::from_utf8(out.stderr).unwrap();
            assert!(!trace.is_empty(), "must exercise an enabled trace sink");
            assert!(!trace.contains("failed to open LIBRA_LOG_FILE"));
            assert!(!trace.contains(CANARY), "raw hints escaped into trace");
        }

        let db = fixture.db().await;
        assert_eq!(
            LocalCodexSession::count(&db, "agent_session", "state='stopped'").await,
            1
        );
        assert_eq!(
            LocalCodexSession::count(&db, "agent_checkpoint", "scope='subagent'").await,
            1
        );
        assert_eq!(
            LocalCodexSession::count(&db, "agent_checkpoint", "subagent_session_id IS NOT NULL")
                .await,
            0
        );
        let checkpoint_count = LocalCodexSession::count(&db, "agent_checkpoint", "1").await;
        assert!(checkpoint_count >= 2, "parent capture remains operational");
        let catalog = codex_finalize_catalog_snapshot(&db).await;
        assert!(!catalog.contains(CANARY));
        let rows = db
            .query_all_raw(Statement::from_string(
                db.get_database_backend(),
                "SELECT checkpoint_id FROM agent_checkpoint",
            ))
            .await
            .unwrap();
        for row in rows {
            let id: String = row.try_get_by("checkpoint_id").unwrap();
            let shown = codex_legacy_show(&fixture, &id).to_string();
            assert!(!shown.contains(CANARY));
        }
        assert!(
            fixture
                .exports(&db)
                .await
                .join("\n")
                .contains("CX15-MULTI-0")
        );
        db.close().await.unwrap();

        // Inspect actual catalog/journal bytes and every decompressed loose
        // object, including lifecycle metadata and objects reachable by traces.
        let storage = fixture.repo.join(".libra");
        let mut pending = vec![storage.clone()];
        let mut objects = 0;
        let mut journals = 0;
        while let Some(dir) = pending.pop() {
            for entry in std::fs::read_dir(dir).unwrap() {
                let entry = entry.unwrap();
                let path = entry.path();
                if entry.file_type().unwrap().is_dir() {
                    pending.push(path);
                    continue;
                }
                let bytes = std::fs::read(&path).unwrap();
                assert!(
                    !bytes.windows(CANARY.len()).any(|w| w == CANARY.as_bytes()),
                    "raw hint persisted at {}",
                    path.display()
                );
                let relative = path.strip_prefix(&storage).unwrap();
                let loose_directory = relative
                    .components()
                    .nth(1)
                    .and_then(|part| part.as_os_str().to_str());
                if relative.starts_with("objects")
                    && relative.components().count() == 3
                    && loose_directory.is_some_and(|part| {
                        part.len() == 2 && part.bytes().all(|byte| byte.is_ascii_hexdigit())
                    })
                {
                    let mut plain = Vec::new();
                    flate2::read::ZlibDecoder::new(bytes.as_slice())
                        .read_to_end(&mut plain)
                        .unwrap();
                    assert!(
                        !plain.windows(CANARY.len()).any(|w| w == CANARY.as_bytes()),
                        "hint persisted in decompressed object"
                    );
                    objects += 1;
                }
                if relative.starts_with("private/capture-diagnostics")
                    && path
                        .extension()
                        .is_some_and(|extension| extension == "jsonl")
                {
                    journals += 1;
                }
            }
        }
        assert!(
            objects > 0 && journals > 0,
            "storage and diagnostic checks must be non-vacuous"
        );
    }
}

const CX09_ROOT: &str = "123e4567-e89b-12d3-a456-426614174031";
const CX09_CHILD: &str = "123e4567-e89b-12d3-a456-426614174099";

fn cx09_rollout(
    fixture: &LocalCodexSession,
    id: &str,
    parent: Option<&str>,
    body: &str,
) -> std::path::PathBuf {
    let path = fixture
        .source
        .parent()
        .unwrap()
        .join(format!("rollout-2026-10-19T01-00-00-{id}.jsonl"));
    let mut header = serde_json::json!({
        "id":id, "session_id":id, "cwd":fixture.repo, "cli_version":"0.162.1"
    });
    if let Some(parent) = parent {
        header["session_id"] = serde_json::json!(parent);
        header["parent_thread_id"] = serde_json::json!(parent);
    }
    let rows = [
        serde_json::json!({"type":"session_meta","payload":header}),
        serde_json::json!({"type":"event_msg","payload":{"type":"task_started","turn_id":"cx09-turn"}}),
        serde_json::json!({"type":"turn_context","payload":{"turn_id":"cx09-turn"}}),
        serde_json::json!({"type":"response_item","payload":{"type":"message","role":"user","content":[{"type":"input_text","text":"child task"}]}}),
        serde_json::json!({"type":"response_item","payload":{"type":"reasoning","id":"cx09-reasoning","summary":[{"type":"summary_text","text":body}],"encrypted_content":format!("opaque:{body}"),"future_field":{"ordinary":format!("unknown:{body}")}}}),
        serde_json::json!({"type":"response_item","payload":{"type":"message","role":"assistant","content":[{"type":"output_text","text":"child answer"}]}}),
        serde_json::json!({"type":"event_msg","payload":{"type":"task_complete","turn_id":"cx09-turn"}}),
    ];
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(
        &path,
        rows.iter()
            .map(serde_json::Value::to_string)
            .collect::<Vec<_>>()
            .join("\n")
            + "\n",
    )
    .unwrap();
    path
}

#[tokio::test]
#[ignore = "operator-local Codex acceptance; never run in CI"]
async fn codex_child_usage_uri_real_helper_preserves_data_or_reports_incomplete_subtotal() {
    use std::{process::Stdio, time::Duration};

    use serde_json::json;
    use tokio::io::AsyncWriteExt;
    const ROOT: &str = "123e4567-e89b-12d3-a456-426614174031";
    const CHILD: &str = "123e4567-e89b-12d3-a456-426614174099";
    const TURN: &str = "01991d45-6f00-7000-8000-000000000001";
    let usage = json!({"input_tokens":3,"output_tokens":2,"cached_input_tokens":1,
        "cache_write_input_tokens":0,"reasoning_output_tokens":1,"total_tokens":5});
    let exact = |id| {
        json!({"thread_id":CHILD,"session_id":ROOT,"turn_id":TURN,
        "root_turn_id":TURN,"response_id":id,"usage":usage,
        "turn_token_usage":usage,"thread_token_usage":usage})
    };
    let compact = format!(
        "{{\"type\":\"compacted\",\"payload\":{{\"message\":\"postgres://u\",\"latest_token_usage_record\":{},\"window_id\":\"x@db tail\"}}}}",
        exact("hidden"),
    );
    let raw = format!(
        "{}\n{}\n{}\n{}\n",
        json!({"type":"session_meta","payload":{"id":CHILD,"session_id":ROOT,
            "parent_thread_id":ROOT,"cli_version":"0.162.1"}}),
        json!({"type":"turn_context","payload":{"turn_id":TURN,"model":"codex-source-model"}}),
        json!({"type":"token_usage_record","payload":exact("visible")}),
        compact,
    );
    let binding = serde_json::to_vec(&json!({"root":ROOT,"parent":ROOT,"child":CHILD,
        "capture_session":"parent-session","hook_turn":null}))
    .unwrap();
    // Exercise the registered legacy request and the actual CLI dispatcher;
    // this is synthetic ordinary data, not native provenance or an AI call.
    let mut request = vec![2];
    request.extend_from_slice(&0u64.to_le_bytes());
    request.extend_from_slice(&u16::try_from(binding.len()).unwrap().to_le_bytes());
    request.extend_from_slice(&binding);
    request.extend_from_slice(raw.as_bytes());
    let mut child = tokio::process::Command::new(env!("CARGO_BIN_EXE_libra"))
        .arg("--libra-internal-agent-subagent-projection-helper")
        .env_clear()
        .env("PATH", "/usr/bin:/bin")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .kill_on_drop(true)
        .spawn()
        .unwrap();
    let mut stdin = child.stdin.take().unwrap();
    stdin.write_all(&request).await.unwrap();
    drop(stdin);
    let out = tokio::time::timeout(Duration::from_secs(10), child.wait_with_output())
        .await
        .unwrap()
        .unwrap();
    assert!(out.status.success());
    assert!(out.stdout.len() >= 56);
    assert_eq!(out.stdout[0], 2, "legacy Codex projection response");
    let body_len =
        usize::try_from(u64::from_le_bytes(out.stdout[10..18].try_into().unwrap())).unwrap();
    let binding_len = u16::from_le_bytes(out.stdout[54..56].try_into().unwrap()) as usize;
    let start = 56 + binding_len;
    assert!(start + body_len <= out.stdout.len());
    let body = &out.stdout[start..start + body_len];
    let rows = std::str::from_utf8(body)
        .unwrap()
        .lines()
        .map(|line| serde_json::from_str::<serde_json::Value>(line).unwrap())
        .collect::<Vec<_>>();
    assert_eq!(
        body.iter().filter(|b| **b == b'\n').count(),
        raw.bytes().filter(|b| *b == b'\n').count()
    );
    let compacted = rows.iter().find(|row| row["type"] == "compacted").unwrap();
    let retained = compacted["payload"]
        .get("latest_token_usage_record")
        .is_some();
    let parsed = libra::internal::ai::observed_agents::extract::extract_codex(body);
    assert_eq!(
        parsed.usage.as_ref().unwrap().input_tokens,
        if retained { 6 } else { 3 }
    );
    if !retained {
        assert!(
            std::str::from_utf8(body)
                .unwrap()
                .contains("<REDACTED:credential-uri>")
        );
        assert!(
            parsed.partial,
            "a real helper's remaining subtotal cannot be complete after losing a response"
        );
        let agent = libra::internal::ai::observed_agents::agent_for(
            libra::internal::ai::observed_agents::AgentKind::Codex,
        );
        assert!(
            agent
                .as_token_calculator()
                .unwrap()
                .calculate_token_usage(body, 0)
                .is_err()
        );
    }
}

#[tokio::test]
#[ignore = "operator-local Codex acceptance; never run in CI"]
async fn codex_child_usage_live_currency_keeps_known_sources_and_derived_references() {
    let fixture = LocalCodexSession::new();
    let root = cx26_complete_rollout(&fixture, CX09_ROOT, None, "CX26-CURRENCY-PARENT");
    let child = cx26_complete_rollout(&fixture, CX09_CHILD, Some(CX09_ROOT), "CX26-CURRENCY-CHILD");
    cx26_append_spawn(&root, CX09_CHILD);
    cx26_append_usage(&root, CX09_ROOT, "cx26-currency-parent", 10, 4, true);
    cx26_append_usage(&child, CX09_CHILD, "cx26-currency-child", 3, 2, true);
    let out = cx09_hook_frame(&fixture, CX09_ROOT, &root, 10_000);
    assert!(out.status.success(), "{out:?}");
    let db = fixture.db().await;
    cx09_assert_settled(&db, CX09_ROOT).await;
    let metadata = cx09_parent_metadata(&fixture, &db).await;
    assert_eq!(metadata.len(), 1);
    let value = &metadata[0]["usage_attribution"];
    assert_eq!(value["scope"], "session_snapshot", "{value}");
    assert_eq!(value["as_of"], "live_capture", "{value}");
    assert_eq!(value["parent_token_usage"]["input_tokens"], 10, "{value}");
    assert_eq!(value["child_token_usage"]["input_tokens"], 3, "{value}");
    assert_eq!(value["unknown_source_count"], 0, "{value}");
    assert_eq!(value["sources"][0]["revision"], 1, "{value}");
    assert!(value["sources"][0]["parent"].is_null(), "{value}");
    assert!(!value.to_string().contains(CX09_ROOT));
    assert!(!value.to_string().contains(CX09_CHILD));
    assert!(
        value["sources"][0]["source"]
            .as_str()
            .unwrap()
            .starts_with("source/subagent-hmac-v2/")
    );
    assert!(uuid::Uuid::parse_str(value["sources"][0]["checkpoint_id"].as_str().unwrap()).is_ok());
    assert_eq!(value["sources"][0]["usage_state"], "complete", "{value}");
    assert_eq!(
        value["sources"][0]["usage_provenance"],
        "computed_this_pass"
    );
    assert_eq!(value["partial"], false);
    assert_eq!(value["aggregate_token_usage"]["input_tokens"], 13);
    assert_eq!(value["aggregate_includes_child_usage"], true);
    assert!(
        cx09_exports(&fixture, &db, "committed")
            .await
            .join("\n")
            .contains("CX26-CURRENCY-PARENT")
    );
    assert!(
        cx09_exports(&fixture, &db, "subagent")
            .await
            .join("\n")
            .contains("CX26-CURRENCY-CHILD")
    );
    db.close().await.unwrap();
}

#[tokio::test]
#[ignore = "operator-local Codex acceptance; never run in CI"]
async fn codex_child_usage_discovery_import_captures_two_level_descendants() {
    const GRANDCHILD: &str = "123e4567-e89b-12d3-a456-426614174101";
    let fixture = LocalCodexSession::new();
    let root = cx26_complete_rollout(&fixture, CX09_ROOT, None, "CX26-CLOSURE-ROOT");
    let child = cx26_complete_rollout(&fixture, CX09_CHILD, Some(CX09_ROOT), "CX26-CLOSURE-CHILD");
    let grand = cx26_complete_rollout(&fixture, GRANDCHILD, Some(CX09_CHILD), "CX26-CLOSURE-GRAND");
    let mut rows = std::fs::read_to_string(&grand)
        .unwrap()
        .lines()
        .map(|line| serde_json::from_str::<serde_json::Value>(line).unwrap())
        .collect::<Vec<_>>();
    rows[0]["payload"]["session_id"] = CX09_ROOT.into();
    std::fs::write(
        &grand,
        rows.iter()
            .map(serde_json::Value::to_string)
            .collect::<Vec<_>>()
            .join("\n")
            + "\n",
    )
    .unwrap();
    cx26_append_spawn(&root, CX09_CHILD);
    cx26_append_spawn(&child, GRANDCHILD);
    cx26_append_usage(&root, CX09_ROOT, "closure-root", 10, 4, false);
    cx26_append_usage(&child, CX09_CHILD, "closure-child", 3, 2, false);
    cx26_append_usage(&grand, GRANDCHILD, "closure-grand", 7, 1, false);
    let out = cx09_import(&fixture, &root);
    assert!(out.status.success(), "{out:?}");
    let db = fixture.db().await;
    assert_eq!(
        LocalCodexSession::count(&db, "agent_subagent_content_claim", "current_revision=1").await,
        2
    );
    let exported = cx09_exports(&fixture, &db, "subagent").await.join("\n");
    assert!(exported.contains("CX26-CLOSURE-CHILD"));
    assert!(exported.contains("CX26-CLOSURE-GRAND"));
    db.close().await.unwrap();
}

fn cx26_complete_rollout(
    fixture: &LocalCodexSession,
    id: &str,
    parent: Option<&str>,
    body: &str,
) -> std::path::PathBuf {
    let path = cx09_rollout(fixture, id, parent, body);
    let mut rows = std::fs::read_to_string(&path)
        .unwrap()
        .lines()
        .map(|line| serde_json::from_str::<serde_json::Value>(line).unwrap())
        .collect::<Vec<_>>();
    for row in &mut rows {
        if row["type"] == "response_item" && row["payload"]["type"] == "reasoning" {
            row["payload"]
                .as_object_mut()
                .unwrap()
                .remove("future_field");
            row["payload"]
                .as_object_mut()
                .unwrap()
                .remove("encrypted_content");
        }
        if row["type"] == "turn_context" {
            row["payload"]["model"] = serde_json::json!("codex-source-model");
        }
        if row["type"] == "response_item" && row["payload"]["role"] == "user" {
            row["payload"]["id"] = serde_json::json!("cx26-user");
            row["payload"]["internal_chat_message_metadata_passthrough"] =
                serde_json::json!({"turn_id":"cx09-turn","content_item_kinds":["user.text"]});
        }
    }
    std::fs::write(
        &path,
        rows.iter()
            .map(serde_json::Value::to_string)
            .collect::<Vec<_>>()
            .join("\n")
            + "\n",
    )
    .unwrap();
    let parsed = libra::internal::ai::observed_agents::extract::extract_codex(
        &std::fs::read(&path).unwrap(),
    );
    assert!(
        !parsed.partial,
        "complete usage control must be parser-complete: {:?}",
        parsed.warnings
    );
    path
}

fn cx26_append_usage(
    path: &std::path::Path,
    thread: &str,
    response: &str,
    input: u64,
    output: u64,
    mirror: bool,
) {
    use std::io::Write;
    let usage = serde_json::json!({
        "input_tokens":input,"output_tokens":output,"cached_input_tokens":1,
        "cache_write_input_tokens":0,"reasoning_output_tokens":1,"total_tokens":input+output
    });
    let record = serde_json::json!({"type":"token_usage_record","payload":{
        "thread_id":thread,"session_id":CX09_ROOT,"turn_id":"cx09-turn",
        "root_turn_id":"cx09-turn","response_id":response,"usage":usage,
        "turn_token_usage":usage,"thread_token_usage":usage
    }});
    let mut file = std::fs::OpenOptions::new().append(true).open(path).unwrap();
    writeln!(file, "{record}").unwrap();
    if mirror {
        writeln!(file, "{record}").unwrap();
        writeln!(
            file,
            "{}",
            serde_json::json!({"type":"event_msg","payload":{
                "type":"token_count","info":{"total_token_usage":usage,"last_token_usage":usage}
            }})
        )
        .unwrap();
    }
}

fn cx26_append_spawn(path: &std::path::Path, child: &str) {
    use std::io::Write;
    let header: serde_json::Value = serde_json::from_str(
        std::fs::read_to_string(path)
            .unwrap()
            .lines()
            .next()
            .unwrap(),
    )
    .unwrap();
    let sender = header["payload"]["id"].as_str().unwrap();
    let mut file = std::fs::OpenOptions::new().append(true).open(path).unwrap();
    writeln!(
        file,
        "{}",
        serde_json::json!({"type":"event_msg","payload":{
            "type":"collab_agent_spawn_end","sender_thread_id":sender,"new_thread_id":child
        }})
    )
    .unwrap();
}

#[tokio::test]
#[ignore = "operator-local Codex acceptance; never run in CI"]
async fn codex_child_usage_live_snapshot_deduplicates_response_and_current_revision() {
    let fixture = LocalCodexSession::new();
    let root = cx26_complete_rollout(&fixture, CX09_ROOT, None, "CX26-PARENT");
    let child = cx26_complete_rollout(&fixture, CX09_CHILD, Some(CX09_ROOT), "CX26-CHILD");
    cx26_append_spawn(&root, CX09_CHILD);
    cx26_append_usage(&root, CX09_ROOT, "cx26-parent-response", 10, 4, true);
    cx26_append_usage(&child, CX09_CHILD, "cx26-child-response", 3, 2, true);
    let out = cx09_hook_frame(&fixture, CX09_ROOT, &root, 10_000);
    assert!(out.status.success(), "{out:?}");
    let db = fixture.db().await;
    cx09_assert_settled(&db, CX09_ROOT).await;
    let metadata = cx09_parent_metadata(&fixture, &db).await;
    assert_eq!(metadata.len(), 1);
    let attribution = &metadata[0]["usage_attribution"];
    assert_eq!(attribution["schema_version"], 1, "{attribution}");
    assert_eq!(attribution["scope"], "session_snapshot", "{attribution}");
    assert_eq!(attribution["source_count"], 1, "{attribution}");
    assert_eq!(attribution["unknown_source_count"], 0, "{attribution}");
    assert_eq!(
        attribution["parent_token_usage"]["input_tokens"], 10,
        "{attribution}"
    );
    assert_eq!(
        attribution["child_token_usage"]["input_tokens"], 3,
        "{attribution}"
    );
    assert_eq!(
        attribution["aggregate_token_usage"]["input_tokens"], 13,
        "{attribution}"
    );
    assert_eq!(
        attribution["aggregate_token_usage"]["output_tokens"], 6,
        "{attribution}"
    );
    assert_eq!(
        attribution["aggregate_includes_child_usage"], true,
        "{attribution}"
    );
    assert_eq!(
        LocalCodexSession::count(&db, "agent_subagent_content_claim", "current_revision=1").await,
        1
    );
    let checkpoints = LocalCodexSession::count(&db, "agent_checkpoint", "1").await;
    let replay = cx09_hook_frame(&fixture, CX09_ROOT, &root, 10_000);
    assert!(replay.status.success(), "{replay:?}");
    assert_eq!(
        LocalCodexSession::count(&db, "agent_checkpoint", "1").await,
        checkpoints
    );
    assert_eq!(cx09_parent_metadata(&fixture, &db).await, metadata);
    assert!(
        cx09_exports(&fixture, &db, "subagent")
            .await
            .join("\n")
            .contains("CX26-CHILD")
    );
    db.close().await.unwrap();
}

#[tokio::test]
#[ignore = "operator-local Codex acceptance; never run in CI"]
async fn codex_child_usage_missing_child_is_unknown_while_thinking_remains_exportable() {
    let fixture = LocalCodexSession::new();
    let root = cx26_complete_rollout(&fixture, CX09_ROOT, None, "CX26-KNOWN-PARENT");
    cx09_rollout(
        &fixture,
        CX09_CHILD,
        Some(CX09_ROOT),
        "CX26-UNKNOWN-USAGE-THINKING",
    );
    cx26_append_spawn(&root, CX09_CHILD);
    cx26_append_usage(&root, CX09_ROOT, "cx26-parent-response", 10, 4, false);
    let out = cx09_hook_frame(&fixture, CX09_ROOT, &root, 10_000);
    assert!(out.status.success(), "{out:?}");
    let db = fixture.db().await;
    cx09_assert_settled(&db, CX09_ROOT).await;
    let metadata = cx09_parent_metadata(&fixture, &db).await;
    assert_eq!(metadata.len(), 1);
    let attribution = &metadata[0]["usage_attribution"];
    assert_eq!(attribution["schema_version"], 1, "{attribution}");
    assert_eq!(
        attribution["parent_token_usage"]["input_tokens"], 10,
        "{attribution}"
    );
    assert_eq!(attribution["unknown_source_count"], 1, "{attribution}");
    assert_eq!(attribution["partial"], true, "{attribution}");
    assert!(attribution["child_token_usage"].is_null(), "{attribution}");
    assert!(
        attribution["aggregate_token_usage"].is_null(),
        "{attribution}"
    );
    assert_eq!(
        attribution["aggregate_includes_child_usage"], false,
        "{attribution}"
    );
    assert_eq!(
        LocalCodexSession::count(&db, "agent_subagent_content_claim", "current_revision=1").await,
        1
    );
    assert!(
        cx09_exports(&fixture, &db, "subagent")
            .await
            .join("\n")
            .contains("CX26-UNKNOWN-USAGE-THINKING")
    );
    db.close().await.unwrap();
}

#[tokio::test]
#[ignore = "operator-local Codex acceptance; never run in CI"]
async fn codex_child_usage_first_import_aggregates_only_durable_child_snapshot() {
    let fixture = LocalCodexSession::new();
    let root = cx26_complete_rollout(&fixture, CX09_ROOT, None, "CX26-IMPORT-PARENT");
    let child = cx26_complete_rollout(&fixture, CX09_CHILD, Some(CX09_ROOT), "CX26-IMPORT-CHILD");
    cx26_append_spawn(&root, CX09_CHILD);
    cx26_append_usage(&root, CX09_ROOT, "cx26-import-parent-response", 10, 4, true);
    cx26_append_usage(&child, CX09_CHILD, "cx26-import-child-response", 3, 2, true);
    let out = cx09_import(&fixture, &root);
    assert!(out.status.success(), "{out:?}");
    let db = fixture.db().await;
    assert_eq!(
        LocalCodexSession::count(&db, "agent_subagent_content_claim", "current_revision=1").await,
        1
    );
    assert!(
        cx09_exports(&fixture, &db, "subagent")
            .await
            .join("\n")
            .contains("CX26-IMPORT-CHILD")
    );
    let metadata = cx09_parent_metadata(&fixture, &db).await;
    assert!(!metadata.is_empty());
    for item in &metadata {
        let attribution = &item["usage_attribution"];
        assert_eq!(attribution["schema_version"], 1, "{attribution}");
        assert_eq!(attribution["scope"], "session_snapshot", "{attribution}");
        assert_eq!(attribution["source_count"], 1, "{attribution}");
        assert_eq!(attribution["unknown_source_count"], 0, "{attribution}");
        assert_eq!(attribution["partial"], false, "{item}");
        assert_eq!(
            attribution["aggregate_token_usage"]["input_tokens"], 13,
            "{attribution}"
        );
        assert_eq!(
            attribution["aggregate_token_usage"]["output_tokens"], 6,
            "{attribution}"
        );
        assert_eq!(
            attribution["aggregate_includes_child_usage"], true,
            "{attribution}"
        );
    }
    let checkpoints = LocalCodexSession::count(&db, "agent_checkpoint", "1").await;
    let replay = cx09_import(&fixture, &root);
    assert!(replay.status.success(), "{replay:?}");
    assert_eq!(
        LocalCodexSession::count(&db, "agent_checkpoint", "1").await,
        checkpoints
    );
    assert_eq!(cx09_parent_metadata(&fixture, &db).await, metadata);
    db.close().await.unwrap();
}

#[tokio::test]
#[ignore = "operator-local Codex acceptance; never run in CI"]
async fn codex_child_usage_parent_foreign_response_never_inflates_aggregate() {
    let fixture = LocalCodexSession::new();
    let root = cx26_complete_rollout(&fixture, CX09_ROOT, None, "CX26-FOREIGN-PARENT");
    let child = cx26_complete_rollout(&fixture, CX09_CHILD, Some(CX09_ROOT), "CX26-FOREIGN-CHILD");
    cx26_append_spawn(&root, CX09_CHILD);
    cx26_append_usage(&root, CX09_ROOT, "parent-own", 10, 4, false);
    cx26_append_usage(&child, CX09_CHILD, "child-own", 3, 2, false);
    cx26_append_usage(&root, CX09_CHILD, "foreign-child-copy", 3, 2, false);
    let out = cx09_hook_frame(&fixture, CX09_ROOT, &root, 10_000);
    assert!(out.status.success(), "{out:?}");
    let db = fixture.db().await;
    let metadata = cx09_parent_metadata(&fixture, &db).await;
    assert!(!metadata.is_empty());
    for item in &metadata {
        let value = &item["usage_attribution"];
        assert_eq!(value["partial"], true, "{value}");
        assert!(value["aggregate_token_usage"].is_null(), "{value}");
        assert_eq!(value["child_token_usage"]["input_tokens"], 3, "{value}");
    }
    assert!(
        cx09_exports(&fixture, &db, "committed")
            .await
            .join("\n")
            .contains("CX26-FOREIGN-PARENT")
    );
    assert!(
        cx09_exports(&fixture, &db, "subagent")
            .await
            .join("\n")
            .contains("CX26-FOREIGN-CHILD")
    );
    db.close().await.unwrap();
}

#[tokio::test]
#[ignore = "operator-local Codex acceptance; never run in CI"]
async fn codex_child_usage_child_metadata_keeps_numeric_usage_and_ordinary_opaque_data() {
    use sea_orm::{ConnectionTrait, Statement};

    let fixture = LocalCodexSession::new();
    let root = cx26_complete_rollout(&fixture, CX09_ROOT, None, "CX26-LEAF-PARENT");
    let child = cx09_rollout(&fixture, CX09_CHILD, Some(CX09_ROOT), "CX26-LEAF-THINKING");
    cx26_append_spawn(&root, CX09_CHILD);
    cx26_append_usage(&root, CX09_ROOT, "cx26-leaf-parent-response", 10, 4, false);
    cx26_append_usage(&child, CX09_CHILD, "cx26-leaf-child-response", 3, 2, false);
    let out = cx09_hook_frame(&fixture, CX09_ROOT, &root, 10_000);
    assert!(out.status.success(), "{out:?}");
    let db = fixture.db().await;
    cx09_assert_settled(&db, CX09_ROOT).await;
    let rows = db
        .query_all_raw(Statement::from_string(
            db.get_database_backend(),
            "SELECT metadata_blob_oid FROM agent_checkpoint WHERE scope='subagent'".to_owned(),
        ))
        .await
        .unwrap();
    assert_eq!(rows.len(), 1);
    let oid: String = rows[0].try_get_by("metadata_blob_oid").unwrap();
    let out = fixture.run(&["cat-file", "-p", &oid]);
    assert!(out.status.success(), "{out:?}");
    let metadata: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
    let usage = &metadata["subagent"]["usage"];
    assert_eq!(usage["schema_version"], 1, "{usage}");
    assert_eq!(usage["token_usage"]["input_tokens"], 3, "{usage}");
    assert_eq!(usage["token_usage"]["output_tokens"], 2, "{usage}");
    assert_eq!(usage["token_usage"]["cached_tokens"], 1, "{usage}");
    assert!(
        matches!(usage["usage_state"].as_str(), Some("complete" | "partial")),
        "{usage}"
    );
    let body = cx09_exports(&fixture, &db, "subagent").await.join("\n");
    for marker in [
        "CX26-LEAF-THINKING",
        "opaque:CX26-LEAF-THINKING",
        "unknown:CX26-LEAF-THINKING",
    ] {
        assert!(body.contains(marker), "ordinary child export lost {marker}");
    }
    assert_eq!(
        LocalCodexSession::count(&db, "agent_subagent_content_claim", "current_revision=1").await,
        1
    );
    let checkpoints = LocalCodexSession::count(&db, "agent_checkpoint", "1").await;
    assert!(
        cx09_hook_frame(&fixture, CX09_ROOT, &root, 10_000)
            .status
            .success()
    );
    assert_eq!(
        LocalCodexSession::count(&db, "agent_checkpoint", "1").await,
        checkpoints
    );
    db.close().await.unwrap();
}

fn cx09_hook_frame(
    fixture: &LocalCodexSession,
    id: &str,
    source: &std::path::Path,
    budget_ms: u64,
) -> std::process::Output {
    cx09_hook(fixture, id, source, budget_ms, "SessionEnd", "session-end")
}

fn cx09_hook(
    fixture: &LocalCodexSession,
    id: &str,
    source: &std::path::Path,
    budget_ms: u64,
    event: &str,
    command: &str,
) -> std::process::Output {
    let frame = serde_json::json!({
        "hook_event_name":event,"session_id":id,"cwd":fixture.repo,
        "transcript_path":source,"turn_id":"cx09-turn"
    });
    cx09_send_hook(fixture, &frame, budget_ms, command)
}

fn cx09_send_hook(
    fixture: &LocalCodexSession,
    frame: &serde_json::Value,
    budget_ms: u64,
    command: &str,
) -> std::process::Output {
    use std::io::Write;
    let mut child = std::process::Command::new(env!("CARGO_BIN_EXE_libra"))
        .current_dir(&fixture.repo)
        .env_clear()
        .env("PATH", "/usr/bin:/bin")
        .env("HOME", &fixture.home)
        .env("USERPROFILE", &fixture.home)
        .env("LIBRA_TEST_HOME", &fixture.home)
        .env("LIBRA_TEST", "1")
        .env("LIBRA_ERROR_JSON", "1")
        .args([
            "hooks",
            "codex",
            command,
            "--capture-budget-ms",
            &budget_ms.to_string(),
        ])
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .unwrap();
    child
        .stdin
        .take()
        .unwrap()
        .write_all(&serde_json::to_vec(frame).unwrap())
        .unwrap();
    child.wait_with_output().unwrap()
}

async fn cx09_terminal_state(db: &sea_orm::DatabaseConnection, native: &str) -> serde_json::Value {
    use sea_orm::{ConnectionTrait, Statement};
    let row = db
        .query_one_raw(Statement::from_sql_and_values(
            db.get_database_backend(),
            "SELECT state,stopped_at,metadata_json FROM agent_session WHERE provider_session_id=? AND agent_kind='codex'",
            [native.into()],
        ))
        .await
        .unwrap()
        .unwrap();
    serde_json::json!({
        "state":row.try_get_by::<String,_>("state").unwrap(),
        "stopped_at":row.try_get_by::<Option<i64>,_>("stopped_at").unwrap(),
        "metadata":serde_json::from_str::<serde_json::Value>(&row.try_get_by::<String,_>("metadata_json").unwrap()).unwrap()
    })
}

async fn cx09_assert_settled(db: &sea_orm::DatabaseConnection, native: &str) {
    let state = cx09_terminal_state(db, native).await;
    assert_eq!(state["state"], "stopped", "{state}");
    assert!(state["stopped_at"].is_i64(), "{state}");
    let entries = state["metadata"]["capture_catalog_receipts_v1"]["entries"]
        .as_array()
        .unwrap();
    let terminals = entries
        .iter()
        .filter(|entry| entry["intent"]["phase"] == "stopped")
        .collect::<Vec<_>>();
    assert_eq!(terminals.len(), 1, "the native terminal receipt must exist");
    assert_eq!(terminals[0]["status"], "complete", "{state}");
    assert!(terminals[0].get("child_gate").is_none(), "{state}");
}

async fn cx09_parent_metadata(
    fixture: &LocalCodexSession,
    db: &sea_orm::DatabaseConnection,
) -> Vec<serde_json::Value> {
    use sea_orm::{ConnectionTrait, Statement};
    db.query_all_raw(Statement::from_string(
        db.get_database_backend(),
        "SELECT metadata_blob_oid FROM agent_checkpoint WHERE scope='committed'".to_owned(),
    ))
    .await
    .unwrap()
    .into_iter()
    .map(|row| {
        let oid: String = row.try_get_by("metadata_blob_oid").unwrap();
        let out = fixture.run(&["cat-file", "-p", &oid]);
        assert!(out.status.success(), "{out:?}");
        serde_json::from_slice(&out.stdout).unwrap()
    })
    .collect()
}

async fn cx09_exports(
    fixture: &LocalCodexSession,
    db: &sea_orm::DatabaseConnection,
    scope: &str,
) -> Vec<String> {
    use sea_orm::{ConnectionTrait, Statement};
    db.query_all_raw(Statement::from_sql_and_values(
        db.get_database_backend(),
        "SELECT checkpoint_id FROM agent_checkpoint WHERE scope=? ORDER BY checkpoint_id",
        [scope.into()],
    ))
    .await
    .unwrap()
    .into_iter()
    .map(|row| {
        let id: String = row.try_get_by("checkpoint_id").unwrap();
        let out = fixture.run(&["agent", "checkpoint", "export", &id]);
        assert!(out.status.success(), "{out:?}");
        String::from_utf8(out.stdout).unwrap()
    })
    .collect()
}

fn cx09_import(fixture: &LocalCodexSession, path: &std::path::Path) -> std::process::Output {
    fixture.run(&[
        "agent",
        "import",
        "--path",
        path.to_str().unwrap(),
        "--agent",
        "codex",
        "--yes",
        "--json",
    ])
}

async fn cx09_artifact_bytes(
    db: &sea_orm::DatabaseConnection,
    key: &str,
    header: &serde_json::Value,
) -> Vec<(String, String, String)> {
    use sea_orm::{ConnectionTrait, Statement};
    db.query_all_raw(Statement::from_sql_and_values(
        db.get_database_backend(),
        "SELECT scope,key,hex(value) AS body FROM metadata_kv WHERE target=? AND
        ((scope='agent_capture_pending' AND key=?) OR
         (scope='agent_capture_pending_chunk' AND key>=? AND key<?) OR
         (scope='agent_capture_session_alias' AND key=?)) ORDER BY scope,key",
        [
            header["binding"]["scope"]["repo_id"]
                .as_str()
                .unwrap()
                .into(),
            key.into(),
            format!("{key}:").into(),
            format!("{key};").into(),
            header["binding"]["session_id"].as_str().unwrap().into(),
        ],
    ))
    .await
    .unwrap()
    .into_iter()
    .map(|row| {
        (
            row.try_get_by("scope").unwrap(),
            row.try_get_by("key").unwrap(),
            row.try_get_by("body").unwrap(),
        )
    })
    .collect()
}

async fn cx09_reserved_claim_snapshot(db: &sea_orm::DatabaseConnection) -> Vec<String> {
    use sea_orm::{ConnectionTrait, Statement};
    db.query_all_raw(Statement::from_string(
        db.get_database_backend(),
        "SELECT json_object('parent',parent_session_id,'provider',provider_kind,
            'source',source_key,'schema',content_schema_version,'revision',current_revision,
            'checkpoint',current_checkpoint_id,'digest',current_digest,'state',state,
            'attempt_digest',attempt_digest,'attempt_checkpoint',attempt_checkpoint_id,
            'owner',owner,'lease',lease_expires_at,'fence',fence_token,
            'created',created_at,'updated',updated_at,'cursor',revision_cursor,
            'sync_revision',sync_revision) AS snapshot
         FROM agent_subagent_content_claim WHERE state='reserved'
         ORDER BY parent_session_id,source_key"
            .to_owned(),
    ))
    .await
    .unwrap()
    .into_iter()
    .map(|row| row.try_get_by("snapshot").unwrap())
    .collect()
}

#[tokio::test]
#[ignore = "operator-local Codex acceptance; never run in CI"]
async fn codex_child_content_busy_terminal_preserves_owner_and_replays_after_expiry() {
    use sea_orm::{ConnectionTrait, Statement};

    let fixture = LocalCodexSession::new();
    let root = cx09_rollout(&fixture, CX09_ROOT, None, "CX09-BUSY-PARENT");
    cx09_rollout(&fixture, CX09_CHILD, Some(CX09_ROOT), "CX09-BUSY-OLD");
    let stop = cx09_hook(&fixture, CX09_ROOT, &root, 10_000, "Stop", "stop");
    assert!(stop.status.success(), "{stop:?}");
    let db = fixture.db().await;
    assert_eq!(
        LocalCodexSession::count(&db, "agent_subagent_content_claim", "1").await,
        1
    );
    assert!(
        cx09_exports(&fixture, &db, "subagent")
            .await
            .join("\n")
            .contains("CX09-BUSY-OLD")
    );
    db.execute_raw(Statement::from_sql_and_values(
        db.get_database_backend(),
        "UPDATE agent_subagent_content_claim SET state='reserved',owner='cx09-other-owner',
         attempt_digest=current_digest,attempt_checkpoint_id='cx09-busy-attempt',
         lease_expires_at=?,fence_token=fence_token+1",
        [(chrono::Utc::now().timestamp_millis() + 60_000).into()],
    ))
    .await
    .unwrap();
    let reserved = cx09_reserved_claim_snapshot(&db).await;
    assert_eq!(reserved.len(), 1);
    cx09_rollout(&fixture, CX09_CHILD, Some(CX09_ROOT), "CX09-BUSY-NEW");
    let busy = cx09_hook_frame(&fixture, CX09_ROOT, &root, 10_000);
    assert!(!busy.status.success(), "{busy:?}");
    assert_eq!(cx09_reserved_claim_snapshot(&db).await, reserved);
    let state = cx09_terminal_state(&db, CX09_ROOT).await;
    assert_eq!(state["state"], "active", "{state}");
    assert!(state["stopped_at"].is_null(), "{state}");
    let terminals = state["metadata"]["capture_catalog_receipts_v1"]["entries"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|entry| entry["intent"]["phase"] == "stopped")
        .collect::<Vec<_>>();
    assert_eq!(terminals.len(), 1, "{state}");
    assert_ne!(terminals[0]["status"], "complete", "{state}");
    assert!(terminals[0].get("child_gate").is_some(), "{state}");
    assert_eq!(
        LocalCodexSession::count(&db, "agent_checkpoint", "scope='subagent'").await,
        1
    );
    assert!(
        cx09_exports(&fixture, &db, "subagent")
            .await
            .join("\n")
            .contains("CX09-BUSY-OLD")
    );
    let parents = LocalCodexSession::count(&db, "agent_checkpoint", "scope='committed'").await;
    db.execute_unprepared("UPDATE agent_subagent_content_claim SET lease_expires_at=0")
        .await
        .unwrap();
    let replay = cx09_hook_frame(&fixture, CX09_ROOT, &root, 10_000);
    assert!(replay.status.success(), "{replay:?}");
    cx09_assert_settled(&db, CX09_ROOT).await;
    assert_eq!(
        LocalCodexSession::count(
            &db,
            "agent_subagent_content_claim",
            "state='idle' AND owner IS NULL AND current_revision=2"
        )
        .await,
        1
    );
    assert_eq!(
        LocalCodexSession::count(&db, "agent_checkpoint", "scope='committed'").await,
        parents
    );
    let bodies = cx09_exports(&fixture, &db, "subagent").await.join("\n");
    assert!(bodies.contains("CX09-BUSY-OLD") && bodies.contains("CX09-BUSY-NEW"));
    let checkpoints = LocalCodexSession::count(&db, "agent_checkpoint", "1").await;
    assert!(
        cx09_hook_frame(&fixture, CX09_ROOT, &root, 10_000)
            .status
            .success()
    );
    cx09_assert_settled(&db, CX09_ROOT).await;
    assert_eq!(
        LocalCodexSession::count(&db, "agent_checkpoint", "1").await,
        checkpoints
    );
    db.close().await.unwrap();
}

#[cfg(unix)]
#[tokio::test]
#[ignore = "operator-local Codex acceptance; never run in CI"]
async fn codex_child_content_permission_denied_keeps_partial_parent_and_imports_after_restore() {
    use std::{io::Write, os::unix::fs::PermissionsExt};

    let fixture = LocalCodexSession::new();
    let root = cx09_rollout(&fixture, CX09_ROOT, None, "CX09-PERMISSION-PARENT");
    let child = cx09_rollout(
        &fixture,
        CX09_CHILD,
        Some(CX09_ROOT),
        "CX09-PERMISSION-CHILD",
    );
    let mut file = std::fs::OpenOptions::new()
        .append(true)
        .open(&root)
        .unwrap();
    writeln!(
        file,
        "{}",
        serde_json::json!({"type":"event_msg","payload":{
            "type":"collab_agent_spawn_end","sender_thread_id":CX09_ROOT,"new_thread_id":CX09_CHILD
        }})
    )
    .unwrap();
    drop(file);
    let permissions = std::fs::metadata(&child).unwrap().permissions();
    std::fs::set_permissions(&child, std::fs::Permissions::from_mode(0o0)).unwrap();
    assert_eq!(
        std::fs::read(&child).unwrap_err().kind(),
        std::io::ErrorKind::PermissionDenied,
        "this operator-local case requires an effective permission denial"
    );
    let out = cx09_hook_frame(&fixture, CX09_ROOT, &root, 10_000);
    std::fs::set_permissions(&child, permissions).unwrap();
    assert!(out.status.success(), "{out:?}");
    let db = fixture.db().await;
    cx09_assert_settled(&db, CX09_ROOT).await;
    assert_eq!(
        LocalCodexSession::count(&db, "agent_subagent_content_claim", "1").await,
        0
    );
    assert_eq!(
        LocalCodexSession::count(&db, "agent_checkpoint", "scope='subagent'").await,
        0
    );
    let metadata = cx09_parent_metadata(&fixture, &db).await;
    assert!(!metadata.is_empty());
    for item in metadata {
        assert_eq!(item["extraction"]["partial"], true, "{item}");
        assert!(
            item["extraction"]["warnings"]
                .as_array()
                .unwrap()
                .iter()
                .any(|warning| warning
                    == "deadline-bound extraction omitted child source aggregation"),
            "{item}"
        );
    }
    assert!(
        cx09_exports(&fixture, &db, "committed")
            .await
            .join("\n")
            .contains("CX09-PERMISSION-PARENT")
    );
    let imported = cx09_import(&fixture, &root);
    assert!(imported.status.success(), "{imported:?}");
    assert_eq!(
        LocalCodexSession::count(&db, "agent_subagent_content_claim", "1").await,
        1
    );
    assert!(
        cx09_exports(&fixture, &db, "subagent")
            .await
            .join("\n")
            .contains("CX09-PERMISSION-CHILD")
    );
    db.close().await.unwrap();
}

#[tokio::test]
#[ignore = "operator-local Codex acceptance; never run in CI"]
async fn codex_child_content_source_budget_caps_seventeen_children_and_settles_partial() {
    use std::io::Write;

    let fixture = LocalCodexSession::new();
    let root = cx09_rollout(&fixture, CX09_ROOT, None, "CX09-SOURCE-CAP-PARENT");
    for index in 0..17 {
        let id = format!("123e4567-e89b-12d3-a456-{:012}", index + 1);
        cx09_rollout(
            &fixture,
            &id,
            Some(CX09_ROOT),
            &format!("CX09-CAPPED-BODY-{index:02}"),
        );
        let mut file = std::fs::OpenOptions::new()
            .append(true)
            .open(&root)
            .unwrap();
        writeln!(
            file,
            "{}",
            serde_json::json!({"type":"event_msg","payload":{
                "type":"collab_agent_spawn_end","sender_thread_id":CX09_ROOT,"new_thread_id":id
            }})
        )
        .unwrap();
    }
    let out = cx09_hook_frame(&fixture, CX09_ROOT, &root, 10_000);
    assert!(out.status.success(), "{out:?}");
    let db = fixture.db().await;
    cx09_assert_settled(&db, CX09_ROOT).await;
    assert_eq!(
        LocalCodexSession::count(&db, "agent_subagent_content_claim", "1").await,
        16
    );
    assert_eq!(
        LocalCodexSession::count(
            &db,
            "agent_subagent_content_claim",
            "state='idle' AND owner IS NULL AND current_revision=1"
        )
        .await,
        16
    );
    assert_eq!(
        LocalCodexSession::count(&db, "agent_checkpoint", "scope='subagent'").await,
        16
    );
    let exports = cx09_exports(&fixture, &db, "subagent").await;
    assert_eq!(exports.len(), 16);
    let bodies = exports.join("\n");
    assert_eq!(
        (0..17)
            .filter(|index| bodies.contains(&format!("CX09-CAPPED-BODY-{index:02}")))
            .count(),
        16,
        "retain sixteen distinct real child bodies without inventing an omitted source"
    );
    let metadata = cx09_parent_metadata(&fixture, &db).await;
    assert!(!metadata.is_empty());
    for item in metadata {
        assert_eq!(item["extraction"]["partial"], true, "{item}");
        assert!(
            item["extraction"]["warnings"]
                .as_array()
                .unwrap()
                .iter()
                .any(|warning| warning
                    == "deadline-bound extraction omitted child source aggregation"),
            "{item}"
        );
    }
    assert!(
        cx09_exports(&fixture, &db, "committed")
            .await
            .join("\n")
            .contains("CX09-SOURCE-CAP-PARENT")
    );
    db.close().await.unwrap();
}

#[tokio::test]
#[ignore = "operator-local Codex acceptance; never run in CI"]
async fn codex_child_content_live_terminal_exports_readable_child_and_replays_once() {
    let fixture = LocalCodexSession::new();
    let root = cx09_rollout(&fixture, CX09_ROOT, None, "CX09-PARENT");
    cx09_rollout(
        &fixture,
        CX09_CHILD,
        Some(CX09_ROOT),
        "CX09-LIVE-CHILD-THINKING",
    );
    let first = cx09_hook_frame(&fixture, CX09_ROOT, &root, 10_000);
    assert!(first.status.success(), "{first:?}");
    let db = fixture.db().await;
    cx09_assert_settled(&db, CX09_ROOT).await;
    assert_eq!(
        LocalCodexSession::count(&db, "agent_checkpoint", "scope='subagent'").await,
        1,
        "terminal child discovery must persist its own checkpoint"
    );
    let children = cx09_exports(&fixture, &db, "subagent").await.join("\n");
    let parents = cx09_exports(&fixture, &db, "committed").await.join("\n");
    for marker in [
        "CX09-LIVE-CHILD-THINKING",
        "opaque:CX09-LIVE-CHILD-THINKING",
        "unknown:CX09-LIVE-CHILD-THINKING",
    ] {
        assert!(
            children.contains(marker),
            "child-scoped default export lost {marker}"
        );
        assert!(
            !parents.contains(marker),
            "parent-only data cannot satisfy the child oracle"
        );
    }
    let before = LocalCodexSession::count(&db, "agent_checkpoint", "1").await;
    let revisions = LocalCodexSession::count(&db, "agent_subagent_content_revision", "1").await;
    let replay = cx09_hook_frame(&fixture, CX09_ROOT, &root, 10_000);
    assert!(replay.status.success(), "{replay:?}");
    cx09_assert_settled(&db, CX09_ROOT).await;
    assert_eq!(
        LocalCodexSession::count(&db, "agent_checkpoint", "1").await,
        before
    );
    assert_eq!(
        LocalCodexSession::count(&db, "agent_subagent_content_revision", "1").await,
        revisions
    );
    assert_eq!(
        LocalCodexSession::count(
            &db,
            "metadata_kv",
            "scope IN ('agent_capture_pending','agent_capture_quarantine')"
        )
        .await,
        0
    );
    std::fs::remove_dir_all(fixture.home.join(".codex/sessions")).unwrap();
    assert_eq!(
        cx09_exports(&fixture, &db, "subagent").await.join("\n"),
        children
    );
    db.close().await.unwrap();
}

#[tokio::test]
#[ignore = "operator-local Codex acceptance; never run in CI"]
async fn codex_child_content_import_exports_child_and_advances_its_independent_revision() {
    let fixture = LocalCodexSession::new();
    let root = cx09_rollout(&fixture, CX09_ROOT, None, "CX09-PARENT");
    cx09_rollout(
        &fixture,
        CX09_CHILD,
        Some(CX09_ROOT),
        "CX09-IMPORT-CHILD-THINKING",
    );
    // A complete discovery positive includes the parent's observed spawn
    // edge. Header-only fallback remains valuable data but is partial.
    {
        use std::io::Write;
        let edge = serde_json::json!({"type":"event_msg","payload":{
            "type":"collab_agent_spawn_end","sender_thread_id":CX09_ROOT,"new_thread_id":CX09_CHILD
        }});
        let mut file = std::fs::OpenOptions::new()
            .append(true)
            .open(&root)
            .unwrap();
        writeln!(file, "{edge}").unwrap();
    }
    let first = cx09_import(&fixture, &root);
    assert!(first.status.success(), "{first:?}");
    let db = fixture.db().await;
    assert_eq!(
        LocalCodexSession::count(&db, "agent_checkpoint", "scope='subagent'").await,
        1,
        "explicit parent import must also retain verified child data"
    );
    let body = cx09_exports(&fixture, &db, "subagent").await.join("\n");
    assert!(body.contains("CX09-IMPORT-CHILD-THINKING"));
    assert_eq!(
        LocalCodexSession::count(&db, "agent_subagent_content_revision", "1").await,
        1
    );
    let before = LocalCodexSession::count(&db, "agent_checkpoint", "1").await;
    let parents = LocalCodexSession::count(&db, "agent_checkpoint", "scope='committed'").await;
    assert!(cx09_import(&fixture, &root).status.success());
    assert_eq!(
        LocalCodexSession::count(&db, "agent_subagent_content_revision", "1").await,
        1
    );
    assert_eq!(
        LocalCodexSession::count(&db, "agent_checkpoint", "1").await,
        before
    );
    cx09_rollout(
        &fixture,
        CX09_CHILD,
        Some(CX09_ROOT),
        "CX09-IMPORT-CHILD-NEW-THINKING",
    );
    let changed = cx09_import(&fixture, &root);
    assert!(changed.status.success(), "{changed:?}");
    assert_eq!(
        LocalCodexSession::count(&db, "agent_subagent_content_revision", "1").await,
        2
    );
    let bodies = cx09_exports(&fixture, &db, "subagent").await.join("\n");
    assert!(
        bodies.contains("CX09-IMPORT-CHILD-THINKING")
            && bodies.contains("CX09-IMPORT-CHILD-NEW-THINKING")
    );
    assert_eq!(
        LocalCodexSession::count(&db, "agent_checkpoint", "scope='committed'").await,
        parents
    );
    db.close().await.unwrap();
}

#[tokio::test]
#[ignore = "operator-local Codex acceptance; never run in CI"]
async fn codex_child_content_partial_import_reports_progress_and_keeps_child() {
    let fixture = LocalCodexSession::new();
    let root = cx09_rollout(&fixture, CX09_ROOT, None, "CX09-PARTIAL-PARENT");
    cx09_rollout(
        &fixture,
        CX09_CHILD,
        Some(CX09_ROOT),
        "CX09-PARTIAL-IMPORT-CHILD",
    );
    let db = fixture.db().await;
    for replay in [false, true] {
        let out = cx09_import(&fixture, &root);
        assert!(
            !out.status.success(),
            "fallback discovery must remain qualified: {out:?}"
        );
        let stderr = String::from_utf8(out.stderr).unwrap();
        let start = stderr.find('{').expect("structured partial import error");
        let report: serde_json::Value = serde_json::from_str(&stderr[start..]).unwrap();
        assert_eq!(report["error_code"], "LBR-AGENT-018", "{report}");
        assert_eq!(
            report["details"]["partial_results"]
                .as_array()
                .unwrap()
                .len(),
            1
        );
        assert_eq!(report["details"]["partial_results"][0]["partial"], true);
        assert_eq!(
            LocalCodexSession::count(&db, "agent_subagent_content_revision", "1").await,
            1
        );
        assert_eq!(
            LocalCodexSession::count(&db, "agent_checkpoint", "scope='subagent'").await,
            1
        );
        let bodies = cx09_exports(&fixture, &db, "subagent").await.join("\n");
        for marker in [
            "CX09-PARTIAL-IMPORT-CHILD",
            "opaque:CX09-PARTIAL-IMPORT-CHILD",
            "unknown:CX09-PARTIAL-IMPORT-CHILD",
        ] {
            assert!(
                bodies.contains(marker),
                "{marker} lost after replay={replay}"
            );
        }
    }
    db.close().await.unwrap();
}

#[tokio::test]
#[ignore = "operator-local Codex acceptance; never run in CI"]
async fn codex_child_content_legacy_root_import_keeps_parent_only_compatibility() {
    let fixture = LocalCodexSession::new();
    let root = cx09_rollout(&fixture, CX09_ROOT, None, "CX09-LEGACY-ROOT-DATA");
    let bytes = std::fs::read_to_string(&root).unwrap();
    let mut rows = bytes
        .lines()
        .map(|line| serde_json::from_str::<serde_json::Value>(line).unwrap())
        .collect::<Vec<_>>();
    rows[0]["payload"]
        .as_object_mut()
        .unwrap()
        .remove("session_id");
    std::fs::write(
        &root,
        rows.iter()
            .map(serde_json::Value::to_string)
            .collect::<Vec<_>>()
            .join("\n")
            + "\n",
    )
    .unwrap();
    let out = cx09_import(&fixture, &root);
    assert!(out.status.success(), "{out:?}");
    let db = fixture.db().await;
    assert!(
        cx09_exports(&fixture, &db, "committed")
            .await
            .join("\n")
            .contains("CX09-LEGACY-ROOT-DATA")
    );
    assert_eq!(
        LocalCodexSession::count(&db, "agent_subagent_content_claim", "1").await,
        0
    );
    assert_eq!(
        LocalCodexSession::count(&db, "agent_checkpoint", "scope='subagent'").await,
        0
    );
    db.close().await.unwrap();
}

#[tokio::test]
#[ignore = "operator-local Codex acceptance; never run in CI"]
async fn codex_child_content_live_then_doctor_reuses_identical_child_revision() {
    use sea_orm::{ConnectionTrait, Statement};
    let fixture = LocalCodexSession::new();
    let root = cx09_rollout(&fixture, CX09_ROOT, None, "CX09-DOCTOR-PARENT");
    cx09_rollout(&fixture, CX09_CHILD, Some(CX09_ROOT), "CX09-DOCTOR-CHILD");
    let out = cx09_hook_frame(&fixture, CX09_ROOT, &root, 10_000);
    assert!(out.status.success(), "{out:?}");
    let db = fixture.db().await;
    cx09_assert_settled(&db, CX09_ROOT).await;
    assert_eq!(
        LocalCodexSession::count(&db, "agent_subagent_content_revision", "1").await,
        1
    );
    let checkpoints = LocalCodexSession::count(&db, "agent_checkpoint", "1").await;
    let bodies = cx09_exports(&fixture, &db, "subagent").await;
    assert!(bodies.join("\n").contains("CX09-DOCTOR-CHILD"));
    let mut metadata = cx09_terminal_state(&db, CX09_ROOT).await["metadata"].clone();
    let ledger = &mut metadata["capture_catalog_receipts_v1"];
    ledger["version"] = serde_json::json!(2);
    let receipt = ledger["entries"]
        .as_array_mut()
        .unwrap()
        .iter_mut()
        .find(|entry| entry["intent"]["phase"] == "stopped")
        .unwrap();
    let revision = receipt["reserved_revision"].as_i64().unwrap();
    receipt["status"] = serde_json::json!("pending");
    receipt["child_gate"] = serde_json::json!({"preparing":true,"requirements":[]});
    // Retained durable-parent/pending-receipt fixture, not a real crash.
    db.execute_raw(Statement::from_sql_and_values(db.get_database_backend(),
        "UPDATE agent_session SET state='active',stopped_at=NULL,sync_revision=?,metadata_json=? WHERE provider_session_id=? AND agent_kind='codex'",
        [revision.into(),metadata.to_string().into(),CX09_ROOT.into()])).await.unwrap();
    let report = codex_finalize_json(fixture.run(&["agent", "doctor", "--repair", "--json"]));
    cx09_assert_settled(&db, CX09_ROOT).await;
    assert_eq!(
        LocalCodexSession::count(&db, "agent_subagent_content_revision", "1").await,
        1,
        "{report}"
    );
    assert_eq!(
        LocalCodexSession::count(&db, "agent_checkpoint", "1").await,
        checkpoints
    );
    assert_eq!(cx09_exports(&fixture, &db, "subagent").await, bodies);
    db.close().await.unwrap();
}

#[tokio::test]
#[ignore = "operator-local Codex acceptance; never run in CI"]
async fn codex_child_content_complete_zero_children_keeps_managed_parent_complete() {
    let fixture = LocalCodexSession::new();
    let root = cx09_rollout(&fixture, CX09_ROOT, None, "CX09-COMPLETE-ROOT");
    let text = std::fs::read_to_string(&root).unwrap();
    let mut rows = text
        .lines()
        .map(|line| serde_json::from_str::<serde_json::Value>(line).unwrap())
        .collect::<Vec<_>>();
    // Other retention positives deliberately carry a future reasoning field.
    // This negative control must be complete to the parent parser as well.
    for row in &mut rows {
        if row["type"] == "response_item" && row["payload"]["type"] == "reasoning" {
            row["payload"]
                .as_object_mut()
                .unwrap()
                .remove("future_field");
            row["payload"]
                .as_object_mut()
                .unwrap()
                .remove("encrypted_content");
        }
        if row["type"] == "turn_context" {
            row["payload"]["model"] = serde_json::json!("codex-source-model");
        }
        if row["type"] == "response_item" && row["payload"]["role"] == "user" {
            row["payload"]["id"] = serde_json::json!("cx09-user");
            row["payload"]["internal_chat_message_metadata_passthrough"] = serde_json::json!({
                "turn_id":"cx09-turn","content_item_kinds":["user.text"]
            });
        }
    }
    std::fs::write(
        &root,
        rows.iter()
            .map(serde_json::Value::to_string)
            .collect::<Vec<_>>()
            .join("\n")
            + "\n",
    )
    .unwrap();
    let summary = libra::internal::ai::observed_agents::extract::extract_codex(
        &std::fs::read(&root).unwrap(),
    );
    assert!(
        !summary.partial,
        "negative fixture must be parser-complete: {:?}",
        summary.warnings
    );
    let db = fixture.db().await;
    for (event, command) in [("Stop", "stop"), ("SessionEnd", "session-end")] {
        let out = cx09_hook(&fixture, CX09_ROOT, &root, 10_000, event, command);
        assert!(out.status.success(), "{out:?}");
        let metadata = cx09_parent_metadata(&fixture, &db).await;
        assert!(!metadata.is_empty());
        for item in metadata {
            assert_eq!(item["extraction"]["partial"], false, "{event}: {item}");
            assert!(
                !item["extraction"]["warnings"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .any(|warning| warning
                        == "deadline-bound extraction omitted child source aggregation"),
                "{event}: {item}"
            );
            assert!(
                item["extraction"].get("subagent_snapshot").is_none(),
                "{item}"
            );
        }
        assert_eq!(
            LocalCodexSession::count(&db, "agent_subagent_content_claim", "1").await,
            0
        );
        assert_eq!(
            LocalCodexSession::count(&db, "agent_checkpoint", "scope='subagent'").await,
            0
        );
    }
    cx09_assert_settled(&db, CX09_ROOT).await;
    db.close().await.unwrap();
}

#[tokio::test]
#[ignore = "operator-local Codex acceptance; never run in CI"]
async fn codex_child_content_invalid_hint_keeps_available_parent_and_settles_partial() {
    let fixture = LocalCodexSession::new();
    cx09_rollout(
        &fixture,
        CX09_ROOT,
        None,
        "CX09-PARENT-WHILE-CHILD-UNAVAILABLE",
    );
    let untrusted = fixture.repo.join("missing-outside-provider-root.jsonl");
    let out = cx09_hook_frame(&fixture, CX09_ROOT, &untrusted, 10_000);
    assert!(out.status.success(), "{out:?}");
    let db = fixture.db().await;
    let parents = cx09_exports(&fixture, &db, "committed").await.join("\n");
    assert!(
        parents.contains("CX09-PARENT-WHILE-CHILD-UNAVAILABLE"),
        "a child wait cannot hide the independently durable parent thinking"
    );
    cx09_assert_settled(&db, CX09_ROOT).await;
    assert_eq!(
        LocalCodexSession::count(&db, "agent_checkpoint", "scope='subagent'").await,
        0
    );
    assert_eq!(
        LocalCodexSession::count(&db, "agent_subagent_content_claim", "1").await,
        0
    );
    let metadata = cx09_parent_metadata(&fixture, &db).await;
    assert_eq!(metadata.len(), 1);
    assert!(metadata[0]["extraction"].get("subagent_snapshot").is_none());
    assert_eq!(metadata[0]["extraction"]["partial"], true, "{metadata:?}");
    assert!(
        metadata[0]["extraction"]["warnings"]
            .as_array()
            .unwrap()
            .iter()
            .any(|warning| warning == "deadline-bound extraction omitted child source aggregation"),
        "{metadata:?}"
    );

    db.close().await.unwrap();
}

#[tokio::test]
#[ignore = "operator-local Codex acceptance; never run in CI"]
async fn codex_child_content_unavailable_root_keeps_prior_parent_and_pending_receipt() {
    let fixture = LocalCodexSession::new();
    let root = cx09_rollout(&fixture, CX09_ROOT, None, "CX09-PARENT-BEFORE-ROOT-REMOVAL");
    let stop = cx09_hook(&fixture, CX09_ROOT, &root, 10_000, "Stop", "stop");
    assert!(stop.status.success(), "{stop:?}");
    let db = fixture.db().await;
    let parents = cx09_exports(&fixture, &db, "committed").await;
    assert!(
        parents
            .join("\n")
            .contains("CX09-PARENT-BEFORE-ROOT-REMOVAL")
    );
    std::fs::remove_dir_all(fixture.home.join(".codex")).unwrap();
    let out = cx09_hook_frame(&fixture, CX09_ROOT, &root, 10_000);
    assert!(
        !out.status.success(),
        "a trusted terminal child wait is a persistence failure, not an acknowledgement: {out:?}"
    );
    assert!(out.stdout.is_empty(), "{out:?}");
    let error = String::from_utf8_lossy(&out.stderr)
        .lines()
        .filter_map(|line| serde_json::from_str::<serde_json::Value>(line).ok())
        .find(|value| value["ok"] == false)
        .expect("trusted terminal must emit its structured persistence error");
    // Public stderr hides causes. Pin that contract, then isolate discovery
    // with the counterfactual below rather than inventing a public wait code.
    assert_eq!(error["error_code"], "LBR-INTERNAL-001", "{error}");
    assert!(
        error["message"]
            .as_str()
            .unwrap()
            .contains("capture could not be completed")
    );
    assert_eq!(
        LocalCodexSession::count(&db, "agent_subagent_content_claim", "1").await,
        0
    );

    let state = cx09_terminal_state(&db, CX09_ROOT).await;
    assert_eq!(state["state"], "active", "{state}");
    assert!(state["stopped_at"].is_null(), "{state}");
    let meta = &state["metadata"];
    let terminal = meta["capture_catalog_receipts_v1"]["entries"]
        .as_array()
        .unwrap()
        .iter()
        .find(|entry| entry["intent"]["phase"] == "stopped")
        .unwrap();
    assert_eq!(terminal["child_gate"]["preparing"], true);
    assert_eq!(terminal["status"], "pending");
    for body in parents {
        assert!(
            cx09_exports(&fixture, &db, "committed")
                .await
                .contains(&body)
        );
    }
    assert_eq!(
        meta["capture_catalog_receipts_v1"]["entries"]
            .as_array()
            .unwrap()
            .iter()
            .filter(|entry| entry["intent"]["phase"] == "stopped")
            .count(),
        1
    );
    assert_eq!(
        terminal["child_gate"]["requirements"],
        serde_json::json!([])
    );
    let attempts = terminal["finalizer"]["attempts"].as_u64().unwrap();
    let committed = LocalCodexSession::count(&db, "agent_checkpoint", "scope='committed'").await;
    let still_missing = cx09_hook_frame(&fixture, CX09_ROOT, &root, 10_000);
    assert!(!still_missing.status.success(), "{still_missing:?}");
    assert!(still_missing.stdout.is_empty());
    let error = String::from_utf8_lossy(&still_missing.stderr)
        .lines()
        .filter_map(|line| serde_json::from_str::<serde_json::Value>(line).ok())
        .find(|value| value["ok"] == false)
        .unwrap();
    assert_eq!(error["error_code"], "LBR-INTERNAL-001");
    assert!(
        error["message"]
            .as_str()
            .unwrap()
            .contains("capture could not be completed")
    );
    let blocked = cx09_terminal_state(&db, CX09_ROOT).await;
    assert_eq!(blocked["state"], "active");
    assert!(blocked["stopped_at"].is_null());
    let terminals = blocked["metadata"]["capture_catalog_receipts_v1"]["entries"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|entry| entry["intent"]["phase"] == "stopped")
        .collect::<Vec<_>>();
    assert_eq!(terminals.len(), 1);
    assert_eq!(terminals[0]["status"], "pending");
    assert_eq!(terminals[0]["child_gate"], terminal["child_gate"]);
    assert!(terminals[0]["finalizer"]["attempts"].as_u64().unwrap() >= attempts);
    assert_eq!(
        LocalCodexSession::count(&db, "agent_subagent_content_claim", "1").await,
        0
    );
    assert_eq!(
        LocalCodexSession::count(&db, "agent_checkpoint", "scope='committed'").await,
        committed
    );
    // Restore only an empty root: parent transcript and provider config
    // remain missing. Available-empty must settle this same action.
    std::fs::create_dir(fixture.home.join(".codex")).unwrap();
    assert!(!root.exists());
    let replay = cx09_hook_frame(&fixture, CX09_ROOT, &root, 10_000);
    assert!(replay.status.success(), "{replay:?}");
    cx09_assert_settled(&db, CX09_ROOT).await;
    assert_eq!(
        LocalCodexSession::count(&db, "agent_subagent_content_claim", "1").await,
        0
    );
    db.close().await.unwrap();
}

#[tokio::test]
#[ignore = "operator-local Codex acceptance; never run in CI"]
async fn codex_child_content_late_boundary_replay_resolves_only_exact_current_turn() {
    use sea_orm::{ConnectionTrait, Statement};

    const TURN: &str = "01991d45-6f00-7000-8000-000000000001";
    let fixture = LocalCodexSession::new();
    let root = cx09_rollout(&fixture, CX09_ROOT, None, "CX09-BOUNDARY-PARENT");
    let missing = root
        .parent()
        .unwrap()
        .join(format!("rollout-late-{CX09_CHILD}.jsonl"));
    let mut frame = serde_json::json!({
        "hook_event_name":"SubagentStop","session_id":CX09_ROOT,"cwd":fixture.repo,
        "transcript_path":root,"agent_transcript_path":missing,"agent_id":CX09_CHILD,
        "turn_id":TURN,"timestamp":"2026-10-21T01:00:00Z","event_id":"cx09-boundary-late"
    });
    let first = cx09_send_hook(&fixture, &frame, 10_000, "subagent-end");
    assert!(first.status.success(), "{first:?}");
    let db = fixture.db().await;
    assert_eq!(
        LocalCodexSession::count(&db, "agent_checkpoint", "scope='subagent'").await,
        1
    );
    assert_eq!(
        LocalCodexSession::count(&db, "agent_subagent_content_claim", "1").await,
        0
    );
    let row = db.query_one_raw(Statement::from_string(db.get_database_backend(),
        "SELECT checkpoint_id,traces_commit,tree_oid,metadata_blob_oid FROM agent_checkpoint WHERE scope='subagent'".to_owned()))
        .await.unwrap().unwrap();
    let boundary: String = row.try_get_by("checkpoint_id").unwrap();
    let original: Vec<String> = ["traces_commit", "tree_oid", "metadata_blob_oid"]
        .iter()
        .map(|name| row.try_get_by(*name).unwrap())
        .collect();
    let metadata: serde_json::Value =
        serde_json::from_slice(&fixture.run(&["cat-file", "-p", &original[2]]).stdout).unwrap();
    assert_eq!(metadata["subagent"]["content_capture"]["state"], "partial");

    let child = cx09_rollout(
        &fixture,
        CX09_CHILD,
        Some(CX09_ROOT),
        "CX09-LATE-BOUNDARY-CHILD",
    );
    std::fs::write(
        &child,
        std::fs::read_to_string(&child)
            .unwrap()
            .replace("cx09-turn", TURN),
    )
    .unwrap();
    // Keep the callback byte-identical: a late file appears at its old hint.
    std::fs::rename(&child, &missing).unwrap();
    let replay = cx09_send_hook(&fixture, &frame, 10_000, "subagent-end");
    assert!(replay.status.success(), "{replay:?}");
    assert_eq!(
        LocalCodexSession::count(&db, "agent_subagent_content_revision", "1").await,
        1
    );
    assert_eq!(
        LocalCodexSession::count(&db, "agent_checkpoint", "scope='subagent'").await,
        2
    );
    let link = db
        .query_one_raw(Statement::from_string(
            db.get_database_backend(),
            "SELECT boundary_checkpoint_id,link_state FROM agent_subagent_link WHERE link_state='resolved'".to_owned(),
        ))
        .await
        .unwrap()
        .unwrap();
    assert_eq!(
        link.try_get_by::<String, _>("link_state").unwrap(),
        "resolved"
    );
    assert_eq!(
        link.try_get_by::<String, _>("boundary_checkpoint_id")
            .unwrap(),
        boundary
    );
    let row = db.query_one_raw(Statement::from_sql_and_values(db.get_database_backend(),
        "SELECT traces_commit,tree_oid,metadata_blob_oid FROM agent_checkpoint WHERE checkpoint_id=?",
        [boundary.clone().into()])).await.unwrap().unwrap();
    let after: Vec<String> = ["traces_commit", "tree_oid", "metadata_blob_oid"]
        .iter()
        .map(|name| row.try_get_by(*name).unwrap())
        .collect();
    assert_eq!(
        after, original,
        "late association must preserve immutable boundary objects"
    );
    assert!(
        cx09_exports(&fixture, &db, "subagent")
            .await
            .join("\n")
            .contains("CX09-LATE-BOUNDARY-CHILD")
    );

    assert_eq!(
        LocalCodexSession::count(&db, "agent_subagent_link", "1").await,
        1
    );
    frame["turn_id"] = serde_json::json!("01991d45-6f00-7000-8000-000000000002");
    frame["event_id"] = serde_json::json!("cx09-boundary-different-turn");
    let wrong = cx09_send_hook(&fixture, &frame, 10_000, "subagent-end");
    assert!(wrong.status.success(), "{wrong:?}");
    assert_eq!(
        LocalCodexSession::count(&db, "agent_subagent_content_revision", "1").await,
        1
    );
    assert_eq!(
        LocalCodexSession::count(&db, "agent_checkpoint", "scope='subagent'").await,
        3
    );
    assert_eq!(
        LocalCodexSession::count(&db, "agent_subagent_link", "link_state='resolved'").await,
        1
    );
    let link = db
        .query_one_raw(Statement::from_string(
            db.get_database_backend(),
            "SELECT boundary_checkpoint_id FROM agent_subagent_link WHERE link_state='resolved'"
                .to_owned(),
        ))
        .await
        .unwrap()
        .unwrap();
    assert_eq!(
        link.try_get_by::<String, _>("boundary_checkpoint_id")
            .unwrap(),
        boundary,
        "same CID in another turn must not steal an existing association"
    );
    assert_eq!(
        LocalCodexSession::count(&db, "agent_subagent_link", "1").await,
        1
    );
    db.close().await.unwrap();
}

#[tokio::test]
#[ignore = "operator-local Codex acceptance; never run in CI"]
async fn codex_child_content_nested_import_archive_move_preserves_identity_and_revision() {
    use std::io::Write;

    use sea_orm::{ConnectionTrait, Statement};

    const GRANDCHILD: &str = "123e4567-e89b-12d3-a456-426614174100";
    let fixture = LocalCodexSession::new();
    cx09_rollout(&fixture, CX09_ROOT, None, "CX09-NESTED-ROOT");
    let parent = cx09_rollout(&fixture, CX09_CHILD, Some(CX09_ROOT), "CX09-NESTED-PARENT");
    let grandchild = cx09_rollout(&fixture, GRANDCHILD, Some(CX09_CHILD), "CX09-NESTED-CHILD");
    let bytes = std::fs::read_to_string(&grandchild).unwrap();
    let mut rows: Vec<serde_json::Value> = bytes
        .lines()
        .map(|line| serde_json::from_str(line).unwrap())
        .collect();
    rows[0]["payload"]["session_id"] = serde_json::json!(CX09_ROOT);
    std::fs::write(
        &grandchild,
        rows.iter()
            .map(serde_json::Value::to_string)
            .collect::<Vec<_>>()
            .join("\n")
            + "\n",
    )
    .unwrap();
    let mut file = std::fs::OpenOptions::new()
        .append(true)
        .open(&parent)
        .unwrap();
    writeln!(
        file,
        "{}",
        serde_json::json!({"type":"event_msg","payload":{
            "type":"collab_agent_spawn_end","sender_thread_id":CX09_CHILD,"new_thread_id":GRANDCHILD
        }})
    )
    .unwrap();
    drop(file);
    let first = cx09_import(&fixture, &parent);
    assert!(first.status.success(), "{first:?}");
    let db = fixture.db().await;
    assert_eq!(
        LocalCodexSession::count(&db, "agent_subagent_content_revision", "1").await,
        1
    );
    let row = db
        .query_one_raw(Statement::from_string(
            db.get_database_backend(),
            "SELECT metadata_blob_oid FROM agent_checkpoint WHERE scope='subagent'".to_owned(),
        ))
        .await
        .unwrap()
        .unwrap();
    let oid: String = row.try_get_by("metadata_blob_oid").unwrap();
    let out = fixture.run(&["cat-file", "-p", &oid]);
    assert!(out.status.success(), "{out:?}");
    let metadata: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(metadata["subagent"]["source_identity"]["root"], CX09_ROOT);
    assert_eq!(
        metadata["subagent"]["source_identity"]["parent"],
        CX09_CHILD
    );
    assert_eq!(metadata["subagent"]["source_identity"]["child"], GRANDCHILD);
    let exports = cx09_exports(&fixture, &db, "subagent").await.join("\n");
    for marker in [
        "CX09-NESTED-CHILD",
        "opaque:CX09-NESTED-CHILD",
        "unknown:CX09-NESTED-CHILD",
    ] {
        assert!(exports.contains(marker), "nested child lost {marker}");
    }
    assert!(!exports.contains("CX09-NESTED-PARENT"));
    let before = LocalCodexSession::count(&db, "agent_checkpoint", "1").await;
    let archived = fixture
        .home
        .join(".codex/archived_sessions")
        .join(grandchild.file_name().unwrap());
    std::fs::create_dir_all(archived.parent().unwrap()).unwrap();
    std::fs::rename(&grandchild, &archived).unwrap();
    let replay = cx09_import(&fixture, &parent);
    assert!(replay.status.success(), "{replay:?}");
    assert_eq!(
        LocalCodexSession::count(&db, "agent_checkpoint", "1").await,
        before
    );
    assert_eq!(
        LocalCodexSession::count(&db, "agent_subagent_content_revision", "1").await,
        1
    );
    assert_eq!(
        cx09_exports(&fixture, &db, "subagent").await.join("\n"),
        exports
    );
    assert_eq!(
        LocalCodexSession::count(&db, "agent_subagent_link", "link_state='resolved'").await,
        0,
        "historical nested discovery alone cannot mint a live boundary proof"
    );

    // Grow only the archived file. An unchanged-parent short-circuit or
    // skipped archive scan cannot satisfy the second revision/body oracle.
    let mut file = std::fs::OpenOptions::new()
        .append(true)
        .open(&archived)
        .unwrap();
    writeln!(
        file,
        "{}",
        serde_json::json!({"type":"response_item","payload":{
            "type":"reasoning","summary":[{"type":"summary_text","text":"CX09-ARCHIVE-GREW"}]
        }})
    )
    .unwrap();
    drop(file);
    let grown = cx09_import(&fixture, &parent);
    assert!(grown.status.success(), "{grown:?}");
    assert_eq!(
        LocalCodexSession::count(&db, "agent_subagent_content_revision", "1").await,
        2
    );
    assert_eq!(
        LocalCodexSession::count(&db, "agent_subagent_content_claim", "1").await,
        1
    );
    assert!(
        cx09_exports(&fixture, &db, "subagent")
            .await
            .join("\n")
            .contains("CX09-ARCHIVE-GREW")
    );
    let rows = db
        .query_all_raw(Statement::from_string(
            db.get_database_backend(),
            "SELECT metadata_blob_oid FROM agent_checkpoint WHERE scope='subagent'".to_owned(),
        ))
        .await
        .unwrap();
    assert_eq!(rows.len(), 2);
    for row in rows {
        let oid: String = row.try_get_by("metadata_blob_oid").unwrap();
        let out = fixture.run(&["cat-file", "-p", &oid]);
        assert!(out.status.success(), "{out:?}");
        let metadata: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
        assert_eq!(metadata["subagent"]["source_identity"]["root"], CX09_ROOT);
        assert_eq!(
            metadata["subagent"]["source_identity"]["parent"],
            CX09_CHILD
        );
        assert_eq!(metadata["subagent"]["source_identity"]["child"], GRANDCHILD);
    }
    assert_eq!(
        LocalCodexSession::count(&db, "agent_subagent_link", "link_state='resolved'").await,
        0
    );

    db.close().await.unwrap();
}

#[tokio::test]
#[ignore = "operator-local Codex acceptance; never run in CI"]
async fn codex_child_content_invalid_boundary_candidates_keep_valid_sibling_data() {
    const BAD_CHILD: &str = "123e4567-e89b-12d3-a456-426614174101";
    for invalid in ["root", "cwd", "outside"] {
        let fixture = LocalCodexSession::new();
        let root = cx09_rollout(&fixture, CX09_ROOT, None, "CX09-SAFE-PARENT");
        cx09_rollout(&fixture, CX09_CHILD, Some(CX09_ROOT), "CX09-VALID-SIBLING");
        let mut bad = cx09_rollout(
            &fixture,
            BAD_CHILD,
            Some(CX09_ROOT),
            "CX09-REJECTED-CANDIDATE",
        );
        let bytes = std::fs::read_to_string(&bad).unwrap();
        let mut rows: Vec<serde_json::Value> = bytes
            .lines()
            .map(|line| serde_json::from_str(line).unwrap())
            .collect();
        match invalid {
            "root" => {
                rows[0]["payload"]["session_id"] =
                    serde_json::json!("123e4567-e89b-12d3-a456-426614174199")
            }
            "cwd" => rows[0]["payload"]["cwd"] = serde_json::json!(fixture.home),
            "outside" => {
                let outside = fixture.home.join(bad.file_name().unwrap());
                std::fs::rename(&bad, &outside).unwrap();
                bad = outside;
            }
            _ => unreachable!(),
        }
        std::fs::write(
            &bad,
            rows.iter()
                .map(serde_json::Value::to_string)
                .collect::<Vec<_>>()
                .join("\n")
                + "\n",
        )
        .unwrap();
        let frame = serde_json::json!({
            "hook_event_name":"SubagentStop","session_id":CX09_ROOT,"cwd":fixture.repo,
            "transcript_path":root,"agent_transcript_path":bad,"agent_id":BAD_CHILD,
            "turn_id":"01991d45-6f00-7000-8000-000000000001"
        });
        let rejected = cx09_send_hook(&fixture, &frame, 10_000, "subagent-end");
        assert!(rejected.status.success(), "{invalid}: {rejected:?}");
        let db = fixture.db().await;
        assert_eq!(
            LocalCodexSession::count(&db, "agent_subagent_content_claim", "1").await,
            0,
            "invalid boundary candidate must not create a content claim: {invalid}"
        );
        assert!(
            !cx09_exports(&fixture, &db, "subagent")
                .await
                .join("\n")
                .contains("CX09-REJECTED-CANDIDATE")
        );
        let end = cx09_hook_frame(&fixture, CX09_ROOT, &root, 10_000);
        assert!(end.status.success(), "{invalid}: {end:?}");
        cx09_assert_settled(&db, CX09_ROOT).await;
        assert_eq!(
            LocalCodexSession::count(&db, "agent_subagent_content_claim", "1").await,
            1
        );
        let bodies = cx09_exports(&fixture, &db, "subagent").await.join("\n");
        assert!(bodies.contains("CX09-VALID-SIBLING"));
        assert!(!bodies.contains("CX09-REJECTED-CANDIDATE"));
        db.close().await.unwrap();
    }
}

#[tokio::test]
#[ignore = "operator-local Codex acceptance; never run in CI"]
async fn codex_child_content_legacy_live_metadata_stays_neutral_and_supported_uuid_import_keeps_data()
 {
    use std::io::Write;

    let fixture = LocalCodexSession::new();
    let root = cx09_rollout(&fixture, "legacy-root-label", None, "CX09-LEGACY-PARENT");
    let legacy_child = cx09_rollout(
        &fixture,
        "legacy-child-label",
        Some("legacy-root-label"),
        "CX09-LEGACY-CHILD",
    );
    let mut file = std::fs::OpenOptions::new()
        .append(true)
        .open(&root)
        .unwrap();
    writeln!(file,"{}",serde_json::json!({"type":"event_msg","payload":{
        "type":"collab_agent_spawn_end","sender_thread_id":"legacy-root-label","new_thread_id":"legacy-child-label"
    }})).unwrap();
    drop(file);
    // This pins legacy metadata while both callbacks finish inside their
    // ten-second work windows; the documented expired-window case is separate.
    let stop = cx09_hook(&fixture, "legacy-root-label", &root, 10_000, "Stop", "stop");
    assert!(stop.status.success(), "{stop:?}");
    let end = cx09_hook_frame(&fixture, "legacy-root-label", &root, 10_000);
    assert!(end.status.success(), "{end:?}");
    let db = fixture.db().await;
    cx09_assert_settled(&db, "legacy-root-label").await;
    assert_eq!(
        LocalCodexSession::count(&db, "agent_subagent_content_claim", "1").await,
        0,
        "legacy live callbacks retain their original parent-only path"
    );
    let metadata = cx09_parent_metadata(&fixture, &db).await;
    assert!(!metadata.is_empty());
    for item in metadata {
        assert!(item["extraction"].get("subagent_snapshot").is_none());
        if let Some(warnings) = item["extraction"]["warnings"].as_array() {
            for warning in warnings {
                let text = warning.as_str().unwrap();
                assert!(
                    !text.contains("subagent") && !text.contains("child source"),
                    "new child diagnostics must not leak into the legacy live result: {text}"
                );
            }
        }
    }

    // Public --path retains its existing UUID-filename validation; the
    // provider's stored port does not broaden that public selector.
    let unsupported = cx09_import(&fixture, &root);
    assert!(!unsupported.status.success());
    let error: serde_json::Value = serde_json::from_slice(&unsupported.stderr).unwrap();
    assert_eq!(error["error_code"], "LBR-CLI-002");
    assert_eq!(
        LocalCodexSession::count(&db, "agent_subagent_content_claim", "1").await,
        0
    );
    std::fs::remove_file(&root).unwrap();
    std::fs::remove_file(&legacy_child).unwrap();

    let root = cx09_rollout(&fixture, CX09_ROOT, None, "CX09-POST-LEGACY-PARENT");
    cx09_rollout(
        &fixture,
        CX09_CHILD,
        Some(CX09_ROOT),
        "CX09-POST-LEGACY-IMPORT-CHILD",
    );
    let mut file = std::fs::OpenOptions::new()
        .append(true)
        .open(&root)
        .unwrap();
    writeln!(
        file,
        "{}",
        serde_json::json!({"type":"event_msg","payload":{
            "type":"collab_agent_spawn_end","sender_thread_id":CX09_ROOT,"new_thread_id":CX09_CHILD
        }})
    )
    .unwrap();
    drop(file);
    let imported = cx09_import(&fixture, &root);
    assert!(imported.status.success(), "{imported:?}");
    assert_eq!(
        LocalCodexSession::count(&db, "agent_subagent_content_claim", "1").await,
        1
    );
    assert_eq!(
        LocalCodexSession::count(&db, "agent_subagent_content_revision", "1").await,
        1
    );
    let exports = cx09_exports(&fixture, &db, "subagent").await.join("\n");
    for marker in [
        "CX09-POST-LEGACY-IMPORT-CHILD",
        "opaque:CX09-POST-LEGACY-IMPORT-CHILD",
        "unknown:CX09-POST-LEGACY-IMPORT-CHILD",
    ] {
        assert!(
            exports.contains(marker),
            "supported UUID child data lost {marker}"
        );
    }
    let replay = cx09_import(&fixture, &root);
    assert!(replay.status.success(), "{replay:?}");
    assert_eq!(
        LocalCodexSession::count(&db, "agent_subagent_content_revision", "1").await,
        1
    );
    assert_eq!(
        LocalCodexSession::count(&db, "agent_subagent_content_claim", "1").await,
        1
    );
    db.close().await.unwrap();
}

#[tokio::test]
#[ignore = "operator-local Codex acceptance; never run in CI"]
async fn codex_child_recover_worker_reaches_sixteenth_behind_fifteen_pending_gates() {
    use sea_orm::{ConnectionTrait, Statement};
    let fixture = LocalCodexSession::new();
    let objects = fixture.repo.join(".libra/objects");
    let saved = fixture.repo.join(".libra/objects-functional-fixture");
    std::fs::rename(&objects, &saved).unwrap();
    std::fs::write(&objects, b"functional object-store failure").unwrap();
    for _ in 0..16 {
        let id = uuid::Uuid::new_v4().to_string();
        let source = cx09_rollout(&fixture, &id, None, &format!("CX09-WORKER-ROOT-{id}"));
        assert!(
            !cx09_hook_frame(&fixture, &id, &source, 60_000)
                .status
                .success()
        );
    }
    std::fs::remove_file(&objects).unwrap();
    std::fs::rename(&saved, &objects).unwrap();
    std::fs::remove_dir_all(fixture.home.join(".codex/sessions")).unwrap();
    let db = fixture.db().await;
    let headers = db
        .query_all_raw(Statement::from_string(
            db.get_database_backend(),
            "SELECT key,value FROM metadata_kv WHERE scope='agent_capture_pending' ORDER BY key"
                .to_owned(),
        ))
        .await
        .unwrap()
        .into_iter()
        .map(|row| {
            (
                row.try_get_by::<String, _>("key").unwrap(),
                serde_json::from_str::<serde_json::Value>(
                    &row.try_get_by::<String, _>("value").unwrap(),
                )
                .unwrap(),
            )
        })
        .collect::<Vec<_>>();
    assert_eq!(
        headers.len(),
        16,
        "all aliases, chunks and signed envelopes must exist"
    );
    let first = headers.first().unwrap().0.clone();
    let last = headers.last().unwrap().0.clone();
    let rows = db
        .query_all_raw(Statement::from_string(
            db.get_database_backend(),
            "SELECT session_id,provider_session_id,metadata_json FROM agent_session".to_owned(),
        ))
        .await
        .unwrap();
    let mut pending_before = Vec::new();
    let mut positive_native = None;
    let mut last_native = None;
    for row in rows {
        let sid: String = row.try_get_by("session_id").unwrap();
        let native: String = row.try_get_by("provider_session_id").unwrap();
        let mut meta: serde_json::Value =
            serde_json::from_str(&row.try_get_by::<String, _>("metadata_json").unwrap()).unwrap();
        let ledger = &mut meta["capture_catalog_receipts_v1"];
        let entry = &mut ledger["entries"][0];
        assert_eq!(entry["status"], "pending");
        // Synthetic unconsumed fixture policy, not a real-crash assertion.
        entry["finalizer"]["attempts"] = serde_json::json!(1);
        let (key, header) = headers
            .iter()
            .find(|(_, h)| h["binding"]["event_id"] == entry["event_id"])
            .unwrap();
        if key == &last {
            // Deliberately reconstruct legacy selection even after the live
            // producer is activated. Keep all signed binding fields intact.
            ledger["version"] = serde_json::json!(1);
            ledger["entries"][0]
                .as_object_mut()
                .unwrap()
                .remove("child_gate");
            assert!(ledger["entries"][0].get("child_gate").is_none());
            last_native = Some(native);
        } else {
            ledger["version"] = serde_json::json!(2);
            ledger["entries"][0]["child_gate"] =
                serde_json::json!({"preparing":true,"requirements":[]});
            if key == &first {
                positive_native = Some(native);
            } else {
                // An available empty scan may finish preparation. Actual
                // registered, uncommitted leaves must still block completion.
                let source_key =
                    format!("source/subagent-hmac-v2/{:064x}", pending_before.len() + 1);
                ledger["entries"][0]["child_gate"] = serde_json::json!({
                    "preparing":false,
                    "requirements":[{"source_key":source_key,"minimum_revision":1}]
                });
                let now = chrono::Utc::now().timestamp_millis();
                db.execute_raw(Statement::from_sql_and_values(
                    db.get_database_backend(),
                    "INSERT INTO agent_subagent_content_claim (
                        parent_session_id,provider_kind,source_key,content_schema_version,
                        current_revision,state,attempt_digest,attempt_checkpoint_id,owner,
                        lease_expires_at,fence_token,created_at,updated_at
                     ) VALUES (?,'codex',?,2,0,'reserved',?,?,'cx09-other-live-owner',?,1,?,?)",
                    [
                        sid.clone().into(),
                        source_key.into(),
                        "a".repeat(64).into(),
                        uuid::Uuid::new_v4().to_string().into(),
                        (now + 300_000).into(),
                        now.into(),
                        now.into(),
                    ],
                ))
                .await
                .unwrap();
                let bytes = cx09_artifact_bytes(&db, key, header).await;
                assert!(
                    bytes.len() >= 3,
                    "header,chunk,and encrypted alias snapshot must be nonempty"
                );
                pending_before.push((
                    sid.clone(),
                    meta.clone(),
                    key.clone(),
                    header.clone(),
                    bytes,
                ));
            }
        }
        db.execute_raw(Statement::from_sql_and_values(
            db.get_database_backend(),
            "UPDATE agent_session SET metadata_json=? WHERE session_id=?",
            [meta.to_string().into(), sid.into()],
        ))
        .await
        .unwrap();
    }
    assert_eq!(pending_before.len(), 14);
    let positive_native = positive_native.unwrap();
    cx09_rollout(
        &fixture,
        &positive_native,
        None,
        &format!("CX09-WORKER-ROOT-{positive_native}"),
    );
    cx09_rollout(
        &fixture,
        &uuid::Uuid::new_v4().to_string(),
        Some(&positive_native),
        "CX09-WORKER-ACTIVE-CHILD",
    );
    // Catalog waiting populations are not queue windows: 16 additional
    // unqueued fixture rows cannot hide a signed header. Admission still
    // rejects more than16 pending+quarantine headers before hydration.
    let columns = db
        .query_all_raw(Statement::from_string(
            db.get_database_backend(),
            "PRAGMA table_info(agent_session)".to_owned(),
        ))
        .await
        .unwrap()
        .into_iter()
        .map(|row| row.try_get_by::<String, _>("name").unwrap())
        .collect::<Vec<_>>();
    assert!(
        columns
            .iter()
            .all(|c| c.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'_'))
    );
    let quoted = columns
        .iter()
        .map(|c| format!("\"{c}\""))
        .collect::<Vec<_>>()
        .join(",");
    let expressions = columns
        .iter()
        .map(|c| match c.as_str() {
            "session_id" => "?1".to_owned(),
            "provider_session_id" => "?2".to_owned(),
            _ => format!("\"{c}\""),
        })
        .collect::<Vec<_>>()
        .join(",");
    let insert = format!(
        "INSERT INTO agent_session({quoted}) SELECT {expressions} FROM agent_session WHERE session_id=?3"
    );
    for _ in 0..16 {
        db.execute_raw(Statement::from_sql_and_values(
            db.get_database_backend(),
            insert.clone(),
            [
                format!("unqueued-cx09-{}", uuid::Uuid::new_v4()).into(),
                uuid::Uuid::new_v4().to_string().into(),
                pending_before[0].0.clone().into(),
            ],
        ))
        .await
        .unwrap();
    }
    assert_eq!(
        LocalCodexSession::count(&db, "agent_session", "1").await,
        32
    );
    let reserved_before = cx09_reserved_claim_snapshot(&db).await;
    assert_eq!(reserved_before.len(), 14);
    let started = std::time::Instant::now();
    let worker = fixture.run(&["__capture-recovery-worker"]);
    let elapsed = started.elapsed();
    assert!(worker.status.success(), "{worker:?}");
    assert!(
        elapsed < std::time::Duration::from_secs(3),
        "original2s worker batch plus process startup must remain bounded: {elapsed:?}"
    );
    assert_eq!(
        LocalCodexSession::count(&db, "agent_checkpoint", &format!("checkpoint_id='{last}'")).await,
        1,
        "the16th key must actually publish in this one worker call"
    );
    let children = cx09_exports(&fixture, &db, "subagent").await.join("\n");
    assert!(
        children.contains("CX09-WORKER-ACTIVE-CHILD"),
        "positive discovery in this worker distinguishes active port from no port"
    );
    cx09_assert_settled(&db, &positive_native).await;
    cx09_assert_settled(&db, last_native.as_ref().unwrap()).await;
    assert_eq!(cx09_reserved_claim_snapshot(&db).await, reserved_before);
    assert_eq!(
        LocalCodexSession::count(&db, "metadata_kv", "scope='agent_capture_pending'").await,
        14
    );
    assert_eq!(
        LocalCodexSession::count(&db, "metadata_kv", "scope='agent_capture_quarantine'").await,
        0
    );
    for (sid, expected, key, header, bytes) in pending_before {
        let actual: String = db
            .query_one_raw(Statement::from_sql_and_values(
                db.get_database_backend(),
                "SELECT metadata_json FROM agent_session WHERE session_id=?",
                [sid.into()],
            ))
            .await
            .unwrap()
            .unwrap()
            .try_get_by("metadata_json")
            .unwrap();
        assert_eq!(
            serde_json::from_str::<serde_json::Value>(&actual).unwrap(),
            expected
        );
        assert_eq!(
            cx09_artifact_bytes(&db, &key, &header).await,
            bytes,
            "waiting alias,chunk,envelope bytes must remain unchanged"
        );
    }
    let exported = fixture.run(&["agent", "checkpoint", "export", &last]);
    assert!(exported.status.success());
    assert!(
        String::from_utf8_lossy(&exported.stdout)
            .contains(&format!("CX09-WORKER-ROOT-{}", last_native.unwrap()))
    );
    db.close().await.unwrap();
}

fn codex_turn_preview(rows: &[serde_json::Value]) -> serde_json::Value {
    use libra::internal::ai::capture::snapshot::{CaptureSnapshotPolicy, CaptureSnapshotService};
    let source = rows
        .iter()
        .map(serde_json::Value::to_string)
        .collect::<Vec<_>>()
        .join("\n");
    CaptureSnapshotService::preview_redacted_coverage_v2(
        "codex",
        source.into_bytes(),
        CaptureSnapshotPolicy::default(),
    )
    .expect("bounded current-source Codex preview")
    .1
}

fn codex_turn_header() -> serde_json::Value {
    serde_json::json!({"type":"session_meta","payload":{"id":"123e4567-e89b-12d3-a456-426614174051","cli_version":"0.162.1"}})
}

fn codex_turn_context(id: &str) -> serde_json::Value {
    serde_json::json!({"type":"turn_context","payload":{"turn_id":id,"model":"source-pin"}})
}

fn codex_turn_user(id: &str, text: &str, kind: &str) -> serde_json::Value {
    serde_json::json!({"type":"response_item","payload":{"type":"message","id":id,"role":"user",
        "internal_chat_message_metadata_passthrough":{"content_item_kinds":[kind]},
        "content":[{"type":"input_text","text":text}]}})
}

#[test]
#[ignore = "operator-local Codex acceptance; never run in CI"]
fn codex_turn_context_groups_steering_and_reentry_without_injected_turns() {
    let rows = vec![
        codex_turn_header(),
        codex_turn_context("turn-a"),
        codex_turn_user(
            "host-a",
            "<environment_context>host</environment_context>",
            "context.environment",
        ),
        codex_turn_user(
            "human-a",
            "<environment_context>human request</environment_context>",
            "user.text",
        ),
        codex_turn_user("human-steer", "steer", "user.text"),
        codex_turn_context("turn-b"),
        codex_turn_user("human-b", "second", "user.text"),
        codex_turn_context("turn-a"),
        serde_json::json!({"type":"response_item","payload":{"type":"message","id":"answer-a","role":"assistant","content":[{"type":"output_text","text":"answer"}]}}),
    ];
    let preview = codex_turn_preview(&rows);
    let turns = preview["turns"].as_array().unwrap();
    assert_eq!(
        turns.len(),
        2,
        "context, rather than each user-role item, defines a turn: {preview}"
    );
    assert_eq!(turns[0]["projection"]["logical_turn_key"], "turn-a");
    assert_eq!(turns[1]["projection"]["logical_turn_key"], "turn-b");
    let records = turns[0]["projection"]["records"].as_array().unwrap();
    assert_eq!(records.len(), 3);
    assert_eq!(
        records[0]["text"],
        "<environment_context>human request</environment_context>"
    );
    assert_eq!(records[1]["text"], "steer");
    assert_eq!(records[2]["text"], "answer");
}

#[test]
#[ignore = "operator-local Codex acceptance; never run in CI"]
fn codex_turn_event_mirrors_and_repeated_real_input_are_distinct() {
    let mut rows = vec![codex_turn_header(), codex_turn_context("turn-a")];
    for id in ["human-a", "human-b"] {
        rows.push(serde_json::json!({"type":"event_msg","payload":{"type":"user_message","client_id":id,"message":"same"}}));
        rows.push(codex_turn_user(id, "same", "user.text"));
        rows.push(serde_json::json!({"type":"event_msg","payload":{"type":"item_completed","thread_id":"123e4567-e89b-12d3-a456-426614174051","turn_id":"turn-a",
            "item":{"type":"UserMessage","id":id,"content":[{"type":"text","text":"same"}]}}}));
    }
    // Replayed same native item cannot create another user record or claim.
    rows.push(codex_turn_user("human-a", "same", "user.text"));
    let preview = codex_turn_preview(&rows);
    assert_eq!(preview["turns"].as_array().unwrap().len(), 1, "{preview}");
    assert_eq!(
        preview["turns"][0]["projection"]["records"]
            .as_array()
            .unwrap()
            .len(),
        2,
        "{preview}"
    );
}

#[test]
#[ignore = "operator-local Codex acceptance; never run in CI"]
fn codex_turn_missing_context_has_stable_partial_compatibility_keys() {
    let rows = vec![
        codex_turn_header(),
        codex_turn_user("human-a", "first", "user.text"),
        codex_turn_user("human-b", "second", "user.text"),
    ];
    let first = codex_turn_preview(&rows);
    let second = codex_turn_preview(&rows);
    assert_eq!(first["turns"], second["turns"]);
    assert_eq!(
        first["partial"], true,
        "an item id does not prove a provider turn id"
    );
    for (index, turn) in first["turns"].as_array().unwrap().iter().enumerate() {
        assert_eq!(
            turn["projection"]["logical_turn_key"],
            format!("ordinal:{index}")
        );
        assert_eq!(turn["projection"]["completeness"], "incomplete");
    }
}

#[test]
#[ignore = "operator-local Codex acceptance; never run in CI"]
fn codex_turn_foreign_inherited_and_developer_items_do_not_open_turns() {
    let mut inherited = codex_turn_user("inherited", "historical", "user.text");
    inherited["metadata"] = serde_json::json!({"inherited_user_message":true});
    let rows = vec![
        codex_turn_header(),
        codex_turn_context("turn-a"),
        inherited,
        serde_json::json!({"type":"event_msg","payload":{"type":"item_completed","thread_id":"foreign-session","turn_id":"turn-foreign",
            "item":{"type":"UserMessage","id":"foreign","content":[{"type":"text","text":"foreign"}]}}}),
        serde_json::json!({"type":"response_item","payload":{"type":"message","role":"developer","content":[{"type":"input_text","text":"developer"}]}}),
        codex_turn_user("human-a", "real", "user.text"),
    ];
    let preview = codex_turn_preview(&rows);
    assert_eq!(preview["turns"].as_array().unwrap().len(), 1, "{preview}");
    assert_eq!(
        preview["turns"][0]["projection"]["logical_turn_key"],
        "turn-a"
    );
    assert_eq!(
        preview["turns"][0]["projection"]["records"],
        serde_json::json!([{"type":"user","text":"real"}])
    );
}

#[test]
#[ignore = "operator-local Codex acceptance; never run in CI"]
fn codex_turn_late_completed_mirror_does_not_rebind_active_context() {
    let preview = codex_turn_preview(&[
        codex_turn_header(),
        codex_turn_context("turn-a"),
        codex_turn_user("human-a", "first", "user.text"),
        codex_turn_context("turn-b"),
        codex_turn_user("human-b", "second", "user.text"),
        serde_json::json!({"type":"event_msg","payload":{"type":"item_completed","thread_id":"123e4567-e89b-12d3-a456-426614174051","turn_id":"turn-a",
            "item":{"type":"UserMessage","id":"human-a","content":[{"type":"text","text":"first"}]}}}),
        serde_json::json!({"type":"response_item","payload":{"type":"message","id":"answer-b","role":"assistant","content":[{"type":"output_text","text":"second answer"}]}}),
    ]);
    assert_eq!(
        preview["turns"][0]["projection"]["records"]
            .as_array()
            .unwrap()
            .len(),
        1,
        "late mirror cannot move the following response: {preview}"
    );
    assert_eq!(
        preview["turns"][1]["projection"]["records"][1]["text"],
        "second answer"
    );
}

#[test]
#[ignore = "operator-local Codex acceptance; never run in CI"]
fn codex_turn_source_identity_replay_and_v1_digest_stay_stable() {
    use libra::internal::ai::observed_agents::coverage::normalize_codex_rollout;
    let legacy = br#"{"type":"event_msg","payload":{"type":"task_started","turn_id":"turn-1"}}
{"type":"event_msg","payload":{"type":"user_message","message":"mirrored"}}
{"type":"response_item","payload":{"type":"message","role":"user","content":[{"type":"input_text","text":"hi"}]}}
{"type":"response_item","payload":{"type":"message","role":"assistant","content":[{"type":"output_text","text":"hello"}]}}
"#;
    let old = normalize_codex_rollout(legacy);
    assert_eq!(old.len(), 1);
    assert_eq!(old[0].logical_turn_key, "turn-1");
    assert_eq!(
        old[0].digest_hex(),
        "df991cd9a1ac5c12c32b8cdf0254c3dfbbf26485b505a5afc83a90d1128ebc54"
    );
    let reasoning = serde_json::json!({"type":"response_item","payload":{"type":"reasoning","id":"reason-a","summary":[{"type":"summary_text","text":"readable"}]}});
    let mut rows = vec![
        codex_turn_header(),
        codex_turn_context("turn-a"),
        codex_turn_user("human-a", "first", "user.text"),
        reasoning.clone(),
    ];
    let before = codex_turn_preview(&rows);
    rows.push(reasoning);
    rows.push(codex_turn_user("human-a", "first", "user.text"));
    let after = codex_turn_preview(&rows);
    assert_eq!(before["turns"].as_array().unwrap().len(), 1);
    assert_eq!(before["turns"][0]["coverage_schema_version"], 2);
    assert_eq!(before["turns"][0]["digest"], after["turns"][0]["digest"]);
    assert_eq!(
        before["turns"][0]["projection"]["records"],
        after["turns"][0]["projection"]["records"]
    );
}

#[test]
#[ignore = "operator-local Codex acceptance; never run in CI"]
fn codex_turn_terminal_and_invalid_context_do_not_inherit_a_completed_turn() {
    let preview = codex_turn_preview(&[
        codex_turn_header(),
        codex_turn_context("turn-a"),
        codex_turn_user("human-a", "first", "user.text"),
        serde_json::json!({"type":"event_msg","payload":{"type":"turn_complete","turn_id":"turn-a"}}),
        serde_json::json!({"type":"response_item","payload":{"type":"message","id":"orphan","role":"assistant","content":[{"type":"output_text","text":"unassigned"}]}}),
        codex_turn_context("invalid:key"),
        codex_turn_user("human-b", "next", "user.text"),
    ]);
    let turns = preview["turns"].as_array().unwrap();
    assert_eq!(turns.len(), 3, "{preview}");
    assert_eq!(
        turns[0]["projection"]["records"].as_array().unwrap().len(),
        1
    );
    for (index, turn) in turns.iter().enumerate().skip(1) {
        assert_eq!(
            turn["projection"]["logical_turn_key"],
            format!("ordinal:{index}")
        );
        assert_eq!(turn["projection"]["completeness"], "incomplete");
    }
}

#[test]
#[ignore = "operator-local Codex acceptance; never run in CI"]
fn codex_turn_unknown_annotations_and_identity_conflicts_are_partial() {
    let mut malformed = codex_turn_user("malformed", "unproven human", "user.text");
    malformed["payload"]["internal_chat_message_metadata_passthrough"]["content_item_kinds"] =
        serde_json::json!([]);
    let rows = vec![
        codex_turn_header(),
        codex_turn_context("turn-a"),
        malformed,
        codex_turn_user("human-a", "real", "user.text"),
        codex_turn_user("human-a", "conflicting replay", "user.text"),
        codex_turn_user("human-b", "qualified", "unknown"),
    ];
    let preview = codex_turn_preview(&rows);
    assert_eq!(preview["partial"], true);
    assert_eq!(preview["turns"].as_array().unwrap().len(), 1);
    assert_eq!(
        preview["turns"][0]["projection"]["records"],
        serde_json::json!([
            {"type":"user","text":"real"}, {"type":"user","text":"qualified"}
        ])
    );
}

#[test]
#[ignore = "operator-local Codex acceptance; never run in CI"]
fn codex_turn_state_line_and_deadline_limits_never_become_empty_success() {
    use libra::internal::ai::capture::snapshot::{CaptureSnapshotPolicy, CaptureSnapshotService};
    let mut input = codex_turn_user("unused", "same", "user.text");
    input["payload"].as_object_mut().unwrap().remove("id");
    let mut rows = vec![codex_turn_header(), codex_turn_context("turn-a")];
    rows.extend(std::iter::repeat_n(input.clone(), 4096));
    let boundary = codex_turn_preview(&rows);
    assert_eq!(
        boundary["turns"][0]["projection"]["records"]
            .as_array()
            .unwrap()
            .len(),
        4096
    );
    rows.push(input);
    let encode = |rows: &[serde_json::Value]| {
        rows.iter()
            .map(serde_json::Value::to_string)
            .collect::<Vec<_>>()
            .join("\n")
            .into_bytes()
    };
    assert!(
        CaptureSnapshotService::preview_redacted_coverage_v2(
            "codex",
            encode(&rows),
            CaptureSnapshotPolicy::default()
        )
        .is_err()
    );
    let mut large = codex_turn_user("large", "real", "user.text");
    large["payload"]["padding"] = serde_json::json!("x".repeat(1024 * 1024));
    assert!(
        CaptureSnapshotService::preview_redacted_coverage_v2(
            "codex",
            encode(&[codex_turn_header(), large]),
            CaptureSnapshotPolicy::default()
        )
        .is_err()
    );
    assert!(
        CaptureSnapshotService::preview_redacted_coverage_v2(
            "codex",
            encode(&[codex_turn_header()]),
            CaptureSnapshotPolicy::with_deadline(Some(std::time::Instant::now()))
        )
        .is_err()
    );
}

#[tokio::test]
#[ignore = "operator-local Codex acceptance; never run in CI"]
async fn codex_turn_explicit_v2_writer_replay_preserves_existing_v1_claim() {
    use std::{collections::BTreeMap, path::Path, time::Duration};

    use libra::internal::ai::{
        agent_import::{ImportRequest, TurnBoundary, import_prepared_with_coverage_versions},
        capture::snapshot::{
            CaptureSnapshotCompleteness, CaptureSnapshotProjection, CaptureSnapshotSource,
            CaptureSnapshotSourceKind,
        },
        capture_scope::CaptureScope,
        observed_agents::{AgentKind, Completeness, NormalizedTurn, SemanticRecord},
    };
    use sea_orm::{ConnectionTrait, Database, Statement};

    let fixture = LocalCodexSession::new();
    let storage = fixture.repo.join(".libra");
    let conn = Database::connect(format!("sqlite://{}", storage.join("libra.db").display()))
        .await
        .unwrap();
    let scope = CaptureScope::main_for_connection(&conn).await.unwrap();
    let preview = codex_turn_preview(&[
        codex_turn_header(),
        codex_turn_context("turn-a"),
        codex_turn_user("human-a", "first", "user.text"),
        codex_turn_user("human-b", "steer", "user.text"),
    ]);
    let source_turn = &preview["turns"][0]["projection"];
    // This exercises explicit ordinary V2, not a fabricated typed-reasoning
    // constructor or automatic provider activation. The source projection is
    // the input; its grouping must survive the actual shared writer twice.
    let turn = NormalizedTurn {
        logical_turn_key: source_turn["logical_turn_key"].as_str().unwrap().to_owned(),
        ordinal: 0,
        completeness: match source_turn["completeness"].as_str().unwrap() {
            "complete" => Completeness::Complete,
            "incomplete" => Completeness::Incomplete,
            other => panic!("unexpected completeness {other}"),
        },
        started_at: None,
        ended_at: None,
        records: source_turn["records"]
            .as_array()
            .unwrap()
            .iter()
            .map(|record| {
                assert_eq!(record["type"], "user");
                SemanticRecord::User {
                    text: record["text"].as_str().unwrap().to_owned(),
                }
            })
            .collect(),
    };
    assert_eq!(
        turn.digest_hex(),
        preview["turns"][0]["digest"].as_str().unwrap()
    );
    assert_eq!(turn.records.len(), 2);
    let request = || {
        let commitment = format!("source/hmac-v2/{}", "a".repeat(64));
        ImportRequest {
            agent_kind: AgentKind::Codex,
            provider_session_id: "cx05-direct".into(),
            session_id: "codex__cx05-direct".into(),
            source_kind: "file".into(),
            source_id: commitment.clone(),
            identity_schema_version: 2,
            content_digest: "caller digest is recalculated".into(),
            started_at: 1,
            ended_at: 2,
            session_state: "stopped".into(),
            stopped_at: Some(2),
            working_dir: fixture.repo.clone(),
            repository_identity: "not_retained:v1".into(),
            capture_scope: Some(scope.clone()),
            source_fingerprint: commitment.clone(),
            existing_session_fingerprint: None,
            redaction_report: serde_json::json!({"pipeline":"typed_allowlist","raw_persisted":false,"matches":[],"bytes_scanned":32,"bytes_redacted":0}),
            transcript_snapshot: Some(CaptureSnapshotProjection {
                completeness: CaptureSnapshotCompleteness::Complete,
                partial_reason: None,
                source: Some(CaptureSnapshotSource {
                    kind: CaptureSnapshotSourceKind::ProviderFile,
                    identity: "not_retained:v1".into(),
                    digest_sha256: Some(commitment),
                    byte_len: Some(32),
                }),
                transcript_redacted_bytes: 32,
                redaction_match_count: 0,
                redaction_bytes_scanned: 32,
                redaction_bytes_redacted: 0,
            }),
            turn_boundaries: BTreeMap::from([(
                turn.logical_turn_key.clone(),
                TurnBoundary {
                    started_at: 1,
                    ended_at: 2,
                },
            )]),
            turns: vec![turn.clone()],
        }
    };
    let claim = || {
        Statement::from_string(conn.get_database_backend(),
        "SELECT coverage_digest, revision, checkpoint_id, completeness FROM agent_coverage_claim WHERE coverage_schema_version = 1".to_owned())
    };
    let first = import_prepared_with_coverage_versions(
        &conn,
        &storage,
        request(),
        &[1],
        Duration::from_secs(30),
        Some(Path::new(env!("CARGO_BIN_EXE_libra"))),
    )
    .await
    .unwrap();
    assert_eq!(first.checkpoints_written, 1);
    let before = conn.query_one_raw(claim()).await.unwrap().unwrap();
    for (written, skipped) in [(1, 0), (0, 1)] {
        let summary = import_prepared_with_coverage_versions(
            &conn,
            &storage,
            request(),
            &[2],
            Duration::from_secs(30),
            Some(Path::new(env!("CARGO_BIN_EXE_libra"))),
        )
        .await
        .unwrap();
        assert_eq!(
            (summary.checkpoints_written, summary.skipped_covered),
            (written, skipped)
        );
        assert!(!summary.partial);
    }
    let after = conn.query_one_raw(claim()).await.unwrap().unwrap();
    for column in ["coverage_digest", "checkpoint_id", "completeness"] {
        assert_eq!(
            before.try_get_by::<String, _>(column).unwrap(),
            after.try_get_by::<String, _>(column).unwrap()
        );
    }
    assert_eq!(
        before.try_get_by::<i64, _>("revision").unwrap(),
        after.try_get_by::<i64, _>("revision").unwrap()
    );
    assert_eq!(
        LocalCodexSession::count(&conn, "agent_coverage_claim", "1").await,
        2
    );
    assert_eq!(
        LocalCodexSession::count(&conn, "agent_checkpoint", "1").await,
        2
    );
    conn.close().await.unwrap();
}

#[test]
#[ignore = "operator-local Codex acceptance; never run in CI"]
fn codex_turn_mismatched_mirrors_never_override_annotated_raw_input() {
    let preview = codex_turn_preview(&[
        codex_turn_header(),
        codex_turn_context("turn-a"),
        serde_json::json!({"type":"event_msg","payload":{"type":"user_message","client_id":"client-a","message":"different mirror"}}),
        codex_turn_user("msg-a", "real raw input", "user.text"),
        serde_json::json!({"type":"event_msg","payload":{"type":"item_completed","thread_id":"123e4567-e89b-12d3-a456-426614174051","turn_id":"turn-a",
            "item":{"type":"UserMessage","id":"item-a","client_id":"client-a","content":[{"type":"text","text":"another mirror"}]}}}),
    ]);
    assert_eq!(preview["partial"], true);
    assert_eq!(preview["turns"].as_array().unwrap().len(), 1);
    assert_eq!(
        preview["turns"][0]["projection"]["records"],
        serde_json::json!([{"type":"user","text":"real raw input"}])
    );
    let event_only = codex_turn_preview(&[
        codex_turn_header(),
        codex_turn_context("turn-a"),
        serde_json::json!({"type":"event_msg","payload":{"type":"user_message","client_id":"client-a","message":"accepted input"}}),
    ]);
    assert_eq!(
        event_only["partial"], true,
        "a standalone accepted-input surface is qualified"
    );
}

#[test]
#[ignore = "operator-local Codex acceptance; never run in CI"]
fn codex_turn_first_header_pins_legacy_codec_across_resumed_current_records() {
    let mut header = codex_turn_header();
    header["payload"]["cli_version"] = serde_json::json!("0.161.0");
    let rows = vec![
        header,
        serde_json::json!({"type":"turn_context","payload":{"model":"legacy"}}),
        serde_json::json!({"type":"event_msg","payload":{"type":"task_started","turn_id":"legacy-turn"}}),
        serde_json::json!({"type":"response_item","payload":{"type":"message","role":"user","content":[{"type":"input_text","text":"legacy input"}]}}),
        serde_json::json!({"type":"response_item","payload":{"type":"message","role":"assistant","content":[{"type":"output_text","text":"legacy output"}]}}),
    ];
    let before = codex_turn_preview(&rows);
    assert_eq!(
        before["turns"][0]["projection"]["records"][0]["text"],
        "legacy input"
    );
    let mut resumed = rows;
    resumed.extend([codex_turn_header(), codex_turn_context("new-turn"),
        serde_json::json!({"type":"event_msg","payload":{"type":"task_started","turn_id":"new-turn"}}),
        codex_turn_user("new-input", "new input", "user.text")]);
    let after = codex_turn_preview(&resumed);
    assert_eq!(
        before["turns"][0], after["turns"][0],
        "appended current markers cannot reinterpret the older prefix"
    );
    assert_eq!(after["turns"].as_array().unwrap().len(), 2);
}

#[test]
#[ignore = "operator-local Codex acceptance; never run in CI"]
fn codex_turn_optional_response_ids_and_independent_input_ids_match_source_shapes() {
    let mut raw = codex_turn_user("not-serialized", "real", "user.text");
    raw["payload"].as_object_mut().unwrap().remove("id");
    let rows = vec![
        codex_turn_header(),
        codex_turn_context("turn-a"),
        raw,
        serde_json::json!({"type":"event_msg","payload":{"type":"user_message","client_id":"client-independent","message":"real"}}),
        serde_json::json!({"type":"event_msg","payload":{"type":"item_completed","thread_id":"123e4567-e89b-12d3-a456-426614174051","turn_id":"turn-a",
            "item":{"type":"UserMessage","id":"item-independent","client_id":"client-independent","content":[{"type":"text","text":"real"}]}}}),
        serde_json::json!({"type":"response_item","payload":{"type":"function_call_output","call_id":"call-a","output":"result"}}),
        serde_json::json!({"type":"response_item","payload":{"type":"reasoning","summary":[{"type":"summary_text","text":"visible"}],"encrypted_content":null}}),
    ];
    let first = codex_turn_preview(&rows);
    assert_eq!(first["partial"], true);
    assert_eq!(first["turns"][0]["coverage_schema_version"], 2);
    assert_eq!(
        first["turns"][0]["projection"]["records"]
            .as_array()
            .unwrap()
            .len(),
        3
    );
    assert_eq!(
        first["turns"],
        codex_turn_preview(&rows)["turns"],
        "identical snapshots replay even without item ids"
    );
}

#[test]
#[ignore = "operator-local Codex acceptance; never run in CI"]
fn codex_turn_scoped_answers_and_owner_records_keep_their_explicit_turns() {
    let mut scoped_a = serde_json::json!({"type":"response_item","payload":{"type":"message","id":"answer-a","role":"assistant","content":[{"type":"output_text","text":"answer a"}]}});
    scoped_a["payload"]["internal_chat_message_metadata_passthrough"] =
        serde_json::json!({"turn_id":"turn-a"});
    let mut foreign_header = codex_turn_header();
    foreign_header["payload"]["id"] = serde_json::json!("123e4567-e89b-12d3-a456-426614174052");
    let scoped_b = serde_json::json!({"type":"response_item","payload":{"type":"message","id":"answer-b","thread_id":"123e4567-e89b-12d3-a456-426614174051",
        "role":"assistant","internal_chat_message_metadata_passthrough":{"turn_id":"turn-b"},"content":[{"type":"output_text","text":"answer b"}]}});
    let preview = codex_turn_preview(&[
        codex_turn_header(),
        codex_turn_context("turn-a"),
        codex_turn_user("human-a", "first", "user.text"),
        codex_turn_context("turn-b"),
        codex_turn_user("human-b", "second", "user.text"),
        scoped_a,
        foreign_header,
        scoped_b,
        codex_turn_user("foreign-user", "foreign", "user.text"),
        codex_turn_header(),
    ]);
    assert_eq!(preview["turns"].as_array().unwrap().len(), 2);
    assert_eq!(
        preview["turns"][0]["projection"]["records"][1]["text"],
        "answer a"
    );
    assert_eq!(
        preview["turns"][1]["projection"]["records"][1]["text"],
        "answer b"
    );
    assert_eq!(
        preview["turns"][1]["projection"]["records"]
            .as_array()
            .unwrap()
            .len(),
        2
    );
    assert_eq!(preview["partial"], true);
}

#[test]
#[ignore = "operator-local Codex acceptance; never run in CI"]
fn codex_turn_unscoped_events_never_open_human_turns_across_boundaries() {
    use libra::internal::ai::capture::snapshot::{CaptureSnapshotPolicy, CaptureSnapshotService};
    let late = serde_json::json!({"type":"event_msg","payload":{"type":"user_message","client_id":"late-client","message":"first"}});
    let mut rows = vec![
        codex_turn_header(),
        codex_turn_context("turn-a"),
        codex_turn_user("raw-a", "first", "user.text"),
        serde_json::json!({"type":"event_msg","payload":{"type":"turn_complete","turn_id":"turn-a"}}),
        late.clone(),
    ];
    let source = rows
        .iter()
        .map(serde_json::Value::to_string)
        .collect::<Vec<_>>()
        .join("\n");
    let (ordinary, projection) = CaptureSnapshotService::preview_redacted_coverage_v2(
        "codex",
        source.into_bytes(),
        CaptureSnapshotPolicy::default(),
    )
    .unwrap();
    assert!(
        std::str::from_utf8(ordinary.bytes())
            .unwrap()
            .contains("late-client"),
        "unattributed event source is retained"
    );
    assert_eq!(
        projection["turns"].as_array().unwrap().len(),
        1,
        "{projection}"
    );
    assert_eq!(projection["partial"], true);
    rows.extend([codex_turn_context("turn-b"), late,
        serde_json::json!({"type":"response_item","payload":{"type":"reasoning","id":"reason-b","summary":[{"type":"summary_text","text":"current thought"}],"encrypted_content":null}})]);
    let projection = codex_turn_preview(&rows);
    assert_eq!(projection["turns"].as_array().unwrap().len(), 2);
    assert!(
        projection["turns"][1]["projection"]["records"]
            .as_array()
            .unwrap()
            .iter()
            .all(|record| record["type"] != "user")
    );
    let early = codex_turn_preview(&[
        codex_turn_header(),
        serde_json::json!({"type":"event_msg","payload":{"type":"user_message","client_id":"early-client","message":"early"}}),
        codex_turn_context("turn-a"),
        codex_turn_user("raw-early", "early", "user.text"),
    ]);
    assert_eq!(early["turns"].as_array().unwrap().len(), 1, "{early}");
    assert_eq!(
        early["turns"][0]["projection"]["records"],
        serde_json::json!([{"type":"user","text":"early"}])
    );
    assert_eq!(early["partial"], true);
}

#[test]
#[ignore = "operator-local Codex acceptance; never run in CI"]
fn codex_turn_optional_context_keeps_lifecycle_proof_and_first_raw_pair_binds_output() {
    let optional = codex_turn_preview(&[
        codex_turn_header(),
        serde_json::json!({"type":"event_msg","payload":{"type":"turn_started","turn_id":"turn-a"}}),
        serde_json::json!({"type":"turn_context","payload":{"model":"pinned-model"}}),
        codex_turn_user("raw-a", "first", "user.text"),
    ]);
    assert_eq!(
        optional["turns"][0]["projection"]["logical_turn_key"],
        "turn-a"
    );
    assert_eq!(optional["partial"], true);
    let completed = serde_json::json!({"type":"event_msg","payload":{"type":"item_completed","thread_id":"123e4567-e89b-12d3-a456-426614174051","turn_id":"turn-a", "item":{"type":"UserMessage","id":"item-a","content":[{"type":"text","text":"first"}]}}});
    let mut raw = codex_turn_user("raw-a", "first", "user.text");
    raw["payload"]["internal_chat_message_metadata_passthrough"]["turn_id"] =
        serde_json::json!("turn-a");
    let paired = codex_turn_preview(&[
        codex_turn_header(),
        completed,
        raw,
        serde_json::json!({"type":"response_item","payload":{"type":"message","id":"answer-a","role":"assistant","content":[{"type":"output_text","text":"answer"}]}}),
    ]);
    assert_eq!(paired["turns"].as_array().unwrap().len(), 1, "{paired}");
    assert_eq!(
        paired["turns"][0]["projection"]["logical_turn_key"],
        "turn-a"
    );
    assert_eq!(
        paired["turns"][0]["projection"]["records"]
            .as_array()
            .unwrap()
            .len(),
        2
    );
}

#[test]
#[ignore = "operator-local Codex acceptance; never run in CI"]
fn codex_turn_unscoped_completed_mirrors_cannot_create_fallback_user_groups() {
    for invalid in [
        None,
        Some(serde_json::json!(null)),
        Some(serde_json::json!("invalid:key")),
        Some(serde_json::json!(19)),
    ] {
        let mut completed = serde_json::json!({"type":"event_msg","payload":{"type":"item_completed","thread_id":"123e4567-e89b-12d3-a456-426614174051","item":{"type":"UserMessage","id":"late-item","content":[{"type":"text","text":"first"}]}}});
        if let Some(id) = invalid {
            completed["payload"]["turn_id"] = id;
        }
        let projection = codex_turn_preview(&[
            codex_turn_header(),
            codex_turn_context("turn-a"),
            codex_turn_user("raw-a", "first", "user.text"),
            serde_json::json!({"type":"event_msg","payload":{"type":"turn_complete","turn_id":"turn-a"}}),
            completed,
        ]);
        assert_eq!(
            projection["turns"].as_array().unwrap().len(),
            1,
            "{projection}"
        );
        assert_eq!(
            projection["turns"][0]["projection"]["records"],
            serde_json::json!([{"type":"user","text":"first"}])
        );
        assert_eq!(projection["partial"], true);
    }
}

#[test]
#[ignore = "operator-local Codex acceptance; never run in CI"]
fn codex_turn_missing_context_without_lifecycle_proof_does_not_merge_humans() {
    let ended_without_new_input = codex_turn_preview(&[
        codex_turn_header(),
        codex_turn_context("turn-a"),
        codex_turn_user("raw-a", "first", "user.text"),
        serde_json::json!({"type":"turn_context","payload":{"model":"missing-turn"}}),
    ]);
    assert_eq!(
        ended_without_new_input["turns"].as_array().unwrap().len(),
        1
    );
    assert_eq!(
        ended_without_new_input["turns"][0]["projection"]["completeness"],
        "complete"
    );
    assert_eq!(
        ended_without_new_input["partial"], true,
        "unassigned damage must remain visible even without another turn"
    );
    let projection = codex_turn_preview(&[
        codex_turn_header(),
        codex_turn_context("turn-a"),
        codex_turn_user("raw-a", "first", "user.text"),
        serde_json::json!({"type":"turn_context","payload":{"model":"missing-turn"}}),
        codex_turn_user("raw-b", "second", "user.text"),
    ]);
    assert_eq!(
        projection["turns"].as_array().unwrap().len(),
        2,
        "{projection}"
    );
    assert_eq!(
        projection["turns"][0]["projection"]["logical_turn_key"],
        "turn-a"
    );
    assert_eq!(
        projection["turns"][1]["projection"]["logical_turn_key"],
        "ordinal:1"
    );
    assert_eq!(projection["partial"], true);
    assert_eq!(
        projection["turns"][0]["projection"]["completeness"],
        "complete"
    );
    assert_eq!(
        projection["turns"][1]["projection"]["completeness"],
        "incomplete"
    );
}

#[test]
#[ignore = "operator-local Codex acceptance; never run in CI"]
fn codex_turn_malformed_terminal_scope_qualifies_the_active_group() {
    for scope in [
        serde_json::Value::Null,
        serde_json::json!("invalid:key"),
        serde_json::json!(19),
        serde_json::json!(true),
        serde_json::json!([]),
    ] {
        let projection = codex_turn_preview(&[
            codex_turn_header(),
            codex_turn_context("turn-a"),
            codex_turn_user("raw-a", "first", "user.text"),
            serde_json::json!({"type":"event_msg","payload":{"type":"turn_complete","turn_id":scope}}),
            serde_json::json!({"type":"response_item","payload":{"type":"message","id":"answer-a","role":"assistant","content":[{"type":"output_text","text":"answer"}]}}),
        ]);
        assert_eq!(
            projection["turns"].as_array().unwrap().len(),
            1,
            "{projection}"
        );
        assert_eq!(projection["partial"], true);
        assert_eq!(
            projection["turns"][0]["projection"]["completeness"],
            "incomplete"
        );
        assert_eq!(
            projection["turns"][0]["projection"]["records"]
                .as_array()
                .unwrap()
                .len(),
            2
        );
    }
}

#[test]
#[ignore = "operator-local Codex acceptance; never run in CI"]
fn codex_turn_fresh_scoped_old_raw_keeps_the_current_turn_for_following_output() {
    let mut late = codex_turn_user("fresh-late-raw", "late steering for first", "user.text");
    late["payload"]["internal_chat_message_metadata_passthrough"]["turn_id"] =
        serde_json::json!("turn-a");
    let projection = codex_turn_preview(&[
        codex_turn_header(),
        codex_turn_context("turn-a"),
        codex_turn_user("raw-a", "first", "user.text"),
        codex_turn_context("turn-b"),
        codex_turn_user("raw-b", "second", "user.text"),
        late,
        serde_json::json!({"type":"response_item","payload":{"type":"message","id":"answer-b","role":"assistant","content":[{"type":"output_text","text":"second answer"}]}}),
    ]);
    assert_eq!(
        projection["turns"].as_array().unwrap().len(),
        2,
        "{projection}"
    );
    assert_eq!(
        projection["turns"][0]["projection"]["records"][1]["text"],
        "late steering for first"
    );
    assert_eq!(
        projection["turns"][1]["projection"]["records"][1]["text"],
        "second answer"
    );
    assert_eq!(
        projection["turns"][0]["projection"]["completeness"],
        "incomplete"
    );
    assert_eq!(
        projection["turns"][1]["projection"]["completeness"],
        "incomplete"
    );
}

/// Use the public snapshot preview, not a hand-built semantic record.
fn codex_summary_preview(reasoning: &str) -> (Vec<u8>, serde_json::Value) {
    use libra::internal::ai::capture::snapshot::{CaptureSnapshotPolicy, CaptureSnapshotService};
    let source = format!(
        "{}\n{}\n{reasoning}\n{}\n",
        r#"{"type":"event_msg","payload":{"type":"task_started","turn_id":"cx15-turn"}}"#,
        r#"{"type":"response_item","payload":{"type":"message","role":"user","content":[{"type":"input_text","text":"hi"}]}}"#,
        r#"{"type":"response_item","payload":{"type":"message","role":"assistant","content":[{"type":"output_text","text":"done"}]}}"#,
    );
    let (ordinary, projection) = CaptureSnapshotService::preview_redacted_coverage_v2(
        "codex",
        source.into_bytes(),
        CaptureSnapshotPolicy::default(),
    )
    .expect("bounded Codex snapshot preview");
    (ordinary.into_inner(), projection)
}

#[test]
#[ignore = "operator-local Codex acceptance; never run in CI"]
fn codex_summary_retains_visible_and_unproven_values() {
    use sha2::{Digest, Sha256};
    let line = r#"{"type":"response_item","payload":{"type":"reasoning","id":"rs-cx15","summary":[{"type":"summary_text","text":"visible 🐍"}],"content":[{"type":"reasoning_text","text":"raw canary"}],"encrypted_content":"CX15-OPAQUE-CANARY","future":{"data":[1,null,"CX15-ASSOCIATED-CANARY"],"time":{"start":1}}}}"#;
    let expected: serde_json::Value = serde_json::from_str(line).unwrap();
    let (ordinary, preview) = codex_summary_preview(line);
    let retained: serde_json::Value =
        serde_json::from_slice(ordinary.split(|b| *b == b'\n').nth(2).unwrap()).unwrap();
    assert_eq!(
        retained, expected,
        "ordinary data preserves every associated JSON value"
    );
    let canonical = r#"[{"role":"user","text":"hi"},{"provider":"codex","role":"reasoning","source_kind":"summary","text":"visible 🐍"},{"provider":"codex","role":"reasoning","source_kind":"reasoning","text":"raw canary"},{"role":"assistant","text":"done"}]"#;
    assert_eq!(preview["turns"][0]["coverage_schema_version"], 2);
    assert_eq!(
        preview["turns"][0]["digest"],
        hex::encode(Sha256::digest(canonical.as_bytes()))
    );
    let sidecar = &preview["codex_reasoning"];
    assert_eq!(sidecar["source_status"], "unproven");
    assert_eq!(sidecar["partial"], true);
    assert_eq!(sidecar["turns"][0]["counts"]["readable_parts"], 2);
    assert_eq!(
        sidecar["turns"][0]["counts"]["readable_utf8_bytes"],
        "visible 🐍".len() + "raw canary".len()
    );
    assert_eq!(sidecar["turns"][0]["counts"]["opaque_nonempty"], 1);
    for canary in [
        "visible",
        "raw canary",
        "CX15-OPAQUE-CANARY",
        "CX15-ASSOCIATED-CANARY",
    ] {
        assert!(!sidecar.to_string().contains(canary));
    }
    let (_, changed) = codex_summary_preview(&line.replace("visible 🐍", "changed 🐍"));
    assert_ne!(preview["turns"][0]["digest"], changed["turns"][0]["digest"]);
}

#[test]
#[ignore = "operator-local Codex acceptance; never run in CI"]
fn codex_summary_scans_decoded_credentials_without_rounding_unknown_numbers() {
    let line = r#"{"type":"response_item","payload":{"type":"reasoning","id":"rs-cx15-secret","summary":[{"type":"summary_text","text":"before \u0073k-ant-EXAMPLE0000000000000000 after"}],"encrypted_content":"\u0073k-ant-EXAMPLE0000000000000000","future":{"large":18446744073709551615,"precise":0.12345678901234567890123456789}}}"#;
    let (ordinary, preview) = codex_summary_preview(line);
    let text = std::str::from_utf8(&ordinary).unwrap();
    assert!(!text.contains("EXAMPLE0000000000000000"));
    assert!(text.contains("18446744073709551615"));
    assert!(text.contains("0.12345678901234567890123456789"));
    let row: serde_json::Value = serde_json::from_str(text.lines().nth(2).unwrap()).unwrap();
    assert_eq!(
        row["payload"]["summary"][0]["text"],
        "before <REDACTED:anthropic-api-key> after"
    );
    assert_eq!(
        row["payload"]["encrypted_content"],
        "<REDACTED:anthropic-api-key>"
    );
    assert_eq!(
        preview["turns"][0]["projection"]["records"][1]["text"],
        row["payload"]["summary"][0]["text"]
    );
    assert!(
        preview["redaction_report"]["dropped_matches"]
            .as_u64()
            .unwrap()
            >= 2
    );
    assert!(
        preview["redaction_report"]["matches"]
            .as_array()
            .unwrap()
            .is_empty(),
        "decoded string offsets must not look like transcript offsets"
    );
}

#[test]
#[ignore = "operator-local Codex acceptance; never run in CI"]
fn codex_summary_distinguishes_empty_null_missing_and_bad_types() {
    let rows = [
        r#"{"type":"response_item","payload":{"type":"reasoning","id":"rs-empty","summary":[],"encrypted_content":""}}"#,
        r#"{"type":"response_item","payload":{"type":"reasoning","id":"rs-null","summary":[],"encrypted_content":null}}"#,
        r#"{"type":"response_item","payload":{"type":"reasoning","id":"rs-absent","summary":[]}}"#,
        r#"{"type":"response_item","payload":{"type":"reasoning","id":"rs-bad","summary":[],"encrypted_content":{"data":"CX15-WRONG-TYPE-CANARY"}}}"#,
    ];
    let (ordinary, preview) = codex_summary_preview(&rows.join("\n"));
    let retained: Vec<serde_json::Value> = ordinary
        .split(|b| *b == b'\n')
        .filter(|l| !l.is_empty())
        .map(|l| serde_json::from_slice(l).unwrap())
        .collect();
    for (actual, expected) in retained[2..6].iter().zip(rows) {
        assert_eq!(
            actual,
            &serde_json::from_str::<serde_json::Value>(expected).unwrap()
        );
    }
    let counts = &preview["codex_reasoning"]["turns"][0]["counts"];
    assert_eq!(counts["readable_parts"], 0);
    assert_eq!(counts["opaque_nonempty"], 0);
    assert_eq!(counts["encrypted_empty"], 1);
    assert_eq!(counts["encrypted_null"], 1);
    assert_eq!(counts["encrypted_missing"], 1);
    assert_eq!(counts["unsupported_records"], 1);
    assert_eq!(preview["partial"], true);
}

#[test]
#[ignore = "operator-local Codex acceptance; never run in CI"]
fn codex_summary_compaction_retains_data_without_guessing_current_turn() {
    let row = r#"{"type":"compacted","payload":{"message":"context","replacement_history":[{"type":"reasoning","id":"rs-history","summary":[{"type":"summary_text","text":"CX15-HISTORY-CANARY"}],"encrypted_content":"CX15-HISTORY-OPAQUE"},{"type":"compaction","encrypted_content":"CX15-NOT-THINKING"}]}}"#;
    let (ordinary, preview) = codex_summary_preview(row);
    let retained: serde_json::Value =
        serde_json::from_slice(ordinary.split(|b| *b == b'\n').nth(2).unwrap()).unwrap();
    assert_eq!(
        retained,
        serde_json::from_str::<serde_json::Value>(row).unwrap()
    );
    let counts = preview["codex_reasoning"]["turns"]
        .as_array()
        .unwrap()
        .iter()
        .find(|r| r["turn_ordinal"].is_null())
        .unwrap();
    assert_eq!(counts["counts"]["readable_parts"], 1);
    assert_eq!(counts["counts"]["opaque_nonempty"], 1);
    assert_eq!(preview["partial"], true);
    assert!(!preview["codex_reasoning"].to_string().contains("HISTORY"));
}

#[test]
#[ignore = "operator-local Codex acceptance; never run in CI"]
fn codex_summary_legacy_text_array_and_usage_snapshots_are_explicit() {
    let rows = [
        // The writer omits text-only content arrays; an existing reader may
        // still encounter this compatible shape. It is not live evidence.
        r#"{"type":"response_item","payload":{"type":"reasoning","id":"rs-text","summary":[],"content":[{"type":"text","text":"CX15-COMPATIBLE-CONTENT"}],"encrypted_content":null}}"#,
        r#"{"type":"token_usage_record","payload":{"turn_id":"cx15-turn","turn_token_usage":{"reasoning_output_tokens":7}}}"#,
        r#"{"type":"token_usage_record","payload":{"turn_id":"cx15-turn","turn_token_usage":{"reasoning_output_tokens":9}}}"#,
    ];
    let (_, preview) = codex_summary_preview(&rows.join("\n"));
    assert_eq!(
        preview["codex_reasoning"]["turns"][0]["reasoning_tokens"],
        9
    );
    assert_eq!(
        preview["codex_reasoning"]["turns"][0]["counts"]["readable_parts"],
        1
    );
    let reset = format!("{}\n{}\n{}", rows.join("\n"), rows[1], rows[2]);
    let (_, preview) = codex_summary_preview(&reset);
    assert!(preview["codex_reasoning"]["turns"][0]["reasoning_tokens"].is_null());
    assert_eq!(preview["partial"], true);
}

#[test]
#[ignore = "operator-local Codex acceptance; never run in CI"]
fn codex_summary_duplicate_envelopes_still_scan_decoded_credentials() {
    let row = r#"{"type":"response_item","payload":{"type":"reasoning","summary":[{"type":"summary_text","text":"\u0073k-ant-EXAMPLE0000000000000000"}]},"payload":{"type":"reasoning","summary":[],"encrypted_content":"\u0073k-ant-EXAMPLE1111111111111111"}}"#;
    let (ordinary, preview) = codex_summary_preview(row);
    let body = std::str::from_utf8(&ordinary).unwrap();
    assert!(!body.contains("EXAMPLE0000000000000000"));
    assert!(!body.contains("EXAMPLE1111111111111111"));
    assert!(body.contains("<REDACTED:anthropic-api-key>"));
    assert_eq!(body.matches("\"payload\"").count(), 5);
    assert_eq!(preview["partial"], true);
}

#[test]
#[ignore = "operator-local Codex acceptance; never run in CI"]
fn codex_summary_associated_part_fields_and_terminal_boundaries_are_partial() {
    let rows = [
        r#"{"type":"response_item","payload":{"type":"reasoning","id":"rs-extra","summary":[{"type":"summary_text","text":"visible","future":"CX15-ASSOCIATED-PART"}],"encrypted_content":null}}"#,
        r#"{"type":"event_msg","payload":{"type":"task_complete","turn_id":"cx15-turn"}}"#,
        r#"{"type":"response_item","payload":{"type":"reasoning","id":"rs-no-turn","summary":[{"type":"summary_text","text":"CX15-UNATTRIBUTED"}],"encrypted_content":null}}"#,
    ];
    let (ordinary, preview) = codex_summary_preview(&rows.join("\n"));
    assert!(
        std::str::from_utf8(&ordinary)
            .unwrap()
            .contains("CX15-ASSOCIATED-PART")
    );
    assert_eq!(
        preview["codex_reasoning"]["turns"][0]["counts"]["unproven_fields"],
        1
    );
    let unassigned = preview["codex_reasoning"]["turns"]
        .as_array()
        .unwrap()
        .iter()
        .find(|row| row["turn_ordinal"].is_null())
        .unwrap();
    assert_eq!(unassigned["counts"]["readable_parts"], 1);
    assert_eq!(preview["partial"], true);
}

#[test]
#[ignore = "operator-local Codex acceptance; never run in CI"]
fn codex_summary_large_tool_output_and_broken_utf8_tail_keep_valid_thinking() {
    use libra::internal::ai::capture::snapshot::{CaptureSnapshotPolicy, CaptureSnapshotService};
    let large = "x".repeat(1024 * 1024 + 1);
    let mut source = format!("{{\"type\":\"event_msg\",\"payload\":{{\"type\":\"task_started\",\"turn_id\":\"large-turn\"}}}}\n{{\"type\":\"response_item\",\"payload\":{{\"type\":\"message\",\"role\":\"user\",\"content\":\"hi\"}}}}\n{{\"type\":\"response_item\",\"payload\":{{\"type\":\"reasoning\",\"id\":\"valid-prefix\",\"summary\":[{{\"type\":\"summary_text\",\"text\":\"CX15-VALID-PREFIX\"}}],\"encrypted_content\":null}}}}\n{{\"type\":\"response_item\",\"payload\":{{\"type\":\"function_call_output\",\"call_id\":\"big\",\"output\":\"{large}\"}}}}\n{{\"type\":\"response_item\",\"payload\":{{\"type\":\"reasoning\",\"summary\":[{{\"type\":\"summary_text\",\"text\":\"").into_bytes();
    source.extend_from_slice(&[0xf0, 0x9f]);
    let (ordinary, preview) = CaptureSnapshotService::preview_redacted_coverage_v2(
        "codex",
        source,
        CaptureSnapshotPolicy::default(),
    )
    .unwrap();
    let retained = std::str::from_utf8(ordinary.bytes()).unwrap();
    assert!(
        retained.contains("CX15-VALID-PREFIX"),
        "bad tail must not erase valid prior thinking"
    );
    assert!(
        retained.contains(&large),
        "large ordinary tool output remains available"
    );
    assert_eq!(preview["partial"], true);
    assert_eq!(preview["codex_reasoning"]["statistics_unavailable"], true);
}

#[test]
#[ignore = "operator-local Codex acceptance; never run in CI"]
fn codex_summary_statistics_cardinality_caps_do_not_remove_ordinary_data() {
    for explicit_turns in [false, true] {
        let mut lines = Vec::new();
        for i in 0..4097 {
            if explicit_turns {
                lines.push(serde_json::json!({"type":"event_msg","payload":{"type":"task_started","turn_id":format!("cap-{i}")}}).to_string());
            }
            lines.push(serde_json::json!({"type":"response_item","payload":{"type":"reasoning","id":format!("cap-rs-{i}"),"summary":[{"type":"summary_text","text":format!("CX15-CAP-{i}")}]}}).to_string());
        }
        let (ordinary, preview) = codex_summary_preview(&lines.join("\n"));
        let text = std::str::from_utf8(&ordinary).unwrap();
        assert!(text.contains("CX15-CAP-0"));
        assert!(text.contains("CX15-CAP-4096"));
        assert_eq!(preview["codex_reasoning"]["statistics_unavailable"], true);
        assert_eq!(preview["codex_reasoning"]["partial"], true);
    }
}

/// Exercise the authorized file reader, real catalog/object writer, and
/// ordinary export. The terminal envelope is a legacy compatibility fixture;
/// it does not claim to be output from a running Codex process.
#[tokio::test]
#[ignore = "operator-local Codex acceptance; never run in CI"]
async fn codex_summary_import_and_ordinary_checkpoint_export_preserve_values() {
    codex_retention_import_case(false, false).await;
}

#[tokio::test]
#[ignore = "operator-local Codex acceptance; never run in CI"]
async fn codex_summary_import_retains_opaque_only_updates_without_rewriting_history() {
    codex_retention_import_case(true, false).await;
}

#[tokio::test]
#[ignore = "operator-local Codex acceptance; never run in CI"]
async fn codex_summary_retention_commit_failure_keeps_claim_and_ref_and_recovers() {
    codex_retention_import_case(true, true).await;
}

async fn codex_retention_import_case(update_opaque: bool, fail_update_once: bool) {
    use std::process::Command;

    use sea_orm::{ConnectionTrait, Database};
    use serde_json::json;

    let temp = tempfile::tempdir().unwrap();
    let repo = temp.path().join("repo");
    let home = temp.path().join("home");
    std::fs::create_dir_all(&repo).unwrap();
    std::fs::create_dir_all(&home).unwrap();
    let repo = repo.canonicalize().unwrap();
    let home = home.canonicalize().unwrap();
    let run = |args: &[&str]| {
        Command::new(env!("CARGO_BIN_EXE_libra"))
            .current_dir(&repo)
            .env("HOME", &home)
            .env("USERPROFILE", &home)
            .env("LIBRA_TEST_HOME", &home)
            .env_remove("CODEX_HOME")
            .args(args)
            .output()
            .expect("run isolated Libra CLI")
    };
    let init = run(&["init"]);
    assert!(
        init.status.success(),
        "init: {}",
        String::from_utf8_lossy(&init.stderr)
    );
    let sid = "123e4567-e89b-12d3-a456-426614174015";
    let source = home
        .join(".codex/sessions/2026/10/19")
        .join(format!("rollout-2026-10-19T01-00-00-{sid}.jsonl"));
    std::fs::create_dir_all(source.parent().unwrap()).unwrap();
    let reasoning = json!({"type":"reasoning","id":"rs-cx15-export","summary":[{"type":"summary_text","text":"CX15-EXPORTED-SUMMARY"}],"content":[{"type":"reasoning_text","text":"CX15-EXPORTED-REASONING"}],"encrypted_content":"CX15-EXPORTED-OPAQUE","future":{"signature":[1,null,"CX15-EXPORTED-ASSOCIATED"],"time":{"start":1,"end":2}}});
    let rows = [
        json!({"type":"session_meta","timestamp":"2026-10-19T01:00:00Z","payload":{"id":sid,"cwd":repo}}),
        json!({"type":"response_item","timestamp":"2026-10-19T01:00:01Z","payload":{"type":"message","role":"user","id":"cx15-user","content":[{"type":"input_text","text":"hi"}]}}),
        json!({"type":"response_item","timestamp":"2026-10-19T01:00:02Z","payload":reasoning}),
        json!({"type":"response_item","timestamp":"2026-10-19T01:00:03Z","payload":{"type":"message","role":"assistant","id":"cx15-assistant","content":[{"type":"output_text","text":"done"}]}}),
        json!({"type":"session_end","timestamp":"2026-10-19T01:00:04Z","payload":{"type":"session_end","cwd":repo}}),
    ];
    let bytes = rows
        .iter()
        .map(serde_json::Value::to_string)
        .collect::<Vec<_>>()
        .join("\n")
        + "\n";
    std::fs::write(&source, &bytes).unwrap();
    let source_path = source.to_str().unwrap();
    let imported = run(&[
        "agent",
        "import",
        "--path",
        source_path,
        "--agent",
        "codex",
        "--yes",
        "--json",
    ]);
    assert!(
        imported.status.success(),
        "import: {}",
        String::from_utf8_lossy(&imported.stderr)
    );
    let db = Database::connect(format!(
        "sqlite://{}?mode=ro",
        repo.join(".libra/libra.db").display()
    ))
    .await
    .unwrap();
    let row = db
        .query_one_raw(sea_orm::Statement::from_string(
            db.get_database_backend(),
            "SELECT checkpoint_id FROM agent_checkpoint".to_owned(),
        ))
        .await
        .unwrap()
        .expect("actual checkpoint created");
    let checkpoint: String = row.try_get_by("checkpoint_id").unwrap();
    let exported = run(&["agent", "checkpoint", "export", &checkpoint]);
    assert!(
        exported.status.success(),
        "ordinary export: {}",
        String::from_utf8_lossy(&exported.stderr)
    );
    let retained = exported
        .stdout
        .split(|b| *b == b'\n')
        .filter(|l| !l.is_empty())
        .map(|l| serde_json::from_slice::<serde_json::Value>(l).unwrap())
        .find(|row| row["payload"]["id"] == "rs-cx15-export")
        .unwrap_or_else(|| {
            panic!(
                "thinking must be present in ordinary checkpoint export: {}",
                String::from_utf8_lossy(&exported.stdout)
            )
        });
    assert_eq!(retained["payload"], reasoning);
    let replay = run(&[
        "agent",
        "import",
        "--path",
        source_path,
        "--agent",
        "codex",
        "--yes",
        "--json",
    ]);
    assert!(
        replay.status.success(),
        "replay: {}",
        String::from_utf8_lossy(&replay.stderr)
    );
    let count = db
        .query_one_raw(sea_orm::Statement::from_string(
            db.get_database_backend(),
            "SELECT COUNT(*) AS n FROM agent_checkpoint".to_owned(),
        ))
        .await
        .unwrap()
        .unwrap();
    assert_eq!(
        count.try_get_by::<i64, _>("n").unwrap(),
        1,
        "replay must be idempotent"
    );
    assert_eq!(
        std::fs::read(&source).unwrap(),
        bytes.as_bytes(),
        "source is never modified"
    );
    for output in [&imported, &replay] {
        assert!(!String::from_utf8_lossy(&output.stdout).contains("CX15-EXPORTED"));
        assert!(!String::from_utf8_lossy(&output.stderr).contains("CX15-EXPORTED"));
    }
    if update_opaque {
        // Coverage-relevant text is unchanged. A text claim must not suppress
        // a newly available opaque/associated ordinary value.
        let updated = bytes
            .replace("CX15-EXPORTED-OPAQUE", "CX15-UPDATED-OPAQUE")
            .replace("CX15-EXPORTED-ASSOCIATED", "CX15-UPDATED-ASSOCIATED");
        std::fs::write(&source, &updated).unwrap();
        let claim_sql = "SELECT json_object('version',coverage_schema_version,'digest',coverage_digest,'revision',revision,'checkpoint',checkpoint_id,'state',state,'owner',owner,'fence',fence_token) AS snapshot FROM agent_coverage_claim";
        let original_claim: String = db
            .query_one_raw(sea_orm::Statement::from_string(
                db.get_database_backend(),
                claim_sql.to_owned(),
            ))
            .await
            .unwrap()
            .unwrap()
            .try_get_by("snapshot")
            .unwrap();
        if fail_update_once {
            let trace_sql = "SELECT \"commit\" AS oid FROM reference WHERE name='traces' AND remote IS NULL AND kind='Branch'";
            let original_ref: String = db
                .query_one_raw(sea_orm::Statement::from_string(
                    db.get_database_backend(),
                    trace_sql.to_owned(),
                ))
                .await
                .unwrap()
                .unwrap()
                .try_get_by("oid")
                .unwrap();
            let writer = Database::connect(format!(
                "sqlite://{}?mode=rw",
                repo.join(".libra/libra.db").display()
            ))
            .await
            .unwrap();
            writer.execute_unprepared("CREATE TRIGGER cx15_fail_supplement BEFORE INSERT ON agent_checkpoint BEGIN SELECT RAISE(ABORT,'synthetic retention commit failure'); END").await.unwrap();
            let rejected = run(&[
                "agent",
                "import",
                "--path",
                source_path,
                "--agent",
                "codex",
                "--yes",
                "--json",
            ]);
            assert!(
                !rejected.status.success(),
                "failed catalog/ref transaction must not be acknowledged as success"
            );
            assert!(!String::from_utf8_lossy(&rejected.stdout).contains("CX15-UPDATED"));
            assert!(!String::from_utf8_lossy(&rejected.stderr).contains("CX15-UPDATED"));
            let current_ref: String = db
                .query_one_raw(sea_orm::Statement::from_string(
                    db.get_database_backend(),
                    trace_sql.to_owned(),
                ))
                .await
                .unwrap()
                .unwrap()
                .try_get_by("oid")
                .unwrap();
            assert_eq!(current_ref, original_ref, "failure cannot advance traces");
            let current_claim: String = db
                .query_one_raw(sea_orm::Statement::from_string(
                    db.get_database_backend(),
                    claim_sql.to_owned(),
                ))
                .await
                .unwrap()
                .unwrap()
                .try_get_by("snapshot")
                .unwrap();
            assert_eq!(
                current_claim, original_claim,
                "failure cannot mutate semantic evidence"
            );
            let count = db
                .query_one_raw(sea_orm::Statement::from_string(
                    db.get_database_backend(),
                    "SELECT COUNT(*) AS n FROM agent_checkpoint".to_owned(),
                ))
                .await
                .unwrap()
                .unwrap();
            assert_eq!(count.try_get_by::<i64, _>("n").unwrap(), 1);
            writer
                .execute_unprepared("DROP TRIGGER cx15_fail_supplement")
                .await
                .unwrap();
            writer.close().await.unwrap();
        }
        let changed = run(&[
            "agent",
            "import",
            "--path",
            source_path,
            "--agent",
            "codex",
            "--yes",
            "--json",
        ]);
        assert!(
            changed.status.success(),
            "updated import: {}",
            String::from_utf8_lossy(&changed.stderr)
        );
        let next = db
            .query_one_raw(sea_orm::Statement::from_sql_and_values(
                db.get_database_backend(),
                "SELECT checkpoint_id FROM agent_checkpoint WHERE checkpoint_id != ?",
                [checkpoint.clone().into()],
            ))
            .await
            .unwrap()
            .expect("opaque-only change needs an ordinary retention supplement");
        let next_id: String = next.try_get_by("checkpoint_id").unwrap();
        let next_export = run(&["agent", "checkpoint", "export", &next_id]);
        assert!(next_export.status.success());
        let body = String::from_utf8(next_export.stdout).unwrap();
        assert!(body.contains("CX15-UPDATED-OPAQUE"));
        assert!(body.contains("CX15-UPDATED-ASSOCIATED"));
        let previous_export = run(&["agent", "checkpoint", "export", &checkpoint]);
        assert_eq!(
            previous_export.stdout, exported.stdout,
            "old ordinary checkpoint is immutable"
        );
        let repeated = run(&[
            "agent",
            "import",
            "--path",
            source_path,
            "--agent",
            "codex",
            "--yes",
            "--json",
        ]);
        assert!(repeated.status.success());
        let count = db
            .query_one_raw(sea_orm::Statement::from_string(
                db.get_database_backend(),
                "SELECT COUNT(*) AS n FROM agent_checkpoint".to_owned(),
            ))
            .await
            .unwrap()
            .unwrap();
        assert_eq!(
            count.try_get_by::<i64, _>("n").unwrap(),
            2,
            "supplement replay is idempotent"
        );
        let current_claim: String = db
            .query_one_raw(sea_orm::Statement::from_string(
                db.get_database_backend(),
                claim_sql.to_owned(),
            ))
            .await
            .unwrap()
            .unwrap()
            .try_get_by("snapshot")
            .unwrap();
        assert_eq!(
            current_claim, original_claim,
            "ordinary retention never advances the semantic claim"
        );
        for output in [&changed, &repeated] {
            assert!(!String::from_utf8_lossy(&output.stdout).contains("CX15-UPDATED"));
            assert!(!String::from_utf8_lossy(&output.stderr).contains("CX15-UPDATED"));
        }
        assert_eq!(std::fs::read(&source).unwrap(), updated.as_bytes());
    }
    db.close().await.unwrap();
}

#[test]
#[ignore = "operator-local Codex acceptance; never run in CI"]
fn codex_ordinary_retention_docs_keep_values_and_claims_separate() {
    const AGENT_CMD_DOC_EN: &str = include_str!("../docs/commands/agent.md");
    const AGENT_CMD_DOC_ZH: &str = include_str!("../docs/commands/zh-CN/agent.md");
    const AGENT_DEV_DOC: &str = include_str!("../docs/development/commands/agent.md");
    const AGENT_DOC: &str = include_str!("../docs/development/tracing/agent.md");
    const COMPATIBILITY_DOC: &str = include_str!("../COMPATIBILITY.md");
    fn assert_contains(document: &str, label: &str, term: &str) {
        assert!(document.contains(term), "{label}: missing {term}");
    }
    fn assert_absent(document: &str, label: &str, term: &str) {
        assert!(!document.contains(term), "{label}: stale {term}");
    }
    for (label, document) in [
        ("EN", AGENT_CMD_DOC_EN),
        ("zh-CN", AGENT_CMD_DOC_ZH),
        ("development", AGENT_DEV_DOC),
        ("tracing", AGENT_DOC),
        ("compatibility", COMPATIBILITY_DOC),
    ] {
        for term in [
            "Codex ordinary thinking retention",
            "summary_text",
            "encrypted_content",
            "JSONL",
            "V1",
            "CX-05",
            "Unproven",
            "Partial",
        ] {
            assert_contains(document, label, term);
        }
    }
    assert_contains(AGENT_CMD_DOC_EN, "EN", "not a live model acceptance");
    assert_contains(AGENT_CMD_DOC_EN, "EN", "supplement checkpoint");
    assert_contains(AGENT_CMD_DOC_ZH, "zh-CN", "并非真实模型验收");
    assert_contains(AGENT_CMD_DOC_ZH, "zh-CN", "补充 checkpoint");
    assert_absent(
        AGENT_CMD_DOC_EN,
        "EN",
        "encrypted-reasoning payload is deliberately dropped",
    );
    assert_absent(
        AGENT_CMD_DOC_ZH,
        "zh-CN",
        "加密 reasoning payload 会被刻意从 capture projection 丢弃",
    );
}

#[test]
#[ignore = "operator-local Codex acceptance; never run in CI"]
fn codex_summary_preview_deduplicates_at_the_identity_budget() {
    let mut lines = (0..4096)
        .map(|i| {
            serde_json::json!({
                "type":"response_item", "payload": {"type":"reasoning", "id":format!("rs-{i}"),
                "summary":[{"type":"summary_text","text":"bounded-summary"}]}
            })
            .to_string()
        })
        .collect::<Vec<_>>();
    lines.push(lines[0].clone());
    lines.push(
        serde_json::json!({"type":"response_item","payload":{"type":"reasoning",
        "id":"rs-over-budget","summary":[{"type":"summary_text","text":"retained-over-budget"}]}})
        .to_string(),
    );
    let (ordinary, preview) = codex_summary_preview(&lines.join("\n"));
    assert!(
        String::from_utf8(ordinary)
            .unwrap()
            .contains("retained-over-budget")
    );
    let records = preview["turns"][0]["projection"]["records"]
        .as_array()
        .unwrap();
    assert_eq!(
        records.iter().filter(|r| r["type"] == "reasoning").count(),
        4096,
        "preview must never recount a duplicate or exceed its identity budget"
    );
    assert!(preview["partial"].as_bool().unwrap());
}

#[test]
#[ignore = "operator-local Codex acceptance; never run in CI"]
fn codex_summary_large_associated_array_never_truncates_later_thinking() {
    let row = serde_json::json!({"type":"response_item","payload":{
        "type":"reasoning","id":"many-strings",
        "future":{"values":vec!["ordinary-value";33000]},
        "summary":[{"type":"summary_text","text":"CX15-AFTER-STRING-BUDGET"}]}});
    let (ordinary, _) = codex_summary_preview(&row.to_string());
    let retained: serde_json::Value =
        serde_json::from_slice(ordinary.split(|byte| *byte == b'\n').nth(2).unwrap()).unwrap();
    assert_eq!(retained, row);
}

const CX17_SESSION: &str = "codex__123e4567-e89b-12d3-a456-426614174031";

fn codex_finalize_json(out: std::process::Output) -> serde_json::Value {
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    serde_json::from_slice::<serde_json::Value>(&out.stdout).unwrap()["data"].clone()
}

fn codex_finalize_show(fixture: &LocalCodexSession) -> serde_json::Value {
    codex_finalize_json(fixture.run(&["agent", "session", "show", CX17_SESSION, "--json"]))
}

// Reconstruct retained pending-receipt/durable-checkpoint evidence for cold
// repair testing. This is a synthetic state, not a process-crash experiment.
async fn codex_finalize_pending_fixture() -> (LocalCodexSession, sea_orm::DatabaseConnection) {
    use libra::internal::workspace::{
        AcquireRequest, RepoIdentity, WorkspaceKind, WorkspaceState, WorkspaceStore,
    };
    use sea_orm::{ConnectionTrait, Statement};
    let fixture = LocalCodexSession::new();
    let db = fixture.db().await;
    let identity = RepoIdentity::resolve(&db).await.unwrap();
    WorkspaceStore::acquire_with_conn(
        &db,
        &identity,
        &AcquireRequest::task(
            WorkspaceKind::TaskCopy,
            &fixture.repo,
            "cx17-fixture-owner",
            300_000,
        )
        .with_initial_state(WorkspaceState::Active),
        chrono::Utc::now().timestamp_millis(),
    )
    .await
    .unwrap();
    assert!(
        fixture
            .cx01_hook("SessionStart", "session-start")
            .status
            .success()
    );
    let mut child = fixture.codex_terminal_spawn(2_000);
    fixture.codex_terminal_send(&mut child);
    let (out, _) = codex_terminal_wait(child);
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let db = fixture.db().await;
    let state = fixture.codex_terminal_state(&db).await;
    let mut metadata = state["metadata"].clone();
    metadata["cx17_unrelated"] = serde_json::json!({
        "source_path":fixture.source, "text":"CX17-SOURCE-MUST-NOT-RENDER"
    });
    let terminal = metadata["capture_catalog_receipts_v1"]["entries"]
        .as_array_mut()
        .unwrap()
        .iter_mut()
        .find(|r| r["intent"]["phase"] == "stopped")
        .unwrap();
    assert_eq!(terminal["status"], "complete");
    let revision = terminal["reserved_revision"].as_i64().unwrap();
    terminal["status"] = serde_json::json!("pending");
    db.execute_raw(Statement::from_sql_and_values(db.get_database_backend(),
        "UPDATE agent_session SET state='active', stopped_at=NULL, sync_revision=?, metadata_json=? WHERE session_id=?",
        [revision.into(), metadata.to_string().into(), CX17_SESSION.into()])).await.unwrap();
    assert_eq!(
        LocalCodexSession::count(&db, "agent_checkpoint", "1").await,
        1
    );
    (fixture, db)
}

async fn codex_finalize_catalog_snapshot(db: &sea_orm::DatabaseConnection) -> String {
    use sea_orm::ConnectionTrait;
    let extra: String = db.query_one_raw(sea_orm::Statement::from_string(db.get_database_backend(),
        "SELECT json_object('sessions',(SELECT json_group_array(json_object('id',session_id,'state',state,'stopped',stopped_at,'start',started_at,'last',last_event_at,'rev',sync_revision,'metadata',hex(metadata_json),'scope',scope_state,'repo',repo_id,'worktree',worktree_id,'workspace',workspace_id,'fence',workspace_fence)) FROM agent_session),'markers',(SELECT json_group_array(json_object('scope',scope,'target',target,'key',key,'value',hex(value))) FROM metadata_kv),'workspaces',(SELECT json_group_array(json_object('id',workspace_id,'owner',lease_owner,'fence',lease_fence,'expires',lease_expires_at,'state',state,'updated',updated_at)) FROM workspace_record),'config',(SELECT json_group_array(json_object('key',key,'value',hex(value),'encrypted',encrypted)) FROM config_kv)) AS snapshot"))
        .await.unwrap().unwrap().try_get_by("snapshot").unwrap();
    format!("{extra}\n{}", codex_legacy_reader_state(db).await)
}

#[tokio::test]
#[ignore = "operator-local Codex acceptance; never run in CI"]
async fn codex_finalize_session_diagnostics_are_read_only_and_content_free() {
    let fixture = LocalCodexSession::new();
    assert!(
        fixture
            .cx01_hook("SessionStart", "session-start")
            .status
            .success()
    );
    let db = fixture.db().await;
    let before = codex_finalize_catalog_snapshot(&db).await;
    let shown = codex_finalize_show(&fixture);
    assert_eq!(
        shown["terminal_capture"]["status"], "unavailable",
        "{shown}"
    );
    assert_eq!(before, codex_finalize_catalog_snapshot(&db).await);

    let mut child = fixture.codex_terminal_spawn(2_000);
    fixture.codex_terminal_send(&mut child);
    assert!(codex_terminal_wait(child).0.status.success());
    let before = codex_finalize_catalog_snapshot(&db).await;
    let shown = codex_finalize_show(&fixture);
    assert_eq!(shown["terminal_capture"]["status"], "completed", "{shown}");
    assert_eq!(shown["terminal_capture"]["completed_receipts"], 1);
    assert_eq!(shown["terminal_capture"]["pending_receipts"], 0);
    assert!(!shown.to_string().contains(fixture.source.to_str().unwrap()));
    assert_eq!(before, codex_finalize_catalog_snapshot(&db).await);
    assert!(
        fixture
            .run(&["agent", "session", "resume", CX17_SESSION])
            .status
            .success()
    );
    let before = codex_finalize_catalog_snapshot(&db).await;
    let resumed = codex_finalize_show(&fixture);
    assert_eq!(resumed["state"], "active");
    assert_eq!(resumed["terminal_capture"]["status"], "completed");
    assert_eq!(resumed["terminal_capture"]["superseded"], true);
    assert_eq!(before, codex_finalize_catalog_snapshot(&db).await);
    tokio::time::sleep(std::time::Duration::from_millis(1_100)).await;
    assert!(
        fixture
            .run(&["agent", "session", "stop", CX17_SESSION])
            .status
            .success()
    );
    let before = codex_finalize_catalog_snapshot(&db).await;
    let stopped = codex_finalize_show(&fixture);
    assert_eq!(stopped["state"], "stopped");
    assert_eq!(stopped["terminal_capture"]["superseded"], true);
    assert_eq!(before, codex_finalize_catalog_snapshot(&db).await);
}

#[tokio::test]
#[ignore = "operator-local Codex acceptance; never run in CI"]
async fn codex_finalize_live_owner_protects_a_day_old_active_session() {
    use libra::internal::ai::traces::{TracesInflightMarker, write_traces_inflight_marker};
    use sea_orm::{ConnectionTrait, Statement};
    let (fixture, db) = codex_finalize_pending_fixture().await;
    let state = fixture.codex_terminal_state(&db).await;
    let terminal = state["metadata"]["capture_catalog_receipts_v1"]["entries"]
        .as_array()
        .unwrap()
        .iter()
        .find(|r| r["intent"]["phase"] == "stopped")
        .unwrap();
    let checkpoint: String = db
        .query_one_raw(Statement::from_string(
            db.get_database_backend(),
            "SELECT checkpoint_id FROM agent_checkpoint",
        ))
        .await
        .unwrap()
        .unwrap()
        .try_get_by("checkpoint_id")
        .unwrap();
    let mut marker = TracesInflightMarker::new(
        CX17_SESSION,
        &checkpoint,
        chrono::Utc::now().timestamp_millis(),
    );
    marker.generation = Some(
        terminal["finalizer"]["marker_generation"]
            .as_str()
            .unwrap()
            .to_owned(),
    );
    write_traces_inflight_marker(&db, &marker).await.unwrap();
    db.execute_raw(Statement::from_sql_and_values(
        db.get_database_backend(),
        "UPDATE agent_session SET started_at=?,last_event_at=? WHERE session_id=?",
        [
            (chrono::Utc::now().timestamp() - 90_000).into(),
            (chrono::Utc::now().timestamp() - 90_000).into(),
            CX17_SESSION.into(),
        ],
    ))
    .await
    .unwrap();
    let before = codex_finalize_catalog_snapshot(&db).await;
    let shown = codex_finalize_show(&fixture);
    assert_eq!(shown["state"], "active");
    assert_eq!(shown["terminal_capture"]["status"], "pending", "{shown}");
    assert_eq!(shown["terminal_capture"]["owner"], "live", "{shown}");
    for repair in [false, true] {
        let args = if repair {
            vec!["agent", "doctor", "--repair", "--json"]
        } else {
            vec!["agent", "doctor", "--json"]
        };
        let report = codex_finalize_json(fixture.run(&args));
        assert_eq!(
            report["checkpoint_store"]["live_inflight_markers"], 1,
            "{report}"
        );
        assert_eq!(before, codex_finalize_catalog_snapshot(&db).await);
    }
    let human = fixture.run(&["agent", "session", "show", CX17_SESSION]);
    assert!(human.status.success());
    let human = String::from_utf8(human.stdout).unwrap();
    assert!(human.contains("terminal_owner: live"), "{human}");
    assert!(!human.contains("CX17-SOURCE-MUST-NOT-RENDER"));
    assert!(!human.contains(fixture.source.to_str().unwrap()));
}

#[tokio::test]
#[ignore = "operator-local Codex acceptance; never run in CI"]
async fn codex_finalize_explicit_repair_completes_the_original_receipt_once() {
    let (fixture, db) = codex_finalize_pending_fixture().await;
    let before = codex_finalize_catalog_snapshot(&db).await;
    let shown = codex_finalize_show(&fixture);
    assert_eq!(shown["terminal_capture"]["status"], "pending", "{shown}");
    assert_eq!(shown["terminal_capture"]["owner"], "absent");
    codex_finalize_json(fixture.run(&["agent", "doctor", "--json"]));
    assert_eq!(before, codex_finalize_catalog_snapshot(&db).await);
    let objects = codex_legacy_reader_state(&db).await;
    codex_finalize_json(fixture.run(&["agent", "doctor", "--repair", "--json"]));
    let shown = codex_finalize_show(&fixture);
    assert_eq!(shown["terminal_capture"]["status"], "completed", "{shown}");
    assert_eq!(shown["state"], "stopped");
    assert_eq!(objects, codex_legacy_reader_state(&db).await);
    assert_eq!(
        LocalCodexSession::count(&db, "agent_checkpoint", "1").await,
        1
    );
    let after = codex_finalize_catalog_snapshot(&db).await;
    codex_finalize_json(fixture.run(&["agent", "doctor", "--repair", "--json"]));
    assert_eq!(after, codex_finalize_catalog_snapshot(&db).await);
}

#[tokio::test]
#[ignore = "operator-local Codex acceptance; never run in CI"]
async fn codex_finalize_stale_workspace_scope_refuses_repair() {
    let (fixture, db) = codex_finalize_pending_fixture().await;
    // Revoke the real owner through its conditional store API, leaving the
    // session's immutable scope and the original receipt intact.
    codex_finalize_release_workspace(&db).await;
    let before = codex_finalize_catalog_snapshot(&db).await;
    let shown = codex_finalize_show(&fixture);
    assert_eq!(
        shown["terminal_capture"]["status"], "unavailable",
        "{shown}"
    );
    assert_eq!(shown["terminal_capture"]["reason"], "scope_unavailable");
    let output = fixture.run(&["agent", "doctor", "--repair", "--json"]);
    assert!(!String::from_utf8_lossy(&output.stderr).contains("CX17-SOURCE-MUST-NOT-RENDER"));
    let report = codex_finalize_json(output);
    assert!(
        report["checkpoint_store"]["findings"]
            .as_array()
            .unwrap()
            .iter()
            .any(|f| f["inconsistency_type"] == "expired_inflight_marker"
                && f["repaired"] == false
                && f["detail"]
                    .as_str()
                    .unwrap_or("")
                    .contains("different worktree/workspace scope")),
        "{report}"
    );
    assert_eq!(before, codex_finalize_catalog_snapshot(&db).await);
    assert_eq!(fixture.codex_terminal_state(&db).await["state"], "active");
}

#[tokio::test]
#[ignore = "operator-local Codex acceptance; never run in CI"]
async fn codex_finalize_invalid_or_oversized_metadata_is_unavailable() {
    use sea_orm::{ConnectionTrait, Statement};
    let (fixture, db) = codex_finalize_pending_fixture().await;
    for metadata in [
        r#"{"capture_catalog_receipts_v1":{},"text":"CX17-SOURCE-MUST-NOT-RENDER"}"#.to_owned(),
        serde_json::json!({"text":format!("CX17-SOURCE-MUST-NOT-RENDER{}", "x".repeat(1_048_576))}).to_string(),
        r#"{"capture_catalog_receipts_v1":null,"capture_catalog_receipts_v1":null,"text":"CX17-SOURCE-MUST-NOT-RENDER"}"#.to_owned(),
    ] {
        db.execute_raw(Statement::from_sql_and_values(db.get_database_backend(),
            "UPDATE agent_session SET metadata_json=? WHERE session_id=?",
            [metadata.into(), CX17_SESSION.into()])).await.unwrap();
        let before = codex_finalize_catalog_snapshot(&db).await;
        let shown = codex_finalize_show(&fixture);
        assert_eq!(shown["terminal_capture"]["status"], "unavailable", "{shown}");
        assert!(!shown.to_string().contains("CX17-SOURCE-MUST-NOT-RENDER"));
        assert_eq!(before, codex_finalize_catalog_snapshot(&db).await);
    }
    let invalid = [br#"{"text":""#.as_slice(), &[0xff], br#""}"#.as_slice()].concat();
    db.execute_raw(Statement::from_sql_and_values(
        db.get_database_backend(),
        "UPDATE agent_session SET metadata_json=CAST(? AS TEXT) WHERE session_id=?",
        [invalid.into(), CX17_SESSION.into()],
    ))
    .await
    .unwrap();
    let before = codex_finalize_catalog_snapshot(&db).await;
    let shown = codex_finalize_show(&fixture);
    assert_eq!(shown["terminal_capture"]["status"], "unavailable");
    assert_eq!(before, codex_finalize_catalog_snapshot(&db).await);
}

#[tokio::test]
#[ignore = "operator-local Codex acceptance; never run in CI"]
async fn codex_finalize_marker_states_are_diagnostic_without_state_mutation() {
    use libra::internal::ai::traces::{TracesInflightMarker, write_traces_inflight_marker};
    use sea_orm::{ConnectionTrait, Statement};
    for completed in [false, true] {
        let (fixture, db) = codex_finalize_pending_fixture().await;
        if completed {
            codex_finalize_json(fixture.run(&["agent", "doctor", "--repair", "--json"]));
        }
        let state = fixture.codex_terminal_state(&db).await;
        let terminal = state["metadata"]["capture_catalog_receipts_v1"]["entries"]
            .as_array()
            .unwrap()
            .iter()
            .find(|r| r["intent"]["phase"] == "stopped")
            .unwrap();
        let checkpoint: String = db
            .query_one_raw(Statement::from_string(
                db.get_database_backend(),
                "SELECT checkpoint_id FROM agent_checkpoint",
            ))
            .await
            .unwrap()
            .unwrap()
            .try_get_by("checkpoint_id")
            .unwrap();
        for (case, expected) in [
            ("expired", "expired"),
            ("cleanup", "cleanup_pending"),
            ("future", "unavailable"),
            ("other-generation", "unavailable"),
            ("malformed", "unavailable"),
            ("invalid-utf8", "unavailable"),
            ("oversized", "unavailable"),
        ] {
            let mut marker = TracesInflightMarker::new(
                CX17_SESSION,
                &checkpoint,
                chrono::Utc::now().timestamp_millis(),
            );
            marker.generation = Some(
                terminal["finalizer"]["marker_generation"]
                    .as_str()
                    .unwrap()
                    .to_owned(),
            );
            match case {
                "expired" => {
                    marker.started_at_ms = 0;
                    marker.ttl_ms = 1;
                }
                "cleanup" => {
                    marker.cleanup_pending = true;
                }
                "future" => {
                    marker.started_at_ms +=
                        libra::internal::ai::traces::TRACES_INFLIGHT_FUTURE_SKEW_MS + 60_000;
                }
                "other-generation" => {
                    marker.generation = Some(uuid::Uuid::new_v4().to_string());
                }
                _ => {}
            }
            // Replace this fixture's deliberately corrupt row between cases;
            // the normal metadata writer rejects an unreadable prior value.
            db.execute_raw(Statement::from_sql_and_values(db.get_database_backend(),
                "DELETE FROM metadata_kv WHERE scope='agent_traces_inflight' AND target=? AND key=?",
                [CX17_SESSION.into(), checkpoint.clone().into()])).await.unwrap();
            write_traces_inflight_marker(&db, &marker).await.unwrap();
            if case == "malformed" {
                db.execute_raw(Statement::from_sql_and_values(db.get_database_backend(),
                "UPDATE metadata_kv SET value=? WHERE scope='agent_traces_inflight' AND target=? AND key=?",
                [r#"{"private":"CX17-SOURCE-MUST-NOT-RENDER"}"#.into(), CX17_SESSION.into(),checkpoint.clone().into()])).await.unwrap();
            }

            if case == "invalid-utf8" {
                db.execute_raw(Statement::from_sql_and_values(db.get_database_backend(),
                "UPDATE metadata_kv SET value=CAST(? AS TEXT) WHERE scope='agent_traces_inflight' AND target=? AND key=?",
                [vec![0xff].into(), CX17_SESSION.into(), checkpoint.clone().into()])).await.unwrap();
            } else if case == "oversized" {
                db.execute_raw(Statement::from_sql_and_values(db.get_database_backend(),
                "UPDATE metadata_kv SET value=? WHERE scope='agent_traces_inflight' AND target=? AND key=?",
                ["x".repeat(1_048_577).into(), CX17_SESSION.into(),checkpoint.clone().into()])).await.unwrap();
            }
            let before = codex_finalize_catalog_snapshot(&db).await;
            let shown = codex_finalize_show(&fixture);
            assert_eq!(
                shown["terminal_capture"]["status"],
                if completed { "completed" } else { "pending" },
                "{case}: {shown}"
            );
            assert_eq!(
                shown["terminal_capture"]["owner"], expected,
                "{case}: {shown}"
            );
            assert!(!shown.to_string().contains("CX17-SOURCE-MUST-NOT-RENDER"));
            assert_eq!(before, codex_finalize_catalog_snapshot(&db).await, "{case}");
        }
    }
}

async fn codex_finalize_release_workspace(db: &sea_orm::DatabaseConnection) {
    use libra::internal::workspace::{WorkspaceLease, WorkspaceStore};
    use sea_orm::{ConnectionTrait, Statement};
    let owner = db.query_one_raw(Statement::from_string(db.get_database_backend(),
        "SELECT workspace_id,lease_owner,lease_fence,lease_expires_at FROM workspace_record WHERE state='active'"))
        .await.unwrap().unwrap();
    WorkspaceStore::release_with_conn(
        db,
        &WorkspaceLease {
            workspace_id: owner.try_get_by("workspace_id").unwrap(),
            owner: owner.try_get_by("lease_owner").unwrap(),
            fence: owner.try_get_by("lease_fence").unwrap(),
            expires_at: owner.try_get_by("lease_expires_at").unwrap(),
        },
        chrono::Utc::now().timestamp_millis(),
    )
    .await
    .unwrap();
}

#[tokio::test]
#[ignore = "operator-local Codex acceptance; never run in CI"]
async fn codex_finalize_completed_history_survives_workspace_release() {
    let (fixture, db) = codex_finalize_pending_fixture().await;
    codex_finalize_json(fixture.run(&["agent", "doctor", "--repair", "--json"]));
    assert_eq!(
        codex_finalize_show(&fixture)["terminal_capture"]["status"],
        "completed"
    );
    codex_finalize_release_workspace(&db).await;
    let before = codex_finalize_catalog_snapshot(&db).await;
    let shown = codex_finalize_show(&fixture);
    assert_eq!(shown["terminal_capture"]["status"], "completed", "{shown}");
    assert_eq!(shown["terminal_capture"]["completed_receipts"], 1);
    assert_eq!(shown["terminal_capture"]["pending_receipts"], 0);
    assert_eq!(before, codex_finalize_catalog_snapshot(&db).await);
}

#[tokio::test]
#[ignore = "operator-local Codex acceptance; never run in CI"]
async fn codex_finalize_legacy_completion_and_tied_receipts_preserve_historical_facts() {
    use sea_orm::{ConnectionTrait, Statement};
    let (fixture, db) = codex_finalize_pending_fixture().await;
    codex_finalize_json(fixture.run(&["agent", "doctor", "--repair", "--json"]));
    let mut metadata = fixture.codex_terminal_state(&db).await["metadata"].clone();
    let entries = metadata["capture_catalog_receipts_v1"]["entries"]
        .as_array_mut()
        .unwrap();
    let complete = entries
        .iter_mut()
        .find(|r| r["intent"]["phase"] == "stopped")
        .unwrap();
    complete.as_object_mut().unwrap().remove("finalizer");
    let legacy = complete.clone();
    // Synthetic pre-ACF-07 rows exercise the retained v1 reader contract.
    // These fields confer no live source or checkpoint publication authority.
    for mode in ["legacy", "complete-tie", "mixed-tie"] {
        let mut candidate = metadata.clone();
        if mode != "legacy" {
            let mut other = legacy.clone();
            let event = uuid::Uuid::new_v4();
            other["event_id"] = serde_json::json!(event.to_string());
            other["action_key"] = serde_json::json!(format!("capture-lifecycle-v1:{event}"));
            other["receipt_key"] = serde_json::json!(format!("capture-action-v1:{event}"));
            if mode == "mixed-tie" {
                other["status"] = serde_json::json!("pending");
            }
            candidate["capture_catalog_receipts_v1"]["entries"]
                .as_array_mut()
                .unwrap()
                .push(other);
        }
        db.execute_raw(Statement::from_sql_and_values(
            db.get_database_backend(),
            "UPDATE agent_session SET metadata_json=? WHERE session_id=?",
            [candidate.to_string().into(), CX17_SESSION.into()],
        ))
        .await
        .unwrap();
        let before = codex_finalize_catalog_snapshot(&db).await;
        let shown = codex_finalize_show(&fixture);
        assert_eq!(
            shown["terminal_capture"]["status"],
            if mode == "mixed-tie" {
                "unavailable"
            } else {
                "completed"
            },
            "{mode}: {shown}"
        );
        assert_eq!(
            shown["terminal_capture"]["owner"], "unavailable",
            "{mode}: {shown}"
        );
        assert_eq!(
            shown["terminal_capture"]["reason"],
            if mode == "legacy" {
                "legacy_finalizer_unavailable"
            } else {
                "ambiguous_terminal_receipts"
            }
        );
        assert_eq!(before, codex_finalize_catalog_snapshot(&db).await);
    }
}

#[tokio::test]
#[ignore = "operator-local Codex acceptance; never run in CI"]
async fn codex_archive_batch_prefers_active_and_deduplicates() {
    let fixture = LocalCodexSession::new();
    fixture.source(1, true);
    let archive = fixture.home.join(".codex/archived_sessions");
    std::fs::create_dir_all(&archive).unwrap();
    let archive_file = archive.join(fixture.source.file_name().unwrap());
    let archived = std::fs::read_to_string(&fixture.source)
        .unwrap()
        .replace("CX15-MULTI-0", "CX10-ARCHIVED-COPY");
    std::fs::write(archive_file, archived).unwrap();

    let output = fixture.run(&[
        "agent", "import", "--all", "--agent", "codex", "--yes", "--json",
    ]);
    assert!(output.status.success(), "{output:?}");
    let result: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(result["data"]["results"].as_array().map(Vec::len), Some(1));
    let db = fixture.db().await;
    let exported = fixture.exports(&db).await.join("\n");
    assert!(exported.contains("CX15-MULTI-0"));
    assert!(!exported.contains("CX10-ARCHIVED-COPY"));
    db.close().await.unwrap();
}

#[test]
#[ignore = "operator-local Codex acceptance; never run in CI"]
fn codex_archive_since_filters_only_winning_active_copy() {
    for broken_copy in ["active", "archive"] {
        let fixture = LocalCodexSession::new();
        fixture.source(1, true);
        let archive = fixture.home.join(".codex/archived_sessions");
        std::fs::create_dir_all(&archive).unwrap();
        let archive_file = archive.join(fixture.source.file_name().unwrap());
        std::fs::copy(&fixture.source, &archive_file).unwrap();
        let broken_path = if broken_copy == "active" {
            &fixture.source
        } else {
            &archive_file
        };
        let source = std::fs::read_to_string(broken_path).unwrap();
        let (_, rest) = source.split_once('\n').unwrap();
        let bad_header = serde_json::json!({
            "type":"session_meta", "timestamp":"2026-10-19T01:00:00Z",
            "payload":{"id":"wrong-id"}
        });
        std::fs::write(broken_path, format!("{bad_header}\n{rest}")).unwrap();
        let output = fixture.run(&[
            "agent",
            "import",
            "--since",
            "2026-10-18T00:00:00Z",
            "--agent",
            "codex",
            "--yes",
            "--json",
        ]);
        if broken_copy == "active" {
            assert!(output.status.success(), "{output:?}");
            let payload: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
            assert_eq!(payload["data"]["results"].as_array().map(Vec::len), Some(0));
            assert_eq!(
                payload["data"]["skipped"][0]["reason_key"],
                "codex_session_header_invalid"
            );
        } else {
            let stderr = String::from_utf8_lossy(&output.stderr);
            assert!(!output.status.success(), "{output:?}");
            assert!(!stderr.contains("codex_session_header_invalid"), "{stderr}");
            let json_start = stderr.find("\n{").expect("structured partial error") + 1;
            let payload: serde_json::Value = serde_json::from_str(&stderr[json_start..]).unwrap();
            assert_eq!(payload["error_code"], "LBR-AGENT-018");
            assert_eq!(payload["details"]["skipped"], serde_json::json!([]));
            assert_eq!(
                payload["details"]["partial_results"]
                    .as_array()
                    .map(Vec::len),
                Some(1)
            );
            assert_eq!(
                payload["details"]["partial_results"][0]["checkpoints_written"],
                1
            );
            assert_eq!(
                payload["details"]["partial_results"][0]["session_id"],
                "codex__123e4567-e89b-12d3-a456-426614174031"
            );
        }
    }
}

#[cfg(unix)]
#[test]
#[ignore = "operator-local Codex acceptance; never run in CI"]
fn codex_archive_has_an_independent_entry_budget() {
    let fixture = LocalCodexSession::new();
    fixture.source(1, true);
    let archive = fixture.home.join(".codex/archived_sessions");
    std::fs::create_dir_all(&archive).unwrap();
    std::fs::rename(
        &fixture.source,
        archive.join(fixture.source.file_name().unwrap()),
    )
    .unwrap();
    let active_day = fixture.source.parent().unwrap();
    // sessions/year/month/day cost three entries; the final day reaches the
    // active 20,000-entry allowance exactly before the archive is examined.
    for index in 0..19_997 {
        std::fs::File::create(active_day.join(format!("ignored-{index:05}"))).unwrap();
    }
    let output = fixture.run(&[
        "agent", "import", "--all", "--agent", "codex", "--yes", "--json",
    ]);
    assert!(!output.status.success(), "{output:?}");
    let stderr = String::from_utf8_lossy(&output.stderr);
    let json_start = stderr.find("\n{").expect("structured partial error") + 1;
    let payload: serde_json::Value = serde_json::from_str(&stderr[json_start..]).unwrap();
    assert_eq!(payload["details"]["skipped"], serde_json::json!([]));
    assert_eq!(
        payload["details"]["partial_results"][0]["checkpoints_written"],
        1
    );
}

#[tokio::test]
#[ignore = "operator-local Codex acceptance; never run in CI"]
async fn codex_archive_since_uses_session_meta_and_imports_archived_rollout() {
    use sea_orm::{ConnectionTrait, Statement};

    let fixture = LocalCodexSession::new();
    fixture.source(1, true);
    let archive = fixture.home.join(".codex/archived_sessions");
    std::fs::create_dir_all(&archive).unwrap();
    let archived_path = archive.join(fixture.source.file_name().unwrap());
    std::fs::rename(&fixture.source, &archived_path).unwrap();
    let source = std::fs::read_to_string(&archived_path).unwrap();
    let mut lines = source.lines().map(str::to_owned).collect::<Vec<_>>();
    let mut header: serde_json::Value = serde_json::from_str(&lines[0]).unwrap();
    header["payload"]["model"] = serde_json::json!("CX10-MODEL-CANARY");
    header["payload"]["title"] = serde_json::json!("CX10-TITLE-CANARY");
    lines[0] = header.to_string();
    std::fs::write(&archived_path, format!("{}\n", lines.join("\n"))).unwrap();

    // The archive file was just moved and has a current mtime, but its
    // session_meta predates this boundary.
    let excluded = fixture.run(&[
        "agent",
        "import",
        "--since",
        "2026-10-20T00:00:00Z",
        "--agent",
        "codex",
        "--yes",
        "--json",
    ]);
    assert!(excluded.status.success(), "{excluded:?}");
    let result: serde_json::Value = serde_json::from_slice(&excluded.stdout).unwrap();
    assert_eq!(result["data"]["results"].as_array().map(Vec::len), Some(0));

    let included = fixture.run(&[
        "agent",
        "import",
        "--since",
        "2026-10-18T00:00:00Z",
        "--agent",
        "codex",
        "--yes",
        "--json",
    ]);
    assert!(included.status.success(), "{included:?}");
    let result: serde_json::Value = serde_json::from_slice(&included.stdout).unwrap();
    assert_eq!(result["data"]["results"].as_array().map(Vec::len), Some(1));
    let db = fixture.db().await;
    let exported = fixture.exports(&db).await.join("\n");
    assert!(exported.contains("CX15-MULTI-0"));
    assert!(exported.contains("opaque-0"));
    let row = db
        .query_one_raw(Statement::from_string(
            db.get_database_backend(),
            "SELECT metadata_json FROM agent_session WHERE agent_kind='codex'",
        ))
        .await
        .unwrap()
        .unwrap();
    let metadata: String = row.try_get_by("metadata_json").unwrap();
    for secret in ["CX10-MODEL-CANARY", "CX10-TITLE-CANARY", "CX15-MULTI-0"] {
        assert!(!metadata.contains(secret), "metadata exposed {secret}");
    }
    db.close().await.unwrap();
}

#[tokio::test]
#[ignore = "operator-local Codex acceptance; never run in CI"]
async fn codex_archive_session_selector_finds_archived_rollout() {
    let fixture = LocalCodexSession::new();
    fixture.source(1, true);
    let archive = fixture.home.join(".codex/archived_sessions");
    std::fs::create_dir_all(&archive).unwrap();
    std::fs::rename(
        &fixture.source,
        archive.join(fixture.source.file_name().unwrap()),
    )
    .unwrap();
    std::fs::remove_dir_all(fixture.home.join(".codex/sessions")).unwrap();
    let output = fixture.run(&[
        "agent",
        "import",
        "--session",
        "123e4567-e89b-12d3-a456-426614174031",
        "--agent",
        "codex",
        "--yes",
        "--json",
    ]);
    assert!(output.status.success(), "{output:?}");
    let result: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(result["data"]["results"].as_array().map(Vec::len), Some(1));
    let db = fixture.db().await;
    assert!(
        fixture
            .exports(&db)
            .await
            .join("\n")
            .contains("CX15-MULTI-0")
    );
    db.close().await.unwrap();
}

#[test]
#[ignore = "operator-local Codex acceptance; never run in CI"]
fn codex_archive_since_reports_unknown_session_time() {
    let fixture = LocalCodexSession::new();
    fixture.source(1, true);
    let archive = fixture.home.join(".codex/archived_sessions");
    std::fs::create_dir_all(&archive).unwrap();
    let path = archive.join(fixture.source.file_name().unwrap());
    let source = std::fs::read_to_string(&fixture.source).unwrap();
    let mut lines = source.lines().map(str::to_owned).collect::<Vec<_>>();
    let mut header: serde_json::Value = serde_json::from_str(&lines[0]).unwrap();
    header.as_object_mut().unwrap().remove("timestamp");
    header["payload"]
        .as_object_mut()
        .unwrap()
        .remove("timestamp");
    lines[0] = header.to_string();
    std::fs::write(&path, format!("{}\n", lines.join("\n"))).unwrap();
    std::fs::remove_file(&fixture.source).unwrap();
    let original_id = "123e4567-e89b-12d3-a456-426614174031";
    for suffix in ["032", "033"] {
        let id = format!("123e4567-e89b-12d3-a456-426614174{suffix}");
        let name = fixture
            .source
            .file_name()
            .unwrap()
            .to_string_lossy()
            .replace(original_id, &id);
        std::fs::write(
            fixture.source.with_file_name(name),
            source.replace(original_id, &id),
        )
        .unwrap();
    }

    let denied = fixture.run(&[
        "agent",
        "import",
        "--since",
        "2026-10-18T00:00:00Z",
        "--agent",
        "codex",
        "--json",
    ]);
    assert!(!denied.status.success(), "{denied:?}");
    let denied_stderr = String::from_utf8_lossy(&denied.stderr);
    assert!(denied_stderr.contains("--yes"), "{denied_stderr}");
    assert!(!denied_stderr.contains("session_meta"), "{denied_stderr}");

    let output = fixture.run(&[
        "agent",
        "import",
        "--since",
        "2026-10-18T00:00:00Z",
        "--agent",
        "codex",
        "--limit",
        "1",
        "--yes",
        "--json",
    ]);
    assert!(output.status.success(), "{output:?}");
    let payload: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(
        payload["data"]["skipped"][0]["reason_key"],
        "codex_session_time_unknown"
    );
    assert!(
        payload["data"]["skipped"][0]["session_id"]
            .as_str()
            .unwrap()
            .starts_with("sha256:")
    );
    assert_eq!(payload["data"]["next_cursor"], 1);
    let next = fixture.run(&[
        "agent",
        "import",
        "--since",
        "2026-10-18T00:00:00Z",
        "--agent",
        "codex",
        "--limit",
        "1",
        "--cursor",
        "1",
        "--yes",
        "--json",
    ]);
    assert!(next.status.success(), "{next:?}");
    let page: serde_json::Value = serde_json::from_slice(&next.stdout).unwrap();
    assert_eq!(page["data"]["skipped"], payload["data"]["skipped"]);
    let warning_exit = fixture.run(&[
        "--exit-code-on-warning",
        "agent",
        "import",
        "--since",
        "2026-10-18T00:00:00Z",
        "--agent",
        "codex",
        "--limit",
        "1",
        "--yes",
        "--json",
    ]);
    assert_eq!(warning_exit.status.code(), Some(9), "{warning_exit:?}");
    let warned: serde_json::Value = serde_json::from_slice(&warning_exit.stdout).unwrap();
    assert!(
        warned["data"]["warnings"]
            .as_array()
            .is_some_and(|items| !items.is_empty()),
        "{warned}"
    );
}

#[cfg(unix)]
#[test]
#[ignore = "operator-local Codex acceptance; never run in CI"]
fn codex_archive_symlink_is_rejected_without_reading_target() {
    let fixture = LocalCodexSession::new();
    fixture.source(1, true);
    let archive = fixture.home.join(".codex/archived_sessions");
    std::fs::create_dir_all(&archive).unwrap();
    let outside = fixture.home.join("outside-rollout.jsonl");
    std::fs::copy(&fixture.source, &outside).unwrap();
    std::os::unix::fs::symlink(&outside, archive.join(fixture.source.file_name().unwrap()))
        .unwrap();
    std::fs::remove_file(&fixture.source).unwrap();

    let output = fixture.run(&[
        "agent", "import", "--all", "--agent", "codex", "--yes", "--json",
    ]);
    assert!(!output.status.success(), "{output:?}");
    assert!(
        String::from_utf8_lossy(&output.stderr).contains("LBR-AGENT-020"),
        "{output:?}"
    );
    assert!(outside.exists());
}

#[test]
#[ignore = "operator-local Codex acceptance; never run in CI"]
fn codex_archive_header_rejects_mismatch_duplicate_and_oversize() {
    const ID: &str = "123e4567-e89b-12d3-a456-426614174031";
    for shape in ["mismatch", "duplicate", "oversize"] {
        let fixture = LocalCodexSession::new();
        fixture.source(1, true);
        let archive = fixture.home.join(".codex/archived_sessions");
        std::fs::create_dir_all(&archive).unwrap();
        let path = archive.join(fixture.source.file_name().unwrap());
        let source = std::fs::read_to_string(&fixture.source).unwrap();
        let mut lines = source.lines().map(str::to_owned).collect::<Vec<_>>();
        lines[0] = match shape {
            "mismatch" => serde_json::json!({
                "type":"session_meta", "timestamp":"2026-10-19T01:00:00Z",
                "payload":{"id":"123e4567-e89b-12d3-a456-426614174099"}
            })
            .to_string(),
            "duplicate" => format!(
                "{{\"type\":\"session_meta\",\"timestamp\":\"2026-10-19T01:00:00Z\",\"timestamp\":\"2026-10-19T02:00:00Z\",\"payload\":{{\"id\":\"{ID}\"}}}}"
            ),
            "oversize" => serde_json::json!({
                "type":"session_meta", "timestamp":"2026-10-19T01:00:00Z",
                "payload":{"id":ID,"title":"x".repeat(1_048_576)}
            })
            .to_string(),
            _ => unreachable!(),
        };
        std::fs::write(&path, format!("{}\n", lines.join("\n"))).unwrap();
        std::fs::remove_file(&fixture.source).unwrap();
        let output = fixture.run(&[
            "agent",
            "import",
            "--since",
            "2026-10-18T00:00:00Z",
            "--agent",
            "codex",
            "--yes",
            "--json",
        ]);
        assert!(output.status.success(), "{shape}: {output:?}");
        let payload: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
        assert_eq!(
            payload["data"]["skipped"][0]["reason_key"], "codex_session_header_invalid",
            "{shape}: {payload}"
        );
        assert!(path.exists(), "discovery must not mutate the source");
    }
}

#[test]
#[ignore = "operator-local Codex acceptance; never run in CI"]
fn codex_archive_since_scans_large_header_and_reports_bad_peer() {
    const ID: &str = "123e4567-e89b-12d3-a456-426614174031";
    const BAD_ID: &str = "123e4567-e89b-12d3-a456-426614174032";
    let fixture = LocalCodexSession::new();
    fixture.source(1, true);
    let source = std::fs::read_to_string(&fixture.source).unwrap();
    let mut lines = source.lines().map(str::to_owned).collect::<Vec<_>>();
    let mut header: serde_json::Value = serde_json::from_str(&lines[0]).unwrap();
    header["payload"]["base_instructions"] = serde_json::json!("x".repeat(80 * 1024));
    lines[0] = header.to_string();
    std::fs::write(&fixture.source, format!("{}\n", lines.join("\n"))).unwrap();

    let archive = fixture.home.join(".codex/archived_sessions");
    std::fs::create_dir_all(&archive).unwrap();
    let bad_name = fixture
        .source
        .file_name()
        .unwrap()
        .to_string_lossy()
        .replace(ID, BAD_ID);
    let bad_header = serde_json::json!({
        "type":"session_meta", "timestamp":"2026-10-19T01:00:00Z",
        "payload":{"id":"wrong-id"}
    });
    std::fs::write(archive.join(bad_name), format!("{bad_header}\n")).unwrap();

    let output = fixture.run(&[
        "agent",
        "import",
        "--since",
        "2026-10-18T00:00:00Z",
        "--agent",
        "codex",
        "--yes",
        "--json",
    ]);
    assert!(!output.status.success(), "{output:?}");
    let error: serde_json::Value = serde_json::from_slice(
        &output.stderr[String::from_utf8_lossy(&output.stderr).find('{').unwrap()..],
    )
    .unwrap();
    assert_eq!(error["details"]["partial"], 1, "{error}");
    assert_eq!(
        error["details"]["partial_results"][0]["checkpoints_written"], 1,
        "{error}"
    );
    assert!(
        error["details"]["skipped"]
            .as_array()
            .unwrap()
            .iter()
            .any(|skip| skip["reason_key"] == "codex_session_header_invalid"),
        "{error}"
    );
}

#[tokio::test]
#[ignore = "operator-local Codex acceptance; never run in CI"]
async fn codex_archive_since_skips_erased_bad_header() {
    use sea_orm::{ConnectionTrait, Statement};

    let fixture = LocalCodexSession::new();
    fixture.source(1, true);
    let bad_header = serde_json::json!({
        "type":"session_meta", "timestamp":"2026-10-19T01:00:00Z",
        "payload":{"id":"wrong-id"}
    });
    std::fs::write(&fixture.source, format!("{bad_header}\n")).unwrap();
    let db = fixture.db().await;
    db.execute_raw(Statement::from_sql_and_values(
        db.get_database_backend(),
        "INSERT INTO agent_import_tombstone (tombstone_id, agent_kind, provider_session_id, erased_session_id, erased_at) VALUES (?, 'codex', ?, ?, 1)",
        [
            "cx10-erased".into(),
            "123e4567-e89b-12d3-a456-426614174031".into(),
            "codex__123e4567-e89b-12d3-a456-426614174031".into(),
        ],
    ))
    .await
    .unwrap();
    db.close().await.unwrap();
    let output = fixture.run(&[
        "agent",
        "import",
        "--since",
        "2026-10-18T00:00:00Z",
        "--agent",
        "codex",
        "--yes",
        "--json",
    ]);
    assert!(output.status.success(), "{output:?}");
    let payload: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(payload["data"]["skipped"].as_array().map(Vec::len), Some(1));
    assert_eq!(
        payload["data"]["failures"].as_array().map(Vec::len),
        Some(0)
    );
}

struct LocalCodexSession {
    _temp: tempfile::TempDir,
    repo: std::path::PathBuf,
    home: std::path::PathBuf,
    source: std::path::PathBuf,
}

struct CodexTerminalChild {
    child: Option<std::process::Child>,
    started: std::time::Instant,
}

impl std::ops::Deref for CodexTerminalChild {
    type Target = std::process::Child;
    fn deref(&self) -> &Self::Target {
        self.child.as_ref().unwrap()
    }
}
impl std::ops::DerefMut for CodexTerminalChild {
    fn deref_mut(&mut self) -> &mut Self::Target {
        self.child.as_mut().unwrap()
    }
}
impl CodexTerminalChild {
    fn into_child(mut self) -> std::process::Child {
        self.child.take().unwrap()
    }
}
impl Drop for CodexTerminalChild {
    fn drop(&mut self) {
        if let Some(mut child) = self.child.take() {
            let _ = child.kill();
            let _ = child.wait();
        }
    }
}

impl LocalCodexSession {
    fn codex_terminal_spawn(&self, budget_ms: u64) -> CodexTerminalChild {
        let started = std::time::Instant::now();
        CodexTerminalChild {
            child: Some(
                std::process::Command::new(env!("CARGO_BIN_EXE_libra"))
                    .args([
                        "hooks",
                        "codex",
                        "session-end",
                        "--capture-budget-ms",
                        &budget_ms.to_string(),
                    ])
                    .current_dir(&self.repo)
                    .env_clear()
                    .env("PATH", "/usr/bin:/bin")
                    .env("HOME", &self.home)
                    .env("USERPROFILE", &self.home)
                    .env("LIBRA_TEST_HOME", &self.home)
                    .env("LIBRA_TEST", "1")
                    .stdin(std::process::Stdio::piped())
                    .stdout(std::process::Stdio::piped())
                    .stderr(std::process::Stdio::piped())
                    .spawn()
                    .unwrap(),
            ),
            started,
        }
    }

    fn codex_terminal_send(&self, child: &mut CodexTerminalChild) {
        use std::io::Write;
        let payload = serde_json::json!({
            "hook_event_name":"SessionEnd", "session_id":"123e4567-e89b-12d3-a456-426614174031",
            "cwd":self.repo, "turn_id":"cx01-turn", "transcript_path":self.source,
            "model":"CX02-NO-LOG-PAYLOAD", "permission_mode":"default"
        });
        child
            .stdin
            .take()
            .unwrap()
            .write_all(payload.to_string().as_bytes())
            .unwrap();
    }

    async fn codex_terminal_state(&self, db: &sea_orm::DatabaseConnection) -> serde_json::Value {
        use sea_orm::ConnectionTrait;
        let row = db.query_one_raw(sea_orm::Statement::from_string(db.get_database_backend(),
            "SELECT state, stopped_at, sync_revision, metadata_json FROM agent_session WHERE agent_kind = 'codex'"))
            .await.unwrap().unwrap();
        serde_json::json!({
            "state": row.try_get_by::<String,_>("state").unwrap(),
            "stopped_at": row.try_get_by::<Option<i64>,_>("stopped_at").unwrap(),
            "revision": row.try_get_by::<i64,_>("sync_revision").unwrap(),
            "metadata": serde_json::from_str::<serde_json::Value>(&row.try_get_by::<String,_>("metadata_json").unwrap()).unwrap()
        })
    }
}

fn codex_terminal_wait(
    mut child: CodexTerminalChild,
) -> (std::process::Output, std::time::Duration) {
    let start = child.started;
    loop {
        if child.try_wait().unwrap().is_some() {
            return (
                child.into_child().wait_with_output().unwrap(),
                start.elapsed(),
            );
        }
        if start.elapsed() > std::time::Duration::from_secs(4) {
            child.kill().unwrap();
            let out = child.into_child().wait_with_output().unwrap();
            panic!(
                "Codex terminal exceeded its 3s host window (4s test reap guard): {}",
                String::from_utf8_lossy(&out.stderr)
            );
        }
        std::thread::sleep(std::time::Duration::from_millis(10));
    }
}

#[tokio::test]
#[ignore = "operator-local Codex acceptance; never run in CI"]
async fn codex_terminal_success_and_native_replay_close_one_checkpoint_atomically() {
    let fixture = LocalCodexSession::new();
    assert!(
        fixture
            .cx01_hook("SessionStart", "session-start")
            .status
            .success()
    );
    let db = fixture.db().await;
    assert_eq!(fixture.codex_terminal_state(&db).await["state"], "active");
    let mut completed = None;
    for attempt in 0..2 {
        let mut child = fixture.codex_terminal_spawn(2_000);
        fixture.codex_terminal_send(&mut child);
        let (out, elapsed) = codex_terminal_wait(child);
        assert!(
            out.status.success(),
            "attempt {attempt}: {}",
            String::from_utf8_lossy(&out.stderr)
        );
        assert!(elapsed < std::time::Duration::from_secs(3), "{elapsed:?}");
        assert!(out.stdout.is_empty());
        assert!(!String::from_utf8_lossy(&out.stderr).contains("CX02-NO-LOG-PAYLOAD"));
        let state = fixture.codex_terminal_state(&db).await;
        assert_eq!(state["state"], "stopped");
        assert!(state["stopped_at"].is_i64());
        let receipts = state["metadata"]["capture_catalog_receipts_v1"]["entries"]
            .as_array()
            .unwrap();
        let terminal = receipts
            .iter()
            .filter(|entry| entry["intent"]["phase"] == "stopped")
            .collect::<Vec<_>>();
        assert_eq!(
            terminal.len(),
            1,
            "native replay must not create another terminal action: {state}"
        );
        assert_eq!(terminal[0]["status"], "complete", "{state}");
        // StoredFinalizeStatus is recovery evidence (pending/quarantined).
        // The receipt plus its durable checkpoint own completion.
        assert_eq!(terminal[0]["finalizer"]["status"], "pending", "{state}");
        assert_eq!(
            LocalCodexSession::count(&db, "agent_checkpoint", "1").await,
            1
        );
        assert_eq!(
            LocalCodexSession::count(&db, "agent_coverage_claim", "1").await,
            0,
            "no Codex live source capability has been activated by this test"
        );
        let objects = codex_legacy_reader_state(&db).await;
        if let Some((previous_state, previous_objects)) = &completed {
            assert_eq!(
                &state, previous_state,
                "native replay must not change the completed lifecycle or its receipt"
            );
            assert_eq!(
                &objects, previous_objects,
                "native replay must not rewrite checkpoint, claim or traces ref"
            );
        }
        completed = Some((state, objects));
    }
}

#[tokio::test]
#[ignore = "operator-local Codex acceptance; never run in CI"]
async fn codex_terminal_busy_first_write_is_bounded_and_does_not_publish_stopped() {
    use sea_orm::{ConnectionTrait, TransactionTrait};
    let fixture = LocalCodexSession::new();
    assert!(
        fixture
            .cx01_hook("SessionStart", "session-start")
            .status
            .success()
    );
    let db = fixture.db().await;
    let before = fixture.codex_terminal_state(&db).await;
    let held = db.begin().await.unwrap();
    held.execute_unprepared("UPDATE agent_session SET state = state")
        .await
        .unwrap();
    let mut child = fixture.codex_terminal_spawn(2_000);
    fixture.codex_terminal_send(&mut child);
    let (out, elapsed) = codex_terminal_wait(child);
    assert!(
        !out.status.success(),
        "busy first write must not acknowledge a terminal transition"
    );
    assert!(elapsed < std::time::Duration::from_secs(3), "{elapsed:?}");
    assert!(out.stdout.is_empty());
    assert!(!String::from_utf8_lossy(&out.stderr).contains("CX02-NO-LOG-PAYLOAD"));
    held.rollback().await.unwrap();
    assert_eq!(fixture.codex_terminal_state(&db).await, before);
    assert_eq!(
        LocalCodexSession::count(&db, "agent_checkpoint", "1").await,
        0
    );
    let mut retry = fixture.codex_terminal_spawn(2_000);
    fixture.codex_terminal_send(&mut retry);
    let (out, elapsed) = codex_terminal_wait(retry);
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(elapsed < std::time::Duration::from_secs(3));
    assert_eq!(fixture.codex_terminal_state(&db).await["state"], "stopped");
    assert_eq!(
        LocalCodexSession::count(&db, "agent_checkpoint", "1").await,
        1
    );
}

#[tokio::test]
#[ignore = "operator-local Codex acceptance; never run in CI"]
async fn codex_terminal_stdin_wait_consumes_budget_without_creating_false_receipts() {
    let fixture = LocalCodexSession::new();
    assert!(
        fixture
            .cx01_hook("SessionStart", "session-start")
            .status
            .success()
    );
    let db = fixture.db().await;
    let before = fixture.codex_terminal_state(&db).await;
    let mut child = fixture.codex_terminal_spawn(2_000);
    let held_stdin = child.stdin.take().unwrap();
    let (out, elapsed) = codex_terminal_wait(child);
    drop(held_stdin);
    assert!(
        out.status.success(),
        "without a trusted session frame the hook keeps its advisory no-evidence contract"
    );
    assert!(elapsed < std::time::Duration::from_secs(3), "{elapsed:?}");
    assert!(
        elapsed >= std::time::Duration::from_millis(1_500),
        "test must actually wait for the 2s deadline, not fail at setup: {elapsed:?}"
    );
    assert!(out.stdout.is_empty());
    assert_eq!(fixture.codex_terminal_state(&db).await, before);
    assert_eq!(
        LocalCodexSession::count(&db, "agent_checkpoint", "1").await,
        0
    );
}

#[tokio::test]
#[ignore = "operator-local Codex acceptance; never run in CI"]
async fn codex_terminal_host_kill_before_ingress_preserves_state_and_allows_redelivery() {
    let fixture = LocalCodexSession::new();
    assert!(
        fixture
            .cx01_hook("SessionStart", "session-start")
            .status
            .success()
    );
    let db = fixture.db().await;
    let before = fixture.codex_terminal_state(&db).await;
    let mut child = fixture.codex_terminal_spawn(2_000);
    let held_stdin = child.stdin.take().unwrap();
    std::thread::sleep(std::time::Duration::from_millis(100));
    assert!(child.try_wait().unwrap().is_none());
    child.kill().unwrap();
    child.wait().unwrap();
    drop(held_stdin);
    assert_eq!(fixture.codex_terminal_state(&db).await, before);
    assert_eq!(
        LocalCodexSession::count(&db, "agent_checkpoint", "1").await,
        0
    );
    let mut retry = fixture.codex_terminal_spawn(2_000);
    fixture.codex_terminal_send(&mut retry);
    let (out, _) = codex_terminal_wait(retry);
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert_eq!(fixture.codex_terminal_state(&db).await["state"], "stopped");
    assert_eq!(
        LocalCodexSession::count(&db, "agent_checkpoint", "1").await,
        1
    );
}

#[tokio::test]
#[ignore = "operator-local Codex acceptance; never run in CI"]
async fn codex_terminal_unobserved_host_kill_keeps_rollout_recoverable_by_explicit_import() {
    let fixture = LocalCodexSession::new();
    fixture.source(1, true);
    let original_source = std::fs::read(&fixture.source).unwrap();
    assert!(
        fixture
            .cx01_hook("SessionStart", "session-start")
            .status
            .success()
    );
    let db = fixture.db().await;
    let before = fixture.codex_terminal_state(&db).await;
    let mut child = fixture.codex_terminal_spawn(2_000);
    let held_stdin = child.stdin.take().unwrap();
    std::thread::sleep(std::time::Duration::from_millis(100));
    assert!(child.try_wait().unwrap().is_none());
    child.kill().unwrap();
    child.wait().unwrap();
    drop(held_stdin);
    assert_eq!(fixture.codex_terminal_state(&db).await, before);
    assert_eq!(
        LocalCodexSession::count(&db, "agent_checkpoint", "1").await,
        0
    );
    let imported = fixture.import();
    assert!(
        imported.status.success(),
        "{}",
        String::from_utf8_lossy(&imported.stderr)
    );
    assert_eq!(
        LocalCodexSession::count(&db, "agent_checkpoint", "1").await,
        1
    );
    assert_eq!(
        LocalCodexSession::count(&db, "agent_session", "agent_kind = 'codex'").await,
        1
    );
    let exports = fixture.exports(&db).await;
    assert!(
        exports
            .iter()
            .any(|text| text.contains("CX15-MULTI-0") && text.contains("opaque-0")),
        "an unseen terminal callback must not prevent recovery of available summary text and opaque data"
    );
    assert_eq!(std::fs::read(&fixture.source).unwrap(), original_source);
    let recovered = fixture.codex_terminal_state(&db).await;
    assert!(
        recovered["metadata"]["capture_catalog_receipts_v1"]["entries"]
            .as_array()
            .unwrap()
            .iter()
            .all(|entry| entry["intent"]["phase"] != "stopped"),
        "import must not invent the native terminal receipt that was never observed"
    );
}

// An old primary checkpoint must exercise the reader, rather than the modern
// import-time scanner. Keep its original claim and publish a consistent legacy
// tree whose transcript has never passed through that scanner.
async fn codex_legacy_reader_fixture(
    transcript: &[u8],
    metadata: &[u8],
) -> (
    LocalCodexSession,
    sea_orm::DatabaseConnection,
    String,
    String,
) {
    use sea_orm::{ConnectionTrait, TransactionTrait};
    fn tree(storage: &std::path::Path, entries: &[(&str, &str, &str)]) -> String {
        let mut body = Vec::new();
        for (mode, name, oid) in entries {
            body.extend_from_slice(format!("{mode} {name}\0").as_bytes());
            body.extend_from_slice(&hex::decode(oid).unwrap());
        }
        libra::utils::object::write_git_object(storage, "tree", &body)
            .unwrap()
            .to_string()
    }
    let fixture = LocalCodexSession::new();
    fixture.source(1, false);
    assert!(fixture.import().status.success());
    let db = fixture.db().await;
    let row = db
        .query_one_raw(sea_orm::Statement::from_string(
            db.get_database_backend(),
            "SELECT checkpoint_id, traces_commit FROM agent_checkpoint",
        ))
        .await
        .unwrap()
        .unwrap();
    let id: String = row.try_get_by("checkpoint_id").unwrap();
    let old_commit: String = row.try_get_by("traces_commit").unwrap();
    let storage = fixture.repo.join(".libra");
    let blob = libra::utils::object::write_git_object(&storage, "blob", transcript)
        .unwrap()
        .to_string();
    let metadata = libra::utils::object::write_git_object(&storage, "blob", metadata)
        .unwrap()
        .to_string();
    let transcripts = tree(&storage, &[("100644", "codex", &blob)]);
    let inner = tree(
        &storage,
        &[
            ("100644", "metadata.json", &metadata),
            ("40000", "transcript", &transcripts),
        ],
    );
    let prefix = tree(&storage, &[("40000", &id[2..], &inner)]);
    let checkpoints = tree(&storage, &[("40000", &id[..2], &prefix)]);
    let root = tree(&storage, &[("40000", "checkpoint", &checkpoints)]);
    let body = format!(
        "tree {root}\nparent {old_commit}\nauthor Fixture <fixture@local> 1 +0000\ncommitter Fixture <fixture@local> 1 +0000\n\nLegacy Codex reader fixture\n"
    );
    let commit = libra::utils::object::write_git_object(&storage, "commit", body.as_bytes())
        .unwrap()
        .to_string();
    let txn = db.begin().await.unwrap();
    txn.execute_raw(sea_orm::Statement::from_sql_and_values(db.get_database_backend(),
        "UPDATE agent_checkpoint SET tree_oid = ?, metadata_blob_oid = ?, traces_commit = ? WHERE checkpoint_id = ?",
        [root.into(), metadata.into(), commit.clone().into(), id.clone().into()])).await.unwrap();
    let updated = txn.execute_raw(sea_orm::Statement::from_sql_and_values(db.get_database_backend(),
        "UPDATE reference SET \"commit\" = ? WHERE name = 'traces' AND remote IS NULL AND kind = 'Branch' AND \"commit\" = ?",
        [commit.into(), old_commit.into()])).await.unwrap();
    assert_eq!(updated.rows_affected(), 1);
    txn.commit().await.unwrap();
    (fixture, db, id, blob)
}

async fn codex_legacy_reader_state(db: &sea_orm::DatabaseConnection) -> String {
    use sea_orm::ConnectionTrait;
    db.query_one_raw(sea_orm::Statement::from_string(db.get_database_backend(),
        "SELECT json_object('checkpoints', (SELECT json_group_array(json_object('id',checkpoint_id,'tree',tree_oid,'commit',traces_commit,'metadata',metadata_blob_oid)) FROM agent_checkpoint), 'claims', (SELECT json_group_array(json_object('digest',coverage_digest,'revision',revision,'checkpoint',checkpoint_id,'state',state)) FROM agent_coverage_claim), 'refs', (SELECT json_group_array(json_object('name',name,'commit',\"commit\")) FROM reference)) AS state"))
        .await.unwrap().unwrap().try_get_by("state").unwrap()
}

fn codex_legacy_reasoning_bytes() -> Vec<u8> {
    r#"{"type":"response_item","payload":{"type":"reasoning","summary":[{"type":"summary_text","text":"CX16-readable-思考"}],"content":[{"type":"reasoning_text","text":"CX16-content"}],"encrypted_content":"CX16-opaque","future":{"n":9007199254740993,"fraction":0.12345678901234567890123456789}}}
"#.as_bytes().to_vec()
}

fn codex_legacy_show(fixture: &LocalCodexSession, id: &str) -> serde_json::Value {
    let out = fixture.run(&["agent", "checkpoint", "show", id, "--json"]);
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    serde_json::from_slice::<serde_json::Value>(&out.stdout).unwrap()["data"].clone()
}

#[tokio::test]
#[ignore = "operator-local Codex acceptance; never run in CI"]
async fn codex_legacy_show_export_and_diagnostics_preserve_scoped_data() {
    use std::str::FromStr;

    use sea_orm::ConnectionTrait;
    for metadata in [b"{}".as_slice(), b"{\"agent_kind\":\"codex\"}"] {
        let bytes = codex_legacy_reasoning_bytes();
        let (fixture, db, id, blob) = codex_legacy_reader_fixture(&bytes, metadata).await;
        let before = codex_legacy_reader_state(&db).await;
        let session_id: String = db
            .query_one_raw(sea_orm::Statement::from_string(
                db.get_database_backend(),
                "SELECT session_id FROM agent_checkpoint",
            ))
            .await
            .unwrap()
            .unwrap()
            .try_get_by("session_id")
            .unwrap();
        let shown = codex_legacy_show(&fixture, &id);
        assert_eq!(shown["reasoning"]["availability"], "unproven");
        assert_eq!(
            shown["reasoning"]["readable"],
            serde_json::json!(["CX16-readable-思考", "CX16-content"])
        );
        assert_eq!(
            shown["reasoning"]["encrypted_content"],
            serde_json::json!(["CX16-opaque"])
        );
        assert_eq!(shown["reasoning"]["readable_obtained"], true);
        assert_eq!(shown["reasoning"]["redacted"], false);
        let out = fixture.run(&["agent", "checkpoint", "export", &id]);
        assert!(
            out.status.success(),
            "{}",
            String::from_utf8_lossy(&out.stderr)
        );
        assert_eq!(
            out.stdout, bytes,
            "ordinary export retains unknown values and precise numbers"
        );
        for args in [
            vec!["agent", "checkpoint", "show", &id],
            vec!["agent", "checkpoint", "list", "--json"],
            vec!["agent", "session", "list", "--json"],
            vec!["agent", "graph", &session_id, "--json"],
            vec!["agent", "doctor", "--json"],
        ] {
            let out = fixture.run(&args);
            assert!(
                out.status.success(),
                "{args:?}: {}",
                String::from_utf8_lossy(&out.stderr)
            );
            let displayed = format!(
                "{}{}",
                String::from_utf8_lossy(&out.stdout),
                String::from_utf8_lossy(&out.stderr)
            );
            assert!(
                !displayed.contains("CX16-"),
                "{args:?} expanded a body: {displayed}"
            );
        }
        assert_eq!(before, codex_legacy_reader_state(&db).await);
        assert_eq!(
            LocalCodexSession::count(&db, "agent_audit_log", "1").await,
            0
        );
        let stored = libra::utils::object::read_git_object(
            &fixture.repo.join(".libra"),
            &git_internal::hash::ObjectHash::from_str(&blob).unwrap(),
        )
        .unwrap();
        assert_eq!(stored, bytes);
        fixture.source(1, true);
        let other_source = std::fs::read_to_string(&fixture.source)
            .unwrap()
            .replace(
                "123e4567-e89b-12d3-a456-426614174031",
                "123e4567-e89b-12d3-a456-426614174032",
            )
            .replace("CX15-MULTI-0", "OTHER-SESSION-THINKING");
        let other_path = fixture.source.with_file_name(
            "rollout-2026-10-19T01-00-00-123e4567-e89b-12d3-a456-426614174032.jsonl",
        );
        std::fs::write(&other_path, other_source).unwrap();
        let imported = fixture.run(&[
            "agent",
            "import",
            "--path",
            other_path.to_str().unwrap(),
            "--agent",
            "codex",
            "--yes",
            "--json",
        ]);
        assert!(
            imported.status.success(),
            "{}",
            String::from_utf8_lossy(&imported.stderr)
        );
        let other_id: String = db
            .query_one_raw(sea_orm::Statement::from_sql_and_values(
                db.get_database_backend(),
                "SELECT checkpoint_id FROM agent_checkpoint WHERE session_id != ?",
                [session_id.into()],
            ))
            .await
            .unwrap()
            .unwrap()
            .try_get_by("checkpoint_id")
            .unwrap();
        assert!(
            !codex_legacy_show(&fixture, &id)
                .to_string()
                .contains("OTHER-SESSION-THINKING")
        );
        let other_show = codex_legacy_show(&fixture, &other_id).to_string();
        assert!(other_show.contains("OTHER-SESSION-THINKING"));
        assert!(!other_show.contains("CX16-"));
        let other_export = fixture.run(&["agent", "checkpoint", "export", &other_id]);
        assert!(other_export.status.success());
        let other_text = String::from_utf8(other_export.stdout).unwrap();
        assert!(other_text.contains("OTHER-SESSION-THINKING"));
        assert!(!other_text.contains("CX16-"), "no cross-session body");
    }
}

#[tokio::test]
#[ignore = "operator-local Codex acceptance; never run in CI"]
async fn codex_legacy_decoded_credentials_are_scanned_without_rewriting_objects() {
    let bytes = br#"{"type":"response_item","payload":{"type":"reasoning","summary":[{"type":"summary_text","text":"CX16-safe sk\u002d1234567890abcdefghijklmnopqrstuvwxyz"}],"encrypted_content":"opaque"}}
{"type":"response_item","payload":{"type":"future","n":9007199254740993,"n":9007199254740994}}
"#;
    let (fixture, db, id, _) = codex_legacy_reader_fixture(bytes, b"{}").await;
    let before = codex_legacy_reader_state(&db).await;
    let shown = codex_legacy_show(&fixture, &id);
    assert_eq!(shown["reasoning"]["redacted"], true);
    let shown = shown.to_string();
    assert!(shown.contains("CX16-safe"));
    assert!(!shown.contains("1234567890abcdefghijklmnopqrstuvwxyz"));
    let out = fixture.run(&["agent", "checkpoint", "export", &id]);
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let text = String::from_utf8(out.stdout).unwrap();
    assert!(text.contains("CX16-safe"));
    assert!(!text.contains("1234567890abcdefghijklmnopqrstuvwxyz"));
    assert!(text.contains("\"n\":9007199254740993,\"n\":9007199254740994"));
    assert_eq!(before, codex_legacy_reader_state(&db).await);
    let denied = fixture.run(&["agent", "checkpoint", "export", "missing", "--raw"]);
    assert!(!denied.status.success());
    let raw = fixture.run(&[
        "agent",
        "checkpoint",
        "export",
        &id,
        "--raw",
        "--allow-raw",
        "--justification",
        "local acceptance",
    ]);
    assert!(
        raw.status.success(),
        "{}",
        String::from_utf8_lossy(&raw.stderr)
    );
    assert_eq!(raw.stdout, bytes);
    assert_eq!(
        LocalCodexSession::count(&db, "agent_audit_log", "granted = 0").await,
        1
    );
    assert_eq!(
        LocalCodexSession::count(&db, "agent_audit_log", "granted = 1").await,
        1
    );
    assert_eq!(before, codex_legacy_reader_state(&db).await);
}

#[tokio::test]
#[ignore = "operator-local Codex acceptance; never run in CI"]
async fn codex_legacy_invalid_binding_and_missing_object_never_fall_back_to_raw() {
    for corrupt in [false, true] {
        let bytes = codex_legacy_reasoning_bytes();
        let metadata = if corrupt {
            b"{}".as_slice()
        } else {
            b"{\"agent_kind\":\"claude_code\"}"
        };
        let (fixture, db, id, blob) = codex_legacy_reader_fixture(&bytes, metadata).await;
        let before = codex_legacy_reader_state(&db).await;
        if corrupt {
            std::fs::remove_file(
                fixture
                    .repo
                    .join(".libra/objects")
                    .join(&blob[..2])
                    .join(&blob[2..]),
            )
            .unwrap();
        }
        for args in [
            vec!["agent", "checkpoint", "show", &id, "--json"],
            vec!["agent", "checkpoint", "export", &id],
        ] {
            let out = fixture.run(&args);
            if args[2] == "export" {
                assert!(!out.status.success(), "{args:?} must report a read failure");
            } else {
                assert!(
                    out.status.success(),
                    "show must retain its safe summary: {}",
                    String::from_utf8_lossy(&out.stderr)
                );
                let shown: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
                assert_eq!(shown["data"]["reasoning"]["availability"], "unavailable");
                assert_eq!(shown["data"]["reasoning"]["readable_obtained"], false);
                assert_eq!(
                    shown["data"]["reasoning"]["readable"],
                    serde_json::json!([])
                );
                assert_eq!(
                    shown["data"]["reasoning"]["encrypted_content"],
                    serde_json::json!([])
                );
                assert_eq!(
                    shown["data"]["reasoning"]["reason"],
                    "checkpoint_unreadable"
                );
            }
            let displayed = format!(
                "{}{}",
                String::from_utf8_lossy(&out.stdout),
                String::from_utf8_lossy(&out.stderr)
            );
            assert!(!displayed.contains("CX16-"), "no raw fallback: {displayed}");
        }
        assert_eq!(before, codex_legacy_reader_state(&db).await);
    }
}

#[tokio::test]
#[ignore = "operator-local Codex acceptance; never run in CI"]
async fn codex_legacy_large_export_uses_stored_read_cap_and_show_is_partial() {
    let filler =
        b"{\"type\":\"response_item\",\"payload\":{\"type\":\"future\",\"text\":\"ordinary\"}}\n";
    let mut bytes = Vec::new();
    while bytes.len() <= 16 * 1024 * 1024 {
        bytes.extend_from_slice(filler);
    }
    bytes.extend(codex_legacy_reasoning_bytes());
    let (fixture, db, id, _) = codex_legacy_reader_fixture(&bytes, b"{}").await;
    let before = codex_legacy_reader_state(&db).await;
    let out = fixture.run(&["agent", "checkpoint", "export", &id]);
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert_eq!(out.stdout, bytes);
    let shown = codex_legacy_show(&fixture, &id);
    assert_eq!(shown["reasoning"]["availability"], "partial");
    assert_eq!(shown["reasoning"]["readable_obtained"], false);
    assert_eq!(before, codex_legacy_reader_state(&db).await);
}

#[tokio::test]
#[ignore = "operator-local Codex acceptance; never run in CI"]
async fn codex_legacy_partial_records_and_configured_limits_are_explicit() {
    let mut bytes = codex_legacy_reasoning_bytes();
    let complete_len = bytes.len();
    bytes.extend_from_slice(b"{\"type\":\"response_item\",\"payload\":{\"type\":\"reasoning\",\"unknown\":\"sk\\u002d1234567890abcdefghijklmnopqrstuvwxyz\"");
    let (fixture, db, id, _) = codex_legacy_reader_fixture(&bytes, b"{}").await;
    let before = codex_legacy_reader_state(&db).await;
    let shown = codex_legacy_show(&fixture, &id);
    assert_eq!(shown["reasoning"]["availability"], "partial");
    assert_eq!(shown["reasoning"]["readable"][0], "CX16-readable-思考");
    let exported = fixture.run(&["agent", "checkpoint", "export", &id]);
    assert!(exported.status.success());
    assert!(
        !String::from_utf8_lossy(&exported.stdout).contains("1234567890abcdefghijklmnopqrstuvwxyz")
    );
    let cap = complete_len.to_string();
    let configured = fixture.run(&["config", "set", "agent.max_transcript_read_bytes", &cap]);
    assert!(
        configured.status.success(),
        "{}",
        String::from_utf8_lossy(&configured.stderr)
    );
    let shown = codex_legacy_show(&fixture, &id);
    assert_eq!(shown["reasoning"]["availability"], "partial");
    assert_eq!(shown["reasoning"]["readable_obtained"], true);
    assert_eq!(shown["reasoning"]["readable"][0], "CX16-readable-思考");
    let exported = fixture.run(&["agent", "checkpoint", "export", &id]);
    assert!(exported.status.success());
    assert_eq!(exported.stdout, bytes[..complete_len]);
    assert_eq!(before, codex_legacy_reader_state(&db).await);
}

#[tokio::test]
#[ignore = "operator-local Codex acceptance; never run in CI"]
async fn codex_legacy_reader_keeps_current_manifest_checkpoints_readable() {
    use sea_orm::ConnectionTrait;
    let fixture = LocalCodexSession::new();
    fixture.source(1, true);
    assert!(fixture.import().status.success());
    let db = fixture.db().await;
    let id: String = db
        .query_one_raw(sea_orm::Statement::from_string(
            db.get_database_backend(),
            "SELECT checkpoint_id FROM agent_checkpoint",
        ))
        .await
        .unwrap()
        .unwrap()
        .try_get_by("checkpoint_id")
        .unwrap();
    let before = codex_legacy_reader_state(&db).await;
    let raw = fixture.run(&["agent", "checkpoint", "export", &id, "--raw", "--allow-raw"]);
    assert!(raw.status.success());
    let ordinary = fixture.run(&["agent", "checkpoint", "export", &id]);
    assert!(ordinary.status.success());
    assert_eq!(
        ordinary.stdout, raw.stdout,
        "scanning a current ordinary checkpoint is idempotent"
    );
    let shown = codex_legacy_show(&fixture, &id);
    assert_eq!(shown["reasoning"]["availability"], "unproven");
    assert_eq!(
        shown["reasoning"]["readable"],
        serde_json::json!(["CX15-MULTI-0"])
    );
    assert_eq!(
        shown["reasoning"]["encrypted_content"],
        serde_json::json!(["opaque-0"])
    );
    assert_eq!(before, codex_legacy_reader_state(&db).await);
}

#[tokio::test]
#[ignore = "operator-local Codex acceptance; never run in CI"]
async fn codex_legacy_duplicate_reasoning_keys_are_partial_without_export_data_loss() {
    let duplicate = br#"{"type":"response_item","payload":{"type":"reasoning","summary":[{"type":"summary_text","text":"first"}],"summary":[{"type":"summary_text","text":"second"}],"encrypted_content":"opaque"}}"#;
    assert!(
        libra::internal::ai::observed_agents::parse_canon_value(duplicate).is_err(),
        "the shared strict reader rejects duplicate keys before classification"
    );
    let mut bytes = codex_legacy_reasoning_bytes();
    bytes.extend_from_slice(duplicate);
    let (fixture, db, id, _) = codex_legacy_reader_fixture(&bytes, b"{}").await;
    let before = codex_legacy_reader_state(&db).await;
    let shown = codex_legacy_show(&fixture, &id);
    assert_eq!(shown["reasoning"]["availability"], "partial");
    assert_eq!(
        shown["reasoning"]["readable"],
        serde_json::json!(["CX16-readable-思考", "CX16-content"])
    );
    let ordinary = fixture.run(&["agent", "checkpoint", "export", &id]);
    assert!(ordinary.status.success());
    assert_eq!(ordinary.stdout, bytes);
    assert_eq!(before, codex_legacy_reader_state(&db).await);
}

#[tokio::test]
#[ignore = "operator-local Codex acceptance; never run in CI"]
async fn codex_legacy_complete_non_unicode_tokens_keep_bytes_but_still_scan_credentials() {
    for (token, secret) in [
        (br#""\ud800""#.as_slice(), false),
        (b"\"raw\xffbyte\"".as_slice(), false),
        (
            br#""\ud800 sk\u002d1234567890abcdefghijklmnopqrstuvwxyz""#.as_slice(),
            true,
        ),
    ] {
        let mut bytes = codex_legacy_reasoning_bytes();
        bytes.extend_from_slice(b"{\"type\":\"future\",\"opaque\":");
        bytes.extend_from_slice(token);
        bytes.extend_from_slice(b"}\n");
        bytes.extend(codex_legacy_reasoning_bytes());
        let (fixture, db, id, _) = codex_legacy_reader_fixture(&bytes, b"{}").await;
        let before = codex_legacy_reader_state(&db).await;
        let exported = fixture.run(&["agent", "checkpoint", "export", &id]);
        assert!(exported.status.success());
        if secret {
            assert!(
                !String::from_utf8_lossy(&exported.stdout)
                    .contains("1234567890abcdefghijklmnopqrstuvwxyz")
            );
        } else {
            assert_eq!(
                exported.stdout, bytes,
                "complete token unchanged when no credential matches"
            );
        }
        let shown = codex_legacy_show(&fixture, &id);
        assert_eq!(shown["reasoning"]["redacted"], secret);
        assert_eq!(shown["reasoning"]["availability"], "partial");
        assert_eq!(
            shown["reasoning"]["readable"].as_array().unwrap().len(),
            4,
            "valid preceding and following thinking remains available"
        );
        assert_eq!(before, codex_legacy_reader_state(&db).await);
    }
    let prefix = codex_legacy_reasoning_bytes();
    let mut bytes = prefix.clone();
    bytes.extend_from_slice(
        br#"{"type":"future","text":"a\qb"}
"#,
    );
    bytes.extend_from_slice(&prefix);
    let (fixture, _, id, _) = codex_legacy_reader_fixture(&bytes, b"{}").await;
    let mut expected = prefix.clone();
    expected.extend_from_slice(b"{\"type\":\"future\",\"text\":?}\n");
    expected.extend_from_slice(&prefix);
    let exported = fixture.run(&["agent", "checkpoint", "export", &id]);
    assert!(exported.status.success());
    assert_eq!(
        exported.stdout, expected,
        "invalid mid-file escape replacement leaves surrounding records intact"
    );
    let shown = codex_legacy_show(&fixture, &id);
    assert_eq!(shown["reasoning"]["availability"], "partial");
    assert_eq!(shown["reasoning"]["redacted"], true);
    assert_eq!(shown["reasoning"]["readable"].as_array().unwrap().len(), 4);

    let bytes = br#"{"type":"response_item","payload":{"type":"reasoning","summary":[{"type":"summary_text","text":"legacy \ud800 sk\u002d1234567890abcdefghijklmnopqrstuvwxyz"}]}}
"#;
    let (fixture, _, id, _) = codex_legacy_reader_fixture(bytes, b"{}").await;
    let shown = codex_legacy_show(&fixture, &id);
    assert_eq!(
        shown["reasoning"]["availability"], "partial",
        "lossy scan text must not erase source qualification after masking makes JSON valid"
    );
    assert_eq!(shown["reasoning"]["redacted"], true);
    assert_eq!(shown["reasoning"]["readable"].as_array().unwrap().len(), 1);
    assert!(
        !shown
            .to_string()
            .contains("1234567890abcdefghijklmnopqrstuvwxyz")
    );
    for broken in [b"\\u00".as_slice(), b"\xf0\x9f".as_slice()] {
        let mut bytes = codex_legacy_reasoning_bytes();
        bytes.extend_from_slice(b"{\"type\":\"future\",\"text\":\"");
        bytes.extend_from_slice(broken);
        let cap = bytes.len();
        bytes.extend_from_slice(b"ab\"}\n");
        let (fixture, _, id, _) = codex_legacy_reader_fixture(&bytes, b"{}").await;
        assert!(
            fixture
                .run(&[
                    "config",
                    "set",
                    "agent.max_transcript_read_bytes",
                    &cap.to_string()
                ])
                .status
                .success()
        );
        let shown = codex_legacy_show(&fixture, &id);
        assert_eq!(shown["reasoning"]["availability"], "partial");
        assert_eq!(
            shown["reasoning"]["redacted"], true,
            "undecodable token replacement is also a data mask"
        );
        let exported = fixture.run(&["agent", "checkpoint", "export", &id]);
        assert!(exported.status.success());
        assert!(exported.stdout.ends_with(b"?"));
    }
}

#[tokio::test]
#[ignore = "operator-local Codex acceptance; never run in CI"]
async fn codex_legacy_empty_or_expanding_source_never_claims_readable_data() {
    use std::str::FromStr;
    for expanding in [false, true] {
        let bytes = if expanding {
            let mut bytes = b"{\"type\":\"future\",\"text\":\"".to_vec();
            bytes.extend(vec![0xff; 100_000]);
            bytes.extend_from_slice(
                br#" sk\u002d1234567890abcdefghijklmnopqrstuvwxyz"}
"#,
            );
            bytes
        } else {
            Vec::new()
        };
        let (fixture, db, id, blob) = codex_legacy_reader_fixture(&bytes, b"{}").await;
        let before = codex_legacy_reader_state(&db).await;
        let shown = codex_legacy_show(&fixture, &id);
        assert_eq!(shown["reasoning"]["availability"], "unavailable");
        assert_eq!(shown["reasoning"]["readable_obtained"], false);
        assert_eq!(shown["reasoning"]["readable"], serde_json::json!([]));
        assert_eq!(
            shown["reasoning"]["encrypted_content"],
            serde_json::json!([])
        );
        assert_eq!(
            shown["reasoning"]["reason"],
            if expanding {
                "checkpoint_unreadable"
            } else {
                "no_transcript"
            }
        );
        let exported = fixture.run(&["agent", "checkpoint", "export", &id]);
        if expanding {
            assert!(!exported.status.success());
            assert!(
                String::from_utf8_lossy(&exported.stderr).contains("masked output expansion limit")
            );
            assert!(
                !String::from_utf8_lossy(&exported.stderr)
                    .contains("1234567890abcdefghijklmnopqrstuvwxyz")
            );
        } else {
            assert!(exported.status.success());
        }
        assert!(exported.stdout.is_empty());
        assert_eq!(before, codex_legacy_reader_state(&db).await);
        assert_eq!(
            libra::utils::object::read_git_object(
                &fixture.repo.join(".libra"),
                &git_internal::hash::ObjectHash::from_str(&blob).unwrap()
            )
            .unwrap(),
            bytes
        );
    }
}

#[tokio::test]
#[ignore = "operator-local Codex acceptance; never run in CI"]
async fn codex_summary_dropped_empty_turn_remaps_retention_without_losing_unassigned_data() {
    use std::str::FromStr;

    use git_internal::hash::ObjectHash;
    use sea_orm::ConnectionTrait;
    use serde_json::json;

    for orphaned in [false, true] {
        let fixture = LocalCodexSession::new();
        fixture.source(2, true);
        let original = std::fs::read_to_string(&fixture.source).unwrap();
        let (metadata, rest) = original.split_once('\n').unwrap();
        let empty = json!({"type":"response_item","payload":{"type":"message","role":"assistant","content":[]}});
        let orphan = if orphaned {
            format!(
                "{}\n",
                json!({"type":"response_item","payload":{"type":"reasoning","summary":[{"type":"summary_text","text":"CX15-UNASSIGNED-PRELUDE"}]}})
            )
        } else {
            String::new()
        };
        let source = format!("{metadata}\n{empty}\n{orphan}{rest}").replace(
            "CX15-MULTI-0",
            "CX15-MULTI-0 sk-ant-EXAMPLE0000000000000000",
        );
        std::fs::write(&fixture.source, source).unwrap();
        let imported = fixture.import();
        assert!(
            imported.status.success(),
            "{}",
            String::from_utf8_lossy(&imported.stderr)
        );
        let db = fixture.db().await;
        let exports = fixture.exports(&db).await;
        assert_eq!(
            exports.len(),
            2,
            "frozen V1 still drops the empty semantic turn"
        );
        let first = exports
            .iter()
            .find(|text| text.contains("question 0"))
            .unwrap();
        let second = exports
            .iter()
            .find(|text| text.contains("question 1"))
            .unwrap();
        assert!(first.contains("CX15-MULTI-0"));
        assert!(!first.contains("CX15-MULTI-1"));
        assert!(second.contains("CX15-MULTI-1"));
        assert!(!second.contains("CX15-MULTI-0"));
        assert!(first.contains("<REDACTED:anthropic-api-key>"));
        assert!(!first.contains("EXAMPLE0000000000000000"));
        assert_eq!(first.contains("CX15-UNASSIGNED-PRELUDE"), orphaned);
        assert!(!second.contains("CX15-UNASSIGNED-PRELUDE"));
        let rows = db
            .query_all_raw(sea_orm::Statement::from_string(
                db.get_database_backend(),
                "SELECT metadata_blob_oid FROM agent_checkpoint".to_owned(),
            ))
            .await
            .unwrap();
        for row in rows {
            let oid: String = row.try_get_by("metadata_blob_oid").unwrap();
            let bytes = libra::utils::object::read_git_object(
                &fixture.repo.join(".libra"),
                &ObjectHash::from_str(&oid).unwrap(),
            )
            .unwrap();
            let metadata: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
            assert_eq!(
                metadata["redaction_report"]["codex_reasoning"]["turn_attribution_unproven"],
                orphaned
            );
        }
        assert!(fixture.import().status.success());
        assert_eq!(
            LocalCodexSession::count(&db, "agent_checkpoint", "1").await,
            2
        );
        db.close().await.unwrap();
    }
}

impl LocalCodexSession {
    fn new() -> Self {
        Self::with_temp(tempfile::tempdir().unwrap())
    }

    fn with_temp(temp: tempfile::TempDir) -> Self {
        let repo = temp.path().join("repo");
        let home = temp.path().join("home");
        std::fs::create_dir_all(&repo).unwrap();
        std::fs::create_dir_all(&home).unwrap();
        let repo = repo.canonicalize().unwrap();
        let home = home.canonicalize().unwrap();
        let source = home.join(".codex/sessions/2026/10/19/rollout-2026-10-19T01-00-00-123e4567-e89b-12d3-a456-426614174031.jsonl");
        std::fs::create_dir_all(source.parent().unwrap()).unwrap();
        let fixture = Self {
            _temp: temp,
            repo,
            home,
            source,
        };
        assert!(fixture.run(&["init"]).status.success());
        fixture
    }

    fn run(&self, args: &[&str]) -> std::process::Output {
        std::process::Command::new(env!("CARGO_BIN_EXE_libra"))
            .current_dir(&self.repo)
            .env_clear()
            .env("PATH", "/usr/bin:/bin")
            .env("HOME", &self.home)
            .env("USERPROFILE", &self.home)
            .env("LIBRA_TEST_HOME", &self.home)
            .env("LIBRA_TEST", "1")
            .args(args)
            .output()
            .unwrap()
    }

    fn import(&self) -> std::process::Output {
        self.run(&[
            "agent",
            "import",
            "--path",
            self.source.to_str().unwrap(),
            "--agent",
            "codex",
            "--yes",
            "--json",
        ])
    }

    fn source(&self, turns: usize, thinking: bool) {
        use serde_json::json;
        let mut rows = vec![
            json!({"type":"session_meta","timestamp":"2026-10-19T01:00:00Z",
            "payload":{"id":"123e4567-e89b-12d3-a456-426614174031","cwd":self.repo}}),
        ];
        for i in 0..turns {
            rows.push(json!({"type":"event_msg","payload":{"type":"task_started","turn_id":format!("cx15-multi-{i}")}}));
            rows.push(json!({"type":"response_item","timestamp":"2026-10-19T01:00:01Z",
                "payload":{"type":"message","role":"user","content":[{"type":"input_text","text":format!("question {i}")}]}}));
            if thinking {
                rows.push(json!({"type":"response_item","timestamp":"2026-10-19T01:00:02Z",
                    "payload":{"type":"reasoning","id":format!("rs-multi-{i}"),
                        "summary":[{"type":"summary_text","text":format!("CX15-MULTI-{i}")}],"encrypted_content":format!("opaque-{i}")}}));
            }
            rows.push(json!({"type":"response_item","timestamp":"2026-10-19T01:00:03Z",
                "payload":{"type":"message","role":"assistant","content":[{"type":"output_text","text":format!("answer {i}")}]}}));
        }
        rows.push(json!({"type":"session_end","timestamp":"2026-10-19T01:00:04Z","payload":{"type":"session_end","cwd":self.repo}}));
        let text = rows
            .iter()
            .map(serde_json::Value::to_string)
            .collect::<Vec<_>>()
            .join("\n")
            + "\n";
        std::fs::write(&self.source, text).unwrap();
    }

    async fn db(&self) -> sea_orm::DatabaseConnection {
        sea_orm::Database::connect(format!(
            "sqlite://{}?mode=rw",
            self.repo.join(".libra/libra.db").display()
        ))
        .await
        .unwrap()
    }

    async fn count(db: &sea_orm::DatabaseConnection, table: &str, predicate: &str) -> i64 {
        use sea_orm::ConnectionTrait;
        db.query_one_raw(sea_orm::Statement::from_string(
            db.get_database_backend(),
            format!("SELECT COUNT(*) AS n FROM {table} WHERE {predicate}"),
        ))
        .await
        .unwrap()
        .unwrap()
        .try_get_by("n")
        .unwrap()
    }

    async fn exports(&self, db: &sea_orm::DatabaseConnection) -> Vec<String> {
        use sea_orm::ConnectionTrait;
        let rows = db
            .query_all_raw(sea_orm::Statement::from_string(
                db.get_database_backend(),
                "SELECT checkpoint_id FROM agent_checkpoint",
            ))
            .await
            .unwrap();
        rows.iter()
            .map(|row| {
                let id: String = row.try_get_by("checkpoint_id").unwrap();
                let out = self.run(&["agent", "checkpoint", "export", &id]);
                assert!(
                    out.status.success(),
                    "{}",
                    String::from_utf8_lossy(&out.stderr)
                );
                String::from_utf8(out.stdout).unwrap()
            })
            .collect()
    }
}

#[tokio::test]
#[ignore = "operator-local Codex acceptance; never run in CI"]
async fn codex_summary_projection_only_primary_gets_an_idempotent_supplement() {
    codex_legacy_primary_case(false, false).await;
}

#[tokio::test]
#[ignore = "operator-local Codex acceptance; never run in CI"]
async fn codex_summary_packed_primary_transcript_does_not_block_new_source_values() {
    codex_legacy_primary_case(true, false).await;
}

#[tokio::test]
#[ignore = "operator-local Codex acceptance; never run in CI"]
async fn codex_summary_manifestless_primary_does_not_block_new_source_values() {
    codex_legacy_primary_case(false, true).await;
}

async fn codex_legacy_primary_case(pack: bool, manifestless: bool) {
    use sea_orm::ConnectionTrait;
    let fixture = LocalCodexSession::new();
    fixture.source(1, false);
    let initial = fixture.import();
    assert!(
        initial.status.success(),
        "{}",
        String::from_utf8_lossy(&initial.stderr)
    );
    let db = fixture.db().await;
    assert_eq!(
        LocalCodexSession::count(&db, "agent_checkpoint", "1").await,
        1
    );
    if pack {
        pack_primary_transcript(&fixture, &db).await;
    }
    if manifestless {
        make_manifestless_primary(&fixture, &db).await;
    }
    let before = db.query_one_raw(sea_orm::Statement::from_string(db.get_database_backend(),
        "SELECT coverage_digest, revision, checkpoint_id FROM agent_coverage_claim WHERE state = 'catalog_committed'"))
        .await.unwrap().unwrap();
    let original: (String, i64, String) = (
        before.try_get_by("coverage_digest").unwrap(),
        before.try_get_by("revision").unwrap(),
        before.try_get_by("checkpoint_id").unwrap(),
    );
    fixture.source(1, true);
    for attempt in 0..2 {
        let result = fixture.import();
        assert!(
            result.status.success(),
            "attempt {attempt}: {}",
            String::from_utf8_lossy(&result.stderr)
        );
        assert_eq!(
            LocalCodexSession::count(&db, "agent_checkpoint", "1").await,
            2
        );
    }
    let next = db
        .query_one_raw(sea_orm::Statement::from_sql_and_values(
            db.get_database_backend(),
            "SELECT checkpoint_id FROM agent_checkpoint WHERE checkpoint_id != ?",
            [original.2.clone().into()],
        ))
        .await
        .unwrap()
        .unwrap();
    let next_id: String = next.try_get_by("checkpoint_id").unwrap();
    let exported = fixture.run(&["agent", "checkpoint", "export", &next_id]);
    assert!(
        exported.status.success(),
        "{}",
        String::from_utf8_lossy(&exported.stderr)
    );
    assert!(
        String::from_utf8(exported.stdout)
            .unwrap()
            .contains("CX15-MULTI-0")
    );
    let after = db.query_one_raw(sea_orm::Statement::from_string(db.get_database_backend(),
        "SELECT coverage_digest, revision, checkpoint_id FROM agent_coverage_claim WHERE state = 'catalog_committed'"))
        .await.unwrap().unwrap();
    assert_eq!(
        original,
        (
            after.try_get_by("coverage_digest").unwrap(),
            after.try_get_by("revision").unwrap(),
            after.try_get_by("checkpoint_id").unwrap()
        )
    );
    db.close().await.unwrap();
}

/// A valid mixed object store with the prior ordinary transcript in a pack.
/// Restore the unrelated loose parents/metadata: this isolates retention's new
/// readback from the existing whole-traces packed-parent writer limitation.
async fn pack_primary_transcript(fixture: &LocalCodexSession, db: &sea_orm::DatabaseConnection) {
    use std::str::FromStr;

    use git_internal::hash::ObjectHash;
    use sea_orm::ConnectionTrait;
    let root: String = db
        .query_one_raw(sea_orm::Statement::from_string(
            db.get_database_backend(),
            "SELECT tree_oid FROM agent_checkpoint",
        ))
        .await
        .unwrap()
        .unwrap()
        .try_get_by("tree_oid")
        .unwrap();
    let listing = fixture.run(&["ls-tree", "-r", &root]);
    assert!(listing.status.success());
    let listing = String::from_utf8(listing.stdout).unwrap();
    let target = listing
        .lines()
        .find(|line| line.ends_with("/transcript/codex.jsonl"))
        .expect("ordinary transcript object in checkpoint tree")
        .split_whitespace()
        .nth(2)
        .unwrap();
    let storage = fixture.repo.join(".libra");
    let mut restore = Vec::new();
    for fanout in std::fs::read_dir(storage.join("objects")).unwrap() {
        let fanout = fanout.unwrap();
        let prefix = fanout.file_name().to_string_lossy().into_owned();
        if prefix.len() != 2
            || !prefix.bytes().all(|b| b.is_ascii_hexdigit())
            || !fanout.path().is_dir()
        {
            continue;
        }
        for entry in std::fs::read_dir(fanout.path()).unwrap() {
            let entry = entry.unwrap();
            let oid = format!("{prefix}{}", entry.file_name().to_string_lossy());
            if oid == target {
                continue;
            }
            let hash = ObjectHash::from_str(&oid).unwrap();
            let kind = fixture.run(&["cat-file", "-t", &oid]);
            assert!(kind.status.success());
            let kind = String::from_utf8(kind.stdout).unwrap().trim().to_owned();
            let bytes = libra::utils::object::read_git_object(&storage, &hash).unwrap();
            restore.push((oid, kind, bytes));
        }
    }
    let packed = fixture.run(&["repack", "-a", "-d"]);
    assert!(
        packed.status.success(),
        "{}",
        String::from_utf8_lossy(&packed.stderr)
    );
    for (oid, kind, bytes) in restore {
        assert_eq!(
            libra::utils::object::write_git_object(&storage, &kind, &bytes)
                .unwrap()
                .to_string(),
            oid
        );
    }
    assert!(
        !storage
            .join("objects")
            .join(&target[..2])
            .join(&target[2..])
            .exists(),
        "the primary transcript must really require the pack reader"
    );
    let cat = fixture.run(&["cat-file", "-t", target]);
    assert!(
        cat.status.success(),
        "the packed transcript is present, not a missing-object fixture"
    );
}

/// Construct a consistent legacy traces commit whose checkpoint omits the
/// modern manifest. The original transcript/metadata and semantic claim stay
/// unchanged; both catalog and traces ref point to the new legacy root.
async fn make_manifestless_primary(fixture: &LocalCodexSession, db: &sea_orm::DatabaseConnection) {
    use std::str::FromStr;

    use git_internal::hash::ObjectHash;
    use sea_orm::{ConnectionTrait, TransactionTrait};
    fn rewrite(storage: &std::path::Path, oid: &str, path: &[&str]) -> String {
        let bytes =
            libra::utils::object::read_git_object(storage, &ObjectHash::from_str(oid).unwrap())
                .unwrap();
        let hash_len = oid.len() / 2;
        let mut at = 0;
        let mut out = Vec::new();
        let mut changed = false;
        while at < bytes.len() {
            let space = at + bytes[at..].iter().position(|b| *b == b' ').unwrap();
            let nul = space + 1 + bytes[space + 1..].iter().position(|b| *b == 0).unwrap();
            let name = std::str::from_utf8(&bytes[space + 1..nul]).unwrap();
            let end = nul + 1 + hash_len;
            if path.is_empty() && name == "manifest.json" {
                changed = true;
                at = end;
                continue;
            }
            out.extend_from_slice(&bytes[at..=nul]);
            if !path.is_empty() && name == path[0] {
                let child = rewrite(storage, &hex::encode(&bytes[nul + 1..end]), &path[1..]);
                out.extend_from_slice(&hex::decode(child).unwrap());
                changed = true;
            } else {
                out.extend_from_slice(&bytes[nul + 1..end]);
            }
            at = end;
        }
        assert!(
            changed,
            "legacy fixture must replace the intended tree entry"
        );
        libra::utils::object::write_git_object(storage, "tree", &out)
            .unwrap()
            .to_string()
    }
    let row = db
        .query_one_raw(sea_orm::Statement::from_string(
            db.get_database_backend(),
            "SELECT checkpoint_id, tree_oid, traces_commit FROM agent_checkpoint",
        ))
        .await
        .unwrap()
        .unwrap();
    let id: String = row.try_get_by("checkpoint_id").unwrap();
    let tree: String = row.try_get_by("tree_oid").unwrap();
    let old_commit: String = row.try_get_by("traces_commit").unwrap();
    let storage = fixture.repo.join(".libra");
    let root = rewrite(&storage, &tree, &["checkpoint", &id[..2], &id[2..]]);
    let body = format!(
        "tree {root}\nparent {old_commit}\nauthor Fixture <fixture@local> 1 +0000\ncommitter Fixture <fixture@local> 1 +0000\n\nLegacy layout fixture\n"
    );
    let commit = libra::utils::object::write_git_object(&storage, "commit", body.as_bytes())
        .unwrap()
        .to_string();
    let txn = db.begin().await.unwrap();
    txn.execute_raw(sea_orm::Statement::from_sql_and_values(
        db.get_database_backend(),
        "UPDATE agent_checkpoint SET tree_oid = ?, traces_commit = ? WHERE checkpoint_id = ?",
        [root.into(), commit.clone().into(), id.into()],
    ))
    .await
    .unwrap();
    let updated = txn.execute_raw(sea_orm::Statement::from_sql_and_values(db.get_database_backend(),
        "UPDATE reference SET \"commit\" = ? WHERE name = 'traces' AND remote IS NULL AND kind = 'Branch' AND \"commit\" = ?",
        [commit.into(), old_commit.into()])).await.unwrap();
    assert_eq!(updated.rows_affected(), 1);
    txn.commit().await.unwrap();
}

#[tokio::test]
#[ignore = "operator-local Codex acceptance; never run in CI"]
async fn codex_summary_partial_supplement_failure_reports_progress_and_releases_lease() {
    use sea_orm::ConnectionTrait;
    let fixture = LocalCodexSession::new();
    fixture.source(3, false);
    assert!(fixture.import().status.success());
    let db = fixture.db().await;
    fixture.source(4, true);
    db.execute_unprepared("CREATE TRIGGER fail_second_supplement BEFORE INSERT ON agent_checkpoint WHEN (SELECT COUNT(*) FROM agent_checkpoint) = 4 BEGIN SELECT RAISE(ABORT,'synthetic later supplement failure'); END").await.unwrap();
    let failed = fixture.import();
    assert!(!failed.status.success());
    assert_eq!(
        LocalCodexSession::count(&db, "agent_checkpoint", "1").await,
        4
    );
    fn progress(value: &serde_json::Value) -> Option<u64> {
        if let Some(rows) = value
            .get("partial_results")
            .and_then(serde_json::Value::as_array)
        {
            return rows
                .iter()
                .find(|row| row["status"] == "partial")?
                .get("checkpoints_written")?
                .as_u64();
        }
        value.as_object()?.values().find_map(progress)
    }
    let written = [&failed.stdout, &failed.stderr]
        .into_iter()
        .find_map(|bytes| {
            bytes
                .iter()
                .position(|byte| *byte == b'{')
                .and_then(|start| serde_json::from_slice::<serde_json::Value>(&bytes[start..]).ok())
                .and_then(|value| progress(&value))
                .or_else(|| {
                    bytes
                        .split(|b| *b == b'\n')
                        .filter_map(|line| serde_json::from_slice::<serde_json::Value>(line).ok())
                        .find_map(|v| progress(&v))
                })
        });
    assert_eq!(
        written,
        Some(1),
        "one committed supplement must be reported as durable partial progress: stdout={} stderr={}",
        String::from_utf8_lossy(&failed.stdout),
        String::from_utf8_lossy(&failed.stderr)
    );
    assert_eq!(
        LocalCodexSession::count(
            &db,
            "agent_import_identity",
            "owner IS NOT NULL AND lease_expires_at > unixepoch() * 1000"
        )
        .await,
        0
    );
    assert_eq!(
        LocalCodexSession::count(
            &db,
            "agent_coverage_claim",
            "state IN ('reserved_import','reserved_live')"
        )
        .await,
        0
    );
    db.execute_unprepared("DROP TRIGGER fail_second_supplement")
        .await
        .unwrap();
    for _ in 0..2 {
        let retry = fixture.import();
        assert!(
            retry.status.success(),
            "{}",
            String::from_utf8_lossy(&retry.stderr)
        );
        assert_eq!(
            LocalCodexSession::count(&db, "agent_checkpoint", "1").await,
            7
        );
    }
    assert!(
        fixture
            .exports(&db)
            .await
            .iter()
            .any(|text| text.contains("CX15-MULTI-3"))
    );
    db.close().await.unwrap();
}

#[tokio::test]
#[ignore = "operator-local Codex acceptance; never run in CI"]
async fn codex_summary_supplement_budget_keeps_new_turn_and_resumes() {
    let fixture = LocalCodexSession::new();
    fixture.source(20, false);
    assert!(fixture.import().status.success());
    let db = fixture.db().await;
    fixture.source(21, true);
    let partial = fixture.import();
    assert!(
        !partial.status.success(),
        "unfinished retention must be reported as partial"
    );
    assert_eq!(
        LocalCodexSession::count(&db, "agent_checkpoint", "1").await,
        37
    );
    assert_eq!(
        LocalCodexSession::count(&db, "agent_coverage_claim", "state = 'catalog_committed'").await,
        21,
        "fresh semantic turns must not starve behind the retention backlog"
    );
    for _ in 0..2 {
        let retry = fixture.import();
        assert!(
            retry.status.success(),
            "{}",
            String::from_utf8_lossy(&retry.stderr)
        );
        assert_eq!(
            LocalCodexSession::count(&db, "agent_checkpoint", "1").await,
            41
        );
    }
    db.close().await.unwrap();
}

#[tokio::test]
#[ignore = "operator-local Codex acceptance; never run in CI"]
async fn codex_summary_full_repack_of_retained_primary_keeps_identical_replay_a_noop() {
    let fixture = LocalCodexSession::new();
    fixture.source(1, true);
    assert!(fixture.import().status.success());
    let db = fixture.db().await;
    let packed = fixture.run(&["repack", "-a", "-d"]);
    assert!(packed.status.success());
    let replay = fixture.import();
    assert!(
        replay.status.success(),
        "identical retained replay must not reopen packed checkpoint objects: {}",
        String::from_utf8_lossy(&replay.stderr)
    );
    assert_eq!(
        LocalCodexSession::count(&db, "agent_checkpoint", "1").await,
        1
    );
    db.close().await.unwrap();
}

#[tokio::test]
#[ignore = "operator-local Codex acceptance; never run in CI"]
async fn codex_summary_completeness_upgrade_keeps_distinct_ordinary_content_id() {
    use sea_orm::ConnectionTrait;
    let fixture = LocalCodexSession::new();
    fixture.source(1, true);
    let text = std::fs::read_to_string(&fixture.source).unwrap();
    let mut rows = text.lines().collect::<Vec<_>>();
    let end: serde_json::Value = serde_json::from_str(rows.pop().unwrap()).unwrap();
    assert_eq!(end["type"], "session_end");
    std::fs::write(&fixture.source, rows.join("\n") + "\n").unwrap();
    let initial = fixture.import();
    assert!(
        initial.status.success(),
        "{}",
        String::from_utf8_lossy(&initial.stderr)
    );
    let db = fixture.db().await;
    let before = db
        .query_one_raw(sea_orm::Statement::from_string(
            db.get_database_backend(),
            "SELECT checkpoint_id, revision, completeness FROM agent_coverage_claim",
        ))
        .await
        .unwrap()
        .unwrap();
    let old: String = before.try_get_by("checkpoint_id").unwrap();
    assert_eq!(
        before.try_get_by::<String, _>("completeness").unwrap(),
        "incomplete"
    );
    fixture.source(1, true);
    let upgraded = fixture.import();
    assert!(
        upgraded.status.success(),
        "{}",
        String::from_utf8_lossy(&upgraded.stderr)
    );
    let after = db
        .query_one_raw(sea_orm::Statement::from_string(
            db.get_database_backend(),
            "SELECT checkpoint_id, revision, completeness FROM agent_coverage_claim",
        ))
        .await
        .unwrap()
        .unwrap();
    assert_ne!(after.try_get_by::<String, _>("checkpoint_id").unwrap(), old);
    assert_eq!(
        after.try_get_by::<i64, _>("revision").unwrap(),
        before.try_get_by::<i64, _>("revision").unwrap() + 1
    );
    assert_eq!(
        after.try_get_by::<String, _>("completeness").unwrap(),
        "complete"
    );
    assert_eq!(
        LocalCodexSession::count(&db, "agent_checkpoint", "1").await,
        2
    );
    assert!(fixture.import().status.success());
    assert_eq!(
        LocalCodexSession::count(&db, "agent_checkpoint", "1").await,
        2
    );
    db.close().await.unwrap();
}

#[test]
#[ignore = "operator-local Codex acceptance; never run in CI"]
fn cx01_interrupt_kind_is_distinct_and_forwarding_policy_is_closed() {
    use libra::internal::ai::hooks::{LifecycleEventKind as K, SessionHookEnvelope, providers};
    let provider = providers::codex_provider();
    let envelope = SessionHookEnvelope {
        hook_event_name: "Interrupt".into(),
        session_id: "cx01-session".into(),
        cwd: "/tmp".into(),
        transcript_path: None,
        extra: serde_json::Map::new(),
    };
    let event = provider.parse_hook_event("Interrupt", &envelope).unwrap();
    assert_eq!(event.kind, K::TurnInterrupted);
    assert_eq!(event.kind.to_string(), "turn_interrupted");
    assert_eq!(K::SubagentEnd as u8, 12);
    assert_eq!(K::TurnInterrupted as u8, 13);
    let mut session = libra::internal::ai::session::SessionState::new("/tmp");
    libra::internal::ai::hooks::lifecycle::apply_lifecycle_event(&mut session, &event, 8);
    assert_eq!(
        session.metadata["last_turn_interrupted_at"],
        serde_json::json!(event.timestamp)
    );
    assert!(session.messages.is_empty());
    let fixture = LocalCodexSession::new();
    let payload = serde_json::json!({"hook_event_name":"Interrupt","session_id":envelope.session_id,
        "cwd":fixture.repo,"turn_id":"cx01-turn","transcript_path":fixture.source,
        "model":"cx01-model","permission_mode":"default"});
    let lowered = libra::internal::ai::capture::ingress::lower_in_process_capture_frame_for_test(
        payload.to_string().as_bytes(),
        libra::internal::ai::hooks::provider::ProviderHookCommand::Stop,
        K::TurnEnd,
        provider,
        Some(&fixture.repo),
    )
    .unwrap();
    let libra::internal::ai::capture::ingress::CaptureIngressOutcome::Command(command) = lowered
    else {
        panic!("a recognized Interrupt must not become UnknownEvent");
    };
    assert_eq!(command.event_kind(), K::TurnInterrupted);
    assert!(provider.accepts_event_kind(K::TurnEnd, event.kind));
    assert!(provider.accepts_event_kind(K::Compaction, K::CompactionCompleted));
    for other in [providers::claude_provider(), providers::opencode_provider()] {
        assert!(!other.accepts_event_kind(K::TurnEnd, event.kind));
        assert!(!other.accepts_event_kind(K::Compaction, K::CompactionCompleted));
    }
    assert!(!provider.accepts_event_kind(K::SessionEnd, event.kind));
    assert!(!provider.accepts_event_kind(K::TurnEnd, K::SessionStart));
    assert!(provider.parse_hook_event("FutureEvent", &envelope).is_err());
}

#[test]
#[ignore = "operator-local Codex acceptance; never run in CI"]
fn cx01_installer_has_twelve_source_pinned_trusted_handlers_and_preserves_user_bytes() {
    use serde_json::json;
    use sha2::{Digest, Sha256};
    let fixture = LocalCodexSession::new();
    let home = fixture.home.join(".codex");
    let hooks_path = home.join("hooks.json");
    let config_path = home.join("config.toml");
    let user_key = format!("{}:pre_tool_use:0:0", hooks_path.display());
    let user_config = format!(
        "# cx01 user bytes\ncustom_metric = 17\n\n[projects.\"user-project\"]\ntrust_level = \"trusted\"\n\n[hooks.state.\"{user_key}\"]\nenabled = false\ntrusted_hash = \"sha256:user-trust\"\n"
    );
    std::fs::write(&config_path, &user_config).unwrap();
    let user_hook = json!({"matcher":"Shell", "hooks":[{"type":"command","command":"user-observer","timeout":7}]});
    std::fs::write(
        &hooks_path,
        serde_json::to_vec(&json!({"hooks":{"PreToolUse":[user_hook.clone()]}})).unwrap(),
    )
    .unwrap();
    let result = fixture.run(&["agent", "enable", "--agent", "codex"]);
    assert!(
        result.status.success(),
        "{}",
        String::from_utf8_lossy(&result.stderr)
    );
    let hooks_bytes = std::fs::read(&hooks_path).unwrap();
    let config_bytes = std::fs::read(&config_path).unwrap();
    assert!(
        std::str::from_utf8(&config_bytes)
            .unwrap()
            .starts_with(&user_config)
    );
    let hooks: serde_json::Value = serde_json::from_slice(&hooks_bytes).unwrap();
    assert_eq!(hooks["hooks"]["PreToolUse"][0], user_hook);
    let config: toml::Value = toml::from_str(std::str::from_utf8(&config_bytes).unwrap()).unwrap();
    let expected = [
        ("SessionStart", "session_start", "session-start"),
        ("SessionEnd", "session_end", "session-end"),
        ("UserPromptSubmit", "user_prompt_submit", "prompt"),
        ("PreToolUse", "pre_tool_use", "tool-use"),
        ("PostToolUse", "post_tool_use", "tool-use"),
        (
            "PermissionRequest",
            "permission_request",
            "permission-request",
        ),
        ("PreCompact", "pre_compact", "compaction"),
        ("PostCompact", "post_compact", "compaction"),
        ("Stop", "stop", "stop"),
        ("Interrupt", "interrupt", "stop"),
        ("SubagentStart", "subagent_start", "subagent-start"),
        ("SubagentStop", "subagent_stop", "subagent-end"),
    ];
    assert_eq!(hooks["hooks"].as_object().unwrap().len(), expected.len());
    assert_eq!(
        config["hooks"]["state"].as_table().unwrap().len(),
        expected.len() + 1
    );
    assert_eq!(
        config["hooks"]["state"][&user_key]["enabled"].as_bool(),
        Some(false)
    );
    assert_eq!(
        config["hooks"]["state"][&user_key]["trusted_hash"].as_str(),
        Some("sha256:user-trust")
    );
    let binary = std::fs::canonicalize(env!("CARGO_BIN_EXE_libra")).unwrap();
    for (event, snake, verb) in expected {
        let short = matches!(event, "Interrupt" | "SessionEnd");
        let (timeout, budget) = if short { (3, 2_000) } else { (30, 29_000) };
        let groups = hooks["hooks"][event].as_array().unwrap();
        let mut matched = 0;
        for (group_index, group) in groups.iter().enumerate() {
            for (handler_index, handler) in group["hooks"].as_array().unwrap().iter().enumerate() {
                if handler["statusMessage"] != "libra capture" {
                    continue;
                }
                matched += 1;
                assert_eq!(handler["timeout"], timeout);
                assert!(group.get("matcher").is_none());
                let command = format!(
                    "{} hooks codex {verb} --capture-budget-ms {budget}",
                    binary.display()
                );
                assert_eq!(handler["command"], command);
                // Independent upstream canonical vector; no installer identity helper.
                let canonical = json!({"event_name":snake,"hooks":[{
                    "type":"command","command":command,"timeout":timeout,
                    "async":false,"statusMessage":"libra capture"
                }]})
                .to_string();
                let expected_hash = format!(
                    "sha256:{}",
                    hex::encode(Sha256::digest(canonical.as_bytes()))
                );
                let key = format!(
                    "{}:{snake}:{group_index}:{handler_index}",
                    hooks_path.display()
                );
                assert_eq!(
                    config["hooks"]["state"][&key]["trusted_hash"].as_str(),
                    Some(expected_hash.as_str())
                );
                assert_ne!(
                    config["hooks"]["state"][&key]["enabled"].as_bool(),
                    Some(false)
                );
            }
        }
        assert_eq!(matched, 1, "{event}");
    }
    assert!(
        fixture
            .run(&["agent", "enable", "--agent", "codex"])
            .status
            .success()
    );
    assert_eq!(std::fs::read(&hooks_path).unwrap(), hooks_bytes);
    assert_eq!(std::fs::read(&config_path).unwrap(), config_bytes);
    assert!(
        fixture
            .run(&["agent", "disable", "--agent", "codex"])
            .status
            .success()
    );
    assert_eq!(std::fs::read_to_string(&config_path).unwrap(), user_config);
    let after: serde_json::Value =
        serde_json::from_slice(&std::fs::read(hooks_path).unwrap()).unwrap();
    assert_eq!(after["hooks"]["PreToolUse"], json!([user_hook]));
    assert!(after["hooks"].get("Interrupt").is_none());
}

impl LocalCodexSession {
    fn cx01_hook(&self, event: &str, verb: &str) -> std::process::Output {
        use std::io::Write;
        let payload = serde_json::json!({
            "hook_event_name":event,"session_id":"123e4567-e89b-12d3-a456-426614174031",
            "cwd":self.repo,"turn_id":"cx01-turn", "transcript_path":self.source,
            "model":"cx01-model","permission_mode":"default"
        });
        let mut child = std::process::Command::new(env!("CARGO_BIN_EXE_libra"))
            .args(["hooks", "codex", verb, "--capture-budget-ms", "2000"])
            .current_dir(&self.repo)
            .env_clear()
            .env("PATH", "/usr/bin:/bin")
            .env("HOME", &self.home)
            .env("LIBRA_TEST_HOME", &self.home)
            .env("LIBRA_TEST", "1")
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .spawn()
            .unwrap();
        child
            .stdin
            .take()
            .unwrap()
            .write_all(payload.to_string().as_bytes())
            .unwrap();
        child.wait_with_output().unwrap()
    }
}

#[tokio::test]
#[ignore = "operator-local Codex acceptance; never run in CI"]
async fn cx01_interrupt_and_postcompact_reach_shared_pipeline_without_committing_a_turn() {
    use sea_orm::{ConnectionTrait, Statement};
    let fixture = LocalCodexSession::new();
    assert!(
        fixture
            .run(&["agent", "enable", "--agent", "codex"])
            .status
            .success()
    );
    fixture.source(1, true);
    let result = fixture.cx01_hook("SessionStart", "session-start");
    assert!(
        result.status.success(),
        "{}",
        String::from_utf8_lossy(&result.stderr)
    );
    assert!(result.stdout.is_empty());
    assert!(
        result.stderr.is_empty(),
        "{}",
        String::from_utf8_lossy(&result.stderr)
    );
    let db = fixture.db().await;
    assert_eq!(LocalCodexSession::count(&db, "agent_session", "1").await, 1);
    assert_eq!(
        LocalCodexSession::count(
            &db,
            "agent_session",
            "state='active' AND stopped_at IS NULL"
        )
        .await,
        1
    );
    assert_eq!(
        LocalCodexSession::count(&db, "agent_checkpoint", "1").await,
        0
    );
    async fn revision(db: &sea_orm::DatabaseConnection) -> i64 {
        db.query_one_raw(Statement::from_string(
            db.get_database_backend(),
            "SELECT sync_revision FROM agent_session".to_owned(),
        ))
        .await
        .unwrap()
        .unwrap()
        .try_get_by_index(0)
        .unwrap()
    }
    let session_start_revision = revision(&db).await;
    let first = fixture.cx01_hook("Interrupt", "stop");
    assert!(first.status.success());
    assert!(first.stdout.is_empty());
    assert!(
        first.stderr.is_empty(),
        "{}",
        String::from_utf8_lossy(&first.stderr)
    );
    assert!(
        revision(&db).await > session_start_revision,
        "the first Interrupt must be durably applied, not silently skipped"
    );
    assert_eq!(
        LocalCodexSession::count(&db, "agent_checkpoint", "1").await,
        0
    );
    let before = revision(&db).await;
    let result = fixture.cx01_hook("Interrupt", "stop");
    assert!(result.status.success());
    assert_eq!(
        revision(&db).await,
        before,
        "native replay must not append a second interrupted event"
    );
    let result = fixture.cx01_hook("PostCompact", "compaction");
    assert!(result.status.success());
    assert!(
        revision(&db).await > before,
        "PostCompact must pass kind validation and persist its event"
    );
    assert_eq!(
        LocalCodexSession::count(&db, "agent_checkpoint", "1").await,
        0
    );
    assert_eq!(
        LocalCodexSession::count(
            &db,
            "agent_session",
            "state='active' AND stopped_at IS NULL"
        )
        .await,
        1
    );
    let interrupted_revision = revision(&db).await;
    let stop = fixture.cx01_hook("Stop", "stop");
    assert!(
        stop.status.success(),
        "{}",
        String::from_utf8_lossy(&stop.stderr)
    );
    assert!(revision(&db).await > interrupted_revision);
    assert!(
        LocalCodexSession::count(&db, "agent_checkpoint", "1").await > 0,
        "Stop with the same native turn_id must not deduplicate against Interrupt"
    );
    db.close().await.unwrap();
}

#[test]
#[ignore = "operator-local Codex acceptance; never run in CI"]
fn cx01_eleven_event_upgrade_preserves_existing_trust_and_only_adds_interrupt() {
    let fixture = LocalCodexSession::new();
    assert!(
        fixture
            .run(&["agent", "enable", "--agent", "codex"])
            .status
            .success()
    );
    let hooks_path = fixture.home.join(".codex/hooks.json");
    let config_path = fixture.home.join(".codex/config.toml");
    let mut old_hooks: serde_json::Value =
        serde_json::from_slice(&std::fs::read(&hooks_path).unwrap()).unwrap();
    old_hooks["hooks"]
        .as_object_mut()
        .unwrap()
        .remove("Interrupt");
    std::fs::write(&hooks_path, serde_json::to_vec_pretty(&old_hooks).unwrap()).unwrap();
    // Seed the exact prior eleven-event shape with the same installed binary
    // path. Preserve the generated ownership markers and every prior hash.
    let config = std::fs::read_to_string(&config_path).unwrap();
    let header = format!("[hooks.state.\"{}:interrupt:0:0\"]", hooks_path.display());
    let at = config.find(&header).unwrap();
    let marker = "# libra-managed codex hook trust entry";
    let start = config[..at].rfind(marker).unwrap();
    let end = config[at..]
        .find(&format!("\n{marker}"))
        .map(|offset| at + offset + 1)
        .unwrap_or(config.len());
    let old_config = format!("{}{}", &config[..start], &config[end..]);
    std::fs::write(&config_path, &old_config).unwrap();
    let before: toml::Value = toml::from_str(&old_config).unwrap();
    assert_eq!(before["hooks"]["state"].as_table().unwrap().len(), 11);
    let doctor = fixture.run(&["agent", "doctor", "--json"]);
    assert!(doctor.status.success());
    let status: serde_json::Value = serde_json::from_slice(&doctor.stdout).unwrap();
    let codex = status["data"]["provider_hooks"]
        .as_array()
        .unwrap()
        .iter()
        .find(|row| row["name"] == "codex")
        .unwrap();
    assert_eq!(
        codex["installed"], false,
        "an old eleven-event install needs refresh"
    );
    let callback = fixture.cx01_hook("SessionStart", "session-start");
    assert!(callback.status.success());
    assert!(
        callback.stderr.is_empty(),
        "existing approved handlers must not become trust gaps"
    );
    assert!(
        fixture
            .run(&["agent", "enable", "--agent", "codex"])
            .status
            .success()
    );
    let mut after: serde_json::Value =
        serde_json::from_slice(&std::fs::read(&hooks_path).unwrap()).unwrap();
    assert!(
        after["hooks"]
            .as_object_mut()
            .unwrap()
            .remove("Interrupt")
            .is_some()
    );
    assert_eq!(
        after, old_hooks,
        "all eleven old handler values must be unchanged"
    );
    let after: toml::Value =
        toml::from_str(&std::fs::read_to_string(config_path).unwrap()).unwrap();
    assert_eq!(after["hooks"]["state"].as_table().unwrap().len(), 12);
    for (key, old) in before["hooks"]["state"].as_table().unwrap() {
        assert_eq!(
            after["hooks"]["state"][key], *old,
            "unchanged prior trust {key}"
        );
    }
}

impl LocalCodexSession {
    fn cx01_bounded_command(&self, args: &[&str]) -> std::process::Output {
        let mut child = std::process::Command::new(env!("CARGO_BIN_EXE_libra"))
            .current_dir(&self.repo)
            .env_clear()
            .env("PATH", "/usr/bin:/bin")
            .env("HOME", &self.home)
            .env("LIBRA_TEST_HOME", &self.home)
            .env("LIBRA_TEST", "1")
            .args(args)
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .spawn()
            .unwrap();
        use std::io::Read;
        let mut stdout = child.stdout.take().unwrap();
        let mut stderr = child.stderr.take().unwrap();
        let stdout = std::thread::spawn(move || {
            let mut bytes = Vec::new();
            stdout.read_to_end(&mut bytes).unwrap();
            bytes
        });
        let stderr = std::thread::spawn(move || {
            let mut bytes = Vec::new();
            stderr.read_to_end(&mut bytes).unwrap();
            bytes
        });
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(3);
        let status = loop {
            if let Some(status) = child.try_wait().unwrap() {
                break status;
            }
            if std::time::Instant::now() >= deadline {
                child.kill().unwrap();
                child.wait().unwrap();
                stdout.join().unwrap();
                stderr.join().unwrap();
                panic!("Codex settings command blocked on a special file: {args:?}");
            }
            std::thread::sleep(std::time::Duration::from_millis(10));
        };
        std::process::Output {
            status,
            stdout: stdout.join().unwrap(),
            stderr: stderr.join().unwrap(),
        }
    }
}

#[cfg(unix)]
#[test]
#[ignore = "operator-local Codex acceptance; never run in CI"]
fn cx01_settings_special_files_are_refused_without_following_or_blocking() {
    use std::os::unix::{ffi::OsStrExt, fs::symlink, net::UnixListener};
    for name in [
        "hooks.json",
        "config.toml",
        ".libra-codex-hooks.transaction.json",
        ".libra-codex-hooks.lock",
    ] {
        for kind in ["symlink", "dangling-symlink", "directory", "fifo", "socket"] {
            let temp = tempfile::Builder::new()
                .prefix("cx-")
                .tempdir_in("/tmp")
                .unwrap();
            let fixture = LocalCodexSession::with_temp(temp);
            assert!(
                fixture
                    .run(&["agent", "enable", "--agent", "codex"])
                    .status
                    .success()
            );
            let home = fixture.home.join(".codex");
            let path = home.join(name);
            if path.exists() {
                std::fs::remove_file(&path).unwrap();
            }
            let foreign = fixture._temp.path().join("foreign");
            let foreign_bytes = if name == "hooks.json" {
                b"{\"hooks\":{}}\n".as_slice()
            } else {
                b"custom_metric = 17\n".as_slice()
            };
            if kind != "dangling-symlink" {
                std::fs::write(&foreign, foreign_bytes).unwrap();
            }
            let mut listener = None;
            match kind {
                "symlink" | "dangling-symlink" => symlink(&foreign, &path).unwrap(),
                "directory" => std::fs::create_dir(&path).unwrap(),
                "fifo" => {
                    let name = std::ffi::CString::new(path.as_os_str().as_bytes()).unwrap();
                    assert_eq!(unsafe { libc::mkfifo(name.as_ptr(), 0o600) }, 0);
                }
                "socket" => listener = Some(UnixListener::bind(&path).unwrap()),
                _ => unreachable!(),
            }
            let initial_kind = std::fs::symlink_metadata(&path).unwrap().file_type();
            let peer = home.join(if name == "hooks.json" {
                "config.toml"
            } else {
                "hooks.json"
            });
            let peer_bytes = std::fs::read(&peer).unwrap();
            let paired_config = if name != "config.toml" {
                Some(std::fs::read(home.join("config.toml")).unwrap())
            } else {
                None
            };
            for command in [
                ["agent", "enable", "--agent", "codex"],
                ["agent", "disable", "--agent", "codex"],
            ] {
                let out = fixture.cx01_bounded_command(&command);
                if name == ".libra-codex-hooks.transaction.json" {
                    assert!(
                        String::from_utf8_lossy(&out.stderr)
                            .contains("regular file without symlinks"),
                        "journal refusal must occur before parsing foreign content"
                    );
                }
                if let Some(before) = &paired_config {
                    assert_eq!(std::fs::read(home.join("config.toml")).unwrap(), *before);
                }
                assert!(
                    !out.status.success(),
                    "{name}/{kind} must fail closed: {:?}",
                    out.status
                );
                assert_eq!(
                    std::fs::symlink_metadata(&path).unwrap().file_type(),
                    initial_kind
                );
                assert_eq!(
                    std::fs::read(&peer).unwrap(),
                    peer_bytes,
                    "{name}/{kind} changed the other settings file"
                );
            }
            let _ = fixture.cx01_bounded_command(&["agent", "doctor", "--json"]);
            assert_eq!(std::fs::read(&peer).unwrap(), peer_bytes);
            if let Some(before) = &paired_config {
                assert_eq!(std::fs::read(home.join("config.toml")).unwrap(), *before);
            }
            if kind == "dangling-symlink" {
                assert!(!foreign.exists());
            } else {
                assert_eq!(std::fs::read(&foreign).unwrap(), foreign_bytes);
            }
            assert_eq!(
                std::fs::symlink_metadata(&path).unwrap().file_type(),
                initial_kind
            );
            drop(listener);
        }
    }
}

#[test]
#[ignore = "operator-local Codex acceptance; never run in CI"]
fn cx01_settings_and_journal_caps_and_parse_errors_preserve_configuration() {
    for (name, cap) in [
        ("hooks.json", 16 * 1024 * 1024),
        ("config.toml", 16 * 1024 * 1024),
        (".libra-codex-hooks.transaction.json", 64 * 1024),
    ] {
        let fixture = LocalCodexSession::new();
        assert!(
            fixture
                .run(&["agent", "enable", "--agent", "codex"])
                .status
                .success()
        );
        let home = fixture.home.join(".codex");
        let path = home.join(name);
        let bytes = vec![b' '; cap + 1];
        std::fs::write(&path, &bytes).unwrap();
        let peer = home.join(if name == "hooks.json" {
            "config.toml"
        } else {
            "hooks.json"
        });
        let peer_bytes = std::fs::read(&peer).unwrap();
        let paired_config = (name == ".libra-codex-hooks.transaction.json")
            .then(|| std::fs::read(home.join("config.toml")).unwrap());
        for command in [
            ["agent", "enable", "--agent", "codex"],
            ["agent", "disable", "--agent", "codex"],
        ] {
            let out = fixture.cx01_bounded_command(&command);
            assert!(!out.status.success());
            assert!(String::from_utf8_lossy(&out.stderr).contains("read limit"));
            assert_eq!(std::fs::read(&path).unwrap(), bytes);
            assert_eq!(std::fs::read(&peer).unwrap(), peer_bytes);
            if let Some(before) = &paired_config {
                assert_eq!(std::fs::read(home.join("config.toml")).unwrap(), *before);
            }
        }
    }
    // Even a valid input at the cap must not cause a too-large paired rewrite.
    let fixture = LocalCodexSession::new();
    assert!(
        fixture
            .run(&["agent", "enable", "--agent", "codex"])
            .status
            .success()
    );
    let config = fixture.home.join(".codex/config.toml");
    let hooks = fixture.home.join(".codex/hooks.json");
    let hooks_before = std::fs::read(&hooks).unwrap();
    let at_cap = vec![b' '; 16 * 1024 * 1024];
    std::fs::write(&config, &at_cap).unwrap();
    let out = fixture.cx01_bounded_command(&["agent", "enable", "--agent", "codex"]);
    assert!(!out.status.success());
    assert!(String::from_utf8_lossy(&out.stderr).contains("config.toml would exceed"));
    assert_eq!(std::fs::read(&config).unwrap(), at_cap);
    assert_eq!(std::fs::read(&hooks).unwrap(), hooks_before);
    assert!(
        !fixture
            .home
            .join(".codex/.libra-codex-hooks.transaction.json")
            .exists()
    );
    let fixture = LocalCodexSession::new();
    assert!(
        fixture
            .run(&["agent", "enable", "--agent", "codex"])
            .status
            .success()
    );
    let config = fixture.home.join(".codex/config.toml");
    let hooks = fixture.home.join(".codex/hooks.json");
    let config_before = std::fs::read(&config).unwrap();
    let at_cap = serde_json::to_vec(
        &serde_json::json!({"hooks":{},"padding":"a".repeat(16 * 1024 * 1024 - 64)}),
    )
    .unwrap();
    assert!(at_cap.len() < 16 * 1024 * 1024);
    std::fs::write(&hooks, &at_cap).unwrap();
    let out = fixture.cx01_bounded_command(&["agent", "enable", "--agent", "codex"]);
    assert!(!out.status.success());
    assert!(String::from_utf8_lossy(&out.stderr).contains("hooks.json would exceed"));
    assert_eq!(std::fs::read(&hooks).unwrap(), at_cap);
    assert_eq!(std::fs::read(&config).unwrap(), config_before);
    assert!(
        !fixture
            .home
            .join(".codex/.libra-codex-hooks.transaction.json")
            .exists()
    );
    let fingerprint = serde_json::json!({"present":false,"digest":"0".repeat(64)});
    let mut journal = serde_json::json!({"version":2,"phase":"prepared","operation":"install_or_refresh",
        "hooks_before":fingerprint,"hooks_after":fingerprint,"config_before":fingerprint,"config_after":fingerprint,"manual_recovery":false});
    journal["hooks_before"]["digest"] = serde_json::json!("CX01-SETTINGS-ERROR-CANARY");
    let fixture = LocalCodexSession::new();
    let path = fixture
        .home
        .join(".codex/.libra-codex-hooks.transaction.json");
    let bytes = serde_json::to_vec(&journal).unwrap();
    std::fs::write(&path, &bytes).unwrap();
    let out = fixture.cx01_bounded_command(&["agent", "enable", "--agent", "codex"]);
    assert!(!out.status.success());
    assert!(String::from_utf8_lossy(&out.stderr).contains("invalid hooks-before digest"));
    assert!(!String::from_utf8_lossy(&out.stderr).contains("CX01-SETTINGS-ERROR-CANARY"));
    assert_eq!(std::fs::read(&path).unwrap(), bytes);
    for (name, bad) in [
        (
            "config.toml",
            b"user_secret = \"CX01-SETTINGS-ERROR-CANARY\" extra".as_slice(),
        ),
        ("hooks.json", b"\"CX01-SETTINGS-ERROR-CANARY\"".as_slice()),
        (
            ".libra-codex-hooks.transaction.json",
            b"\"CX01-SETTINGS-ERROR-CANARY\"".as_slice(),
        ),
    ] {
        let fixture = LocalCodexSession::new();
        let path = fixture.home.join(".codex").join(name);
        std::fs::write(&path, bad).unwrap();
        let out = fixture.cx01_bounded_command(&["agent", "enable", "--agent", "codex"]);
        assert!(!out.status.success());
        assert!(String::from_utf8_lossy(&out.stderr).contains("invalid Codex"));
        assert!(!String::from_utf8_lossy(&out.stderr).contains("CX01-SETTINGS-ERROR-CANARY"));
        assert_eq!(std::fs::read(&path).unwrap(), bad);
    }
}

#[cfg(unix)]
#[test]
#[ignore = "operator-local Codex acceptance; never run in CI"]
fn cx01_settings_parent_requires_only_search_permission_on_ancestors() {
    use std::os::unix::fs::PermissionsExt;
    struct Restore(std::path::PathBuf, std::fs::Permissions);
    impl Drop for Restore {
        fn drop(&mut self) {
            let _ = std::fs::set_permissions(&self.0, self.1.clone());
        }
    }
    let root = tempfile::tempdir().unwrap();
    let ancestor = root.path().join("search-only");
    std::fs::create_dir(&ancestor).unwrap();
    let fixture = LocalCodexSession::with_temp(tempfile::tempdir_in(&ancestor).unwrap());
    let restore = Restore(
        ancestor.clone(),
        std::fs::metadata(&ancestor).unwrap().permissions(),
    );
    std::fs::set_permissions(&ancestor, std::fs::Permissions::from_mode(0o100)).unwrap();
    assert_eq!(
        std::fs::read_dir(&ancestor).unwrap_err().kind(),
        std::io::ErrorKind::PermissionDenied,
        "the current user must have search permission without read permission"
    );
    let out = fixture.cx01_bounded_command(&["agent", "enable", "--agent", "codex"]);
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let out = fixture.cx01_bounded_command(&["agent", "disable", "--agent", "codex"]);
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    drop(restore);
}

#[test]
#[ignore = "operator-local Codex acceptance; never run in CI"]
fn cx01_doctor_distinguishes_managed_configuration_states_without_writing() {
    fn status(fixture: &LocalCodexSession) -> serde_json::Value {
        let out = fixture.cx01_bounded_command(&["agent", "doctor", "--json"]);
        assert!(
            out.status.success(),
            "{}",
            String::from_utf8_lossy(&out.stderr)
        );
        let report: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
        report["data"]["provider_hooks"]
            .as_array()
            .unwrap()
            .iter()
            .find(|row| row["name"] == "codex")
            .unwrap()
            .clone()
    }
    let fixture = LocalCodexSession::new();
    let missing = status(&fixture);
    assert_eq!(missing["installed"], false);
    assert_eq!(missing["events"].as_array().unwrap().len(), 12);
    assert!(
        missing["events"]
            .as_array()
            .unwrap()
            .iter()
            .all(|row| row["reason"] == "missing")
    );
    assert!(
        fixture
            .run(&["agent", "enable", "--agent", "codex"])
            .status
            .success()
    );
    let hooks_path = fixture.home.join(".codex/hooks.json");
    let config_path = fixture.home.join(".codex/config.toml");
    let original_hooks = std::fs::read(&hooks_path).unwrap();
    let original_config = std::fs::read(&config_path).unwrap();
    for case in [
        "configured",
        "missing",
        "drifted",
        "untrusted",
        "disabled",
        "missing-config",
        "async",
        "duplicate",
        "wrong-enabled-type",
    ] {
        let mut hooks: serde_json::Value = serde_json::from_slice(&original_hooks).unwrap();
        let mut config: toml::Value =
            toml::from_str(std::str::from_utf8(&original_config).unwrap()).unwrap();
        let key = format!("{}:interrupt:0:0", hooks_path.display());
        let expected = match case {
            "configured" => "configured",
            "missing" => {
                hooks["hooks"].as_object_mut().unwrap().remove("Interrupt");
                "missing"
            }
            "drifted" => {
                hooks["hooks"]["Interrupt"][0]["hooks"][0]["timeout"] = serde_json::json!(4);
                "drifted"
            }
            "untrusted" => {
                config["hooks"]["state"][&key]["trusted_hash"] =
                    toml::Value::String("CX01-DOCTOR-HASH-CANARY".into());
                "untrusted"
            }
            "disabled" => {
                config["hooks"]["state"][&key]["enabled"] = toml::Value::Boolean(false);
                "disabled"
            }
            "missing-config" => "untrusted",
            "async" => {
                hooks["hooks"]["Interrupt"][0]["hooks"][0]["async"] = serde_json::json!(true);
                "drifted"
            }
            "duplicate" => {
                let mut stale = hooks["hooks"]["Interrupt"][0]["hooks"][0].clone();
                stale["command"] = serde_json::json!("printf CX01-DOCTOR-COMMAND-CANARY");
                hooks["hooks"]["Interrupt"][0]["hooks"]
                    .as_array_mut()
                    .unwrap()
                    .push(stale);
                "drifted"
            }
            "wrong-enabled-type" => {
                config["hooks"]["state"][&key]["enabled"] = toml::Value::String("false".into());
                "drifted"
            }
            _ => unreachable!(),
        };
        let hooks_bytes = serde_json::to_vec_pretty(&hooks).unwrap();
        let config_bytes = toml::to_string(&config).unwrap().into_bytes();
        std::fs::write(&hooks_path, &hooks_bytes).unwrap();
        if case == "missing-config" {
            std::fs::remove_file(&config_path).unwrap();
        } else {
            std::fs::write(&config_path, &config_bytes).unwrap();
        }
        let report = status(&fixture);
        assert_eq!(report["installed"], case == "configured", "{case}");
        assert_eq!(report["error"], serde_json::Value::Null);
        let events = report["events"].as_array().unwrap();
        assert_eq!(events.len(), 12, "{case}");
        let event = events
            .iter()
            .find(|row| row["event"] == "Interrupt")
            .unwrap();
        assert_eq!(event["reason"], expected, "{case}");
        assert_eq!(event["installed"], expected == "configured", "{case}");
        if case == "missing-config" {
            assert!(events.iter().all(|row| row["reason"] == "untrusted"));
        } else {
            assert!(
                events
                    .iter()
                    .filter(|row| row["event"] != "Interrupt")
                    .all(|row| row["reason"] == "configured")
            );
        }
        let text = report.to_string();
        assert!(!text.contains("CX01-DOCTOR-HASH-CANARY"));
        assert!(!text.contains("CX01-DOCTOR-COMMAND-CANARY"));
        assert!(!text.contains("sha256:"));
        assert!(!{
            let needle = hooks_path.to_string_lossy().to_string();
            text.contains(&needle)
        });
        assert_eq!(
            std::fs::read(&hooks_path).unwrap(),
            hooks_bytes,
            "doctor wrote hooks: {case}"
        );
        if case == "missing-config" {
            assert!(!config_path.exists());
        } else {
            assert_eq!(
                std::fs::read(&config_path).unwrap(),
                config_bytes,
                "doctor wrote config: {case}"
            );
        }
        if matches!(case, "missing" | "untrusted" | "missing-config" | "async") {
            let refreshed = fixture.run(&["agent", "enable", "--agent", "codex"]);
            assert!(
                refreshed.status.success(),
                "{}",
                String::from_utf8_lossy(&refreshed.stderr)
            );
            let refreshed = status(&fixture);
            assert_eq!(
                refreshed["installed"], true,
                "refresh did not converge: {case}"
            );
            assert!(
                refreshed["events"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .all(|row| row["reason"] == "configured")
            );
        } else if case == "disabled" {
            let removed = fixture.run(&["agent", "disable", "--agent", "codex"]);
            assert!(
                removed.status.success(),
                "{}",
                String::from_utf8_lossy(&removed.stderr)
            );
            let removed = status(&fixture);
            assert!(
                removed["events"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .all(|row| row["reason"] == "missing"),
                "disabled entries must still be removable"
            );
        }
        std::fs::write(&config_path, &original_config).unwrap();
    }
    // Doctor must still inspect malformed trust even when the bool status
    // short-circuits stale templates. Disable retains its provider-local
    // zero-write error contract rather than reporting an inspection error.
    let mut stale: serde_json::Value = serde_json::from_slice(&original_hooks).unwrap();
    stale["hooks"]["SessionStart"][0]["hooks"][0]["command"] =
        serde_json::json!("/stale/libra hooks codex session-start --capture-budget-ms 29000");
    let stale = serde_json::to_vec_pretty(&stale).unwrap();
    let malformed = b"[hooks.state\nsecret = \"CX01-DOCTOR-PARSE-CANARY\"\n";
    std::fs::write(&hooks_path, &stale).unwrap();
    std::fs::write(&config_path, malformed).unwrap();
    let report = status(&fixture);
    assert_eq!(report["installed"], serde_json::Value::Null);
    assert!(
        report["error"]
            .as_str()
            .unwrap()
            .contains("could not be inspected")
    );
    assert!(!report.to_string().contains("CX01-DOCTOR-PARSE-CANARY"));
    let refused = fixture.run(&["agent", "disable", "--agent", "codex"]);
    assert!(!refused.status.success());
    let error = String::from_utf8_lossy(&refused.stderr);
    assert!(error.contains("failed to disable 'codex'"));
    assert!(error.contains("invalid Codex config TOML"));
    assert!(!error.contains("CX01-DOCTOR-PARSE-CANARY"));
    assert_eq!(std::fs::read(&hooks_path).unwrap(), stale);
    assert_eq!(std::fs::read(&config_path).unwrap(), malformed);
}

fn rollout_jsonl(records: &[serde_json::Value]) -> Vec<u8> {
    let mut data = Vec::new();
    for record in records {
        serde_json::to_writer(&mut data, record).unwrap();
        data.push(b'\n');
    }
    data
}

fn rollout_usage(input: u64, output: u64) -> serde_json::Value {
    serde_json::json!({"input_tokens":input,"cached_input_tokens":0,
        "output_tokens":output,"reasoning_output_tokens":0,"total_tokens":input+output})
}

fn rollout_response_usage(response: &str, input: u64, output: u64) -> serde_json::Value {
    serde_json::json!({"thread_id":"thread-a","session_id":"session-a","turn_id":"turn-a",
        "root_turn_id":"turn-a","response_id":response,"usage":rollout_usage(input,output),
        "turn_token_usage":rollout_usage(input*10,output*10),
        "thread_token_usage":rollout_usage(input*100,output*100)})
}

#[test]
#[ignore = "operator-local Codex acceptance; never run in CI"]
fn codex_rollout_current_prompt_model_and_identity_mirrors() {
    use libra::internal::ai::observed_agents::extract::extract_codex;
    use serde_json::json;
    let records = [
        json!({"type":"session_meta","payload":{"id":"thread-a","session_id":"session-a","cwd":"/repo"}}),
        json!({"type":"turn_context","payload":{"turn_id":"turn-a","model":"codex-source-model","cwd":"/repo"}}),
        json!({"type":"response_item","payload":{"type":"message","id":"user-a","role":"user","content":[{"type":"input_text","text":"/review <system-reminder>literal user text</system-reminder>"}],"internal_chat_message_metadata_passthrough":{"turn_id":"turn-a","content_item_kinds":["user.text"]}}}),
        json!({"type":"event_msg","payload":{"type":"item_completed","thread_id":"thread-a","turn_id":"turn-a","item":{"type":"UserMessage","id":"user-a","content":[{"type":"text","text":"/review <system-reminder>literal user text</system-reminder>"}]}}}),
        json!({"type":"event_msg","payload":{"type":"item_completed","thread_id":"thread-a","turn_id":"turn-a","item":{"type":"UserMessage","id":"user-b","content":[{"type":"text","text":"/review <system-reminder>literal user text</system-reminder>"}]}}}),
    ];
    let out = extract_codex(&rollout_jsonl(&records));
    assert_eq!(
        out.prompts,
        ["/review <system-reminder>literal user text</system-reminder>"; 2]
    );
    assert_eq!(out.model.as_deref(), Some("codex-source-model"));
    assert_eq!(out.skill_events.len(), 2);
    assert_eq!(out.usage, None, "missing usage is unknown, never zero");
    assert!(!out.partial, "{:?}", out.warnings);
}

#[test]
#[ignore = "operator-local Codex acceptance; never run in CI"]
fn codex_rollout_source_annotations_exclude_context_without_text_prefix_guessing() {
    use libra::internal::ai::observed_agents::extract::extract_codex;
    use serde_json::json;
    let mut records =
        vec![json!({"type":"turn_context","payload":{"turn_id":"turn-a","model":"m"}})];
    for (id, kind, text) in [
        (
            "context",
            "environments.instructions",
            "CX04-HIDDEN-CONTEXT",
        ),
        (
            "user",
            "user.text",
            "<environment_context>real user asks this</environment_context>",
        ),
        (
            "unknown",
            "unknown",
            "<system-reminder>unclassified user</system-reminder>",
        ),
    ] {
        records.push(json!({"type":"response_item","payload":{"type":"message","role":"user","id":id,"content":[{"type":"input_text","text":text}],"internal_chat_message_metadata_passthrough":{"content_item_kinds":[kind]}}}));
    }
    let out = extract_codex(&rollout_jsonl(&records));
    assert_eq!(
        out.prompts,
        [
            "<environment_context>real user asks this</environment_context>",
            "<system-reminder>unclassified user</system-reminder>"
        ]
    );
    assert!(!out.warnings.join(" ").contains("CX04-HIDDEN-CONTEXT"));
}

#[test]
#[ignore = "operator-local Codex acceptance; never run in CI"]
fn codex_rollout_exact_usage_deduplicates_response_and_compaction_snapshot() {
    use libra::internal::ai::observed_agents::extract::extract_codex;
    use serde_json::json;
    let first = rollout_response_usage("response-a", 10, 2);
    let out = extract_codex(&rollout_jsonl(&[
        json!({"type":"token_usage_record","payload":first}),
        json!({"type":"token_usage_record","payload":rollout_response_usage("response-b",20,3)}),
        json!({"type":"compacted","payload":{"message":"summary","latest_token_usage_record":first}}),
        json!({"type":"token_usage_record","payload":first}),
    ]));
    let usage = out.usage.unwrap();
    assert_eq!(
        (usage.input_tokens, usage.output_tokens, usage.total_tokens),
        (30, 5, Some(35))
    );
    assert_eq!(out.api_call_count, 2);
    assert!(!out.partial, "{:?}", out.warnings);
}

#[test]
#[ignore = "operator-local Codex acceptance; never run in CI"]
fn codex_rollout_legacy_cumulative_usage_is_differenced_across_turns() {
    use libra::internal::ai::observed_agents::extract::extract_codex;
    use serde_json::json;
    let counts = |input, output| json!({"type":"event_msg","payload":{"type":"token_count","info":{"total_token_usage":rollout_usage(input,output),"last_token_usage":rollout_usage(input,output)}}});
    let out = extract_codex(&rollout_jsonl(&[
        json!({"type":"session_meta","payload":{"id":"thread-a"}}),
        counts(10, 2),
        counts(10, 2),
        counts(30, 5),
        json!({"type":"event_msg","payload":{"type":"task_started","turn_id":"turn-b"}}),
        counts(40, 7),
    ]));
    let usage = out.usage.unwrap();
    assert_eq!((usage.input_tokens, usage.output_tokens), (40, 7));
    assert_eq!(out.api_call_count, 3);
    assert!(!out.partial, "{:?}", out.warnings);
}

#[test]
#[ignore = "operator-local Codex acceptance; never run in CI"]
fn codex_rollout_counter_resets_require_explicit_context_and_overflow_is_partial() {
    use libra::internal::ai::observed_agents::extract::extract_codex;
    use serde_json::json;
    let counts = |input, output| json!({"type":"event_msg","payload":{"type":"token_count","info":{"total_token_usage":rollout_usage(input,output)}}});
    let out = extract_codex(&rollout_jsonl(&[counts(10, 2), counts(4, 1)]));
    assert!(
        out.partial,
        "unexplained counter decrease cannot silently add or reset"
    );
    assert_eq!(out.usage.unwrap().input_tokens, 10);
    let out = extract_codex(&rollout_jsonl(&[
        counts(10, 2),
        json!({"type":"compacted","payload":{"window_id":"window-b","message":"summary"}}),
        counts(4, 1),
    ]));
    assert_eq!(out.usage.unwrap().input_tokens, 14);
    assert!(
        out.partial,
        "legacy reset is an explicitly qualified estimate"
    );
    let mut invalid = rollout_response_usage("huge", 1, 1);
    invalid["usage"]["input_tokens"] = json!(u64::MAX);
    let out = extract_codex(&rollout_jsonl(&[
        json!({"type":"token_usage_record","payload":invalid}),
    ]));
    assert!(out.partial);
    assert_eq!(out.usage, None);
}

#[test]
#[ignore = "operator-local Codex acceptance; never run in CI"]
fn codex_rollout_missing_and_invalid_usage_remain_unknown() {
    use libra::internal::ai::observed_agents::extract::extract_codex;
    use serde_json::json;
    for usage in [
        serde_json::Value::Null,
        json!({"input_tokens":2}),
        json!({"input_tokens":-1,"output_tokens":2,"total_tokens":1}),
    ] {
        let mut record = rollout_response_usage("r", 1, 1);
        record["usage"] = usage;
        let out = extract_codex(&rollout_jsonl(&[
            json!({"type":"token_usage_record","payload":record}),
        ]));
        assert_eq!(out.usage, None);
        assert!(out.partial);
    }
}

#[test]
#[ignore = "operator-local Codex acceptance; never run in CI"]
fn codex_rollout_only_successful_structured_patch_changes_become_modified_files() {
    use libra::internal::ai::observed_agents::extract::extract_codex;
    use serde_json::json;
    let patch = |id: &str, status: &str, success: bool, path: &str| json!({"type":"event_msg","payload":{"type":"patch_apply_end","call_id":id,"turn_id":"turn-a","status":status,"success":success,"changes":{path:{"type":"add","content":"body"}}}});
    let out = extract_codex(&rollout_jsonl(&[
        json!({"type":"turn_context","payload":{"turn_id":"turn-a","cwd":"/repo","model":"m"}}),
        patch("ok", "completed", true, "/repo/src/snake.rs"),
        patch("failed", "failed", false, "/repo/must-not-exist.rs"),
        patch("partial", "failed", true, "/repo/partial.rs"),
        json!({"type":"event_msg","payload":{"type":"patch_apply_end","status":"completed","success":true,"changes":{"/repo/no-call-identity.rs":{"type":"add","content":"x"}}}}),
        json!({"type":"response_item","payload":{"type":"custom_tool_call","call_id":"unexecuted","name":"apply_patch","input":"*** Add File: imaginary.rs"}}),
        json!({"type":"event_msg","payload":{"type":"item_completed","turn_id":"turn-a","item":{"type":"FileChange","id":"ok","status":"completed","changes":{"/repo/src/snake.rs":{"type":"add","content":"body"}}}}}),
    ]));
    assert_eq!(out.modified_files, ["src/snake.rs"]);
    assert!(out.partial, "failure/partial attempts remain qualified");
}

#[test]
#[ignore = "operator-local Codex acceptance; never run in CI"]
fn codex_rollout_unknown_variants_duplicate_keys_and_truncated_tail_are_qualified() {
    use libra::internal::ai::observed_agents::extract::extract_codex;
    use serde_json::json;
    let mut data = rollout_jsonl(&[
        json!({"type":"event_msg","payload":{"type":"user_message","client_id":"u","message":"keep prompt"}}),
        json!({"type":"future_variant","payload":{"secret":"CX04-MUST-NOT-ENTER-WARNING"}}),
    ]);
    data.extend_from_slice(b"{\"type\":\"turn_context\",\"payload\":{\"model\":\"first\",\"model\":\"second\"}}\n{\"type\":");
    let out = extract_codex(&data);
    assert_eq!(out.prompts, ["keep prompt"]);
    assert!(out.partial);
    assert!(
        out.model.is_none(),
        "duplicate JSON fields cannot silently choose a model"
    );
    assert!(
        !out.warnings
            .join(" ")
            .contains("CX04-MUST-NOT-ENTER-WARNING")
    );
    assert_eq!(out.usage, None);
}

#[test]
#[ignore = "operator-local Codex acceptance; never run in CI"]
fn codex_rollout_collection_and_line_budgets_return_partial_metadata_only() {
    use libra::internal::ai::observed_agents::extract::extract_codex;
    use serde_json::json;
    let mut records = vec![json!({"type":"turn_context","payload":{"turn_id":"t","model":"m"}})];
    for i in 0..5000 {
        records.push(json!({"type":"event_msg","payload":{"type":"user_message","client_id":format!("u-{i}"),"message":format!("prompt-{i}")}}));
    }
    let out = extract_codex(&rollout_jsonl(&records));
    assert!(out.partial);
    assert!(!out.prompts.is_empty());
    assert!(out.prompts.len() <= 4096);
    let mut data = rollout_jsonl(&records[..2]);
    data.extend_from_slice(&vec![b'x'; 1024 * 1024 + 1]);
    let out = extract_codex(&data);
    assert!(out.partial);
    assert_eq!(out.prompts, ["prompt-0"]);
    assert!(out.warnings.iter().all(|w| w.len() < 256));
}

#[test]
#[ignore = "operator-local Codex acceptance; never run in CI"]
fn codex_rollout_deadline_projection_obeys_the_existing_tighter_collection_limits() {
    use libra::internal::ai::{
        capture::deadline_provider_extraction_summary, observed_agents::AgentKind,
    };
    use serde_json::json;
    let records = (0..150).map(|i|json!({"type":"event_msg","payload":{"type":"user_message","client_id":format!("id-{i}"),"message":format!("p-{i}")}})).collect::<Vec<_>>();
    let out = deadline_provider_extraction_summary(AgentKind::Codex, &rollout_jsonl(&records));
    assert!(out.partial);
    assert_eq!(out.prompts.len(), 128);
    let out = deadline_provider_extraction_summary(
        AgentKind::Codex,
        &rollout_jsonl(&[json!({"type":"turn_context","payload":{"model":"m".repeat(4097)}})]),
    );
    assert!(out.partial);
    assert_eq!(out.model, None);
}

#[test]
#[ignore = "operator-local Codex acceptance; never run in CI"]
fn codex_rollout_usage_mixed_mirrors_and_new_session_are_not_added_twice() {
    use libra::internal::ai::observed_agents::extract::extract_codex;
    use serde_json::json;
    let counts = |input, output| json!({"type":"event_msg","payload":{"type":"token_count","info":{"total_token_usage":rollout_usage(input,output)}}});
    let out = extract_codex(&rollout_jsonl(&[
        counts(10, 2),
        json!({"type":"token_usage_record","payload":rollout_response_usage("r",10,2)}),
    ]));
    assert_eq!(out.usage.unwrap().input_tokens, 10);
    assert_eq!(out.api_call_count, 1);
    assert!(
        !out.partial,
        "consistent exact/cumulative mirrors are complete: {:?}",
        out.warnings
    );
    let out = extract_codex(&rollout_jsonl(&[
        json!({"type":"session_meta","payload":{"id":"thread-a"}}),
        counts(10, 2),
        json!({"type":"session_meta","payload":{"id":"thread-b"}}),
        counts(4, 1),
    ]));
    let usage = out.usage.unwrap();
    assert_eq!((usage.input_tokens, usage.output_tokens), (10, 2));
    assert_eq!(out.api_call_count, 1);
    assert!(
        out.partial,
        "foreign headers do not start a second measurable session in one rollout"
    );
}

#[test]
#[ignore = "operator-local Codex acceptance; never run in CI"]
fn codex_rollout_patch_paths_are_lexical_and_do_not_reuse_another_sessions_cwd() {
    use libra::internal::ai::observed_agents::extract::extract_codex;
    use serde_json::json;
    let out = extract_codex(&rollout_jsonl(&[
        json!({"type":"session_meta","payload":{"id":"thread-a","cwd":"/repo"}}),
        json!({"type":"event_msg","payload":{"type":"patch_apply_end","call_id":"move","status":"completed","success":true,"changes":{
            "/repo/src/old.rs":{"type":"update","unified_diff":"@@","move_path":"/repo/src/new.rs"},
            "../../escape.rs":{"type":"add","content":"x"},
            "/repo-other/escape.rs":{"type":"add","content":"x"}
        }}}),
        json!({"type":"session_meta","payload":{"id":"thread-b"}}),
        json!({"type":"event_msg","payload":{"type":"patch_apply_end","call_id":"stale-cwd","status":"completed","success":true,"changes":{"/repo/stale-context.rs":{"type":"add","content":"x"}}}}),
    ]));
    assert_eq!(out.modified_files, ["src/old.rs", "src/new.rs"]);
    assert!(out.partial);
}

#[test]
#[ignore = "operator-local Codex acceptance; never run in CI"]
fn codex_rollout_real_idless_and_unrelated_id_mirrors_keep_repeated_user_prompts() {
    use libra::internal::ai::observed_agents::extract::extract_codex;
    use serde_json::json;
    let mut records = vec![json!({"type":"turn_context","payload":{"turn_id":"t","model":"m"}})];
    for i in 0..2 {
        records.push(json!({"type":"response_item","payload":{"type":"message","role":"user","content":[{"type":"input_text","text":"repeat"}],"internal_chat_message_metadata_passthrough":{"content_item_kinds":["user.text"]}}}));
        records.push(json!({"type":"event_msg","payload":{"type":"user_message","client_id":format!("client-{i}"),"message":"repeat"}}));
        records.push(json!({"type":"event_msg","payload":{"type":"item_completed","turn_id":"t","item":{"type":"UserMessage","id":format!("item-{i}"),"content":[{"type":"text","text":"repeat"}]}}}));
    }
    let out = extract_codex(&rollout_jsonl(&records));
    assert_eq!(out.prompts, ["repeat", "repeat"]);
    assert!(
        out.partial,
        "unproven identity pairing must remain qualified"
    );
}

#[test]
#[ignore = "operator-local Codex acceptance; never run in CI"]
fn codex_rollout_unannotated_response_context_does_not_masquerade_as_a_prompt() {
    use libra::internal::ai::observed_agents::extract::extract_codex;
    use serde_json::json;
    let out = extract_codex(&rollout_jsonl(&[
        json!({"type":"response_item","payload":{"type":"message","role":"user","content":[{"type":"input_text","text":"CX04-UNPROVEN-ROLE-CONTEXT"}]}}),
        json!({"type":"response_item","payload":{"type":"message","role":"user","content":[{"type":"input_text","text":"CX04-MISMATCHED-CONTEXT"}],"internal_chat_message_metadata_passthrough":{"content_item_kinds":[]}}}),
        json!({"type":"event_msg","payload":{"type":"user_message","message":"<environment_context>actual user text</environment_context>"}}),
    ]));
    assert_eq!(
        out.prompts,
        ["<environment_context>actual user text</environment_context>"]
    );
    assert!(out.partial);
    assert!(!out.warnings.join(" ").contains("CX04-"));
}

#[test]
#[ignore = "operator-local Codex acceptance; never run in CI"]
fn codex_rollout_current_model_rate_only_cache_counts_and_foreign_threads() {
    use libra::internal::ai::observed_agents::extract::extract_codex;
    use serde_json::json;
    let mut record = rollout_response_usage("r", 10, 2);
    record["usage"]["cached_input_tokens"] = json!(4);
    record["usage"]["cache_write_input_tokens"] = json!(3);
    let out = extract_codex(&rollout_jsonl(&[
        json!({"type":"session_meta","payload":{"id":"thread-a"}}),
        json!({"type":"turn_context","payload":{"turn_id":"ta","model":"old-model"}}),
        json!({"type":"turn_context","payload":{"turn_id":"tb","model":"new-model"}}),
        json!({"type":"event_msg","payload":{"type":"token_count","info":null,"rate_limits":{}}}),
        json!({"type":"token_usage_record","payload":record}),
    ]));
    assert_eq!(out.model.as_deref(), Some("new-model"));
    let unexpected = extract_codex(&rollout_jsonl(&[
        json!({"type":"session_meta","payload":{"id":"thread-a","model":"not-a-turn-context-model"}}),
    ]));
    assert!(unexpected.model.is_none());
    assert_eq!(out.usage.unwrap().cached_tokens, Some(7));
    assert!(!out.partial, "{:?}", out.warnings);
    let out = extract_codex(&rollout_jsonl(&[
        json!({"type":"session_meta","payload":{"id":"root-thread"}}),
        json!({"type":"token_usage_record","payload":record}),
        json!({"type":"event_msg","payload":{"type":"item_completed","thread_id":"child-thread","turn_id":"t","item":{"type":"UserMessage","id":"child-u","content":[{"type":"text","text":"not this thread"}]}}}),
    ]));
    assert_eq!(out.usage, None);
    assert!(out.prompts.is_empty());
    assert!(out.partial);
}

#[test]
#[ignore = "operator-local Codex acceptance; never run in CI"]
fn codex_rollout_stable_item_mirror_survives_turn_context_arriving_late() {
    use libra::internal::ai::observed_agents::extract::extract_codex;
    use serde_json::json;
    let out = extract_codex(&rollout_jsonl(&[
        json!({"type":"response_item","payload":{"type":"message","id":"u","role":"user","content":[{"type":"input_text","text":"hello"}],"internal_chat_message_metadata_passthrough":{"content_item_kinds":["user.text"]}}}),
        json!({"type":"turn_context","payload":{"turn_id":"late-turn","model":"m"}}),
        json!({"type":"event_msg","payload":{"type":"item_completed","turn_id":"late-turn","item":{"type":"UserMessage","id":"u","content":[{"type":"text","text":"hello"}]}}}),
    ]));
    assert_eq!(out.prompts, ["hello"]);
    assert!(
        out.partial,
        "a late inferred turn remains unproven even with shared item identity"
    );
    let unknown = extract_codex(b"{\"type\":\"future_no_payload\"}\n");
    assert!(
        unknown.partial,
        "unknown envelopes cannot silently look complete"
    );
}

#[test]
#[ignore = "operator-local Codex acceptance; never run in CI"]
fn codex_rollout_flat_compatibility_keeps_prompts_beside_oversized_or_invalid_rows() {
    use libra::internal::ai::observed_agents::extract::extract_codex;
    use serde_json::json;
    let mut source = rollout_jsonl(&[
        json!({"role":"user","content":"keep historical prompt"}),
        json!({"model":"historical-model","usage":{"input_tokens":7,"output_tokens":3}}),
        json!({"usage":{"input_tokens":u64::MAX,"output_tokens":1}}),
    ]);
    source.extend_from_slice(&vec![b'x'; 1024 * 1024 + 1]);
    source.push(b'\n');
    let out = extract_codex(&source);
    assert_eq!(out.prompts, ["keep historical prompt"]);
    assert_eq!(out.model.as_deref(), Some("historical-model"));
    assert_eq!(out.usage.unwrap().total_tokens, Some(10));
    assert!(out.partial);
}

#[test]
#[ignore = "operator-local Codex acceptance; never run in CI"]
fn codex_rollout_mirror_candidates_do_not_consume_the_final_output_item_budget() {
    use libra::internal::ai::{
        capture::deadline_provider_extraction_summary, observed_agents::AgentKind,
    };
    use serde_json::json;
    let mut records = Vec::new();
    for i in 0..100 {
        records.push(json!({"type":"response_item","payload":{"type":"message","id":format!("u-{i}"),"role":"user","content":[{"type":"input_text","text":format!("p-{i}")}],"internal_chat_message_metadata_passthrough":{"content_item_kinds":["user.text"]}}}));
        records.push(json!({"type":"event_msg","payload":{"type":"item_completed","item":{"type":"UserMessage","id":format!("u-{i}"),"content":[{"type":"text","text":format!("p-{i}")}]}}}));
    }
    let out = deadline_provider_extraction_summary(AgentKind::Codex, &rollout_jsonl(&records));
    assert_eq!(out.prompts.len(), 100);
    assert!(!out.partial, "{:?}", out.warnings);
}

#[test]
#[ignore = "operator-local Codex acceptance; never run in CI"]
fn codex_rollout_exact_usage_does_not_inherit_a_covered_legacy_reset_warning() {
    use libra::internal::ai::observed_agents::extract::extract_codex;
    use serde_json::json;
    let counts = |i, o| json!({"type":"event_msg","payload":{"type":"token_count","info":{"total_token_usage":rollout_usage(i,o)}}});
    let out = extract_codex(&rollout_jsonl(&[
        counts(10, 2),
        json!({"type":"compacted","payload":{"message":"m","window_id":"w2"}}),
        counts(4, 1),
        json!({"type":"token_usage_record","payload":rollout_response_usage("r1",10,2)}),
        json!({"type":"token_usage_record","payload":rollout_response_usage("r2",4,1)}),
    ]));
    assert_eq!(out.usage.unwrap().input_tokens, 14);
    assert!(!out.partial, "{:?}", out.warnings);
}

#[test]
#[ignore = "operator-local Codex acceptance; never run in CI"]
fn codex_rollout_first_session_meta_remains_owner_after_a_copied_parent_header() {
    use libra::internal::ai::observed_agents::extract::extract_codex;
    use serde_json::json;
    let mut parent = rollout_response_usage("parent-r", 100, 20);
    parent["thread_id"] = json!("parent-thread");
    let mut child = rollout_response_usage("child-r", 10, 2);
    child["thread_id"] = json!("child-thread");
    let out = extract_codex(&rollout_jsonl(&[
        json!({"type":"session_meta","payload":{"id":"child-thread","forked_from_id":"parent-thread"}}),
        json!({"type":"session_meta","payload":{"id":"parent-thread"}}),
        json!({"type":"token_usage_record","payload":parent}),
        json!({"type":"token_usage_record","payload":child}),
        json!({"type":"event_msg","payload":{"type":"item_completed","thread_id":"child-thread","turn_id":"child-t","item":{"type":"UserMessage","id":"child-u","content":[{"type":"text","text":"current child input"}]}}}),
    ]));
    assert_eq!(out.prompts, ["current child input"]);
    assert_eq!(out.usage.unwrap().input_tokens, 10);
    assert_eq!(out.api_call_count, 1);
    assert!(out.partial);
}

#[test]
#[ignore = "operator-local Codex acceptance; never run in CI"]
fn codex_rollout_typed_flat_compatibility_and_aggregate_overflow_are_explicit() {
    use libra::internal::ai::observed_agents::extract::extract_codex;
    use serde_json::json;
    let out = extract_codex(&rollout_jsonl(&[
        json!({"type":"message","role":"user","content":[{"type":"input_text","text":"bare historical input"}]}),
        json!({"type":"user","message":{"role":"user","content":"wrapper historical input"}}),
        json!({"type":"assistant","message":{"role":"assistant","model":"legacy-model"},"usage":{"input_tokens":7,"output_tokens":3}}),
    ]));
    assert_eq!(
        out.prompts,
        ["bare historical input", "wrapper historical input"]
    );
    assert_eq!(out.model.as_deref(), Some("legacy-model"));
    assert_eq!(out.usage.unwrap().total_tokens, Some(10));
    assert!(
        out.partial,
        "bare role-only history is qualified rather than promoted to current source semantics"
    );
    let out = extract_codex(&rollout_jsonl(&[
        json!({"usage":{"input_tokens":u64::MAX/2+1,"output_tokens":0}}),
        json!({"usage":{"input_tokens":u64::MAX/2+1,"output_tokens":0}}),
    ]));
    assert_eq!(out.usage.unwrap().input_tokens, u64::MAX / 2 + 1);
    assert!(
        out.partial,
        "representable individual rows cannot silently saturate the aggregate"
    );
    let out = extract_codex(&rollout_jsonl(&[
        json!({"type":"event_msg","payload":{"type":"user_message","kind":null,"message":"legacy optional kind"}}),
    ]));
    assert_eq!(out.prompts, ["legacy optional kind"]);
    let nullable=extract_codex(br#"{"model":"m","usage":{"input_tokens":1,"output_tokens":1,"cache_read_tokens":null,"api_call_count":null,"subagent_tokens":null}}"#);
    assert_eq!(nullable.model.as_deref(), Some("m"));
    assert_eq!(nullable.usage.unwrap().total_tokens, Some(2));
    assert!(!nullable.partial);
    let shadowed = extract_codex(&rollout_jsonl(&[
        json!({"message":{"role":"assistant","model":"m","usage":null},"usage":{"input_tokens":u64::MAX,"output_tokens":1}}),
    ]));
    assert_eq!(
        shadowed.usage, None,
        "the compatibility reader consumes only the first usage choice, not both"
    );
    assert_eq!(shadowed.api_call_count, 0);
    let malformed = extract_codex(br#"{"model":"m","usage":"bad shape"}"#);
    assert_eq!(malformed.model.as_deref(), Some("m"));
    assert_eq!(malformed.usage, None);
    assert!(malformed.partial);
}

fn cx14_source_fixture() -> &'static [u8] {
    include_bytes!("fixtures/agent_transcripts/codex-capabilities-01621.jsonl")
}

#[test]
#[ignore = "operator-local Codex acceptance; never run in CI"]
fn codex_rollout_capability_registry_and_accessors_advertise_only_wired_methods() {
    use libra::internal::ai::observed_agents::{
        AgentKind, DeclaredAgentCaps, agent_for, registration_for,
    };
    let agent = agent_for(AgentKind::Codex);
    let expected = DeclaredAgentCaps {
        hooks: true,
        transcript_analyzer: true,
        token_calculator: true,
        ..Default::default()
    };
    assert_eq!(registration_for(AgentKind::Codex).capabilities, expected);
    assert_eq!(agent.declared_capabilities(), expected);
    assert!(agent.as_transcript_analyzer().is_some());
    assert!(agent.as_prompt_extractor().is_some());
    assert!(agent.as_token_calculator().is_some());
    assert!(agent.as_model_extractor().is_some());
    assert!(agent.as_skill_event_extractor().is_some());
    assert!(agent.as_subagent_aware_extractor().is_none());
    assert!(agent.as_transcript_preparer().is_none());
}

#[test]
#[ignore = "operator-local Codex acceptance; never run in CI"]
fn codex_rollout_capability_source_fixture_matches_public_adapter_projections() {
    use libra::internal::ai::observed_agents::{AgentKind, agent_for};
    let agent = agent_for(AgentKind::Codex);
    let data = cx14_source_fixture();
    let analyzer = agent.as_transcript_analyzer().expect("CX14 Codex analyzer");
    assert_eq!(analyzer.transcript_position(data).unwrap(), data.len());
    assert_eq!(
        analyzer
            .extract_modified_files_from_offset(data, 0)
            .unwrap(),
        [
            std::path::PathBuf::from("src/snake.rs"),
            std::path::PathBuf::from("src/ui.rs")
        ]
    );
    assert_eq!(
        agent
            .as_prompt_extractor()
            .unwrap()
            .extract_prompts(data, 0)
            .unwrap(),
        ["/review implement a TUI snake", "Keep the score visible."]
    );
    assert_eq!(
        agent
            .as_model_extractor()
            .unwrap()
            .extract_model(data)
            .unwrap()
            .as_deref(),
        Some("codex-fixture-model-b")
    );
    let usage = agent
        .as_token_calculator()
        .unwrap()
        .calculate_token_usage(data, 0)
        .unwrap();
    assert_eq!(
        (usage.input_tokens, usage.output_tokens, usage.total_tokens),
        (17, 7, Some(24))
    );
    assert_eq!(
        (usage.cached_tokens, usage.reasoning_tokens),
        (Some(4), Some(1))
    );
    let skills = agent
        .as_skill_event_extractor()
        .unwrap()
        .extract_skill_events(data, 0)
        .unwrap();
    assert_eq!(skills.len(), 1);
    assert_eq!(skills[0].skill.name, "/review");
}

#[test]
#[ignore = "operator-local Codex acceptance; never run in CI"]
fn codex_rollout_capability_missing_usage_is_an_actionable_error_not_measured_zero() {
    use libra::internal::ai::observed_agents::{AgentKind, agent_for};
    let calculator = agent_for(AgentKind::Codex).as_token_calculator().unwrap();
    assert!(calculator.calculate_token_usage(b"", 0).is_err());
    assert!(
        calculator
            .calculate_token_usage(cx14_source_fixture(), usize::MAX)
            .is_err()
    );
}

#[test]
#[ignore = "operator-local Codex acceptance; never run in CI"]
fn codex_rollout_capability_nonzero_offset_preserves_cumulative_usage_baseline() {
    use libra::internal::ai::observed_agents::{AgentKind, agent_for};
    use serde_json::json;
    let count = |i, o| json!({"type":"event_msg","payload":{"type":"token_count","info":{"total_token_usage":rollout_usage(i,o)}}});
    let mut data = rollout_jsonl(&[
        json!({"type":"session_meta","payload":{"id":"thread-a","cwd":"/fixture/codex"}}),
        count(10, 4),
    ]);
    let offset = data.len();
    data.extend(rollout_jsonl(&[count(17, 7)]));
    let usage = agent_for(AgentKind::Codex)
        .as_token_calculator()
        .unwrap()
        .calculate_token_usage(&data, offset)
        .unwrap();
    assert_eq!(
        (usage.input_tokens, usage.output_tokens, usage.total_tokens),
        (7, 3, Some(10))
    );
}

#[test]
#[ignore = "operator-local Codex acceptance; never run in CI"]
fn codex_rollout_capability_original_cx12_fault_corpus_preserves_known_partial_usage() {
    use libra::internal::ai::observed_agents::extract::extract_codex;
    use sha2::{Digest, Sha256};
    let corpus = include_bytes!("fixtures/agent_transcripts/codex-rollout-01621.jsonl");
    assert_eq!(corpus.len(), 12433);
    assert_eq!(
        hex::encode(Sha256::digest(corpus)),
        "bb1453259ac10a91c9bfd2529eb4b64ab1c963c70bf8e67fb5941d4d65b1fe69"
    );
    assert_ne!(corpus.last(), Some(&b'\n'));
    let first = corpus
        .split_inclusive(|byte| *byte == b'\n')
        .take(40)
        .flatten()
        .copied()
        .collect::<Vec<_>>();
    let projection = extract_codex(&first);
    assert!(
        projection.partial,
        "fault corpus must never look like a complete natural session"
    );
    let usage = projection
        .usage
        .expect("known response survives unrelated fault records");
    assert_eq!(
        (usage.input_tokens, usage.output_tokens, usage.total_tokens),
        (10, 4, Some(14))
    );
    assert_eq!(
        (usage.cached_tokens, usage.reasoning_tokens),
        (Some(3), Some(3))
    );
    assert_eq!(projection.api_call_count, 1);
}

#[test]
#[ignore = "operator-local Codex acceptance; never run in CI"]
fn codex_rollout_capability_historical_flat_adapter_projection() {
    use libra::internal::ai::observed_agents::{AgentKind, agent_for};
    // Codex: prompts / model / tokens / skills (best-effort rollout form).
    let codex = agent_for(AgentKind::Codex);
    let data: &[u8] = include_bytes!("fixtures/agent_transcripts/codex.jsonl");
    let prompts = codex
        .as_prompt_extractor()
        .expect("codex prompts")
        .extract_prompts(data, 0)
        .unwrap();
    assert_eq!(prompts.len(), 2);
    let usage = codex
        .as_token_calculator()
        .expect("codex tokens")
        .calculate_token_usage(data, 0)
        .unwrap();
    assert_eq!(usage.total_tokens, Some(260));
    let model = codex
        .as_model_extractor()
        .expect("codex model")
        .extract_model(data)
        .unwrap();
    assert_eq!(model.as_deref(), Some("gpt-5.3-codex"));
    // Historical flat exports lack completed patch records; the adapter still
    // exposes the analyzer for native rollouts.
    assert!(
        codex
            .as_transcript_analyzer()
            .unwrap()
            .extract_modified_files_from_offset(data, 0)
            .unwrap()
            .is_empty()
    );
    assert!(codex.as_subagent_aware_extractor().is_none());
    let offset = data
        .split_inclusive(|byte| *byte == b'\n')
        .take(2)
        .map(<[u8]>::len)
        .sum();
    assert_eq!(
        codex
            .as_prompt_extractor()
            .unwrap()
            .extract_prompts(data, offset)
            .unwrap(),
        ["Summarize the findings."]
    );
    assert_eq!(
        codex
            .as_token_calculator()
            .unwrap()
            .calculate_token_usage(data, offset)
            .unwrap()
            .total_tokens,
        Some(105)
    );
    assert!(
        codex
            .as_skill_event_extractor()
            .unwrap()
            .extract_skill_events(data, offset)
            .unwrap()
            .is_empty()
    );
    let mut flat = rollout_jsonl(&[serde_json::json!({"role":"user","content":"/review first"})]);
    let offset = flat.len();
    flat.extend(rollout_jsonl(&[
        serde_json::json!({"role":"user","content":"/review next"}),
    ]));
    let full = codex
        .as_skill_event_extractor()
        .unwrap()
        .extract_skill_events(&flat, 0)
        .unwrap();
    let delta = codex
        .as_skill_event_extractor()
        .unwrap()
        .extract_skill_events(&flat, offset)
        .unwrap();
    assert_ne!(full[0].id, delta[0].id);
    assert_eq!(full[1].id, delta[0].id);
    let proposed=br#"{"role":"assistant","content":[{"type":"tool_use","name":"Write","input":{"file_path":"proposed.rs"}}]}"#;
    assert!(
        codex
            .as_transcript_analyzer()
            .unwrap()
            .extract_modified_files_from_offset(proposed, 0)
            .unwrap()
            .is_empty()
    );
}

#[test]
#[ignore = "operator-local Codex acceptance; never run in CI"]
fn codex_rollout_capability_offset_keeps_prefix_cwd_and_new_completed_patch() {
    use libra::internal::ai::observed_agents::{AgentKind, agent_for};
    use serde_json::json;
    let patch = |id: &str| json!({"type":"event_msg","payload":{"type":"item_completed","turn_id":"turn-a","item":{"type":"FileChange","id":id,"status":"completed","changes":{"/repo/sub/src/snake.rs":{"type":"update"}}}}});
    let mut data = rollout_jsonl(&[
        json!({"type":"session_meta","payload":{"id":"thread-a","cwd":"/repo/sub"}}),
        json!({"type":"turn_context","payload":{"turn_id":"turn-a","cwd":"/repo/sub"}}),
        patch("old-patch"),
    ]);
    let offset = data.len();
    data.extend(rollout_jsonl(&[patch("old-patch")]));
    let agent = agent_for(AgentKind::Codex);
    let analyzer = agent.as_transcript_analyzer().unwrap();
    assert!(
        analyzer
            .extract_modified_files_from_offset(&data, offset)
            .unwrap()
            .is_empty(),
        "old item mirror does not report a new modification"
    );
    data.extend(rollout_jsonl(&[patch("new-patch")]));
    assert_eq!(
        analyzer
            .extract_modified_files_from_offset(&data, offset)
            .unwrap(),
        [std::path::PathBuf::from("src/snake.rs")],
        "a new completed edit to an old path uses prefix cwd"
    );
    assert!(
        analyzer
            .extract_modified_files_from_offset(&data, usize::MAX)
            .unwrap()
            .is_empty()
    );
}

#[test]
#[ignore = "operator-local Codex acceptance; never run in CI"]
fn codex_rollout_capability_offset_deduplicates_prompt_mirrors_but_keeps_new_identity() {
    use libra::internal::ai::observed_agents::{AgentKind, agent_for};
    use serde_json::json;
    let message = |id: &str| json!({"type":"event_msg","payload":{"type":"item_completed","turn_id":"turn-a","item":{"type":"UserMessage","id":id,"content":[{"type":"text","text":"/review 🐍"}]}}});
    let mut data = rollout_jsonl(&[
        json!({"type":"session_meta","payload":{"id":"thread-a"}}),
        message("old"),
    ]);
    let offset = data.len();
    data.extend(rollout_jsonl(&[message("old")]));
    let agent = agent_for(AgentKind::Codex);
    assert!(
        agent
            .as_prompt_extractor()
            .unwrap()
            .extract_prompts(&data, offset)
            .unwrap()
            .is_empty()
    );
    assert!(
        agent
            .as_skill_event_extractor()
            .unwrap()
            .extract_skill_events(&data, offset)
            .unwrap()
            .is_empty()
    );
    data.extend(rollout_jsonl(&[message("new")]));
    assert_eq!(
        agent
            .as_prompt_extractor()
            .unwrap()
            .extract_prompts(&data, offset)
            .unwrap(),
        ["/review 🐍"]
    );
    assert_eq!(
        agent
            .as_skill_event_extractor()
            .unwrap()
            .extract_skill_events(&data, offset)
            .unwrap()
            .len(),
        1
    );
    let skills = agent.as_skill_event_extractor().unwrap();
    let full = skills.extract_skill_events(&data, 0).unwrap();
    let delta = skills.extract_skill_events(&data, offset).unwrap();
    assert_ne!(
        full[0].id, delta[0].id,
        "new event must not collide with the prefix"
    );
    assert_eq!(
        full[1].id, delta[0].id,
        "the same event must have an offset-invariant ID"
    );
    let legacy =
        json!({"type":"event_msg","payload":{"type":"user_message","message":"/review 🐍"}});
    let mut mixed = rollout_jsonl(&[
        json!({"type":"turn_context","payload":{"turn_id":"turn-a"}}),
        legacy.clone(),
        message("old"),
    ]);
    let offset = mixed.len();
    mixed.extend(rollout_jsonl(&[legacy]));
    assert_eq!(
        agent
            .as_prompt_extractor()
            .unwrap()
            .extract_prompts(&mixed, offset)
            .unwrap(),
        ["/review 🐍"],
        "an earlier completed mirror must not hide the next legacy occurrence"
    );
}

#[test]
#[ignore = "operator-local Codex acceptance; never run in CI"]
fn codex_rollout_capability_offset_distinguishes_record_completion_from_newline_only() {
    use libra::internal::ai::observed_agents::{AgentKind, agent_for};
    use serde_json::json;
    let record = rollout_jsonl(&[
        json!({"type":"event_msg","payload":{"type":"user_message","message":"你好 🐍"}}),
    ]);
    let agent = agent_for(AgentKind::Codex);
    let prompts = agent.as_prompt_extractor().unwrap();
    assert!(
        prompts
            .extract_prompts(&record, record.len() - 1)
            .unwrap()
            .is_empty(),
        "adding LF to a complete JSON value does not replay it"
    );
    let utf8 = record
        .windows("🐍".len())
        .position(|part| part == "🐍".as_bytes())
        .unwrap();
    assert_eq!(
        prompts.extract_prompts(&record, utf8 + 1).unwrap(),
        ["你好 🐍"],
        "completing a previously incomplete value emits it, even for a byte offset within UTF-8"
    );
    assert!(
        prompts
            .extract_prompts(&record, usize::MAX)
            .unwrap()
            .is_empty()
    );
}

#[test]
#[ignore = "operator-local Codex acceptance; never run in CI"]
fn codex_rollout_capability_offset_does_not_recount_exact_compaction_mirrors() {
    use libra::internal::ai::observed_agents::{AgentKind, agent_for};
    use serde_json::json;
    let first = rollout_response_usage("r1", 10, 4);
    let mut data = rollout_jsonl(&[
        json!({"type":"session_meta","payload":{"id":"thread-a"}}),
        json!({"type":"token_usage_record","payload":first}),
    ]);
    let offset = data.len();
    data.extend(rollout_jsonl(&[
        json!({"type":"compacted","payload":{"latest_token_usage_record":first}}),
    ]));
    let agent = agent_for(AgentKind::Codex);
    let calculator = agent.as_token_calculator().unwrap();
    assert!(
        calculator.calculate_token_usage(&data, offset).is_err(),
        "only an old exact response snapshot is not new measured usage"
    );
    data.extend(rollout_jsonl(&[json!({"type":"event_msg","payload":{"type":"token_count","info":{"total_token_usage":rollout_usage(10,4)}}})]));
    assert_eq!(
        calculator
            .calculate_token_usage(&data, offset)
            .unwrap_err()
            .to_string(),
        "Codex token usage is unavailable at or after this offset; wait for a completed response or import more transcript data"
    );
    data.extend(rollout_jsonl(&[
        json!({"type":"token_usage_record","payload":rollout_response_usage("r2",7,3)}),
    ]));
    let usage = calculator.calculate_token_usage(&data, offset).unwrap();
    assert_eq!(
        (usage.input_tokens, usage.output_tokens, usage.total_tokens),
        (7, 3, Some(10))
    );
}

#[test]
#[ignore = "operator-local Codex acceptance; never run in CI"]
fn codex_rollout_capability_unknown_usage_display_is_actionable() {
    use libra::internal::ai::observed_agents::{AgentKind, agent_for};
    let agent = agent_for(AgentKind::Codex);
    assert_eq!(
        agent
            .as_token_calculator()
            .unwrap()
            .calculate_token_usage(b"", 0)
            .unwrap_err()
            .to_string(),
        "Codex token usage is unavailable at or after this offset; wait for a completed response or import more transcript data"
    );
}

#[test]
#[ignore = "operator-local Codex acceptance; never run in CI"]
fn codex_rollout_capability_other_provider_rows_keep_their_original_bits() {
    use libra::internal::ai::observed_agents::{
        AgentKind, DeclaredAgentCaps, agent_for, registration_for,
    };
    let none = DeclaredAgentCaps::default();
    let hooks = DeclaredAgentCaps {
        hooks: true,
        ..none
    };
    let tokens = DeclaredAgentCaps {
        token_calculator: true,
        ..hooks
    };
    let claude = DeclaredAgentCaps {
        transcript_analyzer: true,
        transcript_preparer: true,
        subagent_aware_extractor: true,
        ..tokens
    };
    for (kind, expected) in [
        (AgentKind::ClaudeCode, claude),
        (AgentKind::Cursor, none),
        (AgentKind::Gemini, none),
        (AgentKind::OpenCode, tokens),
        (AgentKind::Copilot, none),
        (AgentKind::FactoryAi, none),
        (AgentKind::Grok, hooks),
    ] {
        assert_eq!(registration_for(kind).capabilities, expected, "{kind:?}");
        assert_eq!(
            agent_for(kind).declared_capabilities(),
            expected,
            "{kind:?}"
        );
    }
}

#[test]
#[ignore = "operator-local Codex acceptance; never run in CI"]
fn codex_rollout_capability_cli_lists_the_wired_analyzer() {
    use std::process::Command;
    let temp = tempfile::tempdir().unwrap();
    let home = temp.path().join("home");
    std::fs::create_dir(&home).unwrap();
    let run = |args: &[&str]| {
        Command::new(env!("CARGO_BIN_EXE_libra"))
            .current_dir(temp.path())
            .env("HOME", &home)
            .env("USERPROFILE", &home)
            .env("LIBRA_TEST_HOME", &home)
            .env("CODEX_HOME", home.join(".codex"))
            .env("CLAUDE_CONFIG_DIR", home.join(".claude"))
            .args(args)
            .output()
            .unwrap()
    };
    let init = run(&["init"]);
    assert!(
        init.status.success(),
        "{}",
        String::from_utf8_lossy(&init.stderr)
    );
    let output = run(&["agent", "list", "--json"]);
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let value: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    let payload = value.get("data").unwrap_or(&value);
    let row = payload["agents"]
        .as_array()
        .unwrap()
        .iter()
        .find(|row| row["slug"] == "codex")
        .unwrap();
    assert_eq!(row["capabilities"]["transcript_analyzer"], true);
    assert_eq!(row["capabilities"]["token_calculator"], true);
    assert_eq!(row["capabilities"]["subagent_aware_extractor"], false);
}

#[test]
#[ignore = "operator-local Codex acceptance; never run in CI"]
fn codex_rollout_capability_prefix_text_does_not_exhaust_new_prompt_budget() {
    use libra::internal::ai::observed_agents::{AgentKind, agent_for};
    use serde_json::json;
    let text = "x".repeat(60 * 1024);
    let mut data = Vec::new();
    for id in 0..60 {
        data.extend(rollout_jsonl(&[json!({"type":"event_msg","payload":{"type":"user_message","client_id":format!("prefix-{id}"),"message":text}})]));
    }
    let offset = data.len();
    data.extend(rollout_jsonl(&[json!({"type":"event_msg","payload":{"type":"user_message","client_id":"new","message":"/review new prompt"}})]));
    let agent = agent_for(AgentKind::Codex);
    assert_eq!(
        agent
            .as_prompt_extractor()
            .unwrap()
            .extract_prompts(&data, offset)
            .unwrap(),
        ["/review new prompt"]
    );
    assert_eq!(
        agent
            .as_skill_event_extractor()
            .unwrap()
            .extract_skill_events(&data, offset)
            .unwrap()
            .len(),
        1
    );
}

#[test]
#[ignore = "operator-local Codex acceptance; never run in CI"]
fn codex_rollout_capability_budget_and_partial_usage_errors_name_recovery() {
    use libra::internal::ai::observed_agents::{AgentKind, agent_for};
    let agent = agent_for(AgentKind::Codex);
    let oversized = vec![b'x'; 1024 * 1024 + 1];
    let expected = |projection| {
        format!(
            "Codex {projection} projection exceeded the 1 MiB JSONL record limit; this session cannot be projected by this capability"
        )
    };
    assert_eq!(
        agent
            .as_prompt_extractor()
            .unwrap()
            .extract_prompts(&oversized, 0)
            .unwrap_err()
            .to_string(),
        expected("prompt")
    );
    assert_eq!(
        agent
            .as_skill_event_extractor()
            .unwrap()
            .extract_skill_events(&oversized, 0)
            .unwrap_err()
            .to_string(),
        expected("skill")
    );
    assert_eq!(
        agent
            .as_transcript_analyzer()
            .unwrap()
            .extract_modified_files_from_offset(&oversized, 0)
            .unwrap_err()
            .to_string(),
        expected("file")
    );
    assert_eq!(
        agent
            .as_token_calculator()
            .unwrap()
            .calculate_token_usage(&oversized, 0)
            .unwrap_err()
            .to_string(),
        expected("token usage")
    );
    let mut envelope =
        rollout_jsonl(&[serde_json::json!({"type":"session_meta","payload":{"id":"thread-a"}})]);
    envelope.extend_from_slice(&oversized);
    assert_eq!(
        agent
            .as_token_calculator()
            .unwrap()
            .calculate_token_usage(&envelope, 0)
            .unwrap_err()
            .to_string(),
        expected("token usage")
    );
    let malformed = br#"{"type":"event_msg","payload":{"type":"token_count","info":{}}}"#;
    assert_eq!(
        agent
            .as_token_calculator()
            .unwrap()
            .calculate_token_usage(malformed, 0)
            .unwrap_err()
            .to_string(),
        "Codex token usage could not be established for this offset; verify the source format and response identities; an offset-zero total replaces earlier totals and must not be added to them"
    );
    assert!(
        agent
            .as_prompt_extractor()
            .unwrap()
            .extract_prompts(&vec![b' '; 1024 * 1024 + 1], 0)
            .unwrap()
            .is_empty()
    );
}

#[test]
#[ignore = "operator-local Codex acceptance; never run in CI"]
fn codex_rollout_capability_projection_budgets_do_not_hide_unrelated_usage_or_files() {
    use libra::internal::ai::observed_agents::{AgentKind, agent_for};
    use serde_json::json;
    let prompt = json!({"type":"event_msg","payload":{"type":"user_message","client_id":"huge","message":"x".repeat(65*1024)}});
    let patch = json!({"type":"event_msg","payload":{"type":"item_completed","turn_id":"turn-a","item":{"type":"FileChange","id":"patch","status":"completed","changes":{"/repo/src/snake.rs":{"type":"update"}}}}});
    let data = rollout_jsonl(&[
        json!({"type":"session_meta","payload":{"id":"thread-a","cwd":"/repo"}}),
        prompt,
        json!({"type":"token_usage_record","payload":rollout_response_usage("r1",2,1)}),
        patch,
    ]);
    let agent = agent_for(AgentKind::Codex);
    assert_eq!(
        agent
            .as_token_calculator()
            .unwrap()
            .calculate_token_usage(&data, 0)
            .unwrap()
            .total_tokens,
        Some(3)
    );
    assert_eq!(
        agent
            .as_transcript_analyzer()
            .unwrap()
            .extract_modified_files_from_offset(&data, 0)
            .unwrap(),
        [std::path::PathBuf::from("src/snake.rs")]
    );
    assert!(
        agent
            .as_prompt_extractor()
            .unwrap()
            .extract_prompts(&data, 0)
            .unwrap_err()
            .to_string()
            .contains("64 KiB string")
    );
    assert!(
        agent
            .as_skill_event_extractor()
            .unwrap()
            .extract_skill_events(&data, 0)
            .unwrap()
            .is_empty()
    );
    let flat = rollout_jsonl(&[
        json!({"role":"user","content":"x".repeat(65*1024)}),
        json!({"role":"assistant","usage":{"input_tokens":2,"output_tokens":1}}),
    ]);
    assert_eq!(
        agent
            .as_token_calculator()
            .unwrap()
            .calculate_token_usage(&flat, 0)
            .unwrap()
            .total_tokens,
        Some(3)
    );
    assert!(
        agent
            .as_prompt_extractor()
            .unwrap()
            .extract_prompts(&flat, 0)
            .is_err()
    );
    let input_overflow = vec![b'\n'; 16 * 1024 * 1024 + 1];
    assert!(
        agent
            .as_token_calculator()
            .unwrap()
            .calculate_token_usage(&input_overflow, 0)
            .unwrap_err()
            .to_string()
            .contains("16 MiB input")
    );
    let context = json!({"type":"turn_context","payload":{"turn_id":"t".repeat(64*1024)}});
    let request = json!({"type":"event_msg","payload":{"type":"user_message","client_id":"short","message":"/review hi"}});
    let native = rollout_jsonl(&[context, request]);
    assert_eq!(
        agent
            .as_prompt_extractor()
            .unwrap()
            .extract_prompts(&native, 0)
            .unwrap(),
        ["/review hi"]
    );
    assert!(
        agent
            .as_skill_event_extractor()
            .unwrap()
            .extract_skill_events(&native, 0)
            .is_err(),
        "skill context budget is separate from valid prompt text"
    );
    let flat = rollout_jsonl(&[
        json!({"role":"user","content":"/review hi","timestamp":"t".repeat(64*1024)}),
    ]);
    assert_eq!(
        agent
            .as_prompt_extractor()
            .unwrap()
            .extract_prompts(&flat, 0)
            .unwrap(),
        ["/review hi"]
    );
    assert!(
        agent
            .as_skill_event_extractor()
            .unwrap()
            .extract_skill_events(&flat, 0)
            .is_err()
    );
    for native in [true, false] {
        let mut data = Vec::new();
        for id in 0..5000 {
            let row = if native {
                json!({"type":"event_msg","payload":{"type":"user_message","client_id":format!("ordinary{id}"),"message":"ordinary"}})
            } else {
                json!({"role":"user","content":"ordinary"})
            };
            data.extend(rollout_jsonl(&[row]));
        }
        let row = if native {
            json!({"type":"event_msg","payload":{"type":"user_message","client_id":"slash","message":"/review hi"}})
        } else {
            json!({"role":"user","content":"/review hi"})
        };
        data.extend(rollout_jsonl(&[row]));
        assert_eq!(
            agent
                .as_skill_event_extractor()
                .unwrap()
                .extract_skill_events(&data, 0)
                .unwrap()
                .len(),
            1,
            "non-skill output must not consume skill retention/count budget"
        );
        assert!(
            agent
                .as_prompt_extractor()
                .unwrap()
                .extract_prompts(&data, 0)
                .is_err()
        );
    }
}

#[test]
#[ignore = "operator-local Codex acceptance; never run in CI"]
fn codex_rollout_capability_unrelated_partial_does_not_claim_usage_is_malformed() {
    use libra::internal::ai::observed_agents::{AgentKind, agent_for};
    let data=br#"{"type":"response_item","payload":{"type":"message","role":"user","content":[{"type":"input_text","text":"source only"}]}}"#;
    let agent = agent_for(AgentKind::Codex);
    assert_eq!(
        agent
            .as_token_calculator()
            .unwrap()
            .calculate_token_usage(data, 0)
            .unwrap_err()
            .to_string(),
        "Codex token usage is unavailable at or after this offset; wait for a completed response or import more transcript data"
    );
}

#[test]
#[ignore = "operator-local Codex acceptance; never run in CI"]
fn codex_rollout_capability_identity_limits_are_projection_specific() {
    use libra::internal::ai::observed_agents::{AgentKind, agent_for};
    use serde_json::json;
    let agent = agent_for(AgentKind::Codex);
    let header = json!({"type":"session_meta","payload":{"id":"thread-a"}});
    let input = json!({"type":"event_msg","payload":{"type":"user_message","client_id":"new","message":"/review hello"}});
    let measured =
        json!({"type":"token_usage_record","payload":rollout_response_usage("measured",2,1)});
    let mut data = rollout_jsonl(std::slice::from_ref(&header));
    for id in 0..4097 {
        data.extend(rollout_jsonl(&[json!({"type":"token_usage_record","payload":rollout_response_usage(&format!("r{id}"),1,1)})]));
    }
    data.extend(rollout_jsonl(std::slice::from_ref(&input)));
    assert!(
        agent
            .as_token_calculator()
            .unwrap()
            .calculate_token_usage(&data, 0)
            .unwrap_err()
            .to_string()
            .contains("identity-state")
    );
    assert_eq!(
        agent
            .as_prompt_extractor()
            .unwrap()
            .extract_prompts(&data, 0)
            .unwrap(),
        ["/review hello"]
    );
    let mut data = rollout_jsonl(std::slice::from_ref(&header));
    for id in 0..4097 {
        data.extend(rollout_jsonl(&[json!({"type":"event_msg","payload":{"type":"item_completed","turn_id":"turn-a","item":{"type":"FileChange","id":format!("p{id}"),"status":"completed","changes":{"src/snake.rs":{"type":"update"}}}}})]));
    }
    data.extend(rollout_jsonl(&[measured.clone(), input]));
    assert!(
        agent
            .as_transcript_analyzer()
            .unwrap()
            .extract_modified_files_from_offset(&data, 0)
            .unwrap_err()
            .to_string()
            .contains("identity-state")
    );
    assert_eq!(
        agent
            .as_token_calculator()
            .unwrap()
            .calculate_token_usage(&data, 0)
            .unwrap()
            .total_tokens,
        Some(3)
    );
    assert_eq!(
        agent
            .as_prompt_extractor()
            .unwrap()
            .extract_prompts(&data, 0)
            .unwrap(),
        ["/review hello"]
    );
    let mut data = rollout_jsonl(&[header]);
    for id in 0..12289 {
        data.extend(rollout_jsonl(&[json!({"type":"event_msg","payload":{"type":"user_message","client_id":format!("u{id}"),"message":"hello"}})]));
    }
    data.extend(rollout_jsonl(&[measured]));
    assert!(
        agent
            .as_prompt_extractor()
            .unwrap()
            .extract_prompts(&data, 0)
            .unwrap_err()
            .to_string()
            .contains("identity-state")
    );
    assert_eq!(
        agent
            .as_token_calculator()
            .unwrap()
            .calculate_token_usage(&data, 0)
            .unwrap()
            .total_tokens,
        Some(3)
    );
}

#[test]
#[ignore = "operator-local Codex acceptance; never run in CI"]
fn codex_rollout_capability_wide_prefix_preserves_new_prompt_and_skill_projection() {
    use libra::internal::ai::observed_agents::{AgentKind, agent_for};
    use serde_json::json;
    let mut data = rollout_jsonl(&[
        json!({"type":"event_msg","payload":{"type":"user_message","client_id":"wide","message":"x".repeat(65*1024)}}),
    ]);
    let offset = data.len();
    data.extend(rollout_jsonl(&[json!({"type":"event_msg","payload":{"type":"user_message","client_id":"next","message":"/review next"}})]));
    let agent = agent_for(AgentKind::Codex);
    assert_eq!(
        agent
            .as_prompt_extractor()
            .unwrap()
            .extract_prompts(&data, offset)
            .unwrap(),
        ["/review next"]
    );
    assert_eq!(
        agent
            .as_skill_event_extractor()
            .unwrap()
            .extract_skill_events(&data, offset)
            .unwrap()
            .len(),
        1
    );
    assert_eq!(
        agent
            .as_skill_event_extractor()
            .unwrap()
            .extract_skill_events(&data, 0)
            .unwrap()
            .len(),
        1
    );
}

#[test]
#[ignore = "operator-local Codex acceptance; never run in CI"]
fn codex_rollout_capability_large_image_and_tool_output_do_not_hide_exact_usage_or_files() {
    use libra::internal::ai::observed_agents::{AgentKind, agent_for};
    use serde_json::json;
    let header = json!({"type":"session_meta","payload":{"id":"thread-a","cwd":"/repo"}});
    let image = json!({"type":"response_item","payload":{"type":"message","id":"image","role":"user","content":[{"type":"input_image","image_url":format!("data:image/png;base64,{}","A".repeat(1100*1024))}],"internal_chat_message_metadata_passthrough":{"turn_id":"turn-a","cwd":"/repo/sub","content_item_kinds":["user.image"]}}});
    let output = json!({"type":"response_item","payload":{"type":"function_call_output","call_id":"tool","output":"x".repeat(1100*1024)}});
    let patch = json!({"type":"event_msg","payload":{"type":"item_completed","turn_id":"turn-a","item":{"type":"FileChange","id":"patch","status":"completed","changes":{"/repo/sub/src/snake.rs":{"type":"update"}}}}});
    let usage = json!({"type":"token_usage_record","payload":rollout_response_usage("r1",2,1)});
    let data = rollout_jsonl(&[
        header.clone(),
        image.clone(),
        output.clone(),
        usage.clone(),
        patch.clone(),
    ]);
    let agent = agent_for(AgentKind::Codex);
    assert_eq!(
        agent
            .as_token_calculator()
            .unwrap()
            .calculate_token_usage(&data, 0)
            .unwrap()
            .total_tokens,
        Some(3)
    );
    assert_eq!(
        agent
            .as_transcript_analyzer()
            .unwrap()
            .extract_modified_files_from_offset(&data, 0)
            .unwrap(),
        [std::path::PathBuf::from("src/snake.rs")]
    );
    assert!(
        agent
            .as_prompt_extractor()
            .unwrap()
            .extract_prompts(&data, 0)
            .is_err(),
        "the large user body still exceeds the prompt record limit"
    );
    let only_output = rollout_jsonl(&[header, output]);
    assert!(
        agent
            .as_prompt_extractor()
            .unwrap()
            .extract_prompts(&only_output, 0)
            .unwrap()
            .is_empty()
    );
    assert!(
        agent
            .as_skill_event_extractor()
            .unwrap()
            .extract_skill_events(&only_output, 0)
            .unwrap()
            .is_empty()
    );
    let mut inherited = image;
    inherited["metadata"] = json!({"inherited_user_message":true});
    let inherited = rollout_jsonl(&[
        json!({"type":"session_meta","payload":{"id":"thread-a","cwd":"/repo"}}),
        inherited,
        usage,
        patch,
    ]);
    assert_eq!(
        agent
            .as_transcript_analyzer()
            .unwrap()
            .extract_modified_files_from_offset(&inherited, 0)
            .unwrap(),
        [std::path::PathBuf::from("sub/src/snake.rs")],
        "inherited user context cannot change the owner cwd"
    );
    let unknown = rollout_jsonl(&[
        json!({"type":"session_meta","payload":{"id":"thread-a"}}),
        json!({"future":"x".repeat(1100*1024)}),
    ]);
    assert!(
        agent
            .as_token_calculator()
            .unwrap()
            .calculate_token_usage(&unknown, 0)
            .unwrap_err()
            .to_string()
            .contains("1 MiB JSONL record")
    );
}

#[test]
#[ignore = "operator-local Codex acceptance; never run in CI"]
fn codex_rollout_capability_foreign_context_never_rebinds_the_owner() {
    use libra::internal::ai::observed_agents::{AgentKind, agent_for, extract::extract_codex};
    use serde_json::json;
    let data = rollout_jsonl(&[
        json!({"type":"session_meta","payload":{"id":"thread-a","cwd":"/repo"}}),
        json!({"type":"turn_context","payload":{"turn_id":"turn-a","model":"own-model"}}),
        json!({"type":"turn_context","payload":{"thread_id":"foreign","turn_id":"foreign-turn","cwd":"/elsewhere","model":"foreign-model"}}),
        json!({"type":"response_item","payload":{"type":"message","thread_id":"foreign","id":"foreign-user","role":"user","content":[{"type":"input_text","text":"foreign prompt"}],"internal_chat_message_metadata_passthrough":{"cwd":"/elsewhere","turn_id":"foreign-turn","content_item_kinds":["user.text"]}}}),
        json!({"type":"event_msg","payload":{"type":"item_completed","turn_id":"turn-a","item":{"type":"FileChange","id":"patch","status":"completed","changes":{"/repo/src/snake.rs":{"type":"update"}}}}}),
    ]);
    let projection = extract_codex(&data);
    assert!(projection.partial);
    assert_eq!(projection.model.as_deref(), Some("own-model"));
    assert!(projection.prompts.is_empty());
    let agent = agent_for(AgentKind::Codex);
    assert_eq!(
        agent
            .as_transcript_analyzer()
            .unwrap()
            .extract_modified_files_from_offset(&data, 0)
            .unwrap(),
        [std::path::PathBuf::from("src/snake.rs")]
    );
}

#[test]
#[ignore = "operator-local Codex acceptance; never run in CI"]
fn codex_rollout_capability_known_non_user_large_records_do_not_hide_usage() {
    use libra::internal::ai::observed_agents::{AgentKind, agent_for};
    use serde_json::json;
    let agent = agent_for(AgentKind::Codex);
    let mut records = vec![
        json!({"type":"response_item","payload":{"type":"message","role":"developer","content":"x".repeat(1100*1024)}}),
        json!({"type":"response_item","payload":{"type":"message","content":"x".repeat(1100*1024)}}),
    ];
    for kind in [
        "world_state",
        "security_risk_score",
        "inter_agent_communication_metadata",
    ] {
        records.push(json!({"type":kind,"payload":{"data":"x".repeat(1100*1024),"type":123}}));
    }
    for record in records {
        let data = rollout_jsonl(&[
            json!({"type":"session_meta","payload":{"id":"thread-a","cwd":"/repo"}}),
            record,
            json!({"type":"token_usage_record","payload":rollout_response_usage("r1",2,1)}),
            json!({"type":"event_msg","payload":{"type":"item_completed","turn_id":"turn-a","item":{"type":"FileChange","id":"p","status":"completed","changes":{"src/snake.rs":{"type":"update"}}}}}),
        ]);
        assert_eq!(
            agent
                .as_token_calculator()
                .unwrap()
                .calculate_token_usage(&data, 0)
                .unwrap()
                .total_tokens,
            Some(3)
        );
        assert_eq!(
            agent
                .as_transcript_analyzer()
                .unwrap()
                .extract_modified_files_from_offset(&data, 0)
                .unwrap(),
            [std::path::PathBuf::from("src/snake.rs")]
        );
        assert!(
            agent
                .as_prompt_extractor()
                .unwrap()
                .extract_prompts(&data, 0)
                .unwrap()
                .is_empty()
        );
    }
}

#[test]
#[ignore = "operator-local Codex acceptance; never run in CI"]
fn codex_rollout_capability_large_owner_header_and_foreign_body_keep_scope_errors_visible() {
    use libra::internal::ai::observed_agents::{AgentKind, agent_for};
    use serde_json::json;
    let agent = agent_for(AgentKind::Codex);
    let header = json!({"type":"session_meta","payload":{"id":"thread-a","cwd":"/repo"}});
    let data = rollout_jsonl(&[
        header.clone(),
        json!({"type":"session_meta","payload":{"id":"foreign"}}),
        json!({"type":"session_meta","payload":{"id":"thread-a","cwd":"/repo","data":"x".repeat(1100*1024)}}),
        json!({"type":"token_usage_record","payload":rollout_response_usage("r1",2,1)}),
    ]);
    assert!(
        agent
            .as_token_calculator()
            .unwrap()
            .calculate_token_usage(&data, 0)
            .unwrap_err()
            .to_string()
            .contains("1 MiB JSONL record"),
        "large owner header cannot be hidden by the foreign partition"
    );
    let data = rollout_jsonl(&[
        header,
        json!({"type":"response_item","payload":{"type":"function_call_output","thread_id":"foreign","output":"x".repeat(1100*1024)}}),
        json!({"type":"token_usage_record","payload":rollout_response_usage("r1",2,1)}),
    ]);
    assert_eq!(
        agent
            .as_token_calculator()
            .unwrap()
            .calculate_token_usage(&data, 0)
            .unwrap()
            .total_tokens,
        Some(3)
    );
    assert!(
        agent
            .as_prompt_extractor()
            .unwrap()
            .extract_prompts(&data, 0)
            .unwrap()
            .is_empty()
    );
}

#[test]
#[ignore = "operator-local Codex acceptance; never run in CI"]
fn codex_rollout_capability_usage_gaps_return_error_instead_of_a_lower_bound() {
    use libra::internal::ai::observed_agents::{AgentKind, agent_for};
    use serde_json::json;
    let count = |i, o| json!({"type":"event_msg","payload":{"type":"token_count","info":{"total_token_usage":rollout_usage(i,o)}}});
    let mut data = rollout_jsonl(&[
        json!({"type":"session_meta","payload":{"id":"thread-a"}}),
        json!({"type":"token_usage_record","payload":rollout_response_usage("r1",10,4)}),
        count(10, 4),
    ]);
    let offset = data.len();
    data.extend(rollout_jsonl(&[
        json!({"type":"token_usage_record","payload":rollout_response_usage("r2",7,3)}),
        count(27, 11),
    ]));
    let agent = agent_for(AgentKind::Codex);
    let calculator = agent.as_token_calculator().unwrap();
    assert!(
        calculator
            .calculate_token_usage(&data, offset)
            .unwrap_err()
            .to_string()
            .contains("could not be established")
    );
    assert!(calculator.calculate_token_usage(&data, 0).is_err());
    data.extend(rollout_jsonl(&[
        json!({"type":"token_usage_record","payload":rollout_response_usage("missing",10,4)}),
    ]));
    let usage = calculator.calculate_token_usage(&data, offset).unwrap();
    assert_eq!(
        (usage.input_tokens, usage.output_tokens, usage.total_tokens),
        (17, 7, Some(24))
    );
}

#[test]
#[ignore = "operator-local Codex acceptance; never run in CI"]
fn codex_rollout_capability_legacy_first_offsets_never_recount_late_exact_records() {
    use libra::internal::ai::observed_agents::{AgentKind, agent_for};
    use serde_json::json;
    let count = |i, o| json!({"type":"event_msg","payload":{"type":"token_count","info":{"total_token_usage":rollout_usage(i,o)}}});
    let agent = agent_for(AgentKind::Codex);
    let calculator = agent.as_token_calculator().unwrap();
    let mut data = rollout_jsonl(&[
        json!({"type":"session_meta","payload":{"id":"thread-a"}}),
        count(10, 4),
    ]);
    assert_eq!(
        calculator
            .calculate_token_usage(&data, 0)
            .unwrap()
            .total_tokens,
        Some(14)
    );
    let offset = data.len();
    data.extend(rollout_jsonl(&[
        json!({"type":"token_usage_record","payload":rollout_response_usage("r1",10,4)}),
    ]));
    assert!(
        calculator.calculate_token_usage(&data, offset).is_err(),
        "a new exact mirror is not proof of new usage after a legacy prefix"
    );
    data.extend(rollout_jsonl(&[count(17, 7)]));
    let usage = calculator.calculate_token_usage(&data, offset).unwrap();
    assert_eq!((usage.input_tokens, usage.output_tokens), (7, 3));
    let next = data.len();
    data.extend(rollout_jsonl(&[
        json!({"type":"token_usage_record","payload":rollout_response_usage("r2",7,3)}),
    ]));
    assert!(
        calculator.calculate_token_usage(&data, next).is_err(),
        "later windows must keep the same legacy baseline"
    );
    data.extend(rollout_jsonl(&[count(17, 7)]));
    assert_eq!(
        calculator
            .calculate_token_usage(&data, next)
            .unwrap()
            .total_tokens,
        Some(0),
        "unchanged measured cumulative counters prove zero additional tokens"
    );
    data.extend(rollout_jsonl(&[
        count(20, 8),
        json!({"type":"token_usage_record","payload":rollout_response_usage("r3",3,1)}),
    ]));
    let usage = calculator.calculate_token_usage(&data, next).unwrap();
    assert_eq!((usage.input_tokens, usage.output_tokens), (3, 1));
    assert_eq!(
        calculator
            .calculate_token_usage(&data, 0)
            .unwrap()
            .total_tokens,
        Some(28)
    );
}

// CX-03 exercises the real shared hook boundary and its runtime artifacts.
// Synthetic hook frames prove local observability, not model thinking capture.
impl LocalCodexSession {
    fn capture_diagnostic_dir(&self) -> std::path::PathBuf {
        self.repo.join(".libra/private/capture-diagnostics")
    }

    fn capture_diagnostic_hook(
        &self,
        legacy: bool,
        event: &str,
        verb: &str,
    ) -> std::process::Output {
        use std::io::Write;
        let mut args = if legacy {
            vec!["agent", "hooks", "codex"]
        } else {
            vec!["hooks", "codex"]
        };
        args.push(verb);
        let payload = serde_json::json!({
            "hook_event_name":event,"session_id":"123e4567-e89b-12d3-a456-426614174031",
            "cwd":self.repo,"transcript_path":self.source,"turn_id":"cx03-turn",
            "model":"CX03-PAYLOAD-CANARY", "permission_mode":"default"
        });
        let mut child = std::process::Command::new(env!("CARGO_BIN_EXE_libra"))
            .args(args)
            .current_dir(&self.repo)
            .env_clear()
            .env("PATH", "/usr/bin:/bin")
            .env("HOME", &self.home)
            .env("USERPROFILE", &self.home)
            .env("LIBRA_TEST_HOME", &self.home)
            .env("LIBRA_TEST", "1")
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .spawn()
            .unwrap();
        child
            .stdin
            .take()
            .unwrap()
            .write_all(payload.to_string().as_bytes())
            .unwrap();
        child.wait_with_output().unwrap()
    }

    fn capture_diagnostic_summary(&self, doctor: bool) -> serde_json::Value {
        let args = if doctor {
            vec!["agent", "doctor", "--json"]
        } else {
            vec!["agent", "status", "--json"]
        };
        let out = self.run(&args);
        assert!(
            out.status.success(),
            "{}",
            String::from_utf8_lossy(&out.stderr)
        );
        let json: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
        json["data"]["capture_diagnostics"].clone()
    }

    fn capture_diagnostic_records(&self) -> Vec<serde_json::Value> {
        let mut result = Vec::new();
        for slot in ["0.jsonl", "1.jsonl"] {
            let path = self.capture_diagnostic_dir().join(slot);
            if !path.exists() {
                continue;
            }
            let bytes = std::fs::read(&path).unwrap();
            let text = std::str::from_utf8(&bytes).unwrap();
            for line in text.lines() {
                result.push(serde_json::from_str(line).unwrap());
            }
            for private in [
                "CX03-PAYLOAD-CANARY",
                "123e4567-e89b-12d3-a456-426614174031",
                self.repo.to_str().unwrap(),
                self.source.to_str().unwrap(),
            ] {
                assert!(
                    !text.contains(private),
                    "diagnostic must not contain source data"
                );
            }
        }
        result.sort_by_key(|r: &serde_json::Value| r["sequence"].as_u64().unwrap());
        result
    }
}

#[test]
#[ignore = "operator-local Codex acceptance; never run in CI"]
fn capture_diagnostic_absence_and_unknown_ingress_remain_read_only() {
    let fixture = LocalCodexSession::new();
    for doctor in [false, true] {
        let summary = fixture.capture_diagnostic_summary(doctor);
        assert_eq!(summary["state"], "absent");
        assert_eq!(summary["retained_events"], 0);
        assert_eq!(summary["may_have_gaps"], true);
        assert!(!fixture.capture_diagnostic_dir().exists());
    }
    for legacy in [false, true] {
        let out = fixture.capture_diagnostic_hook(legacy, "Unknown-CX03-PAYLOAD-CANARY", "stop");
        assert!(out.status.success());
        assert!(
            !fixture.capture_diagnostic_dir().exists(),
            "unknown input must not gain storage writes"
        );
    }
}

#[tokio::test]
#[ignore = "operator-local Codex acceptance; never run in CI"]
async fn capture_diagnostic_both_entries_count_raw_failure_before_host_policy() {
    use sea_orm::ConnectionTrait;
    for legacy in [false, true] {
        let fixture = LocalCodexSession::new();
        fixture.source(1, true);
        assert!(
            fixture
                .capture_diagnostic_hook(legacy, "SessionStart", "session-start")
                .status
                .success()
        );
        let db = fixture.db().await;
        db.execute_unprepared("CREATE TRIGGER cx03_fail_write BEFORE UPDATE ON agent_session BEGIN SELECT RAISE(ABORT,'CX03-PAYLOAD-CANARY'); END").await.unwrap();
        let out = fixture.capture_diagnostic_hook(legacy, "UserPromptSubmit", "prompt");
        assert_eq!(
            out.status.success(),
            !legacy,
            "retain each existing host exit policy"
        );
        let records = fixture.capture_diagnostic_records();
        assert_eq!(records.len(), 2);
        assert_eq!(records[0]["result"], "runtime_ok");
        assert_eq!(records[1]["result"], "runtime_failed");
        assert_eq!(records[1]["event"], "prompt");
        assert_eq!(
            LocalCodexSession::count(&db, "agent_checkpoint", "1").await,
            0
        );
        let before = codex_finalize_catalog_snapshot(&db).await;
        let journal_before = ["0.jsonl", "1.jsonl", "lock"]
            .map(|name| std::fs::read(fixture.capture_diagnostic_dir().join(name)).unwrap());
        let a = fixture.capture_diagnostic_summary(false);
        let b = fixture.capture_diagnostic_summary(true);
        assert_eq!(a, b);
        assert_eq!(a["runtime_ok"], 1);
        assert_eq!(a["runtime_failed"], 1);
        assert_eq!(a["terminal_failed"], 0);
        assert_eq!(a["retained_events"], 2);
        assert_eq!(before, codex_finalize_catalog_snapshot(&db).await);
        assert_eq!(
            journal_before,
            ["0.jsonl", "1.jsonl", "lock"]
                .map(|name| std::fs::read(fixture.capture_diagnostic_dir().join(name)).unwrap())
        );
        let out = fixture.capture_diagnostic_hook(legacy, "SessionEnd", "session-end");
        assert!(
            !out.status.success(),
            "trusted terminal failure must remain fatal"
        );
        let records = fixture.capture_diagnostic_records();
        assert_eq!(records.last().unwrap()["result"], "terminal_failed");
        assert_eq!(records.last().unwrap()["event"], "session-end");
        db.close().await.unwrap();
    }
}

#[cfg(unix)]
#[test]
#[ignore = "operator-local Codex acceptance; never run in CI"]
fn capture_diagnostic_rejects_links_permissions_and_content_without_recursion() {
    use std::os::unix::fs::{PermissionsExt, symlink};
    for attack in [
        "directory-link",
        "file-link",
        "hard-link",
        "permissions",
        "corrupt",
        "fifo",
        "lock-link",
        "directory-permissions",
        "oversize",
        "truncated-tail",
    ] {
        let fixture = LocalCodexSession::new();
        fixture.source(1, true);
        assert!(
            fixture
                .capture_diagnostic_hook(true, "SessionStart", "session-start")
                .status
                .success()
        );
        let dir = fixture.capture_diagnostic_dir();
        assert_eq!(
            std::fs::metadata(&dir).unwrap().permissions().mode() & 0o777,
            0o700
        );
        for name in ["0.jsonl", "1.jsonl", "lock"] {
            assert_eq!(
                std::fs::metadata(dir.join(name))
                    .unwrap()
                    .permissions()
                    .mode()
                    & 0o777,
                0o600
            );
        }
        let outside = fixture._temp.path().join("outside");
        std::fs::write(&outside, b"CX03-OUTSIDE-CANARY").unwrap();
        match attack {
            "directory-link" => {
                std::fs::remove_dir_all(&dir).unwrap();
                symlink(fixture._temp.path(), &dir).unwrap();
            }
            "file-link" => {
                std::fs::remove_file(dir.join("0.jsonl")).unwrap();
                symlink(&outside, dir.join("0.jsonl")).unwrap();
            }
            "hard-link" => {
                use std::os::unix::fs::MetadataExt;
                // Isolate nlink validation from both permission and decoder
                // rejection: the linked file has an otherwise valid record.
                std::fs::write(&outside, std::fs::read(dir.join("0.jsonl")).unwrap()).unwrap();
                std::fs::set_permissions(&outside, std::fs::Permissions::from_mode(0o600)).unwrap();
                std::fs::remove_file(dir.join("0.jsonl")).unwrap();
                std::fs::hard_link(&outside, dir.join("0.jsonl")).unwrap();
                assert_eq!(std::fs::metadata(dir.join("0.jsonl")).unwrap().nlink(), 2);
            }
            "permissions" => std::fs::set_permissions(
                dir.join("0.jsonl"),
                std::fs::Permissions::from_mode(0o644),
            )
            .unwrap(),
            "corrupt" => {
                std::fs::write(dir.join("0.jsonl"), b"untrusted CX03-PAYLOAD-CANARY\n").unwrap()
            }
            "fifo" => {
                use std::os::unix::ffi::OsStrExt;
                std::fs::remove_file(dir.join("0.jsonl")).unwrap();
                let name =
                    std::ffi::CString::new(dir.join("0.jsonl").as_os_str().as_bytes()).unwrap();
                assert_eq!(unsafe { libc::mkfifo(name.as_ptr(), 0o600) }, 0);
            }
            "lock-link" => {
                std::fs::remove_file(dir.join("lock")).unwrap();
                symlink(&outside, dir.join("lock")).unwrap();
            }
            "directory-permissions" => {
                std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o755)).unwrap()
            }
            "oversize" => std::fs::OpenOptions::new()
                .write(true)
                .open(dir.join("0.jsonl"))
                .unwrap()
                .set_len(5 * 1024 * 1024 + 1)
                .unwrap(),
            "truncated-tail" => {
                use std::io::Write;
                std::fs::OpenOptions::new()
                    .append(true)
                    .open(dir.join("0.jsonl"))
                    .unwrap()
                    .write_all(b"{\"schema\":1")
                    .unwrap();
            }
            _ => unreachable!(),
        }
        let outside_before = std::fs::read(&outside).unwrap();
        let out = fixture.capture_diagnostic_hook(true, "UserPromptSubmit", "prompt");
        assert!(
            out.status.success(),
            "logging failure must not break successful capture"
        );
        assert!(
            out.stderr.is_empty(),
            "logging failure must not recurse to stderr"
        );
        for doctor in [false, true] {
            let summary = fixture.capture_diagnostic_summary(doctor);
            assert_eq!(summary["state"], "unavailable", "{attack}: {summary}");
            assert_eq!(summary["retained_events"], 0);
            assert!(!summary.to_string().contains("CANARY"));
        }
        assert_eq!(std::fs::read(&outside).unwrap(), outside_before);
    }
}

#[test]
#[ignore = "operator-local Codex acceptance; never run in CI"]
fn capture_diagnostic_lock_contention_preserves_capture_and_readers_do_not_write() {
    let fixture = LocalCodexSession::new();
    fixture.source(1, true);
    assert!(
        fixture
            .capture_diagnostic_hook(true, "SessionStart", "session-start")
            .status
            .success()
    );
    let dir = fixture.capture_diagnostic_dir();
    let lock = std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .open(dir.join("lock"))
        .unwrap();
    lock.try_lock().unwrap();
    let before = std::fs::read(dir.join("0.jsonl")).unwrap();
    assert!(
        fixture
            .capture_diagnostic_hook(true, "UserPromptSubmit", "prompt")
            .status
            .success()
    );
    assert_eq!(std::fs::read(dir.join("0.jsonl")).unwrap(), before);
    for doctor in [false, true] {
        assert_eq!(
            fixture.capture_diagnostic_summary(doctor)["state"],
            "unavailable"
        );
    }
    assert_eq!(std::fs::read(dir.join("0.jsonl")).unwrap(), before);
    drop(lock);
    assert_eq!(
        fixture.capture_diagnostic_summary(false)["retained_events"],
        1
    );
    let shared = std::fs::OpenOptions::new()
        .read(true)
        .open(dir.join("lock"))
        .unwrap();
    shared.try_lock_shared().unwrap();
    for doctor in [false, true] {
        let summary = fixture.capture_diagnostic_summary(doctor);
        assert_eq!(summary["state"], "available", "readers must coexist");
        assert_eq!(summary["retained_events"], 1);
    }
    assert!(
        fixture
            .capture_diagnostic_hook(true, "UserPromptSubmit", "prompt")
            .status
            .success()
    );
    assert_eq!(std::fs::read(dir.join("0.jsonl")).unwrap(), before);
    drop(shared);
}

#[test]
#[ignore = "operator-local Codex acceptance; never run in CI"]
fn capture_diagnostic_rotation_caps_total_bytes_and_exposes_retained_window() {
    use std::io::Write;
    let fixture = LocalCodexSession::new();
    fixture.source(1, true);
    assert!(
        fixture
            .capture_diagnostic_hook(false, "SessionStart", "session-start")
            .status
            .success()
    );
    let dir = fixture.capture_diagnostic_dir();
    const CAP: u64 = 5 * 1024 * 1024;
    let mut seq = 0u64;
    for name in ["0.jsonl", "1.jsonl"] {
        let mut file = std::fs::OpenOptions::new()
            .write(true)
            .truncate(true)
            .open(dir.join(name))
            .unwrap();
        let mut size = 0;
        loop {
            let line = format!(
                "{{\"schema\":1,\"sequence\":{},\"provider\":\"codex\",\"event\":\"session-start\",\"result\":\"runtime_ok\",\"count\":1}}\n",
                seq + 1
            );
            // Leave less than one record free in each slot.
            if size + line.len() as u64 > CAP {
                break;
            }
            file.write_all(line.as_bytes()).unwrap();
            size += line.len() as u64;
            seq += 1;
        }
    }
    assert!(
        fixture
            .capture_diagnostic_hook(false, "UserPromptSubmit", "prompt")
            .status
            .success()
    );
    let records = fixture.capture_diagnostic_records();
    assert_eq!(records.last().unwrap()["sequence"], seq + 1);
    assert!(
        records[0]["sequence"].as_u64().unwrap() > 1,
        "rotation drops older slot only"
    );
    assert_eq!(records.last().unwrap()["event"], "prompt");
    let total: u64 = ["0.jsonl", "1.jsonl", "lock"]
        .map(|n| std::fs::metadata(dir.join(n)).unwrap().len())
        .iter()
        .sum();
    assert!(total <= 10 * 1024 * 1024);
    let summary = fixture.capture_diagnostic_summary(false);
    assert_eq!(summary["state"], "available");
    assert_eq!(summary["retained_events"], records.len());
    assert_eq!(summary["first_sequence"], records[0]["sequence"]);
    assert_eq!(summary["last_sequence"], seq + 1);
    assert_eq!(summary, fixture.capture_diagnostic_summary(true));
}

#[test]
#[ignore = "operator-local Codex acceptance; never run in CI"]
fn capture_diagnostic_linked_worktree_shares_the_repository_window() {
    let mut fixture = LocalCodexSession::new();
    let main = fixture.repo.clone();
    let linked = fixture._temp.path().join("linked");
    let out = fixture.run(&[
        "worktree",
        "add",
        "linked",
        "--path",
        linked.to_str().unwrap(),
    ]);
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    fixture.source(1, true);
    assert!(
        fixture
            .capture_diagnostic_hook(false, "SessionStart", "session-start")
            .status
            .success()
    );
    assert_eq!(
        fixture.capture_diagnostic_summary(false)["retained_events"],
        1
    );
    fixture.repo = linked.canonicalize().unwrap();
    assert_eq!(
        fixture.capture_diagnostic_summary(false)["retained_events"],
        1
    );
    assert_eq!(
        fixture.capture_diagnostic_summary(true)["retained_events"],
        1
    );
    fixture.source(1, true);
    assert!(
        fixture
            .capture_diagnostic_hook(false, "UserPromptSubmit", "prompt")
            .status
            .success()
    );
    let from_linked = fixture.capture_diagnostic_summary(false);
    assert_eq!(from_linked["retained_events"], 2);
    assert!(
        !fixture.capture_diagnostic_dir().exists(),
        "journal belongs to common storage"
    );
    fixture.repo = main;
    assert_eq!(from_linked, fixture.capture_diagnostic_summary(false));
    assert_eq!(from_linked, fixture.capture_diagnostic_summary(true));
    assert_eq!(fixture.capture_diagnostic_records().len(), 2);
}

#[tokio::test]
#[ignore = "operator-local Codex acceptance; never run in CI"]
async fn capture_diagnostic_other_provider_admission_does_not_create_journal() {
    use std::io::Write;
    let fixture = LocalCodexSession::new();
    let payload = serde_json::json!({"hook_event_name":"SessionStart",
        "session_id":"cx03-other-provider", "cwd":fixture.repo});
    let mut child = std::process::Command::new(env!("CARGO_BIN_EXE_libra"))
        .args(["agent", "hooks", "claude-code", "session-start"])
        .current_dir(&fixture.repo)
        .env_clear()
        .env("PATH", "/usr/bin:/bin")
        .env("HOME", &fixture.home)
        .env("USERPROFILE", &fixture.home)
        .env("LIBRA_TEST_HOME", &fixture.home)
        .env("LIBRA_TEST", "1")
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .unwrap();
    child
        .stdin
        .take()
        .unwrap()
        .write_all(payload.to_string().as_bytes())
        .unwrap();
    let out = child.wait_with_output().unwrap();
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let db = fixture.db().await;
    assert_eq!(
        LocalCodexSession::count(&db, "agent_session", "agent_kind = 'claude_code'").await,
        1,
        "prove actual admission instead of a rejected/no-op frame"
    );
    assert!(!fixture.capture_diagnostic_dir().exists());
    assert_eq!(fixture.capture_diagnostic_summary(false)["state"], "absent");
    assert_eq!(fixture.capture_diagnostic_summary(true)["state"], "absent");
    assert!(!fixture.capture_diagnostic_dir().exists());
    db.close().await.unwrap();
}

impl LocalCodexSession {
    fn codex_session_summary_source(&self, turns: usize, usage: bool) {
        use serde_json::json;
        self.source(turns, true);
        let text = std::fs::read_to_string(&self.source).unwrap();
        let mut rows = Vec::new();
        let mut turn = 0usize;
        for line in text.lines() {
            let mut row: serde_json::Value = serde_json::from_str(line).unwrap();
            match row["type"].as_str().unwrap() {
                "session_meta" => {
                    row["payload"]["cli_version"] = json!("0.162.1");
                    rows.push(row);
                    rows.push(json!({"type":"turn_context","payload":{"turn_id":"cx15-multi-0","model":"CX07-MODEL"}}));
                    continue;
                }
                "event_msg" if row["payload"]["type"] == "task_started" => {
                    turn = row["payload"]["turn_id"]
                        .as_str()
                        .unwrap()
                        .rsplit('-')
                        .next()
                        .unwrap()
                        .parse()
                        .unwrap();
                    row["timestamp"] = json!(format!("2026-10-19T01:00:{:02}Z", turn * 10 + 1));
                }
                "response_item" => {
                    let (tag, offset) = match row["payload"]["type"].as_str().unwrap() {
                        "reasoning" => ("reasoning", 3),
                        "message" if row["payload"]["role"] == "user" => {
                            row["payload"]["internal_chat_message_metadata_passthrough"] =
                                json!({"content_item_kinds":["user.text"]});
                            ("user", 2)
                        }
                        _ => ("assistant", 4),
                    };
                    row["payload"]["id"] = json!(format!("cx07-{tag}-{turn}"));
                    row["timestamp"] =
                        json!(format!("2026-10-19T01:00:{:02}Z", turn * 10 + offset));
                }
                "session_end" => {
                    if usage {
                        let mut counters = rollout_usage(17, 7);
                        counters["cached_input_tokens"] = json!(2);
                        counters["cache_write_input_tokens"] = json!(3);
                        counters["reasoning_output_tokens"] = json!(1);
                        rows.push(json!({"type":"event_msg","payload":{"type":"token_count","info":{"total_token_usage":counters}}}));
                    }
                    rows.push(json!({"type":"event_msg","payload":{"type":"item_completed","turn_id":format!("cx15-multi-{turn}"),"item":{"type":"FileChange","id":"cx07-patch","status":"completed","changes":{"src/snake.rs":{"type":"update"}}}}}));
                    row["timestamp"] = json!(format!("2026-10-19T01:00:{:02}Z", turn * 10 + 7));
                }
                _ => {}
            }
            rows.push(row);
        }
        std::fs::write(&self.source, rollout_jsonl(&rows)).unwrap();
    }

    fn codex_session_summary_view(&self) -> serde_json::Value {
        let json = codex_finalize_show(self);
        assert!(
            !json["summary"].is_null(),
            "Codex summary must be explicit: {json}"
        );
        json["summary"].clone()
    }
}

#[tokio::test]
#[ignore = "operator-local Codex acceptance; never run in CI"]
async fn codex_session_summary_import_uses_stored_snapshot_after_source_removal() {
    let fixture = LocalCodexSession::new();
    fixture.codex_session_summary_source(2, true);
    let imported = fixture.import();
    assert!(
        imported.status.success(),
        "{}",
        String::from_utf8_lossy(&imported.stderr)
    );
    let db = fixture.db().await;
    let before = codex_finalize_catalog_snapshot(&db).await;
    std::fs::remove_file(&fixture.source).unwrap();
    let summary = fixture.codex_session_summary_view();
    assert!(
        matches!(summary["status"].as_str(), Some("available" | "partial")),
        "{summary}"
    );
    assert_eq!(summary["source_status"], "unproven");
    assert_eq!(summary["model"], "CX07-MODEL");
    assert_eq!(summary["prompt_count"], 2);
    assert_eq!(summary["modified_file_count"], 1);
    assert_eq!(summary["token_usage"]["input_tokens"], 17);
    assert_eq!(summary["token_usage"]["output_tokens"], 7);
    assert_eq!(summary["token_usage"]["total_tokens"], 24);
    assert_eq!(summary["token_usage"]["cached_tokens"], 5);
    assert_eq!(summary["token_usage"]["reasoning_tokens"], 1);
    assert!(summary["as_of_checkpoint"].as_str().is_some());
    assert!(!summary.to_string().contains("CX15-MULTI"));
    assert!(
        !summary
            .to_string()
            .contains(fixture.source.to_str().unwrap())
    );
    assert_eq!(before, codex_finalize_catalog_snapshot(&db).await);
    let exports = fixture.exports(&db).await.join("\n");
    assert!(
        exports.contains("CX15-MULTI-0") && exports.contains("CX15-MULTI-1"),
        "optional metadata must preserve the ordinary thinking fixture"
    );
    db.close().await.unwrap();
}

#[tokio::test]
#[ignore = "operator-local Codex acceptance; never run in CI"]
async fn codex_session_summary_metadata_only_live_never_falls_back_to_provider_file() {
    let mut fixture = LocalCodexSession::new();
    fixture.codex_session_summary_source(2, true);
    let decoy = fixture._temp.path().join("untrusted-transcript.jsonl");
    std::fs::rename(&fixture.source, &decoy).unwrap();
    fixture.source = decoy;
    assert!(
        fixture
            .cx01_hook("SessionStart", "session-start")
            .status
            .success()
    );
    assert!(fixture.cx01_hook("Stop", "stop").status.success());
    // A path supplied by the hook is outside the provider root. Its valid
    // contents cannot authorize a reader-side fallback, including after the
    // later live-candidate card has been implemented.
    assert!(fixture.source.exists());
    let summary = fixture.codex_session_summary_view();
    assert_eq!(summary["status"], "unknown");
    assert!(summary["model"].is_null());
    assert!(summary["prompt_count"].is_null());
    assert!(summary["modified_file_count"].is_null());
    assert!(summary["token_usage"].is_null());
    assert!(summary["as_of_checkpoint"].is_string());
    let db = fixture.db().await;
    assert_eq!(
        LocalCodexSession::count(&db, "agent_checkpoint", "1").await,
        1
    );
    db.close().await.unwrap();
}

#[tokio::test]
#[ignore = "operator-local Codex acceptance; never run in CI"]
async fn codex_session_summary_pending_masks_usage_and_keeps_completed_evidence() {
    let (fixture, db) = codex_finalize_pending_fixture().await;
    let before = codex_finalize_catalog_snapshot(&db).await;
    let summary = fixture.codex_session_summary_view();
    assert_eq!(summary["status"], "pending");
    assert!(summary["token_usage"].is_null());
    assert_eq!(before, codex_finalize_catalog_snapshot(&db).await);
    assert_eq!(
        LocalCodexSession::count(&db, "agent_checkpoint", "1").await,
        1
    );
    db.close().await.unwrap();
}

#[tokio::test]
#[ignore = "operator-local Codex acceptance; never run in CI"]
async fn codex_session_summary_legacy_and_unmeasured_usage_are_unknown() {
    let legacy = LocalCodexSession::new();
    assert!(
        legacy
            .cx01_hook("SessionStart", "session-start")
            .status
            .success()
    );
    let summary = legacy.codex_session_summary_view();
    assert_eq!(summary["status"], "unknown");
    assert!(summary["model"].is_null());
    assert!(summary["prompt_count"].is_null());
    assert!(summary["token_usage"].is_null());
    assert!(summary["as_of_checkpoint"].is_null());
    let fixture = LocalCodexSession::new();
    fixture.codex_session_summary_source(1, false);
    assert!(fixture.import().status.success());
    let summary = fixture.codex_session_summary_view();
    assert_eq!(summary["model"], "CX07-MODEL");
    assert_eq!(summary["prompt_count"], 1);
    assert!(
        summary["token_usage"].is_null(),
        "absence must not turn into measured zero"
    );
}

#[tokio::test]
#[ignore = "operator-local Codex acceptance; never run in CI"]
async fn codex_session_summary_replay_append_and_corrupt_role_do_not_rewrite_checkpoints() {
    use sea_orm::ConnectionTrait;
    let fixture = LocalCodexSession::new();
    fixture.codex_session_summary_source(2, true);
    assert!(fixture.import().status.success());
    let db = fixture.db().await;
    let before = codex_legacy_reader_state(&db).await;
    let old_rows = db.query_all_raw(sea_orm::Statement::from_string(db.get_database_backend(),
        "SELECT checkpoint_id, tree_oid, metadata_blob_oid, traces_commit FROM agent_checkpoint".to_owned())).await.unwrap();
    let immutable: Vec<(String, String, String, String)> = old_rows
        .iter()
        .map(|row| {
            (
                row.try_get_by("checkpoint_id").unwrap(),
                row.try_get_by("tree_oid").unwrap(),
                row.try_get_by("metadata_blob_oid").unwrap(),
                row.try_get_by("traces_commit").unwrap(),
            )
        })
        .collect();
    let first = fixture.codex_session_summary_view();
    assert_eq!(first["prompt_count"], 2);
    assert!(fixture.import().status.success());
    assert_eq!(before, codex_legacy_reader_state(&db).await);
    fixture.codex_session_summary_source(3, true);
    assert!(fixture.import().status.success());
    assert_eq!(fixture.codex_session_summary_view()["prompt_count"], 3);
    for (id, tree, metadata, commit) in immutable {
        let row = db.query_one_raw(sea_orm::Statement::from_sql_and_values(db.get_database_backend(),
            "SELECT tree_oid, metadata_blob_oid, traces_commit FROM agent_checkpoint WHERE checkpoint_id=?", [id.into()])).await.unwrap().unwrap();
        assert_eq!(row.try_get_by::<String, _>("tree_oid").unwrap(), tree);
        assert_eq!(
            row.try_get_by::<String, _>("metadata_blob_oid").unwrap(),
            metadata
        );
        assert_eq!(
            row.try_get_by::<String, _>("traces_commit").unwrap(),
            commit
        );
    }
    let rows = db.query_all_raw(sea_orm::Statement::from_string(db.get_database_backend(),
        "SELECT checkpoint_id, tree_oid, metadata_blob_oid FROM agent_checkpoint ORDER BY created_at, checkpoint_id".to_owned())).await.unwrap();
    assert!(rows.len() >= 3);
    db.execute_unprepared("UPDATE agent_checkpoint SET metadata_blob_oid = printf('%040d',0)")
        .await
        .unwrap();
    let damaged = codex_finalize_catalog_snapshot(&db).await;
    let summary = fixture.codex_session_summary_view();
    assert_eq!(summary["status"], "unavailable");
    assert!(summary["model"].is_null());
    assert!(summary["token_usage"].is_null());
    assert_eq!(damaged, codex_finalize_catalog_snapshot(&db).await);
    db.close().await.unwrap();
}

#[tokio::test]
#[ignore = "operator-local Codex acceptance; never run in CI"]
async fn codex_session_summary_nullable_legacy_metadata_keeps_checkpoint_facts() {
    use sea_orm::ConnectionTrait;
    let fixture = LocalCodexSession::new();
    fixture.codex_session_summary_source(1, true);
    assert!(fixture.import().status.success());
    let db = fixture.db().await;
    // Current migration has NOT NULL. Reconstruct an older/embedded nullable
    // row shape without changing the production schema or migration history.
    db.execute_unprepared("PRAGMA foreign_keys=OFF")
        .await
        .unwrap();
    db.execute_unprepared("ALTER TABLE agent_session RENAME TO cx07_original_session")
        .await
        .unwrap();
    db.execute_unprepared("CREATE TABLE agent_session AS SELECT * FROM cx07_original_session")
        .await
        .unwrap();
    db.execute_unprepared("UPDATE agent_session SET metadata_json=NULL")
        .await
        .unwrap();
    let before = codex_finalize_catalog_snapshot(&db).await;
    let summary = fixture.codex_session_summary_view();
    assert!(
        matches!(summary["status"].as_str(), Some("available" | "partial")),
        "{summary}"
    );
    assert_eq!(summary["model"], "CX07-MODEL");
    assert_eq!(summary["token_usage"]["total_tokens"], 24);
    assert_eq!(before, codex_finalize_catalog_snapshot(&db).await);
    db.close().await.unwrap();
}

#[tokio::test]
#[ignore = "operator-local Codex acceptance; never run in CI"]
async fn codex_session_summary_import_pending_masks_measured_usage_and_recovers() {
    use sea_orm::{ConnectionTrait, Statement};
    let fixture = LocalCodexSession::new();
    fixture.codex_session_summary_source(1, true);
    assert!(fixture.import().status.success());
    let db = fixture.db().await;
    let ready = fixture.codex_session_summary_view();
    assert_eq!(ready["token_usage"]["total_tokens"], 24);
    for state in ["leased", "writing"] {
        db.execute_unprepared(&format!("UPDATE agent_import_identity SET state='{state}'"))
            .await
            .unwrap();
        let before = codex_finalize_catalog_snapshot(&db).await;
        let pending = fixture.codex_session_summary_view();
        assert_eq!(pending["status"], "pending");
        assert!(pending["token_usage"].is_null());
        assert_eq!(pending["model"], "CX07-MODEL");
        assert_eq!(pending["prompt_count"], 1);
        assert_eq!(before, codex_finalize_catalog_snapshot(&db).await);
    }
    db.execute_unprepared("UPDATE agent_import_identity SET state='committed'")
        .await
        .unwrap();
    assert_eq!(fixture.codex_session_summary_view(), ready);
    let row = db
        .query_one_raw(Statement::from_string(
            db.get_database_backend(),
            "SELECT metadata_json FROM agent_session".to_owned(),
        ))
        .await
        .unwrap()
        .unwrap();
    let original: String = row.try_get_by("metadata_json").unwrap();
    let mut metadata: serde_json::Value = serde_json::from_str(&original).unwrap();
    metadata["capture_status"] = serde_json::json!("retryable_error");
    db.execute_raw(Statement::from_sql_and_values(
        db.get_database_backend(),
        "UPDATE agent_session SET metadata_json=?",
        [serde_json::to_string_pretty(&metadata).unwrap().into()],
    ))
    .await
    .unwrap();
    let before = codex_finalize_catalog_snapshot(&db).await;
    let pending = fixture.codex_session_summary_view();
    assert_eq!(pending["status"], "pending");
    assert!(pending["token_usage"].is_null());
    assert_eq!(pending["model"], "CX07-MODEL");
    assert_eq!(before, codex_finalize_catalog_snapshot(&db).await);
    db.execute_raw(Statement::from_sql_and_values(
        db.get_database_backend(),
        "UPDATE agent_session SET metadata_json=?",
        [original.into()],
    ))
    .await
    .unwrap();
    assert_eq!(fixture.codex_session_summary_view(), ready);
    db.close().await.unwrap();
}

#[tokio::test]
#[ignore = "operator-local Codex acceptance; never run in CI"]
async fn codex_index_live_stores_thinking_and_summary_without_source_fallback() {
    let fixture = LocalCodexSession::new();
    fixture.codex_session_summary_source(2, true);
    let output = fixture.cx01_hook("Stop", "stop");
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let db = fixture.db().await;
    assert_eq!(
        LocalCodexSession::count(&db, "agent_checkpoint", "1").await,
        1
    );
    let exports = fixture.exports(&db).await.join("\n");
    assert!(
        exports.contains("CX15-MULTI-0") && exports.contains("CX15-MULTI-1"),
        "a successful advisory hook is insufficient: {exports}"
    );
    let summary = fixture.codex_session_summary_view();
    assert_eq!(summary["model"], "CX07-MODEL");
    assert_eq!(summary["prompt_count"], 2);
    assert_eq!(summary["token_usage"]["total_tokens"], 24);
    let before = codex_finalize_catalog_snapshot(&db).await;
    std::fs::remove_file(&fixture.source).unwrap();
    assert_eq!(fixture.exports(&db).await.join("\n"), exports);
    assert_eq!(fixture.codex_session_summary_view(), summary);
    assert_eq!(before, codex_finalize_catalog_snapshot(&db).await);
    db.close().await.unwrap();
}

#[tokio::test]
#[ignore = "operator-local Codex acceptance; never run in CI"]
async fn codex_index_live_scope_mismatch_does_not_return_thinking() {
    let fixture = LocalCodexSession::new();
    fixture.codex_session_summary_source(1, true);
    let raw = std::fs::read_to_string(&fixture.source).unwrap();
    let mut rows: Vec<serde_json::Value> = raw
        .lines()
        .map(|line| serde_json::from_str(line).unwrap())
        .collect();
    rows[0]["payload"]["cwd"] = serde_json::json!(fixture.home);
    std::fs::write(&fixture.source, rollout_jsonl(&rows)).unwrap();
    let output = fixture.cx01_hook("Stop", "stop");
    assert!(output.status.success());
    let db = fixture.db().await;
    assert!(!fixture.exports(&db).await.join("\n").contains("CX15-MULTI"));
    assert!(fixture.codex_session_summary_view()["model"].is_null());
    db.close().await.unwrap();
}

impl LocalCodexSession {
    fn codex_index_hook(
        &self,
        budget: Option<u64>,
        root: Option<&std::path::Path>,
        omit_home: bool,
    ) -> std::process::Output {
        self.codex_index_hook_for_turn(budget, root, omit_home, "cx01-turn")
    }

    fn codex_index_hook_for_turn(
        &self,
        budget: Option<u64>,
        root: Option<&std::path::Path>,
        omit_home: bool,
        turn_id: &str,
    ) -> std::process::Output {
        self.codex_index_hook_at(budget, root, omit_home, turn_id, &self.repo)
    }

    fn codex_index_hook_at(
        &self,
        budget: Option<u64>,
        root: Option<&std::path::Path>,
        omit_home: bool,
        turn_id: &str,
        cwd: &std::path::Path,
    ) -> std::process::Output {
        use std::io::Write;
        let payload = serde_json::json!({"hook_event_name":"Stop", "session_id":"123e4567-e89b-12d3-a456-426614174031", "cwd":cwd, "turn_id":turn_id, "transcript_path":self.source});
        let mut command = std::process::Command::new(env!("CARGO_BIN_EXE_libra"));
        command
            .args(["hooks", "codex", "stop"])
            .current_dir(&self.repo)
            .env_clear()
            .env("PATH", "/usr/bin:/bin")
            .env("LIBRA_TEST_HOME", &self.home)
            .env("LIBRA_TEST", "1")
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped());
        if !omit_home {
            command
                .env("HOME", &self.home)
                .env("USERPROFILE", &self.home);
        }
        if let Some(root) = root {
            command.env("CODEX_HOME", root);
        }
        if let Some(budget) = budget {
            command.args(["--capture-budget-ms", &budget.to_string()]);
        }
        let mut child = command.spawn().unwrap();
        child
            .stdin
            .take()
            .unwrap()
            .write_all(payload.to_string().as_bytes())
            .unwrap();
        child.wait_with_output().unwrap()
    }
}

#[tokio::test]
#[ignore = "operator-local Codex acceptance; never run in CI"]
async fn codex_index_live_explicit_home_with_and_without_home_environment() {
    for omit_home in [false, true] {
        let fixture = LocalCodexSession::new();
        fixture.codex_session_summary_source(1, true);
        let root = fixture._temp.path().join("alternate-codex");
        let relative = fixture
            .source
            .strip_prefix(fixture.home.join(".codex"))
            .unwrap();
        let moved = root.join(relative);
        std::fs::create_dir_all(moved.parent().unwrap()).unwrap();
        std::fs::rename(&fixture.source, &moved).unwrap();
        let output = fixture.codex_index_hook(Some(2000), Some(&root), omit_home);
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        let db = fixture.db().await;
        assert!(
            fixture
                .exports(&db)
                .await
                .join("\n")
                .contains("CX15-MULTI-0")
        );
        db.close().await.unwrap();
    }
}

#[tokio::test]
#[ignore = "operator-local Codex acceptance; never run in CI"]
async fn codex_index_live_missing_host_budget_never_reads_parent() {
    let fixture = LocalCodexSession::new();
    fixture.codex_session_summary_source(1, true);
    let before = std::fs::read(&fixture.source).unwrap();
    let output = fixture.codex_index_hook(None, None, false);
    assert!(output.status.success());
    let db = fixture.db().await;
    assert!(
        !fixture
            .exports(&db)
            .await
            .join("\n")
            .contains("CX15-MULTI-0")
    );
    assert_eq!(std::fs::read(&fixture.source).unwrap(), before);
    assert!(fixture.codex_session_summary_view()["model"].is_null());
    // The paired codex_index_no_deadline_never_reads_parent lib case uses
    // counters/deny adapters to prove zero resolver/raw-read/locator IO.
    db.close().await.unwrap();
}

#[tokio::test]
#[ignore = "operator-local Codex acceptance; never run in CI"]
async fn codex_index_live_identical_replay_is_idempotent_and_growth_appends() {
    let fixture = LocalCodexSession::new();
    fixture.codex_session_summary_source(1, true);
    assert!(fixture.cx01_hook("Stop", "stop").status.success());
    let db = fixture.db().await;
    let before = fixture.exports(&db).await;
    assert_eq!(before.len(), 1);
    assert!(fixture.cx01_hook("Stop", "stop").status.success());
    assert_eq!(fixture.exports(&db).await, before);
    fixture.codex_session_summary_source(2, true);
    assert!(
        fixture
            .codex_index_hook_for_turn(Some(2000), None, false, "cx15-multi-1")
            .status
            .success()
    );
    let after = fixture.exports(&db).await;
    assert_eq!(after.len(), 2);
    assert!(
        after.contains(&before[0]),
        "prior stored checkpoint changed"
    );
    assert!(after.join("\n").contains("CX15-MULTI-1"));
    db.close().await.unwrap();
}

#[tokio::test]
#[ignore = "operator-local Codex acceptance; never run in CI"]
async fn codex_index_live_reasoning_only_and_escaped_credentials_remain_readable() {
    let fixture = LocalCodexSession::new();
    fixture.codex_session_summary_source(1, true);
    let raw = std::fs::read_to_string(&fixture.source).unwrap();
    let mut rows: Vec<serde_json::Value> = raw
        .lines()
        .map(|line| serde_json::from_str(line).unwrap())
        .collect();
    rows.retain(|row| !(row["type"] == "response_item" && row["payload"]["type"] == "message"));
    let escaped = "sk-ant-EXAMPLE0000000000000000"
        .bytes()
        .map(|b| format!("\\u{b:04x}"))
        .collect::<String>();
    let source = String::from_utf8(rollout_jsonl(&rows))
        .unwrap()
        .replace("CX15-MULTI-0", &format!("CX15-MULTI-0 {escaped}"));
    assert!(!source.contains("EXAMPLE0000000000000000"));
    std::fs::write(&fixture.source, source).unwrap();
    assert!(fixture.cx01_hook("Stop", "stop").status.success());
    let db = fixture.db().await;
    let export = fixture.exports(&db).await.join("\n");
    assert!(
        export.contains("CX15-MULTI-0"),
        "reasoning-only body was lost: {export}"
    );
    assert!(export.contains("<REDACTED:anthropic-api-key>"));
    assert!(!export.contains("EXAMPLE0000000000000000"));
    assert!(!export.contains(&escaped));
    assert!(
        export.contains("opaque-0"),
        "opaque data was silently dropped"
    );
    db.close().await.unwrap();
}

#[tokio::test]
#[ignore = "operator-local Codex acceptance; never run in CI"]
async fn codex_index_live_newest_target_ignores_thousands_of_old_rollouts() {
    let fixture = LocalCodexSession::new();
    fixture.codex_session_summary_source(1, true);
    let old = fixture.home.join(".codex/sessions/2020/01/01");
    std::fs::create_dir_all(&old).unwrap();
    for i in 0..5000 {
        std::fs::write(
            old.join(format!("rollout-old-{i}.jsonl")),
            b"old history must not be read",
        )
        .unwrap();
    }
    let started = std::time::Instant::now();
    assert!(
        fixture
            .codex_index_hook(Some(2000), None, false)
            .status
            .success()
    );
    assert!(started.elapsed() < std::time::Duration::from_secs(4));
    let db = fixture.db().await;
    assert!(
        fixture
            .exports(&db)
            .await
            .join("\n")
            .contains("CX15-MULTI-0")
    );
    db.close().await.unwrap();
}

#[tokio::test]
#[ignore = "operator-local Codex acceptance; never run in CI"]
async fn codex_index_live_wrong_header_id_is_unavailable() {
    let fixture = LocalCodexSession::new();
    fixture.codex_session_summary_source(1, true);
    let text = std::fs::read_to_string(&fixture.source).unwrap().replace(
        "123e4567-e89b-12d3-a456-426614174031",
        "123e4567-e89b-12d3-a456-426614174099",
    );
    std::fs::write(&fixture.source, text).unwrap();
    assert!(fixture.cx01_hook("Stop", "stop").status.success());
    let db = fixture.db().await;
    assert!(
        !fixture
            .exports(&db)
            .await
            .join("\n")
            .contains("CX15-MULTI-0")
    );
    assert!(fixture.codex_session_summary_view()["model"].is_null());
    db.close().await.unwrap();
}

impl LocalCodexSession {
    fn codex_index_many_short_strings_source(&self) -> usize {
        self.codex_session_summary_source(1, true);
        let mut bytes = std::fs::read(&self.source).unwrap();
        let line = b"{\"type\":\"event_msg\",\"payload\":{\"type\":\"agent_reasoning\",\"text\":\"short\",\"extra\":[\"a\",\"b\",\"c\",\"d\"]}}\n";
        let cap = 16 * 1024 * 1024;
        let mut rows = 0;
        while bytes.len() + line.len() <= cap {
            bytes.extend_from_slice(line);
            rows += 1;
        }
        // Finish with JSON whitespace, keeping the source exactly at the cap.
        bytes.resize(cap, b'\n');
        std::fs::write(&self.source, &bytes).unwrap();
        assert_eq!(bytes.len(), cap);
        assert!(rows > 100_000);
        rows
    }
}

#[tokio::test]
#[ignore = "operator-local Codex acceptance; never run in CI"]
async fn codex_index_live_many_short_strings_deadline() {
    let fixture = LocalCodexSession::new();
    let rows = fixture.codex_index_many_short_strings_source();
    let start = std::time::Instant::now();
    let output = fixture.codex_index_hook(Some(300), None, false);
    let elapsed = start.elapsed();
    assert!(
        elapsed < std::time::Duration::from_secs(3),
        "unbounded short-string scanner: {elapsed:?}"
    );
    assert!(output.stdout.is_empty());
    assert!(!String::from_utf8_lossy(&output.stderr).contains("CX15-MULTI"));
    let db = fixture.db().await;
    assert!(
        !fixture.exports(&db).await.join("\n").contains("CX15-MULTI"),
        "a killed partial source was published"
    );
    eprintln!(
        "CX08_SHORT_STRINGS bytes={} rows={rows} elapsed_ms={} exit={:?}",
        16 * 1024 * 1024,
        elapsed.as_millis(),
        output.status.code()
    );
    db.close().await.unwrap();
}

#[tokio::test]
#[ignore = "operator-local Codex acceptance; never run in CI"]
async fn codex_index_live_terminal_deadline_never_completes() {
    let fixture = LocalCodexSession::new();
    assert!(
        fixture
            .cx01_hook("SessionStart", "session-start")
            .status
            .success()
    );
    let rows = fixture.codex_index_many_short_strings_source();
    let db = fixture.db().await;
    let before = fixture.codex_terminal_state(&db).await;
    assert_eq!(before["state"], "active");
    let count = LocalCodexSession::count(&db, "agent_checkpoint", "1").await;
    let mut child = fixture.codex_terminal_spawn(300);
    fixture.codex_terminal_send(&mut child);
    let (output, elapsed) = codex_terminal_wait(child);
    assert!(elapsed < std::time::Duration::from_secs(3), "{elapsed:?}");
    let after = fixture.codex_terminal_state(&db).await;
    // Exit 0 is the existing settled/advisory hook policy. Completion must
    // be established from durable state and receipts, never the host status.
    assert!(output.stdout.is_empty());
    assert!(!String::from_utf8_lossy(&output.stderr).contains("CX15-MULTI"));
    let receipts = after["metadata"]["capture_catalog_receipts_v1"]["entries"]
        .as_array()
        .unwrap();
    let terminal = receipts
        .iter()
        .filter(|receipt| receipt["intent"]["phase"] == "stopped")
        .collect::<Vec<_>>();
    assert_eq!(terminal.len(), 1);
    assert_eq!(terminal[0]["status"], "pending");
    assert_eq!(terminal[0]["finalizer"]["status"], "pending");
    assert_eq!(after["state"], "active", "{after}");
    assert!(after["stopped_at"].is_null());
    assert_eq!(
        LocalCodexSession::count(&db, "agent_checkpoint", "1").await,
        count
    );
    assert!(!fixture.exports(&db).await.join("\n").contains("CX15-MULTI"));
    eprintln!(
        "CX08_TERMINAL_SHORT_STRINGS bytes={} rows={rows} elapsed_ms={} exit={:?}",
        16 * 1024 * 1024,
        elapsed.as_millis(),
        output.status.code()
    );
    db.close().await.unwrap();
}

impl LocalCodexSession {
    async fn codex_index_stored_snapshot(
        &self,
        db: &sea_orm::DatabaseConnection,
    ) -> serde_json::Value {
        use std::str::FromStr;

        use sea_orm::ConnectionTrait;
        let row = db.query_one_raw(sea_orm::Statement::from_string(db.get_database_backend(), "SELECT metadata_blob_oid FROM agent_checkpoint ORDER BY created_at DESC, checkpoint_id DESC LIMIT 1")).await.unwrap().unwrap();
        let oid: String = row.try_get_by("metadata_blob_oid").unwrap();
        let body = libra::utils::object::read_git_object(
            &self.repo.join(".libra"),
            &git_internal::hash::ObjectHash::from_str(&oid).unwrap(),
        )
        .unwrap();
        serde_json::from_slice::<serde_json::Value>(&body).unwrap()["transcript_snapshot"].clone()
    }
}

#[cfg(unix)]
#[tokio::test]
#[ignore = "operator-local Codex acceptance; never run in CI"]
async fn codex_index_live_unsafe_links_and_oversize_never_export_body() {
    for attack in ["leaf-link", "root-link", "oversize"] {
        let fixture = LocalCodexSession::new();
        fixture.codex_session_summary_source(1, true);
        match attack {
            "leaf-link" => {
                let outside = fixture._temp.path().join("outside-rollout.jsonl");
                std::fs::rename(&fixture.source, &outside).unwrap();
                std::os::unix::fs::symlink(outside, &fixture.source).unwrap();
            }
            "root-link" => {
                let original = fixture.home.join(".codex");
                let outside = fixture._temp.path().join("outside-provider");
                std::fs::rename(&original, &outside).unwrap();
                std::os::unix::fs::symlink(outside, original).unwrap();
            }
            "oversize" => std::fs::OpenOptions::new()
                .write(true)
                .open(&fixture.source)
                .unwrap()
                .set_len(16 * 1024 * 1024 + 1)
                .unwrap(),
            _ => unreachable!(),
        }
        assert!(
            fixture
                .codex_index_hook(Some(2000), None, false)
                .status
                .success()
        );
        let db = fixture.db().await;
        assert!(
            !fixture
                .exports(&db)
                .await
                .join("\n")
                .contains("CX15-MULTI-0"),
            "{attack}"
        );
        let snapshot = fixture.codex_index_stored_snapshot(&db).await;
        assert_eq!(snapshot["completeness"], "partial", "{attack}: {snapshot}");
        if attack == "oversize" {
            assert_eq!(snapshot["partial_reason"], "source_oversize");
        }
        assert!(fixture.codex_session_summary_view()["model"].is_null());
        db.close().await.unwrap();
    }
}

#[tokio::test]
#[ignore = "operator-local Codex acceptance; never run in CI"]
async fn codex_index_live_same_worktree_subdirectory_has_readable_body() {
    let fixture = LocalCodexSession::new();
    fixture.codex_session_summary_source(1, true);
    let sub = fixture.repo.join("subdir");
    std::fs::create_dir(&sub).unwrap();
    let text = std::fs::read_to_string(&fixture.source).unwrap();
    let mut rows: Vec<serde_json::Value> = text
        .lines()
        .map(|line| serde_json::from_str(line).unwrap())
        .collect();
    rows[0]["payload"]["cwd"] = serde_json::json!(sub);
    std::fs::write(&fixture.source, rollout_jsonl(&rows)).unwrap();
    let result = fixture.codex_index_hook_at(Some(2000), None, false, "cx01-turn", &sub);
    assert!(
        result.status.success(),
        "{}",
        String::from_utf8_lossy(&result.stderr)
    );
    let db = fixture.db().await;
    assert!(
        fixture
            .exports(&db)
            .await
            .join("\n")
            .contains("CX15-MULTI-0"),
        "verified subdirectory was incorrectly widened or lost"
    );
    db.close().await.unwrap();
}

#[tokio::test]
#[ignore = "operator-local Codex acceptance; never run in CI"]
async fn codex_index_live_scan_limit_is_unavailable_instead_of_missing() {
    let fixture = LocalCodexSession::new();
    let day = fixture.source.parent().unwrap();
    // The source is absent, but discovery must not call a bounded, unfinished
    // enumeration a successful missing result. No historical body is opened.
    for i in 0..20_001 {
        std::fs::write(
            day.join(format!("rollout-other-{i}.jsonl")),
            b"unread historical body",
        )
        .unwrap();
    }
    assert!(
        fixture
            .codex_index_hook(Some(10000), None, false)
            .status
            .success()
    );
    let db = fixture.db().await;
    let snapshot = fixture.codex_index_stored_snapshot(&db).await;
    assert_eq!(snapshot["completeness"], "partial");
    assert_eq!(
        snapshot["partial_reason"], "source_read_error",
        "discovery incorrectly claimed missing: {snapshot}"
    );
    assert!(
        !fixture
            .exports(&db)
            .await
            .join("\n")
            .contains("unread historical body")
    );
    db.close().await.unwrap();
}

#[cfg(target_os = "macos")]
#[tokio::test]
#[ignore = "operator-local Codex acceptance; never run in CI"]
async fn codex_index_live_system_alias_root_keeps_descriptor_authority() {
    let fixture = LocalCodexSession::with_temp(tempfile::tempdir_in("/var/tmp").unwrap());
    fixture.codex_session_summary_source(1, true);
    let canonical_root = fixture.home.join(".codex");
    let alias = std::path::Path::new("/").join(
        canonical_root
            .strip_prefix("/private")
            .expect("macOS temp home is under /private/var"),
    );
    assert!(alias.starts_with("/var"));
    assert!(
        fixture
            .codex_index_hook(Some(2000), Some(&alias), false)
            .status
            .success()
    );
    let db = fixture.db().await;
    assert!(
        fixture
            .exports(&db)
            .await
            .join("\n")
            .contains("CX15-MULTI-0")
    );
    assert_eq!(fixture.codex_session_summary_view()["model"], "CX07-MODEL");
    db.close().await.unwrap();
}

impl LocalCodexSession {
    async fn codex_reconcile_backup(
        &self,
        entries: &[(&str, std::path::PathBuf)],
    ) -> std::path::PathBuf {
        use sea_orm::sqlx::{Connection, SqliteConnection, sqlite::SqliteConnectOptions};
        let path = self
            ._temp
            .path()
            .join(format!("index-{}.sqlite", uuid::Uuid::new_v4()));
        let mut db = SqliteConnection::connect_with(
            &SqliteConnectOptions::new()
                .filename(&path)
                .create_if_missing(true),
        )
        .await
        .unwrap();
        for sql in [
            "CREATE TABLE _sqlx_migrations(version INTEGER PRIMARY KEY,success INTEGER NOT NULL,checksum BLOB NOT NULL)",
            "CREATE TABLE threads(id TEXT PRIMARY KEY,rollout_path TEXT NOT NULL,cwd TEXT NOT NULL,archived INTEGER NOT NULL,cli_version TEXT NOT NULL,created_at_ms INTEGER,updated_at_ms INTEGER)",
            "CREATE TABLE thread_spawn_edges(parent_thread_id TEXT NOT NULL,child_thread_id TEXT PRIMARY KEY,status TEXT NOT NULL)",
        ] {
            sea_orm::sqlx::query(sql).execute(&mut db).await.unwrap();
        }
        for (version, digest) in [
            (
                1,
                "627ef19164c9bb298a0cd99945981c9b7bda3d9e6cf12eb35145e3b1d3bf7cf8740f0dbaa0b475185fc2993397078049",
            ),
            (
                5,
                "1ac64f85e083e5b49b974bf498fc39c630f0032057fb4bf4cb31708619fac5a80189f261cf3e38dafd92dc406e73c220",
            ),
            (
                21,
                "ea1aebe4f5a1d56c8effcc15f705c272275dd8adfeb01bc97d3f7b283474a3b8c2b0c8c69de55786e8bda17cae6f4de7",
            ),
            (
                25,
                "d6fb51d79941b93c3278fe2a8a0c6fa882407b7b716e32d72329f0b7b60c181a42f49e56cd552315efb0402a111ede12",
            ),
        ] {
            sea_orm::sqlx::query("INSERT INTO _sqlx_migrations VALUES(?,1,?)")
                .bind(version)
                .bind(hex::decode(digest).unwrap())
                .execute(&mut db)
                .await
                .unwrap();
        }
        let now = chrono::Utc::now().timestamp_millis() - 1000;
        for (id, cwd) in entries {
            sea_orm::sqlx::query("INSERT INTO threads VALUES(?,'PRIVATE_PROVIDER_PATH_MUST_NOT_RENDER',?,0,'0.162.1',?,?)")
                .bind(*id).bind(cwd.to_str().unwrap()).bind(now).bind(now).execute(&mut db).await.unwrap();
        }
        db.close().await.unwrap();
        path
    }

    fn codex_reconcile_view(
        &self,
        path: &std::path::Path,
        page: Option<&str>,
    ) -> serde_json::Value {
        let mut args = vec![
            "agent",
            "doctor",
            "--codex-index-backup",
            path.to_str().unwrap(),
            "--json",
        ];
        if let Some(page) = page {
            args.extend(["--codex-index-page", page]);
        }
        let out = self.run(&args);
        assert!(
            out.status.success(),
            "{}",
            String::from_utf8_lossy(&out.stderr)
        );
        let result: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
        assert!(
            !String::from_utf8_lossy(&out.stdout).contains("PRIVATE_PROVIDER_PATH_MUST_NOT_RENDER")
        );
        result["data"]["codex_reconciliation"].clone()
    }
}

#[test]
#[ignore = "operator-local Codex acceptance; never run in CI"]
fn codex_reconcile_flags_are_opt_in_and_conflict_with_repair() {
    let fixture = LocalCodexSession::new();
    let out = fixture.run(&["agent", "doctor", "--json"]);
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let result: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
    assert!(result["data"].get("codex_reconciliation").is_none());
    let human = fixture.run(&["agent", "doctor"]);
    assert!(human.status.success());
    assert!(!String::from_utf8_lossy(&human.stdout).contains("Codex reconciliation"));
    for args in [
        vec!["agent", "doctor", "--codex-index-page", "1"],
        vec![
            "agent",
            "doctor",
            "--codex-index-backup",
            "unused",
            "--codex-index-page",
            "0",
        ],
        vec![
            "agent",
            "doctor",
            "--codex-index-backup",
            "unused",
            "--codex-index-page",
            "11",
        ],
        vec![
            "agent",
            "doctor",
            "--codex-index-backup",
            "unused",
            "--repair",
        ],
    ] {
        let result = fixture.run(&args);
        assert!(!result.status.success(), "{args:?}");
        assert!(result.stdout.is_empty());
    }
}

#[tokio::test]
#[ignore = "operator-local Codex acceptance; never run in CI"]
async fn codex_reconcile_real_stored_source_is_readonly_without_provider_fallback() {
    let fixture = LocalCodexSession::new();
    fixture.codex_session_summary_source(2, true);
    assert!(fixture.cx01_hook("Stop", "stop").status.success());
    let db = fixture.db().await;
    let exported = fixture.exports(&db).await.join("\n");
    assert!(exported.contains("CX15-MULTI-0"));
    let backup = fixture
        .codex_reconcile_backup(&[("123e4567-e89b-12d3-a456-426614174031", fixture.repo.clone())])
        .await;
    {
        use sea_orm::sqlx::{Connection, SqliteConnection, sqlite::SqliteConnectOptions};
        let mut index =
            SqliteConnection::connect_with(&SqliteConnectOptions::new().filename(&backup))
                .await
                .unwrap();
        sea_orm::sqlx::query("UPDATE threads SET archived=1")
            .execute(&mut index)
            .await
            .unwrap();
        sea_orm::sqlx::query("INSERT INTO thread_spawn_edges VALUES('unproved-parent','123e4567-e89b-12d3-a456-426614174031','completed')").execute(&mut index).await.unwrap();
        index.close().await.unwrap();
    }
    let before = codex_finalize_catalog_snapshot(&db).await;
    let bytes = std::fs::read(&backup).unwrap();
    std::fs::remove_file(&fixture.source).unwrap();
    let view = fixture.codex_reconcile_view(&backup, None);
    assert_eq!(view["status"], "available", "{view}");
    assert_eq!(view["rows"][0]["status"], "stored_source", "{view}");
    assert_eq!(
        view["rows"][0]["archived"], true,
        "archived child retains its independent stored source: {view}"
    );
    assert_eq!(view["page_totals"]["stored_source"], 1);
    assert_eq!(view["scope_candidate_count"], 1);
    assert_eq!(view["totals_scope"], "page");
    assert!(view["rows"][0].get("import_hint").is_none());
    assert!(!view.to_string().contains("CX15-MULTI"));
    assert!(!view.to_string().contains(fixture.repo.to_str().unwrap()));
    assert_eq!(codex_finalize_catalog_snapshot(&db).await, before);
    assert_eq!(std::fs::read(&backup).unwrap(), bytes);
    assert_eq!(fixture.exports(&db).await.join("\n"), exported);
    db.close().await.unwrap();
}

#[tokio::test]
#[ignore = "operator-local Codex acceptance; never run in CI"]
async fn codex_reconcile_pending_and_erased_without_session_never_become_missing() {
    use sea_orm::{ConnectionTrait, Statement};
    let fixture = LocalCodexSession::new();
    fixture.source(1, true);
    assert!(
        fixture
            .cx01_hook("SessionStart", "session-start")
            .status
            .success()
    );
    let db = fixture.db().await;
    let scope = db
        .query_one_raw(Statement::from_string(
            db.get_database_backend(),
            "SELECT repo_id,worktree_id FROM agent_session LIMIT 1",
        ))
        .await
        .unwrap()
        .unwrap();
    let repo_id: String = scope.try_get_by("repo_id").unwrap();
    let worktree_id: String = scope.try_get_by("worktree_id").unwrap();
    for (id, owner) in [
        ("pending", &repo_id),
        ("erased", &repo_id),
        ("committed", &repo_id),
        ("foreign", &"other-repo".to_string()),
    ] {
        db.execute_raw(Statement::from_sql_and_values(db.get_database_backend(),
            "INSERT INTO agent_import_identity(identity_id,agent_kind,provider_session_id,source_kind,source_id,schema_version,state,created_at,updated_at,scope_state,repo_id,worktree_id) VALUES(?,'codex',?,'provider_file',?,1,'leased',1,1,'scoped',?,?)",
            [id.into(),id.into(),format!("local-{id}").into(),owner.clone().into(),worktree_id.clone().into()])).await.unwrap();
    }
    db.execute_unprepared("INSERT INTO agent_import_tombstone(tombstone_id,agent_kind,provider_session_id,erased_session_id,erased_at) VALUES('erased','codex','erased','codex__erased',1)").await.unwrap();
    db.execute_unprepared(
        "UPDATE agent_import_identity SET state='committed' WHERE provider_session_id='committed'",
    )
    .await
    .unwrap();
    let ids = [
        "pending",
        "erased",
        "foreign",
        "missing",
        "child",
        "committed",
    ];
    let entries = ids
        .iter()
        .map(|id| (*id, fixture.repo.clone()))
        .collect::<Vec<_>>();
    let backup = fixture.codex_reconcile_backup(&entries).await;
    {
        use sea_orm::sqlx::{Connection, SqliteConnection, sqlite::SqliteConnectOptions};
        let mut index =
            SqliteConnection::connect_with(&SqliteConnectOptions::new().filename(&backup))
                .await
                .unwrap();
        sea_orm::sqlx::query(
            "INSERT INTO thread_spawn_edges VALUES('old-or-foreign-parent','child','completed')",
        )
        .execute(&mut index)
        .await
        .unwrap();
        sea_orm::sqlx::query("UPDATE threads SET cwd=? WHERE id='pending'")
            .bind(fixture.repo.with_file_name("REPO").to_str().unwrap())
            .execute(&mut index)
            .await
            .unwrap();
        index.close().await.unwrap();
    }
    db.execute_unprepared(
        "UPDATE agent_import_identity SET lease_expires_at=1 WHERE provider_session_id='pending'",
    )
    .await
    .unwrap();
    let before = codex_finalize_catalog_snapshot(&db).await;
    let view = fixture.codex_reconcile_view(&backup, None);
    assert_eq!(view["status"], "available", "{view}");
    let rows = view["rows"].as_array().unwrap();
    for (id, expected) in [
        ("pending", "pending"),
        ("erased", "erased"),
        ("foreign", "unknown"),
        ("committed", "unknown"),
        ("missing", "missing"),
        ("child", "unknown"),
    ] {
        let row = rows
            .iter()
            .find(|row| row["provider_session_id"] == id)
            .unwrap();
        assert_eq!(row["status"], expected, "{view}");
        if expected == "missing" {
            let hint = row["import_hint"].as_str().unwrap();
            assert_eq!(hint, "libra agent import --agent codex --session=missing");
            assert!(!hint.contains("--yes"));
            use clap::Parser;
            #[derive(Parser)]
            struct AgentHint {
                #[command(flatten)]
                args: libra::command::agent::AgentArgs,
            }
            let parsed = AgentHint::try_parse_from(hint.split_whitespace().skip(1)).unwrap();
            let libra::command::agent::AgentSubcommand::Import(import) = parsed.args.command else {
                panic!("hint did not select import")
            };
            assert_eq!(import.session.as_deref(), Some("missing"));
            assert!(!import.yes);
        } else {
            assert!(row.get("import_hint").is_none(), "{row}");
        }
    }
    assert_eq!(
        rows.iter()
            .find(|row| row["provider_session_id"] == "pending")
            .unwrap()["detail"],
        "import_lease_expired"
    );
    assert_eq!(codex_finalize_catalog_snapshot(&db).await, before);
    assert_eq!(
        LocalCodexSession::count(
            &db,
            "agent_session",
            "provider_session_id IN ('pending','erased','foreign','missing','child')"
        )
        .await,
        0
    );
    db.execute_unprepared("UPDATE agent_import_identity SET lease_expires_at=(unixepoch('now')*1000)+60000 WHERE provider_session_id='pending'").await.unwrap();
    let live_lease = fixture.codex_reconcile_view(&backup, None);
    let row = live_lease["rows"]
        .as_array()
        .unwrap()
        .iter()
        .find(|row| row["provider_session_id"] == "pending")
        .unwrap();
    assert_eq!(row["status"], "pending", "{row}");
    assert!(
        row["detail"].is_null(),
        "a live millisecond lease is not expired: {row}"
    );
    db.close().await.unwrap();
}

#[tokio::test]
#[ignore = "operator-local Codex acceptance; never run in CI"]
async fn codex_reconcile_retryable_session_receipt_stays_pending_without_import_hint() {
    let fixture = LocalCodexSession::new();
    fixture.codex_session_summary_source(1, true);
    assert!(
        fixture
            .cx01_hook("SessionStart", "session-start")
            .status
            .success()
    );
    let objects = fixture.repo.join(".libra/objects");
    let retained = fixture.repo.join(".libra/objects-retained");
    std::fs::rename(&objects, &retained).unwrap();
    std::fs::write(&objects, b"fixture rejects object writes").unwrap();
    let failed = fixture.cx01_hook("Stop", "stop");
    std::fs::remove_file(&objects).unwrap();
    std::fs::rename(&retained, &objects).unwrap();
    assert!(failed.status.success(), "Codex Stop remains advisory");
    let db = fixture.db().await;
    assert_eq!(
        LocalCodexSession::count(&db, "agent_checkpoint", "1").await,
        0
    );
    let state = fixture.codex_terminal_state(&db).await;
    assert_eq!(state["metadata"]["capture_status"], "retryable", "{state}");
    let backup = fixture
        .codex_reconcile_backup(&[("123e4567-e89b-12d3-a456-426614174031", fixture.repo.clone())])
        .await;
    let before = codex_finalize_catalog_snapshot(&db).await;
    let view = fixture.codex_reconcile_view(&backup, None);
    assert_eq!(view["rows"][0]["status"], "pending", "{view}");
    assert!(view["rows"][0].get("import_hint").is_none());
    assert!(
        before == codex_finalize_catalog_snapshot(&db).await,
        "doctor changed a pending receipt"
    );
    db.close().await.unwrap();
}

#[tokio::test]
#[ignore = "operator-local Codex acceptance; never run in CI"]
async fn codex_reconcile_missing_local_metadata_is_unknown_and_never_repaired() {
    use sea_orm::{ConnectionTrait, Statement};
    let fixture = LocalCodexSession::new();
    fixture.codex_session_summary_source(1, true);
    assert!(fixture.cx01_hook("Stop", "stop").status.success());
    let db = fixture.db().await;
    let oid: String = db
        .query_one_raw(Statement::from_string(
            db.get_database_backend(),
            "SELECT metadata_blob_oid FROM agent_checkpoint LIMIT 1",
        ))
        .await
        .unwrap()
        .unwrap()
        .try_get_by("metadata_blob_oid")
        .unwrap();
    let object = fixture
        .repo
        .join(".libra/objects")
        .join(&oid[..2])
        .join(&oid[2..]);
    std::fs::remove_file(&object).unwrap();
    let backup = fixture
        .codex_reconcile_backup(&[("123e4567-e89b-12d3-a456-426614174031", fixture.repo.clone())])
        .await;
    let before = codex_finalize_catalog_snapshot(&db).await;
    let view = fixture.codex_reconcile_view(&backup, None);
    assert_eq!(view["status"], "available", "{view}");
    assert_eq!(view["rows"][0]["status"], "unknown", "{view}");
    assert!(view["rows"][0].get("import_hint").is_none());
    assert!(!object.exists());
    assert_eq!(codex_finalize_catalog_snapshot(&db).await, before);
    db.close().await.unwrap();
}

#[tokio::test]
#[ignore = "operator-local Codex acceptance; never run in CI"]
async fn codex_reconcile_empty_fallback_and_uncheckpointed_session_are_not_source_data() {
    for stop_without_source in [false, true] {
        let fixture = LocalCodexSession::new();
        fixture.source(1, true);
        assert!(
            fixture
                .cx01_hook("SessionStart", "session-start")
                .status
                .success()
        );
        let db = fixture.db().await;
        std::fs::remove_file(&fixture.source).unwrap();
        if stop_without_source {
            assert!(fixture.cx01_hook("Stop", "stop").status.success());
        }
        let backup = fixture
            .codex_reconcile_backup(&[(
                "123e4567-e89b-12d3-a456-426614174031",
                fixture.repo.clone(),
            )])
            .await;
        let before = codex_finalize_catalog_snapshot(&db).await;
        let view = fixture.codex_reconcile_view(&backup, None);
        assert_eq!(view["status"], "available", "{view}");
        assert_eq!(
            view["rows"][0]["status"],
            if stop_without_source {
                "partial"
            } else {
                "missing_data"
            },
            "{view}"
        );
        assert_eq!(view["page_totals"]["stored_source"], 0);
        assert_eq!(codex_finalize_catalog_snapshot(&db).await, before);
        db.close().await.unwrap();
    }
    let (fixture, db, _, _) =
        codex_legacy_reader_fixture(b"legacy ordinary", br#"{"schema_version":1}"#).await;
    let backup = fixture
        .codex_reconcile_backup(&[("123e4567-e89b-12d3-a456-426614174031", fixture.repo.clone())])
        .await;
    let view = fixture.codex_reconcile_view(&backup, None);
    assert_eq!(view["status"], "available", "{view}");
    assert_eq!(view["rows"][0]["status"], "unknown", "{view}");
    assert_eq!(view["page_totals"]["stored_source"], 0);
    db.close().await.unwrap();
}

#[tokio::test]
#[ignore = "operator-local Codex acceptance; never run in CI"]
async fn codex_reconcile_paging_filters_scope_before_selecting_rows() {
    let fixture = LocalCodexSession::new();
    fixture.source(1, true);
    assert!(
        fixture
            .cx01_hook("SessionStart", "session-start")
            .status
            .success()
    );
    let db = fixture.db().await;
    let names = (0..1001)
        .map(|index| format!("row-{index:04}"))
        .collect::<Vec<_>>();
    let mut entries = names
        .iter()
        .map(|id| (id.as_str(), fixture.repo.clone()))
        .collect::<Vec<_>>();
    entries.push((
        "000-foreign",
        fixture._temp.path().join("foreign-never-open"),
    ));
    let backup = fixture.codex_reconcile_backup(&entries).await;
    let before = codex_finalize_catalog_snapshot(&db).await;
    let first = fixture.codex_reconcile_view(&backup, Some("1"));
    let second = fixture.codex_reconcile_view(&backup, Some("2"));
    for page in [&first, &second] {
        assert_eq!(page["status"], "available", "{page}");
        assert_eq!(page["scope_candidate_count"], 1001);
        assert_eq!(page["totals_scope"], "page");
    }
    assert_eq!(first["rows"].as_array().unwrap().len(), 1000);
    assert_eq!(first["page_totals"]["missing"], 1000);
    assert_eq!(first["rows"][0]["provider_session_id"], "row-0000");
    assert_eq!(second["rows"].as_array().unwrap().len(), 1);
    assert_eq!(second["page_totals"]["missing"], 1);
    assert_eq!(second["rows"][0]["provider_session_id"], "row-1000");
    assert!(!first.to_string().contains("000-foreign"));
    let invalid = fixture.codex_reconcile_view(&backup, Some("3"));
    assert!(invalid["page_totals"].is_null(), "{invalid}");
    assert_eq!(invalid["reason"], "page_out_of_range");
    assert_eq!(codex_finalize_catalog_snapshot(&db).await, before);
    db.close().await.unwrap();
}

#[tokio::test]
#[ignore = "operator-local Codex acceptance; never run in CI"]
async fn codex_reconcile_stale_empty_and_invalid_index_never_share_zero_missing() {
    use sea_orm::sqlx::{Connection, SqliteConnection, sqlite::SqliteConnectOptions};
    let fixture = LocalCodexSession::new();
    fixture.source(1, true);
    assert!(
        fixture
            .cx01_hook("SessionStart", "session-start")
            .status
            .success()
    );
    let backup = fixture
        .codex_reconcile_backup(&[("missing", fixture.repo.clone())])
        .await;
    let mut index = SqliteConnection::connect_with(&SqliteConnectOptions::new().filename(&backup))
        .await
        .unwrap();
    sea_orm::sqlx::query("UPDATE threads SET created_at_ms=1,updated_at_ms=1")
        .execute(&mut index)
        .await
        .unwrap();
    index.close().await.unwrap();
    let stale = fixture.codex_reconcile_view(&backup, None);
    assert_eq!(stale["status"], "outside_window", "{stale}");
    assert!(stale["page_totals"].is_null());
    let mut index = SqliteConnection::connect_with(&SqliteConnectOptions::new().filename(&backup))
        .await
        .unwrap();
    sea_orm::sqlx::query("UPDATE threads SET updated_at_ms=NULL")
        .execute(&mut index)
        .await
        .unwrap();
    index.close().await.unwrap();
    let invalid = fixture.codex_reconcile_view(&backup, None);
    assert_eq!(invalid["status"], "unavailable", "{invalid}");
    assert!(invalid["page_totals"].is_null());
    assert!(invalid["scope_candidate_count"].is_null());
    let mut index = SqliteConnection::connect_with(&SqliteConnectOptions::new().filename(&backup))
        .await
        .unwrap();
    sea_orm::sqlx::query("DELETE FROM threads")
        .execute(&mut index)
        .await
        .unwrap();
    index.close().await.unwrap();
    let empty = fixture.codex_reconcile_view(&backup, None);
    assert_eq!(empty["status"], "empty_snapshot", "{empty}");
    assert_eq!(empty["page_totals"]["missing"], 0);
    assert!(empty["coverage_until_ms"].is_null());
    let page = fixture.codex_reconcile_view(&backup, Some("2"));
    assert_eq!(page["reason"], "page_out_of_range", "{page}");
    assert!(page["page_totals"].is_null());
}

#[tokio::test]
#[ignore = "operator-local Codex acceptance; never run in CI"]
async fn codex_reconcile_later_nested_and_gone_directories_remain_unknown() {
    let fixture = LocalCodexSession::new();
    fixture.source(1, true);
    assert!(
        fixture
            .cx01_hook("SessionStart", "session-start")
            .status
            .success()
    );
    let nested = fixture.repo.join("nested");
    std::fs::create_dir(&nested).unwrap();
    std::fs::write(nested.join(".git"), b"new repository").unwrap();
    let backup = fixture
        .codex_reconcile_backup(&[("nested", nested), ("gone", fixture.repo.join("gone"))])
        .await;
    let view = fixture.codex_reconcile_view(&backup, None);
    assert_eq!(view["status"], "available", "{view}");
    assert_eq!(view["scope_unknown_count"], 2);
    assert_eq!(view["scope_ambiguous_count"], 0);
    assert_eq!(view["page_totals"]["unknown"], 2);
    assert!(
        view["rows"]
            .as_array()
            .unwrap()
            .iter()
            .all(|row| row.get("import_hint").is_none())
    );
}

#[tokio::test]
#[ignore = "operator-local Codex acceptance; never run in CI"]
async fn codex_reconcile_relative_backup_uses_invocation_subdirectory_and_human_fields() {
    let mut fixture = LocalCodexSession::new();
    fixture.source(1, true);
    assert!(
        fixture
            .cx01_hook("SessionStart", "session-start")
            .status
            .success()
    );
    let backup = fixture
        .codex_reconcile_backup(&[("missing", fixture.repo.clone())])
        .await;
    let name = backup.file_name().unwrap().to_str().unwrap();
    let view = fixture.codex_reconcile_view(std::path::Path::new(&format!("../{name}")), None);
    assert_eq!(view["rows"][0]["status"], "missing", "{view}");
    let subdir = fixture.repo.join("sub");
    std::fs::create_dir(&subdir).unwrap();
    fixture.repo = subdir.canonicalize().unwrap();
    let relative = format!("../../{name}");
    let output = fixture.run(&["agent", "doctor", "--codex-index-backup", &relative]);
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let text = String::from_utf8(output.stdout).unwrap();
    assert!(text.contains("freshness: within_window"), "{text}");
    assert!(text.contains("scope candidates: 1; alias ambiguous: 0; layout unknown: 0"));
    let section = text
        .split("Codex reconciliation")
        .nth(1)
        .unwrap()
        .split("Frozen Code residue")
        .next()
        .unwrap();
    assert!(!section.contains("Some(") && !section.contains("None"));
}

#[tokio::test]
#[ignore = "operator-local Codex acceptance; never run in CI"]
async fn codex_reconcile_import_sizes_follow_stored_manifest_not_full_source_snapshot() {
    let fixture = LocalCodexSession::new();
    fixture.codex_session_summary_source(2, true);
    let imported = fixture.import();
    assert!(
        imported.status.success(),
        "{}",
        String::from_utf8_lossy(&imported.stderr)
    );
    let db = fixture.db().await;
    let exports = fixture.exports(&db).await;
    let snapshot = fixture.codex_index_stored_snapshot(&db).await;
    let source_len = snapshot["transcript_redacted_bytes"].as_u64().unwrap();
    assert!(
        exports.iter().any(|body| body.len() as u64 != source_len),
        "per-turn import and full-source snapshot have different byte semantics"
    );
    let backup = fixture
        .codex_reconcile_backup(&[("123e4567-e89b-12d3-a456-426614174031", fixture.repo.clone())])
        .await;
    let view = fixture.codex_reconcile_view(&backup, None);
    assert_eq!(view["rows"][0]["status"], "stored_source", "{view}");
    db.close().await.unwrap();
}

#[tokio::test]
#[ignore = "operator-local Codex acceptance; never run in CI"]
async fn codex_reconcile_non_sha1_repositories_have_verified_stored_source() {
    for format in ["sha256", "blake3"] {
        for imported in [false, true] {
            let fixture = LocalCodexSession::new();
            // Only this isolated fixture is recreated, before it owns any capture.
            std::fs::remove_dir_all(fixture.repo.join(".libra")).unwrap();
            let init = fixture.run(&["init", "--object-format", format]);
            assert!(
                init.status.success(),
                "{format}: {}",
                String::from_utf8_lossy(&init.stderr)
            );
            fixture.codex_session_summary_source(1, true);
            let capture = if imported {
                fixture.import()
            } else {
                fixture.cx01_hook("Stop", "stop")
            };
            assert!(
                capture.status.success(),
                "{format}/{imported}: stdout {}; stderr {}",
                String::from_utf8_lossy(&capture.stdout),
                String::from_utf8_lossy(&capture.stderr)
            );
            let db = fixture.db().await;
            assert_eq!(
                LocalCodexSession::count(&db, "agent_checkpoint", "1").await,
                1,
                "{format}/{imported}: {}",
                fixture.codex_terminal_state(&db).await
            );
            let exports = fixture.exports(&db).await;
            assert_eq!(exports.len(), 1);
            assert!(exports[0].contains("CX15-MULTI-0"));
            let durable_before = codex_legacy_reader_state(&db).await;
            let replay = if imported {
                fixture.import()
            } else {
                fixture.cx01_hook("Stop", "stop")
            };
            assert!(
                replay.status.success(),
                "{format}/{imported}: {}",
                String::from_utf8_lossy(&replay.stderr)
            );
            assert_eq!(
                LocalCodexSession::count(&db, "agent_checkpoint", "1").await,
                1
            );
            assert_eq!(durable_before, codex_legacy_reader_state(&db).await);
            // Import replay can advance its session receipt/revision legitimately.
            // Only the following doctor invocation promises complete catalog immutability.
            let before = codex_finalize_catalog_snapshot(&db).await;
            let backup = fixture
                .codex_reconcile_backup(&[(
                    "123e4567-e89b-12d3-a456-426614174031",
                    fixture.repo.clone(),
                )])
                .await;
            let view = fixture.codex_reconcile_view(&backup, None);
            assert_eq!(
                view["rows"][0]["status"], "stored_source",
                "{format}/{imported}: {view}"
            );
            assert!(
                before == codex_finalize_catalog_snapshot(&db).await,
                "{format}/{imported}: doctor changed the catalog"
            );
            db.close().await.unwrap();
        }
    }
}

#[test]
#[ignore = "operator-local Codex acceptance; never run in CI"]
fn codex_reconcile_helper_object_format_is_explicit_and_legacy_wire_is_unchanged() {
    use base64::{Engine as _, engine::general_purpose::STANDARD};
    use git_internal::{
        hash::{HashKind, ObjectHash},
        internal::object::types::ObjectType,
    };
    use libra::internal::ai::history::run_checkpoint_object_io_helper;
    let temp = tempfile::tempdir().unwrap();
    // Each test owns its thread's TLS; no global/environment mutation or serial lane.
    let _ambient = git_internal::hash::set_hash_kind_for_test(HashKind::Sha1);
    let request = serde_json::json!({
        "repo_path_base64": STANDARD.encode(temp.path().to_str().unwrap().as_bytes()),
        "operation": {"kind":"write", "object_type":"tree", "data_base64":""}
    });
    let call = |request: &serde_json::Value| -> serde_json::Value {
        serde_json::from_slice(
            &run_checkpoint_object_io_helper(&serde_json::to_vec(request).unwrap()).unwrap(),
        )
        .unwrap()
    };
    let legacy = call(&request);
    assert_eq!(legacy["status"], "written");
    assert_eq!(legacy["oid"].as_str().unwrap().len(), 40);
    for kind in [HashKind::Sha256, HashKind::Blake3] {
        let mut explicit = request.clone();
        explicit["object_format"] = serde_json::json!(kind.as_str());
        let expected = ObjectHash::from_type_and_data_for_kind(kind, ObjectType::Tree, b"")
            .unwrap()
            .to_string();
        let written = call(&explicit);
        assert_eq!(written["oid"], expected);
        assert_eq!(git_internal::hash::get_hash_kind(), HashKind::Sha1);
        explicit["operation"] =
            serde_json::json!({"kind":"read", "oid":expected, "expected_type":"tree"});
        assert_eq!(call(&explicit)["status"], "read");
        assert_eq!(git_internal::hash::get_hash_kind(), HashKind::Sha1);
        explicit["operation"]["expected_type"] = serde_json::json!("unsupported");
        assert_eq!(call(&explicit)["code"], "unsupported_read_type");
        assert_eq!(git_internal::hash::get_hash_kind(), HashKind::Sha1);
        for operation in [
            serde_json::json!({"kind":"write_opaque_blob", "data_base64":"", "object_format":"sha1"}),
            serde_json::json!({"kind":"verify_reasoning_artifacts", "checkpoints":[], "object_format":"sha1"}),
        ] {
            explicit["operation"] = operation;
            assert_eq!(call(&explicit)["code"], "invalid_request");
            assert_eq!(git_internal::hash::get_hash_kind(), HashKind::Sha1);
        }
        let mut legacy_artifact = request.clone();
        legacy_artifact["operation"] = serde_json::json!({"kind":"verify_reasoning_artifacts", "checkpoints":[], "object_format":kind.as_str()});
        assert_eq!(call(&legacy_artifact)["status"], "reasoning_artifacts");
        assert_eq!(git_internal::hash::get_hash_kind(), HashKind::Sha1);
    }
    let mut unknown = request;
    unknown["object_format"] = serde_json::json!("unknown");
    assert_eq!(call(&unknown)["code"], "invalid_request");
    assert_eq!(git_internal::hash::get_hash_kind(), HashKind::Sha1);
}

#[tokio::test]
#[ignore = "operator-local Codex acceptance; never run in CI"]
async fn codex_reconcile_legacy_short_oid_residue_does_not_damage_current_checkpoints() {
    use base64::{Engine as _, engine::general_purpose::STANDARD};
    for format in ["sha256", "blake3"] {
        let fixture = LocalCodexSession::new();
        std::fs::remove_dir_all(fixture.repo.join(".libra")).unwrap();
        assert!(
            fixture
                .run(&["init", "--object-format", format])
                .status
                .success()
        );
        fixture.codex_session_summary_source(1, true);
        assert!(fixture.cx01_hook("Stop", "stop").status.success());
        let db = fixture.db().await;
        let before = fixture.exports(&db).await;
        assert_eq!(before.len(), 1);
        // Reproduce the old child's 40-hex loose residue in this isolated store.
        let storage = fixture.repo.join(".libra");
        let request = serde_json::json!({"repo_path_base64":STANDARD.encode(storage.to_str().unwrap().as_bytes()),
            "operation":{"kind":"write","object_type":"blob","data_base64":STANDARD.encode(b"legacy ordinary residue")}});
        let residue: serde_json::Value = {
            let _ambient =
                git_internal::hash::set_hash_kind_for_test(git_internal::hash::HashKind::Sha1);
            serde_json::from_slice(
                &libra::internal::ai::history::run_checkpoint_object_io_helper(
                    &serde_json::to_vec(&request).unwrap(),
                )
                .unwrap(),
            )
            .unwrap()
        };
        assert_eq!(residue["status"], "written");
        assert_eq!(residue["oid"].as_str().unwrap().len(), 40);
        for args in [
            &["status", "--porcelain"][..],
            &["fsck", "--full"][..],
            &["maintenance", "run", "--task", "gc"][..],
        ] {
            let output = fixture.run(args);
            assert!(
                output.status.success(),
                "{format} {args:?}: stdout {}; stderr {}",
                String::from_utf8_lossy(&output.stdout),
                String::from_utf8_lossy(&output.stderr)
            );
        }
        assert_eq!(before, fixture.exports(&db).await);
        db.close().await.unwrap();
    }
}

#[tokio::test]
#[ignore = "operator-local Codex acceptance; never run in CI"]
async fn codex_reconcile_manifest_mismatch_is_unknown_and_verified_empty_is_missing_data() {
    use std::str::FromStr;

    use git_internal::{
        hash::{HashKind, ObjectHash},
        internal::object::{ObjectTrait, tree::Tree},
    };
    use sea_orm::{ConnectionTrait, Statement, TransactionTrait};
    fn read_tree(storage: &std::path::Path, oid: &str) -> Tree {
        let oid = ObjectHash::from_str(oid).unwrap();
        Tree::from_bytes(
            &libra::utils::object::read_git_object(storage, &oid).unwrap(),
            oid,
        )
        .unwrap()
    }
    fn entry(storage: &std::path::Path, root: &str, path: &[&str]) -> String {
        if path.is_empty() {
            return root.to_string();
        }
        let tree = read_tree(storage, root);
        entry(
            storage,
            &tree
                .tree_items
                .iter()
                .find(|item| item.name == path[0])
                .unwrap()
                .id
                .to_string(),
            &path[1..],
        )
    }
    fn replace(storage: &std::path::Path, root: &str, path: &[&str], replacement: &str) -> String {
        if path.is_empty() {
            return replacement.to_string();
        }
        let mut tree = read_tree(storage, root);
        let item = tree
            .tree_items
            .iter_mut()
            .find(|item| item.name == path[0])
            .unwrap();
        item.id = ObjectHash::from_str(&replace(
            storage,
            &item.id.to_string(),
            &path[1..],
            replacement,
        ))
        .unwrap();
        libra::utils::object::write_git_object(storage, "tree", &tree.to_data().unwrap())
            .unwrap()
            .to_string()
    }
    for mode in ["mismatch", "empty"] {
        let fixture = LocalCodexSession::new();
        fixture.codex_session_summary_source(1, true);
        assert!(fixture.cx01_hook("Stop", "stop").status.success());
        let db = fixture.db().await;
        let row = db
            .query_one_raw(Statement::from_string(
                db.get_database_backend(),
                "SELECT checkpoint_id,tree_oid,traces_commit FROM agent_checkpoint",
            ))
            .await
            .unwrap()
            .unwrap();
        let id: String = row.try_get_by("checkpoint_id").unwrap();
        let root: String = row.try_get_by("tree_oid").unwrap();
        let old_commit: String = row.try_get_by("traces_commit").unwrap();
        let storage = fixture.repo.join(".libra");
        let (new_root, new_commit) = {
            let _hash = git_internal::hash::set_hash_kind_for_test(HashKind::Sha1);
            let manifest_path = ["checkpoint", &id[..2], &id[2..], "manifest.json"];
            let manifest_oid =
                ObjectHash::from_str(&entry(&storage, &root, &manifest_path)).unwrap();
            let mut manifest: serde_json::Value = serde_json::from_slice(
                &libra::utils::object::read_git_object(&storage, &manifest_oid).unwrap(),
            )
            .unwrap();
            let mut new_root = root;
            if mode == "empty" {
                let empty = libra::utils::object::write_git_object(&storage, "blob", b"")
                    .unwrap()
                    .to_string();
                let path = manifest["entries"]["transcript"]["path"].as_str().unwrap();
                let mut selected = vec!["checkpoint", &id[..2], &id[2..]];
                selected.extend(path.split('/'));
                new_root = replace(&storage, &new_root, &selected, &empty);
                manifest["entries"]["transcript"]["oid"] = serde_json::json!(empty);
                manifest["entries"]["transcript"]["byte_len"] = serde_json::json!(0);
            } else {
                let actual = manifest["entries"]["transcript"]["byte_len"]
                    .as_u64()
                    .unwrap();
                manifest["entries"]["transcript"]["byte_len"] = serde_json::json!(actual + 1);
            }
            let manifest = libra::utils::object::write_git_object(
                &storage,
                "blob",
                &serde_json::to_vec(&manifest).unwrap(),
            )
            .unwrap()
            .to_string();
            new_root = replace(&storage, &new_root, &manifest_path, &manifest);
            let body = format!(
                "tree {new_root}\nparent {old_commit}\nauthor Fixture <fixture@local> 1 +0000\ncommitter Fixture <fixture@local> 1 +0000\n\nManifest fault fixture\n"
            );
            let commit =
                libra::utils::object::write_git_object(&storage, "commit", body.as_bytes())
                    .unwrap()
                    .to_string();
            (new_root, commit)
        };
        let tx = db.begin().await.unwrap();
        tx.execute_raw(Statement::from_sql_and_values(
            db.get_database_backend(),
            "UPDATE agent_checkpoint SET tree_oid=?,traces_commit=? WHERE checkpoint_id=?",
            [new_root.into(), new_commit.clone().into(), id.into()],
        ))
        .await
        .unwrap();
        tx.execute_raw(Statement::from_sql_and_values(db.get_database_backend(), "UPDATE reference SET \"commit\"=? WHERE name='traces' AND remote IS NULL AND kind='Branch'", [new_commit.into()])).await.unwrap();
        tx.commit().await.unwrap();
        let backup = fixture
            .codex_reconcile_backup(&[(
                "123e4567-e89b-12d3-a456-426614174031",
                fixture.repo.clone(),
            )])
            .await;
        let view = fixture.codex_reconcile_view(&backup, None);
        let row = &view["rows"][0];
        if mode == "empty" {
            assert_eq!(row["status"], "missing_data", "{view}");
            assert_eq!(row["detail"], "stored_transcript_empty", "{view}");
            assert!(row.get("import_hint").is_some());
        } else {
            assert_eq!(row["status"], "unknown", "{view}");
            assert_eq!(row["detail"], "stored_size_mismatch", "{view}");
            assert!(row.get("import_hint").is_none());
        }
        db.close().await.unwrap();
    }
}

#[tokio::test]
#[ignore = "operator-local Codex acceptance; never run in CI"]
async fn codex_reconcile_bad_rows_and_budget_caps_do_not_hide_healthy_rows() {
    use sea_orm::{ConnectionTrait, Statement};
    let fixture = LocalCodexSession::new();
    fixture.codex_session_summary_source(1, true);
    assert!(fixture.cx01_hook("Stop", "stop").status.success());
    let db = fixture.db().await;
    for (id, metadata) in [
        ("bad", vec![0xff]),
        ("budget", vec![b'x'; 1024 * 1024 + 1]),
        ("healthy", b"{}".to_vec()),
        ("retryable", br#"{"capture_status":"retryable"}"#.to_vec()),
        ("pending-phase", b"{}".to_vec()),
    ] {
        db.execute_raw(Statement::from_sql_and_values(db.get_database_backend(),
            "INSERT INTO agent_session(session_id,agent_kind,provider_session_id,working_dir,state,started_at,last_event_at,repo_id,worktree_id,workspace_id,workspace_fence,scope_state,metadata_json)
             SELECT ?,'codex',?,working_dir,'active',1,1,repo_id,worktree_id,workspace_id,workspace_fence,scope_state,CAST(? AS TEXT)
             FROM agent_session WHERE provider_session_id='123e4567-e89b-12d3-a456-426614174031'",
            [format!("codex__{id}").into(),id.into(),metadata.into()])).await.unwrap();
    }
    db.execute_unprepared(
        "UPDATE agent_session SET state='pending' WHERE provider_session_id='pending-phase'",
    )
    .await
    .unwrap();
    let entries = [
        "bad",
        "budget",
        "healthy",
        "retryable",
        "pending-phase",
        "123e4567-e89b-12d3-a456-426614174031",
    ]
    .map(|id| (id, fixture.repo.clone()));
    let backup = fixture.codex_reconcile_backup(&entries).await;
    let view = fixture.codex_reconcile_view(&backup, None);
    let row = |name: &str| {
        view["rows"]
            .as_array()
            .unwrap()
            .iter()
            .find(|row| row["provider_session_id"] == name)
            .unwrap()
    };
    assert_eq!(view["status"], "available", "{view}");
    assert_eq!(row("retryable")["status"], "pending");
    assert!(row("retryable").get("import_hint").is_none());
    assert_eq!(row("pending-phase")["status"], "pending");

    assert_eq!(row("bad")["status"], "unknown");
    assert_eq!(row("budget")["detail"], "budget_exhausted", "{view}");
    assert_eq!(row("healthy")["status"], "missing_data", "{view}");
    assert_eq!(
        row("123e4567-e89b-12d3-a456-426614174031")["status"],
        "stored_source"
    );
    db.execute_unprepared("UPDATE agent_session SET state='quarantined' WHERE provider_session_id='123e4567-e89b-12d3-a456-426614174031'").await.unwrap();
    let quarantined = fixture.codex_reconcile_view(&backup, None);
    let row = quarantined["rows"]
        .as_array()
        .unwrap()
        .iter()
        .find(|row| row["provider_session_id"] == "123e4567-e89b-12d3-a456-426614174031")
        .unwrap();
    assert_eq!(row["status"], "unknown", "{row}");
    assert!(row.get("import_hint").is_none());
    assert_eq!(row["detail"], "catalog_unproven");
    db.execute_unprepared("UPDATE agent_session SET state='active' WHERE provider_session_id='123e4567-e89b-12d3-a456-426614174031'").await.unwrap();
    db.execute_unprepared("UPDATE agent_checkpoint SET tree_oid='invalid-oid'")
        .await
        .unwrap();
    let view = fixture.codex_reconcile_view(&backup, None);
    assert_eq!(view["status"], "available", "{view}");
    let rows = view["rows"].as_array().unwrap();
    assert_eq!(
        rows.iter()
            .find(|r| r["provider_session_id"] == "healthy")
            .unwrap()["status"],
        "missing_data"
    );
    assert_eq!(
        rows.iter()
            .find(|r| r["provider_session_id"] == "123e4567-e89b-12d3-a456-426614174031")
            .unwrap()["status"],
        "unknown"
    );
    db.close().await.unwrap();
}

#[cfg(unix)]
#[tokio::test]
#[ignore = "operator-local Codex acceptance; never run in CI"]
async fn codex_reconcile_subdir_link_and_alias_have_separate_scope_states() {
    let fixture = LocalCodexSession::new();
    fixture.source(1, true);
    assert!(
        fixture
            .cx01_hook("SessionStart", "session-start")
            .status
            .success()
    );
    let sub = fixture.repo.join("sub");
    std::fs::create_dir(&sub).unwrap();
    let foreign = fixture._temp.path().join("foreign");
    std::fs::create_dir(&foreign).unwrap();
    let link = fixture.repo.join("link");
    std::os::unix::fs::symlink(foreign, &link).unwrap();
    let alias = fixture.repo.with_file_name("REPO").join("sub");
    let backup = fixture
        .codex_reconcile_backup(&[("sub", sub), ("alias", alias), ("link", link)])
        .await;
    let view = fixture.codex_reconcile_view(&backup, None);
    assert_eq!(view["scope_unknown_count"], 1, "{view}");
    assert_eq!(view["scope_ambiguous_count"], 1, "{view}");
    let rows = view["rows"].as_array().unwrap();
    let row = |id: &str| {
        rows.iter()
            .find(|row| row["provider_session_id"] == id)
            .unwrap()
    };
    assert_eq!(row("sub")["scope"], "proven");
    assert_eq!(row("sub")["status"], "missing");
    assert_eq!(row("alias")["scope"], "alias_ambiguous");
    assert_eq!(row("link")["scope"], "layout_unknown");
}

#[tokio::test]
#[ignore = "operator-local Codex acceptance; never run in CI"]
async fn codex_reconcile_cli_invalid_millisecond_types_and_future_clock_are_unavailable() {
    use sea_orm::sqlx::{Connection, SqliteConnection, sqlite::SqliteConnectOptions};
    let fixture = LocalCodexSession::new();
    fixture.source(1, true);
    assert!(
        fixture
            .cx01_hook("SessionStart", "session-start")
            .status
            .success()
    );
    for sql in [
        "UPDATE threads SET updated_at_ms='invalid'",
        "UPDATE threads SET updated_at_ms=1.5",
        "UPDATE threads SET updated_at_ms=9223372036854775807",
    ] {
        let backup = fixture
            .codex_reconcile_backup(&[("missing", fixture.repo.clone())])
            .await;
        let mut db = SqliteConnection::connect_with(&SqliteConnectOptions::new().filename(&backup))
            .await
            .unwrap();
        sea_orm::sqlx::query(sql).execute(&mut db).await.unwrap();
        db.close().await.unwrap();
        let view = fixture.codex_reconcile_view(&backup, None);
        assert_eq!(view["status"], "unavailable", "{view}");
        assert!(view["page_totals"].is_null());
        assert!(view["scope_candidate_count"].is_null());
    }
}

impl LocalCodexSession {
    async fn codex_uncaptured_backup_rows(
        &self,
        rows: &[(&str, std::path::PathBuf)],
    ) -> std::path::PathBuf {
        use sea_orm::sqlx::{Connection, SqliteConnection, sqlite::SqliteConnectOptions};
        let entries = rows
            .iter()
            .map(|(id, _)| (*id, self.repo.clone()))
            .collect::<Vec<_>>();
        let backup = self.codex_reconcile_backup(&entries).await;
        let mut connection =
            SqliteConnection::connect_with(&SqliteConnectOptions::new().filename(&backup))
                .await
                .unwrap();
        // Full-history import must work without the later millisecond migration.
        for sql in [
            "DELETE FROM _sqlx_migrations WHERE version=25",
            "ALTER TABLE threads DROP COLUMN created_at_ms",
            "ALTER TABLE threads DROP COLUMN updated_at_ms",
        ] {
            sea_orm::sqlx::query(sql)
                .execute(&mut connection)
                .await
                .unwrap();
        }
        for (id, path) in rows {
            sea_orm::sqlx::query("UPDATE threads SET rollout_path=?,archived=1 WHERE id=?")
                .bind(path.to_str().unwrap())
                .bind(*id)
                .execute(&mut connection)
                .await
                .unwrap();
        }
        connection.close().await.unwrap();
        backup
    }
}

fn codex_uncaptured_error(out: &std::process::Output) -> serde_json::Value {
    let stderr = String::from_utf8_lossy(&out.stderr);
    stderr
        .lines()
        .filter_map(|line| serde_json::from_str::<serde_json::Value>(line).ok())
        .find(|value| value.get("error_code").is_some())
        .unwrap_or_else(|| panic!("missing JSON error: {stderr}"))
}

#[tokio::test]
#[ignore = "operator-local Codex acceptance; never run in CI"]
async fn codex_uncaptured_full_history_archive_roundtrip_skips_existing_without_source() {
    let mut fixture = LocalCodexSession::new();
    fixture.source(1, true);
    let text = std::fs::read_to_string(&fixture.source)
        .unwrap()
        .replace("2026-10-19", "2026-09-01");
    let archived = fixture
        .home
        .join(".codex/archived_sessions")
        .join(fixture.source.file_name().unwrap());
    std::fs::create_dir_all(archived.parent().unwrap()).unwrap();
    std::fs::remove_file(&fixture.source).unwrap();
    std::fs::write(&archived, text).unwrap();
    fixture.source = archived;
    let backup = fixture
        .codex_uncaptured_backup_rows(&[(
            "123e4567-e89b-12d3-a456-426614174031",
            fixture.source.clone(),
        )])
        .await;
    let before = std::fs::read(&backup).unwrap();
    let import = fixture.run(&[
        "agent",
        "import",
        "--codex-index-backup",
        backup.to_str().unwrap(),
        "--yes",
        "--json",
    ]);
    assert!(
        import.status.success(),
        "{}",
        String::from_utf8_lossy(&import.stderr)
    );
    let value: serde_json::Value = serde_json::from_slice(&import.stdout).unwrap();
    assert_eq!(value["data"]["results"].as_array().unwrap().len(), 1);
    assert!(
        value["data"]["results"][0]["checkpoints_written"]
            .as_u64()
            .unwrap()
            > 0
    );
    let db = fixture.db().await;
    let exported = fixture.exports(&db).await;
    assert!(exported.join("\n").contains("CX15-MULTI-0"));
    let checkpoints = LocalCodexSession::count(&db, "agent_checkpoint", "1").await;
    let claims = LocalCodexSession::count(&db, "agent_coverage_claim", "1").await;
    std::fs::remove_file(&fixture.source).unwrap();
    let replay = fixture.run(&[
        "agent",
        "import",
        "--codex-index-backup",
        backup.to_str().unwrap(),
        "--yes",
        "--json",
    ]);
    assert!(
        replay.status.success(),
        "{}",
        String::from_utf8_lossy(&replay.stderr)
    );
    let value: serde_json::Value = serde_json::from_slice(&replay.stdout).unwrap();
    assert!(value["data"]["results"].as_array().unwrap().is_empty());
    assert_eq!(value["data"]["existing"].as_array().unwrap().len(), 1);
    assert_eq!(
        value["data"]["existing"][0]["selection_reason"],
        "existing_catalog_session"
    );
    assert_eq!(
        LocalCodexSession::count(&db, "agent_checkpoint", "1").await,
        checkpoints
    );
    assert_eq!(
        LocalCodexSession::count(&db, "agent_coverage_claim", "1").await,
        claims
    );
    assert_eq!(fixture.exports(&db).await, exported);
    assert_eq!(std::fs::read(&backup).unwrap(), before);
    db.close().await.unwrap();
}

#[tokio::test]
#[ignore = "operator-local Codex acceptance; never run in CI"]
async fn codex_uncaptured_auth_path_is_refused_without_session_or_trace_writes() {
    const SENTINEL: &str = "CX19-CREDENTIAL-MUST-NOT-LEAVE-FIXTURE";
    let mut fixture = LocalCodexSession::new();
    fixture.source(1, true);
    let archived = fixture
        .home
        .join(".codex/archived_sessions")
        .join(fixture.source.file_name().unwrap());
    std::fs::create_dir_all(archived.parent().unwrap()).unwrap();
    std::fs::rename(&fixture.source, &archived).unwrap();
    fixture.source = archived;
    let root = fixture.home.join(".codex");
    let mut rows = vec![(
        "123e4567-e89b-12d3-a456-426614174031",
        fixture.source.clone(),
    )];
    for (id, name) in [
        ("bad-auth", "auth.json"),
        ("bad-config", "config.toml"),
        ("bad-history", "history.jsonl"),
        ("bad-state", "state_5.sqlite"),
    ] {
        let path = root.join(name);
        std::fs::write(&path, SENTINEL).unwrap();
        rows.push((id, path));
    }
    rows.extend([
        ("bad-relative", std::path::PathBuf::from("auth.json")),
        ("bad-parent", root.join("..").join("auth.json")),
        ("bad-sid", fixture.source.clone()),
    ]);
    let backup = fixture.codex_uncaptured_backup_rows(&rows).await;
    let before = std::fs::read(&backup).unwrap();
    let mut command = std::process::Command::new(env!("CARGO_BIN_EXE_libra"));
    command
        .current_dir(&fixture.repo)
        .env_clear()
        .env("PATH", "/usr/bin:/bin")
        .env("HOME", &fixture.home)
        .env("USERPROFILE", &fixture.home)
        .env("LIBRA_TEST_HOME", &fixture.home)
        .env("LIBRA_TEST", "1")
        .args([
            "agent",
            "import",
            "--codex-index-backup",
            backup.to_str().unwrap(),
            "--limit",
            "100",
            "--yes",
            "--json=compact",
        ]);
    // Observe the real file sink through an existing descriptor; no log artifact.
    #[cfg(unix)]
    command
        .env("LIBRA_LOG", "libra=debug")
        .env("LIBRA_LOG_FILE", "/dev/fd/2");
    let out = command.output().unwrap();
    assert!(!out.status.success());
    assert!(out.stdout.is_empty());
    let stderr = String::from_utf8_lossy(&out.stderr);
    let error = stderr
        .lines()
        .filter_map(|line| serde_json::from_str::<serde_json::Value>(line).ok())
        .find(|row| row.get("error_code").is_some())
        .unwrap();
    assert_eq!(error["error_code"], "LBR-AGENT-018");
    let failures = error["details"]["failures"].as_array().unwrap();
    assert_eq!(failures.len(), 7);
    assert!(
        failures
            .iter()
            .all(|row| row["error_code"] == "LBR-AGENT-020")
    );
    assert_eq!(error["details"]["results"].as_array().unwrap().len(), 1);
    assert!(!stderr.contains(SENTINEL));
    assert!(!String::from_utf8_lossy(&out.stdout).contains(SENTINEL));
    assert!(!stderr.contains("failed to open LIBRA_LOG_FILE"));
    let db = fixture.db().await;
    assert_eq!(
        LocalCodexSession::count(&db, "agent_session", "agent_kind='codex'").await,
        1
    );
    assert!(
        fixture
            .exports(&db)
            .await
            .join("\n")
            .contains("CX15-MULTI-0")
    );
    for id in [
        "bad-auth",
        "bad-config",
        "bad-history",
        "bad-state",
        "bad-relative",
        "bad-parent",
        "bad-sid",
    ] {
        use sea_orm::{ConnectionTrait, Statement};
        let row = db.query_one_raw(Statement::from_sql_and_values(db.get_database_backend(),
            "SELECT COUNT(*) AS n FROM agent_session WHERE agent_kind='codex' AND provider_session_id=?",[id.into()])).await.unwrap().unwrap();
        assert_eq!(row.try_get_by::<i64, _>("n").unwrap(), 0);
    }
    assert_eq!(std::fs::read(&backup).unwrap(), before);
    db.close().await.unwrap();
}

#[tokio::test]
#[ignore = "operator-local Codex acceptance; never run in CI"]
async fn codex_uncaptured_paging_existing_and_errors_preserve_slots_without_parent_grants() {
    use sea_orm::sqlx::{Connection, SqliteConnection, sqlite::SqliteConnectOptions};
    use sha2::{Digest, Sha256};
    const FIRST: &str = "123e4567-e89b-12d3-a456-426614174031";
    const SECOND: &str = "123e4567-e89b-12d3-a456-426614174032";
    let fixture = LocalCodexSession::new();
    fixture.source(1, true);
    let second = fixture.source.with_file_name(
        fixture
            .source
            .file_name()
            .unwrap()
            .to_str()
            .unwrap()
            .replace(FIRST, SECOND),
    );
    std::fs::write(
        &second,
        std::fs::read_to_string(&fixture.source)
            .unwrap()
            .replace(FIRST, SECOND),
    )
    .unwrap();
    let backup = fixture
        .codex_uncaptured_backup_rows(&[(SECOND, second.clone()), (FIRST, fixture.source.clone())])
        .await;
    let mut index = SqliteConnection::connect_with(&SqliteConnectOptions::new().filename(&backup))
        .await
        .unwrap();
    sea_orm::sqlx::query("INSERT INTO thread_spawn_edges VALUES(?,?,'completed')")
        .bind(FIRST)
        .bind(SECOND)
        .execute(&mut index)
        .await
        .unwrap();
    index.close().await.unwrap();
    let original_backup = std::fs::read(&backup).unwrap();
    let run_page = |cursor: &str| {
        fixture.run(&[
            "agent",
            "import",
            "--codex-index-backup",
            backup.to_str().unwrap(),
            "--cursor",
            cursor,
            "--limit",
            "1",
            "--yes",
            "--json=compact",
        ])
    };
    let first = run_page("0");
    assert!(
        first.status.success(),
        "{}",
        String::from_utf8_lossy(&first.stderr)
    );
    let first: serde_json::Value = serde_json::from_slice(&first.stdout).unwrap();
    assert_eq!(first["data"]["next_cursor"], 1);
    assert_eq!(first["data"]["results"].as_array().unwrap().len(), 1);
    std::fs::remove_file(&fixture.source).unwrap();
    let replay = run_page("0");
    assert!(
        replay.status.success(),
        "{}",
        String::from_utf8_lossy(&replay.stderr)
    );
    let replay: serde_json::Value = serde_json::from_slice(&replay.stdout).unwrap();
    assert_eq!(replay["data"]["next_cursor"], 1);
    assert_eq!(replay["data"]["existing"].as_array().unwrap().len(), 1);
    assert_eq!(
        replay["data"]["existing"][0]["session_id"],
        format!(
            "sha256:{}",
            hex::encode(&Sha256::digest(FIRST.as_bytes())[..6])
        )
    );
    assert!(replay["data"]["results"].as_array().unwrap().is_empty());
    let next = run_page("1");
    assert!(
        next.status.success(),
        "{}",
        String::from_utf8_lossy(&next.stderr)
    );
    let next: serde_json::Value = serde_json::from_slice(&next.stdout).unwrap();
    assert_eq!(next["data"]["next_cursor"], serde_json::Value::Null);
    assert_eq!(next["data"]["results"].as_array().unwrap().len(), 1);
    let db = fixture.db().await;
    assert_eq!(
        LocalCodexSession::count(&db, "agent_session", "agent_kind='codex'").await,
        2
    );
    assert_eq!(
        LocalCodexSession::count(&db, "agent_session", "parent_session_id IS NOT NULL").await,
        0
    );
    assert_eq!(
        LocalCodexSession::count(&db, "agent_subagent_link", "1").await,
        0
    );
    let exported = fixture.exports(&db).await;
    assert_eq!(
        exported
            .iter()
            .filter(|text| text.contains("CX15-MULTI-0"))
            .count(),
        2
    );
    assert_eq!(std::fs::read(&backup).unwrap(), original_backup);

    // The same child imported through the original session selector has the
    // same ordinary checkpoint shape and receives no inferred parent link.
    let mut control = LocalCodexSession::new();
    control.source(1, true);
    let original = control.source.clone();
    control.source = original.with_file_name(
        original
            .file_name()
            .unwrap()
            .to_str()
            .unwrap()
            .replace(FIRST, SECOND),
    );
    std::fs::write(
        &control.source,
        std::fs::read_to_string(&original)
            .unwrap()
            .replace(FIRST, SECOND),
    )
    .unwrap();
    std::fs::remove_file(original).unwrap();
    let explicit = control.run(&[
        "agent",
        "import",
        "--session",
        SECOND,
        "--agent",
        "codex",
        "--yes",
        "--json=compact",
    ]);
    assert!(
        explicit.status.success(),
        "{}",
        String::from_utf8_lossy(&explicit.stderr)
    );
    let explicit: serde_json::Value = serde_json::from_slice(&explicit.stdout).unwrap();
    assert_eq!(
        explicit["data"]["results"][0]["checkpoints_written"],
        next["data"]["results"][0]["checkpoints_written"]
    );
    let control_db = control.db().await;
    assert_eq!(
        LocalCodexSession::count(
            &control_db,
            "agent_session",
            "parent_session_id IS NOT NULL"
        )
        .await,
        0
    );
    assert_eq!(
        LocalCodexSession::count(&control_db, "agent_subagent_link", "1").await,
        0
    );
    assert!(
        control
            .exports(&control_db)
            .await
            .join("\n")
            .contains("CX15-MULTI-0")
    );
    control_db.close().await.unwrap();

    // A vanished second source must also remain existing, while a new bad slot
    // is reported alongside both existing slots in the error document.
    std::fs::remove_file(&second).unwrap();
    let mut index = SqliteConnection::connect_with(&SqliteConnectOptions::new().filename(&backup))
        .await
        .unwrap();
    sea_orm::sqlx::query("INSERT INTO threads(id,rollout_path,cwd,cli_version,archived) VALUES('bad-slot',? ,?,'',1)")
        .bind(fixture.home.join(".codex/auth.json").to_str().unwrap())
        .bind(fixture.repo.to_str().unwrap()).execute(&mut index).await.unwrap();
    index.close().await.unwrap();
    let out = fixture.run(&[
        "agent",
        "import",
        "--codex-index-backup",
        backup.to_str().unwrap(),
        "--yes",
        "--json=compact",
    ]);
    assert!(!out.status.success());
    assert!(out.stdout.is_empty());
    let error: serde_json::Value = codex_uncaptured_error(&out);
    assert_eq!(error["error_code"], "LBR-AGENT-018");
    assert_eq!(error["details"]["existing"].as_array().unwrap().len(), 2);
    assert_eq!(error["details"]["failures"].as_array().unwrap().len(), 1);
    assert_eq!(
        error["details"]["failures"][0]["error_code"],
        "LBR-AGENT-020"
    );
    assert_eq!(
        error["details"]["failures"][0]["session_id"],
        format!("sha256:{}", hex::encode(&Sha256::digest(b"bad-slot")[..6]))
    );
    let human = fixture.run(&[
        "agent",
        "import",
        "--codex-index-backup",
        backup.to_str().unwrap(),
        "--yes",
    ]);
    assert!(!human.status.success());
    assert!(String::from_utf8_lossy(&human.stderr).contains("2 already cataloged (not refreshed)"));
    assert_eq!(fixture.exports(&db).await, exported);
    db.close().await.unwrap();
}

#[tokio::test]
#[ignore = "operator-local Codex acceptance; never run in CI"]
async fn codex_uncaptured_consent_usage_and_whole_backup_fail_without_batch_or_writes() {
    let mut fixture = LocalCodexSession::new();
    fixture.source(1, true);
    let backup = fixture
        .codex_uncaptured_backup_rows(&[(
            "123e4567-e89b-12d3-a456-426614174031",
            fixture.source.clone(),
        )])
        .await;
    let db = fixture.db().await;
    let before = codex_finalize_catalog_snapshot(&db).await;
    for extra in [
        vec![],
        vec!["--yes", "--all"],
        vec!["--yes", "--agent", "claude-code"],
        vec!["--yes", "--restore-erased"],
        vec!["--yes", "--limit", "0"],
        vec!["--yes", "--limit", "101"],
        vec!["--yes", "--cursor", "2"],
    ] {
        let mut args = vec![
            "agent",
            "import",
            "--codex-index-backup",
            backup.to_str().unwrap(),
            "--json=compact",
        ];
        args.extend(extra);
        let out = fixture.run(&args);
        assert!(!out.status.success(), "{args:?}");
        assert!(out.stdout.is_empty(), "{args:?}");
        let error: serde_json::Value = codex_uncaptured_error(&out);
        assert_eq!(error["error_code"], "LBR-CLI-002", "{args:?}: {error}");
        assert_eq!(codex_finalize_catalog_snapshot(&db).await, before);
    }
    let bad = fixture.home.join("not-a-backup.sqlite");
    for path in [bad.clone(), backup.with_file_name("missing.sqlite")] {
        if path == bad {
            std::fs::write(&path, "CX19-NOT-A-DATABASE").unwrap();
        }
        let out = fixture.run(&[
            "agent",
            "import",
            "--codex-index-backup",
            path.to_str().unwrap(),
            "--yes",
            "--json=compact",
        ]);
        assert!(!out.status.success());
        assert!(out.stdout.is_empty());
        let error: serde_json::Value = codex_uncaptured_error(&out);
        assert_eq!(error["error_code"], "LBR-AGENT-018", "{error}");
        assert!(error["details"].get("results").is_none());
        assert!(!String::from_utf8_lossy(&out.stderr).contains("CX19-NOT-A-DATABASE"));
        assert_eq!(codex_finalize_catalog_snapshot(&db).await, before);
    }
    // Backup paths are relative to the invocation directory, with lexical '..'
    // supported; this does not change the untrusted rollout-path contract.
    let relative = fixture.repo.join("backup.sqlite");
    std::fs::copy(&backup, &relative).unwrap();
    let subdir = fixture.repo.join("subdir");
    std::fs::create_dir(&subdir).unwrap();
    fixture.repo = subdir;
    let out = fixture.run(&[
        "agent",
        "import",
        "--codex-index-backup",
        "../backup.sqlite",
        "--yes",
        "--json=compact",
    ]);
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let value: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(value["data"]["results"].as_array().unwrap().len(), 1);
    assert_eq!(LocalCodexSession::count(&db, "agent_session", "1").await, 1);
    // The original selector still omits the additive existing array.
    let old = fixture.import();
    assert!(
        old.status.success(),
        "{}",
        String::from_utf8_lossy(&old.stderr)
    );
    let old: serde_json::Value = serde_json::from_slice(&old.stdout).unwrap();
    assert!(old["data"].get("existing").is_none());
    db.close().await.unwrap();
}

#[tokio::test]
#[ignore = "operator-local Codex acceptance; never run in CI"]
async fn codex_uncaptured_catalog_pending_erased_unknown_and_legacy_are_not_missing() {
    use sea_orm::{ConnectionTrait, Statement};
    let fixture = LocalCodexSession::new();
    fixture.source(1, true);
    assert!(fixture.import().status.success());
    let db = fixture.db().await;
    let owner = db
        .query_one_raw(Statement::from_string(
            db.get_database_backend(),
            "SELECT repo_id,worktree_id FROM agent_session LIMIT 1",
        ))
        .await
        .unwrap()
        .unwrap();
    let repo_id: String = owner.try_get_by("repo_id").unwrap();
    let worktree_id: String = owner.try_get_by("worktree_id").unwrap();
    for id in ["pending", "erased", "committed"] {
        db.execute_raw(Statement::from_sql_and_values(db.get_database_backend(),
            "INSERT INTO agent_import_identity(identity_id,agent_kind,provider_session_id,source_kind,source_id,schema_version,state,created_at,updated_at,scope_state,repo_id,worktree_id,lease_expires_at) VALUES(?,'codex',?,'provider_file',?,1,'leased',1,1,'scoped',?,?,1)",
            [id.into(),id.into(),format!("local-{id}").into(),repo_id.clone().into(),worktree_id.clone().into()])).await.unwrap();
    }
    db.execute_unprepared(
        "UPDATE agent_import_identity SET state='committed' WHERE provider_session_id='committed'",
    )
    .await
    .unwrap();
    db.execute_unprepared("INSERT INTO agent_import_tombstone(tombstone_id,agent_kind,provider_session_id,erased_session_id,erased_at) VALUES('erased','codex','erased','codex__erased',1)").await.unwrap();
    // All three source files are deliberately absent; catalog state wins.
    let rows = ["pending", "erased", "committed"].map(|id| {
        (
            id,
            fixture
                .source
                .with_file_name(format!("rollout-old-{id}.jsonl")),
        )
    });
    let backup = fixture.codex_uncaptured_backup_rows(&rows).await;
    let before = codex_finalize_catalog_snapshot(&db).await;
    let out = fixture.run(&[
        "agent",
        "import",
        "--codex-index-backup",
        backup.to_str().unwrap(),
        "--yes",
        "--json=compact",
    ]);
    assert!(!out.status.success());
    assert!(out.stdout.is_empty());
    let error: serde_json::Value = codex_uncaptured_error(&out);
    assert_eq!(error["error_code"], "LBR-AGENT-018");
    let failures = error["details"]["failures"].as_array().unwrap();
    assert_eq!(failures.len(), 2);
    assert!(
        failures
            .iter()
            .all(|row| row["error_code"] == "LBR-AGENT-018")
    );
    assert_eq!(error["details"]["skipped"].as_array().unwrap().len(), 1);
    assert_eq!(
        error["details"]["skipped"][0]["reason_code"],
        "LBR-AGENT-019"
    );
    assert_eq!(codex_finalize_catalog_snapshot(&db).await, before);
    db.execute_raw(Statement::from_sql_and_values(db.get_database_backend(),
        "INSERT INTO agent_session(session_id,agent_kind,provider_session_id,state,working_dir,started_at,last_event_at) VALUES('codex__legacy','codex','legacy','stopped',?,1,1)",
        [fixture.repo.to_str().unwrap().into()]))
    .await
    .unwrap();
    let backup = fixture
        .codex_uncaptured_backup_rows(&[(
            "legacy",
            fixture.source.with_file_name("rollout-old-legacy.jsonl"),
        )])
        .await;
    let before = codex_finalize_catalog_snapshot(&db).await;
    let out = fixture.run(&[
        "agent",
        "import",
        "--codex-index-backup",
        backup.to_str().unwrap(),
        "--yes",
        "--json=compact",
    ]);
    assert!(!out.status.success());
    let error: serde_json::Value = codex_uncaptured_error(&out);
    assert_eq!(error["error_code"], "LBR-AGENT-018");
    assert_eq!(error["details"]["failures"].as_array().unwrap().len(), 1);
    assert!(error["details"].get("existing").is_none());
    assert_eq!(codex_finalize_catalog_snapshot(&db).await, before);
    db.close().await.unwrap();
}

#[tokio::test]
#[ignore = "operator-local Codex acceptance; never run in CI"]
async fn codex_uncaptured_linked_worktree_existing_uses_its_pinned_scope() {
    let mut fixture = LocalCodexSession::new();
    let db = fixture.db().await;
    let linked = fixture._temp.path().join("linked");
    let out = fixture.run(&[
        "worktree",
        "add",
        "linked",
        "--path",
        linked.to_str().unwrap(),
    ]);
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(linked.join(".libra").is_dir());
    assert!(linked.join(".libra/commondir").is_file());
    fixture.repo = linked.canonicalize().unwrap();
    fixture.source(1, true);
    let backup = fixture
        .codex_uncaptured_backup_rows(&[(
            "123e4567-e89b-12d3-a456-426614174031",
            fixture.source.clone(),
        )])
        .await;
    let out = fixture.run(&[
        "agent",
        "import",
        "--codex-index-backup",
        backup.to_str().unwrap(),
        "--yes",
        "--json=compact",
    ]);
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let value: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(value["data"]["results"].as_array().unwrap().len(), 1);
    let exported = fixture.exports(&db).await;
    assert!(exported.join("\n").contains("CX15-MULTI-0"));
    std::fs::remove_file(&fixture.source).unwrap();
    let out = fixture.run(&[
        "agent",
        "import",
        "--codex-index-backup",
        backup.to_str().unwrap(),
        "--yes",
        "--json=compact",
    ]);
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let value: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(value["data"]["existing"].as_array().unwrap().len(), 1);
    assert!(value["data"]["results"].as_array().unwrap().is_empty());
    assert_eq!(fixture.exports(&db).await, exported);
    assert_eq!(LocalCodexSession::count(&db, "agent_session", "1").await, 1);
    db.close().await.unwrap();
}

#[tokio::test]
#[ignore = "operator-local Codex acceptance; never run in CI"]
async fn codex_uncaptured_stale_header_and_changed_cwd_fail_per_slot_without_writes() {
    let fixture = LocalCodexSession::new();
    fixture.source(1, true);
    let body = std::fs::read_to_string(&fixture.source).unwrap();
    let wrong_sid = fixture.source.with_file_name("rollout-old-wrong-sid.jsonl");
    std::fs::write(&wrong_sid, &body).unwrap();
    let wrong_cwd = fixture.source.with_file_name("rollout-old-wrong-cwd.jsonl");
    let outside = fixture._temp.path().join("outside");
    std::fs::create_dir(&outside).unwrap();
    let mut rows = body
        .lines()
        .map(|line| serde_json::from_str::<serde_json::Value>(line).unwrap())
        .collect::<Vec<_>>();
    rows[0]["payload"]["id"] = serde_json::json!("wrong-cwd");
    rows[0]["payload"]["cwd"] = serde_json::json!(outside);
    let body = rows
        .iter()
        .map(serde_json::Value::to_string)
        .collect::<Vec<_>>()
        .join("\n")
        + "\n";
    std::fs::write(&wrong_cwd, body).unwrap();
    let missing = fixture.source.with_file_name("rollout-old-missing.jsonl");
    let backup = fixture
        .codex_uncaptured_backup_rows(&[
            ("wrong-sid", wrong_sid),
            ("wrong-cwd", wrong_cwd),
            ("missing", missing),
            ("123e4567-e89b-12d3-a456-426614174031", fixture.home.join(".codex/archived_sessions/rollout-old-123e4567-e89b-12d3-a456-426614174031.jsonl")),
        ])
        .await;
    let db = fixture.db().await;
    let before = codex_finalize_catalog_snapshot(&db).await;
    let out = fixture.run(&[
        "agent",
        "import",
        "--codex-index-backup",
        backup.to_str().unwrap(),
        "--yes",
        "--json=compact",
    ]);
    assert!(!out.status.success());
    assert!(out.stdout.is_empty());
    let error = codex_uncaptured_error(&out);
    assert_eq!(error["error_code"], "LBR-AGENT-018");
    let failures = error["details"]["failures"].as_array().unwrap();
    assert_eq!(failures.len(), 4);
    assert!(
        failures
            .iter()
            .all(|row| row["error_code"] == "LBR-AGENT-020"),
        "{error}"
    );
    assert!(error["details"]["results"].as_array().unwrap().is_empty());
    assert!(!String::from_utf8_lossy(&out.stderr).contains("CX15-MULTI-0"));
    assert_eq!(codex_finalize_catalog_snapshot(&db).await, before);
    assert_eq!(LocalCodexSession::count(&db, "agent_session", "1").await, 0);
    db.close().await.unwrap();
}

#[cfg(unix)]
#[tokio::test]
#[ignore = "operator-local Codex acceptance; never run in CI"]
async fn codex_uncaptured_mid_batch_root_or_catalog_failure_preserves_committed_slots() {
    use sea_orm::{ConnectionTrait, Statement};
    const FIRST: &str = "123e4567-e89b-12d3-a456-426614174031";
    for fault in ["root", "catalog"] {
        let fixture = LocalCodexSession::new();
        fixture.source(1, true);
        let body = std::fs::read_to_string(&fixture.source).unwrap();
        let padding =
            "{\"type\":\"event_msg\",\"payload\":{\"type\":\"token_count\",\"info\":null}}\n"
                .repeat(100_000);
        let mut entries = vec![(FIRST, fixture.source.clone())];
        for id in [
            "123e4567-e89b-12d3-a456-426614174032",
            "123e4567-e89b-12d3-a456-426614174033",
        ] {
            let path = fixture
                .source
                .with_file_name(format!("rollout-old-{id}.jsonl"));
            std::fs::write(&path, body.replace(FIRST, id) + &padding).unwrap();
            entries.push((id, path));
        }
        let backup = fixture.codex_uncaptured_backup_rows(&entries).await;
        let db = fixture.db().await;
        let schema: String = db
            .query_one_raw(Statement::from_string(
                db.get_database_backend(),
                "SELECT sql FROM sqlite_master WHERE name='agent_import_tombstone'",
            ))
            .await
            .unwrap()
            .unwrap()
            .try_get_by("sql")
            .unwrap();
        let started = std::time::Instant::now();
        let mut child = CodexTerminalChild {
            child: Some(
                std::process::Command::new(env!("CARGO_BIN_EXE_libra"))
                    .current_dir(&fixture.repo)
                    .env_clear()
                    .env("PATH", "/usr/bin:/bin")
                    .env("HOME", &fixture.home)
                    .env("USERPROFILE", &fixture.home)
                    .env("LIBRA_TEST_HOME", &fixture.home)
                    .env("LIBRA_TEST", "1")
                    .args([
                        "agent",
                        "import",
                        "--codex-index-backup",
                        backup.to_str().unwrap(),
                        "--yes",
                        "--json=compact",
                    ])
                    .stdin(std::process::Stdio::null())
                    .stdout(std::process::Stdio::piped())
                    .stderr(std::process::Stdio::piped())
                    .spawn()
                    .unwrap(),
            ),
            started,
        };
        // Observe both commit and retirement of the first slot's object-index
        // barrier before changing inputs. This cannot turn its final advisory
        // tombstone read into a partial result by racing that read.
        loop {
            let retired = db.query_one_raw(Statement::from_sql_and_values(db.get_database_backend(),
                "SELECT EXISTS(SELECT 1 FROM agent_checkpoint WHERE session_id=?) AND NOT EXISTS(SELECT 1 FROM metadata_kv WHERE scope='agent_import_index_repair' AND target=? AND key='object-index-v1') AS retired",
                [format!("codex__{FIRST}").into(),format!("codex__{FIRST}").into()])).await.unwrap().unwrap().try_get_by::<i64,_>("retired").unwrap();
            if retired == 1 {
                break;
            }
            assert!(
                started.elapsed() < std::time::Duration::from_secs(20),
                "first checkpoint barrier not retired: {fault}"
            );
            assert!(
                child.try_wait().unwrap().is_none(),
                "import ended before fixture mutation: {fault}"
            );
            tokio::time::sleep(std::time::Duration::from_millis(1)).await;
        }
        let saved = fixture._temp.path().join("saved-repo");
        if fault == "root" {
            std::fs::rename(&fixture.repo, &saved).unwrap();
            std::fs::create_dir(&fixture.repo).unwrap();
        } else {
            db.execute_unprepared("DROP TABLE agent_import_tombstone")
                .await
                .unwrap();
        }
        while child.try_wait().unwrap().is_none() {
            assert!(
                started.elapsed() < std::time::Duration::from_secs(30),
                "import did not terminate: {fault}"
            );
            tokio::time::sleep(std::time::Duration::from_millis(5)).await;
        }
        let out = child.into_child().wait_with_output().unwrap();
        assert!(!out.status.success(), "mutation was not observed: {fault}");
        assert!(out.stdout.is_empty());
        let error = codex_uncaptured_error(&out);
        assert_eq!(error["error_code"], "LBR-AGENT-018", "{error}");
        assert_eq!(
            error["details"]["reason_key"],
            if fault == "root" {
                "scope_root_unproven"
            } else {
                "catalog_unavailable"
            },
            "{error}"
        );
        let results = error["details"]["results"].as_array().unwrap();
        assert!(!results.is_empty(), "committed progress missing: {error}");
        assert!(results[0]["checkpoints_written"].as_u64().unwrap() > 0);
        let failures = error["details"]["failures"].as_array().unwrap();
        assert!(!failures.is_empty());
        assert!(
            failures
                .iter()
                .all(|row| row["error_code"] == "LBR-AGENT-018"),
            "{error}"
        );
        assert_eq!(
            results.len() + failures.len(),
            3,
            "every selected slot remains visible: {error}"
        );
        // Restore only the deliberately broken temporary fixture after the
        // command is terminal, then prove earlier thinking is still readable.
        if fault == "root" {
            std::fs::remove_dir(&fixture.repo).unwrap();
            std::fs::rename(saved, &fixture.repo).unwrap();
        } else {
            db.execute_unprepared(&schema).await.unwrap();
        }
        assert!(
            fixture
                .exports(&db)
                .await
                .join("\n")
                .contains("CX15-MULTI-0")
        );
        db.close().await.unwrap();
    }
}
