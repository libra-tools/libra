//! AG-21 transcript intelligence over the first-batch adapters
//! (`docs/development/tracing/agent.md` E6/E7; plan.md Task A6).
//!
//! Fixtures live in `tests/fixtures/agent_transcripts/` with a provenance
//! manifest (`MANIFEST.md`) — assertion failures should be triaged against
//! that table (implementation regression vs upstream format drift).

use libra::internal::ai::observed_agents::{
    AgentKind, agent_for,
    coverage::{Completeness, SemanticRecord, normalize_opencode_export},
    extract::{self, CLAUDE_CODE_SKILL_REGISTRY, CODEX_SKILL_REGISTRY, OPENCODE_SKILL_REGISTRY},
};

fn opencode_turns(
    messages: serde_json::Value,
) -> Vec<libra::internal::ai::observed_agents::coverage::NormalizedTurn> {
    normalize_opencode_export(
        &serde_json::to_vec(&serde_json::json!({"messages":messages})).unwrap(),
    )
}

fn native_user(id: &str, text: &str) -> serde_json::Value {
    serde_json::json!({"type":"user","id":id,"text":text})
}

fn native_idle(outcome: &str) -> serde_json::Value {
    serde_json::json!({"type":"idle","id":"msg_idle","outcome":outcome})
}

#[test]
fn opencode_known_parts_not_poisoned() {
    let user = serde_json::json!({"info":{"role":"user","id":"msg_u"},"parts":[{"type":"text","text":"hi"}]});
    let baseline = opencode_turns(serde_json::json!([user.clone()]));
    for kind in [
        "step-finish",
        "patch",
        "step-start",
        "snapshot",
        "retry",
        "compaction",
        "agent",
        "subtask",
        "file",
    ] {
        let turns = opencode_turns(
            serde_json::json!([user.clone(),{"info":{"role":"assistant"},"parts":[{"type":kind,"text":"not semantic","tokens":{"input":900},"path":"not projected","url":"private://not-read"}]}]),
        );
        assert_eq!(turns.len(), 1, "{kind}");
        assert_eq!(turns[0].completeness, Completeness::Complete, "{kind}");
        assert_eq!(
            turns[0].records, baseline[0].records,
            "{kind}: no projection"
        );
        assert_eq!(
            turns[0].digest_hex(),
            baseline[0].digest_hex(),
            "{kind}: no metadata in digest"
        );
    }
}

#[test]
fn opencode_unknown_part_incomplete() {
    for part in [
        serde_json::json!({"type":"reasoning","text":"opaque not consumed"}),
        serde_json::json!({"type":"future","text":"not consumed"}),
        serde_json::json!({"type":3}),
        serde_json::json!({"type":"text","text":3}),
        serde_json::json!(null),
    ] {
        let turns = opencode_turns(
            serde_json::json!([{"info":{"role":"user","id":"msg_u"},"parts":[{"type":"text","text":"hi"}]},{"info":{"role":"assistant"},"parts":[part]}]),
        );
        assert_eq!(turns.len(), 1);
        assert_eq!(turns[0].completeness, Completeness::Incomplete);
        assert_eq!(
            turns[0].records,
            vec![SemanticRecord::User { text: "hi".into() }]
        );
    }
}

#[test]
fn opencode_native_human_keys_timestamps_and_metadata_digest() {
    let mut u1 = native_user("msg_u1", "hi");
    u1["time"] = serde_json::json!({"created":1790000000000i64});
    let mut a = opencode_assistant(
        "msg_a1",
        serde_json::json!([{"type":"text","text":"hello"}]),
    );
    a["time"] = serde_json::json!({"created":"2026-09-21T19:33:22Z"});
    let turns = opencode_turns(serde_json::json!([
        u1.clone(),
        a.clone(),
        native_user("msg_u2", "next"),
        native_idle("succeeded")
    ]));
    assert_eq!(turns.len(), 2);
    assert_eq!(turns[0].logical_turn_key, "msg_u1");
    assert_eq!(turns[1].logical_turn_key, "msg_u2");
    assert_eq!(turns[1].ordinal, 1);
    assert!(
        turns
            .iter()
            .all(|t| t.completeness == Completeness::Complete)
    );
    assert_eq!(turns[0].started_at, Some(1790000000));
    assert_eq!(turns[0].ended_at, Some(1790019202));
    let classic = opencode_turns(
        serde_json::json!([{"info":{"id":"msg_u1","role":"user"},"parts":[{"type":"text","text":"hi"}]},{"info":{"role":"assistant"},"parts":[{"type":"text","text":"hello"}]}]),
    );
    assert_eq!(turns[0].digest_hex(), classic[0].digest_hex());
    a["providerState"] = serde_json::json!({"signature":"never-consumed"});
    a["tokens"] = opencode_tokens(100);
    a["model"] = serde_json::json!({"id":"another"});
    let mut decorated = vec![u1];
    for kind in [
        "agent-switched",
        "model-switched",
        "location-switched",
        "synthetic",
        "system",
        "skill",
        "shell",
        "compaction",
    ] {
        decorated.push(serde_json::json!({"type":kind,"id":format!("msg_{kind}"),"text":"not human","metadata":{"private":"not digest"}}));
    }
    decorated.extend([a, native_idle("succeeded")]);
    let metadata = opencode_turns(serde_json::json!(decorated));
    assert_eq!(metadata.len(), 1);
    assert_eq!(metadata[0].completeness, Completeness::Complete);
    assert_eq!(metadata[0].records, turns[0].records);
    assert_eq!(metadata[0].digest_hex(), turns[0].digest_hex());
}

#[test]
fn opencode_native_idle_closes_capture_without_washing_violations() {
    for outcome in ["succeeded", "failed", "interrupted"] {
        let clean = opencode_turns(serde_json::json!([
            native_user("msg_u", "hi"),
            native_idle(outcome)
        ]));
        assert_eq!(clean[0].completeness, Completeness::Complete);
        for content in [
            serde_json::json!([{"type":"reasoning","text":"not consumed"}]),
            serde_json::json!([{"type":"future"}]),
            serde_json::json!("wrong"),
        ] {
            let turns = opencode_turns(serde_json::json!([
                native_user("msg_u", "hi"),
                opencode_assistant("msg_a", content),
                native_idle(outcome)
            ]));
            assert_eq!(turns[0].completeness, Completeness::Incomplete);
            assert_eq!(turns[0].records.len(), 1);
        }
    }
    for idle in [
        serde_json::json!(null),
        native_idle("future"),
        serde_json::json!({"type":"idle","id":3,"outcome":"succeeded"}),
    ] {
        let turns = opencode_turns(serde_json::json!([native_user("msg_u", "hi"), idle]));
        assert!(
            turns
                .iter()
                .any(|t| t.completeness == Completeness::Incomplete)
        );
    }
    let trailing = opencode_turns(serde_json::json!([
        native_user("msg_u", "hi"),
        native_idle("succeeded"),
        native_user("msg_next", "waiting")
    ]));
    assert_eq!(trailing[0].completeness, Completeness::Complete);
    assert_eq!(trailing[1].completeness, Completeness::Incomplete);
    let no_idle = opencode_turns(serde_json::json!([native_user("msg_u", "hi")]));
    assert_eq!(no_idle[0].completeness, Completeness::Incomplete);
}

#[test]
fn opencode_native_user_attachments_skills_and_agents_fail_visible() {
    let baseline = opencode_turns(serde_json::json!([
        native_user("msg_u", "hi"),
        native_idle("succeeded")
    ]));
    for (field, value) in [
        (
            "files",
            serde_json::json!([{"data":"c2VjcmV0","source":{"type":"uri","uri":"private://secret"}}]),
        ),
        ("files", serde_json::json!({})),
        ("files", serde_json::json!(null)),
        (
            "skills",
            serde_json::json!([{"id":"skill_s","name":"s","text":"secret"}]),
        ),
        (
            "skills",
            serde_json::json!([{"id":"skill_s","name":"s","text":""}]),
        ),
        ("skills", serde_json::json!([{"id":3,"name":"s"}])),
        ("skills", serde_json::json!("wrong")),
        ("agents", serde_json::json!([{"name":3}])),
    ] {
        let mut u = native_user("msg_u", "hi");
        u[field] = value;
        let turns = opencode_turns(serde_json::json!([u, native_idle("succeeded")]));
        assert_eq!(turns[0].completeness, Completeness::Incomplete, "{field}");
        assert_eq!(
            turns[0].records, baseline[0].records,
            "attachment bytes never projected"
        );
    }
    let mut u = native_user("msg_u", "hi");
    u["files"] = serde_json::json!([]);
    u["skills"] = serde_json::json!([{"id":"skill_s","name":"s"}]);
    u["agents"] = serde_json::json!([{"name":"a","mention":{"start":0,"end":1,"text":"a"}}]);
    let clean = opencode_turns(serde_json::json!([u, native_idle("succeeded")]));
    assert_eq!(clean[0].completeness, Completeness::Complete);
    assert_eq!(clean[0].digest_hex(), baseline[0].digest_hex());
}

#[test]
fn opencode_native_tool_content_error_and_inflight() {
    let tool = serde_json::json!({"type":"tool","id":"call_1","name":"read","state":{"status":"completed","input":{"path":"src/lib.rs"},"content":[{"type":"text","text":"first"},{"type":"text","text":"second"}]}});
    let capture = |tool: serde_json::Value| {
        opencode_turns(serde_json::json!([
            native_user("msg_u", "read"),
            opencode_assistant("msg_a", serde_json::json!([tool])),
            native_idle("succeeded")
        ]))
    };
    let turns = capture(tool.clone());
    assert_eq!(turns[0].completeness, Completeness::Complete);
    assert_eq!(turns[0].records.len(), 3);
    assert!(
        matches!(&turns[0].records[1],SemanticRecord::ToolCall{call_id:Some(id),name,..}if id=="call_1"&&name=="read")
    );
    assert_eq!(
        turns[0].records[2],
        SemanticRecord::ToolResult {
            call_id: Some("call_1".into()),
            content: "first\nsecond".into(),
            is_error: false
        }
    );
    let mut error = tool.clone();
    error["state"] = serde_json::json!({"status":"error","input":{},"error":{"type":"Tool.Error","message":"failed","response":{"body":"not semantic"}}});
    let turns = capture(error.clone());
    assert_eq!(turns[0].completeness, Completeness::Complete);
    assert_eq!(
        turns[0].records[2],
        SemanticRecord::ToolResult {
            call_id: Some("call_1".into()),
            content: "failed".into(),
            is_error: true
        }
    );
    error["state"]["content"] = serde_json::json!([{"type":"text","text":"explicit failure"}]);
    assert!(
        matches!(&capture(error)[0].records[2],SemanticRecord::ToolResult{content,is_error:true,..}if content=="explicit failure")
    );
    for (field, value) in [
        ("status", serde_json::json!("running")),
        ("status", serde_json::json!("streaming")),
        ("content", serde_json::json!([])),
        ("content", serde_json::json!("wrong")),
        (
            "content",
            serde_json::json!([{"type":"file","uri":"private://secret","mime":"text/plain"}]),
        ),
        ("content", serde_json::json!([{"type":"future"}])),
        ("input", serde_json::json!({"ratio":1.5})),
        ("input", serde_json::json!("wrong")),
    ] {
        let mut bad = tool.clone();
        bad["state"][field] = value;
        let turns = capture(bad);
        assert_eq!(turns[0].completeness, Completeness::Incomplete, "{field}");
        assert!(!format!("{:?}", turns[0].records).contains("private://secret"));
    }
    for field in ["id", "name", "state"] {
        let mut bad = tool.clone();
        bad[field] = serde_json::json!(3);
        assert_eq!(
            capture(bad)[0].completeness,
            Completeness::Incomplete,
            "{field}"
        );
    }
}

#[test]
fn opencode_native_malformed_and_mixed_messages_are_incomplete() {
    for bad in [
        serde_json::json!({"type":"user","id":3,"text":"hi"}),
        serde_json::json!({"type":"user","id":"msg_bad","text":3}),
        serde_json::json!({"type":"future","id":"msg_bad"}),
        serde_json::json!({"type":3,"id":"msg_bad"}),
        serde_json::json!({"info":{"role":"assistant"},"parts":[{"type":"text","text":"mixed"}]}),
    ] {
        let turns = opencode_turns(serde_json::json!([
            native_user("msg_u", "hi"),
            bad,
            native_idle("succeeded")
        ]));
        assert!(
            turns
                .iter()
                .any(|t| t.completeness == Completeness::Incomplete)
        );
    }
}

#[test]
fn opencode_native_text_and_tool_match_classic_coverage_records() {
    let tool = serde_json::json!({"type":"tool","id":"call_1","name":"read","state":{"status":"completed","input":{},"content":[{"type":"text","text":"result"}]}});
    let native = opencode_turns(serde_json::json!([
        native_user("msg_u", "hi"),
        opencode_assistant(
            "msg_a",
            serde_json::json!([{"type":"text","text":"before"},tool,{"type":"text","text":"after"}])
        ),
        native_idle("succeeded")
    ]));
    let classic = opencode_turns(
        serde_json::json!([{"info":{"role":"user","id":"msg_u"},"parts":[{"type":"text","text":"hi"}]},{"info":{"role":"assistant"},"parts":[{"type":"text","text":"before"},{"type":"tool","tool":"read","callID":"call_1","state":{"status":"completed","input":{},"output":"result"}},{"type":"text","text":"after"}]}]),
    );
    assert_eq!(native[0].completeness, Completeness::Complete);
    assert_eq!(native[0].records, classic[0].records);
    assert_eq!(native[0].digest_hex(), classic[0].digest_hex());
    let empty = opencode_turns(serde_json::json!([
        native_user("msg_u", "hi"),
        opencode_assistant("msg_a", serde_json::json!([{"type":"text","text":""}])),
        native_idle("succeeded")
    ]));
    assert_eq!(
        empty[0].records,
        vec![SemanticRecord::User { text: "hi".into() }]
    );
    let trailing = opencode_turns(serde_json::json!([
        native_user("msg_u", "hi"),
        native_idle("succeeded"),
        opencode_assistant("msg_a", serde_json::json!([{"type":"text","text":"later"}]))
    ]));
    assert_eq!(trailing[0].completeness, Completeness::Incomplete);
}

#[test]
fn opencode_system_reminder_stripped() {
    assert_eq!(extract::OPENCODE_INJECTION_PREFIXES, ["<system-reminder>"]);
    for (source, expected, partial) in [
        (
            "<system-reminder>injected private instructions</system-reminder>",
            None,
            false,
        ),
        (
            "<system-reminder>injected</system-reminder> /review human",
            Some(" /review human"),
            false,
        ),
        (
            "human <system-reminder>injected</system-reminder> remainder",
            Some("human  remainder"),
            false,
        ),
        (
            "<system-reminder>outer<system-reminder>inner</system-reminder>still private</system-reminder>human",
            Some("human"),
            false,
        ),
        (
            "human<system-reminder>unterminated private",
            Some("human"),
            true,
        ),
        ("<system-reminder>unterminated private", None, true),
        (
            "human </system-reminder>",
            Some("human </system-reminder>"),
            true,
        ),
        ("普通 human text", Some("普通 human text"), false),
    ] {
        let native = opencode_export(serde_json::json!([
            native_user("msg_u", source),
            native_idle("succeeded")
        ]));
        let classic = serde_json::json!({"info":{"location":{"directory":"/project"}},"messages":[{"info":{"id":"msg_u","role":"user"},"parts":[{"type":"text","text":source}]}]});
        for doc in [native, classic] {
            let bytes = serde_json::to_vec(&doc).unwrap();
            let summary = extract::extract_opencode(&bytes);
            assert_eq!(
                summary.prompts,
                expected.into_iter().map(str::to_owned).collect::<Vec<_>>(),
                "{source}"
            );
            assert_eq!(summary.partial, partial, "{source}");
            let turns = normalize_opencode_export(&bytes);
            let users = turns
                .iter()
                .flat_map(|t| &t.records)
                .filter_map(|r| match r {
                    SemanticRecord::User { text } => Some(text.as_str()),
                    _ => None,
                })
                .collect::<Vec<_>>();
            assert_eq!(
                users,
                expected.into_iter().collect::<Vec<_>>(),
                "{source}: same filter in normalization"
            );
            assert_eq!(
                turns
                    .iter()
                    .any(|t| t.completeness == Completeness::Incomplete),
                partial,
                "{source}: malformed not washed by idle"
            );
        }
        let flat = serde_json::json!({"role":"user","content":source});
        let summary = extract::extract_opencode(&serde_json::to_vec(&flat).unwrap());
        assert_eq!(
            summary.prompts,
            expected.into_iter().map(str::to_owned).collect::<Vec<_>>()
        );
        assert_eq!(summary.partial, partial);
    }
    let doc = opencode_export(serde_json::json!([native_user(
        "msg_u",
        "<system-reminder>/review not human</system-reminder>ordinary"
    )]));
    assert!(opencode_summary(&doc).skill_events.is_empty());
    let doc = opencode_export(serde_json::json!([native_user(
        "msg_u",
        "<system-reminder>private</system-reminder>/review human"
    )]));
    assert_eq!(opencode_summary(&doc).skill_events.len(), 1);
    let flat = serde_json::json!({"role":"user","timestamp":"2026-10-11T00:00:00Z","content":"<system-reminder>private</system-reminder>/review human","usage":{"input_tokens":12,"output_tokens":3}});
    let result = extract::extract_opencode(&serde_json::to_vec(&flat).unwrap());
    assert_eq!(result.usage.unwrap().total_tokens, Some(15));
    assert_eq!(result.skill_events[0].timestamp, "2026-10-11T00:00:00Z");
    let flat = serde_json::json!({"role":"user","content":"<system-reminder>private</system-reminder>","usage":{"input_tokens":12,"output_tokens":3}});
    let result = extract::extract_opencode(&serde_json::to_vec(&flat).unwrap());
    assert!(result.prompts.is_empty());
    assert_eq!(
        result.usage.unwrap().total_tokens,
        Some(15),
        "filtering prompt must not discard independent usage metadata"
    );
}

#[test]
fn opencode_extraction_fail_open_partial() {
    for bytes in [
        b"".as_slice(),
        b"   \n",
        b"null",
        b"[]",
        b"42",
        b"{}",
        b"{\"message\":[]}",
        b"{broken private-fixture",
    ] {
        let result = extract::extract_opencode(bytes);
        assert!(
            result.partial,
            "invalid source must not become complete zero output: {bytes:?}"
        );
        assert!(result.prompts.is_empty());
        assert!(!result.warnings.is_empty());
    }
}

#[test]
fn opencode_extraction_partial_marker_asserted() {
    let summary = extract::extract_opencode(
        br#"{"model":"fixture-model","usage":{"input_tokens":12,"output_tokens":3}}"#,
    );
    assert!(
        summary.partial,
        "metadata without recognized role is incomplete"
    );
    assert_eq!(summary.model.as_deref(), Some("fixture-model"));
    assert_eq!(
        summary.usage.unwrap().total_tokens,
        Some(15),
        "independent valid metadata retained"
    );
    let valid = opencode_summary(&opencode_export(serde_json::json!([])));
    assert!(
        !valid.partial,
        "a structurally valid empty export is distinct from failed parsing"
    );
}

#[test]
fn opencode_extraction_warnings_redacted_no_payload() {
    for bytes in [
        b"{broken payload-private-fixture AKIAIOSFODNN7EXAMPLE".as_slice(),
        br#"{"info":7,"messages":"payload-private-fixture"}"#,
    ] {
        let result = extract::extract_opencode(bytes);
        assert!(result.partial);
        assert!(!result.warnings.is_empty());
        let warnings = result.warnings.join("\n");
        assert!(!warnings.contains("payload-private-fixture"));
        assert!(!warnings.contains("AKIAIOSFODNN7EXAMPLE"));
        assert!(!warnings.contains("{broken"));
    }
}

fn fixture(name: &str) -> Vec<u8> {
    let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures/agent_transcripts")
        .join(name);
    std::fs::read(&path).unwrap_or_else(|err| {
        panic!(
            "read fixture {} (see tests/fixtures/agent_transcripts/MANIFEST.md \
             for provenance): {err}",
            path.display()
        )
    })
}

/// Each first-batch adapter extracts metadata from its fixture — or
/// (for dimensions its format cannot carry) stays absent without
/// erroring. Capability accessors gate what each adapter exposes.
#[test]
fn claude_opencode_fixtures_extract_metadata_or_partial() {
    // Claude Code: full surface.
    let claude = agent_for(AgentKind::ClaudeCode);
    let data = fixture("claude_code.jsonl");
    let analyzer = claude.as_transcript_analyzer().expect("claude analyzer");
    assert_eq!(analyzer.transcript_position(&data).unwrap(), data.len());
    let files = analyzer
        .extract_modified_files_from_offset(&data, 0)
        .unwrap();
    assert_eq!(
        files,
        [
            std::path::PathBuf::from("src/lib.rs"),
            std::path::PathBuf::from("docs/readme.md")
        ]
    );
    let prompts = claude
        .as_prompt_extractor()
        .expect("claude prompts")
        .extract_prompts(&data, 0)
        .unwrap();
    assert_eq!(prompts.len(), 2);
    let usage = claude
        .as_token_calculator()
        .expect("claude tokens")
        .calculate_token_usage(&data, 0)
        .unwrap();
    assert_eq!(usage.input_tokens, 200);
    assert_eq!(usage.output_tokens, 65);
    assert_eq!(usage.cached_tokens, Some(40));
    let model = claude
        .as_model_extractor()
        .expect("claude model")
        .extract_model(&data)
        .unwrap();
    assert_eq!(model.as_deref(), Some("claude-sonnet-5"));
    let subagent_total = claude
        .as_subagent_aware_extractor()
        .expect("claude subagent")
        .total_token_usage_including_subagents(&data)
        .unwrap();
    assert_eq!(subagent_total.input_tokens, 200);

    // Revised Codex capability acceptance lives in the operator-local, ignored
    // codex_capture_local_test target (CX-14).

    // OpenCode: prompts / model / skills from the JSON export form.
    let opencode = agent_for(AgentKind::OpenCode);
    let data = fixture("opencode.json");
    let prompts = opencode
        .as_prompt_extractor()
        .expect("opencode prompts")
        .extract_prompts(&data, 0)
        .unwrap();
    assert_eq!(prompts.len(), 2);
    let model = opencode
        .as_model_extractor()
        .expect("opencode model")
        .extract_model(&data)
        .unwrap();
    assert_eq!(model.as_deref(), Some("claude-sonnet-5"));

    // Non-first-batch promoted agents expose NO extraction capabilities.
    for kind in [AgentKind::Cursor, AgentKind::Copilot, AgentKind::FactoryAi] {
        let agent = agent_for(kind);
        assert!(agent.as_prompt_extractor().is_none(), "{kind:?}");
        assert!(agent.as_token_calculator().is_none(), "{kind:?}");
        assert!(agent.as_skill_event_extractor().is_none(), "{kind:?}");
    }
}

/// E6: the frozen wire keys map explicitly onto `CompletionUsageSummary`
/// (documented decisions pinned here).
#[test]
fn token_usage_mapping_uses_e6_wire_keys() {
    let value = serde_json::json!({
        "input_tokens": 1000,
        "cache_creation_tokens": 100,
        "cache_read_tokens": 50,
        "output_tokens": 300,
        "api_call_count": 7,
        "subagent_tokens": 40,
    });
    let full = extract::map_e6_token_usage_full(&value);
    assert_eq!(full.summary.input_tokens, 1000);
    assert_eq!(full.summary.output_tokens, 300);
    assert_eq!(full.summary.cached_tokens, Some(150), "creation+read sum");
    assert_eq!(
        full.summary.total_tokens,
        Some(1300),
        "computed input+output"
    );
    assert_eq!(
        full.summary.reasoning_tokens, None,
        "E6 carries no reasoning split"
    );
    assert_eq!(full.summary.cost_usd, None, "E6 carries no cost");
    // The count/subagent wire keys are consumed, not dropped.
    assert_eq!(full.api_call_count, 7);
    assert_eq!(full.subagent_tokens, 40);

    // Key-name sensitivity: the Claude-native spellings must NOT be
    // picked up by the E6 mapper (they go through the Claude parser).
    let native = serde_json::json!({
        "input_tokens": 10,
        "output_tokens": 5,
        "cache_creation_input_tokens": 4,
        "cache_read_input_tokens": 2,
    });
    let native_summary = extract::map_e6_token_usage(&native);
    assert_eq!(
        native_summary.cached_tokens, None,
        "native cache keys are not E6 keys"
    );
}

/// Missing/empty/garbage transcripts yield partial-or-empty results —
/// never a panic and never a hard error from the extraction layer.
#[test]
fn missing_optional_files_return_partial_not_panic() {
    let claude = agent_for(AgentKind::ClaudeCode);
    for data in [&b""[..], &b"\x00\xffgarbage\nmore\n"[..]] {
        let prompts = claude
            .as_prompt_extractor()
            .unwrap()
            .extract_prompts(data, 0)
            .expect("fail-open");
        assert!(prompts.is_empty());
        let usage = claude
            .as_token_calculator()
            .unwrap()
            .calculate_token_usage(data, 0)
            .expect("fail-open");
        assert_eq!(usage.input_tokens, 0);
    }
    let summary = extract::extract_claude_code(b"not-json\n");
    assert!(summary.partial, "undecodable lines flag partial");
    assert!(
        summary
            .warnings
            .iter()
            .any(|w| w.contains("not valid JSON")),
        "warning explains the partial state"
    );

    // Offsets past the end are clamped, not panicking.
    let data = fixture("claude_code.jsonl");
    let prompts = claude
        .as_prompt_extractor()
        .unwrap()
        .extract_prompts(&data, data.len() + 100)
        .unwrap();
    assert!(prompts.is_empty());
}

/// E7: curated skill registries project slash-command invocations for
/// claude-code and codex (opencode shares the single-entry registry).
#[test]
fn skill_events_project_for_claude_and_codex() {
    assert_eq!(
        CLAUDE_CODE_SKILL_REGISTRY,
        ["/review", "/security-review", "/simplify"]
    );
    assert_eq!(CODEX_SKILL_REGISTRY, ["/review"]);
    assert_eq!(OPENCODE_SKILL_REGISTRY, ["/review"]);

    let claude = agent_for(AgentKind::ClaudeCode);
    let events = claude
        .as_skill_event_extractor()
        .expect("claude skills")
        .extract_skill_events(&fixture("claude_code.jsonl"), 0)
        .unwrap();
    assert_eq!(events.len(), 1);
    assert_eq!(events[0].skill.name, "/review");
    assert_eq!(events[0].source.agent, "claude-code");
    let wire = serde_json::to_value(&events[0]).unwrap();
    assert_eq!(wire["event_type"], "prompt_invocation");
    assert_eq!(wire["source"]["signal"], "input_slash_command");

    let codex = agent_for(AgentKind::Codex);
    let events = codex
        .as_skill_event_extractor()
        .expect("codex skills")
        .extract_skill_events(&fixture("codex.jsonl"), 0)
        .unwrap();
    assert_eq!(events.len(), 1);
    assert_eq!(events[0].skill.name, "/review");
    assert_eq!(events[0].source.agent, "codex");
}

/// A0-07: the searchable [`SkillEventProjection`] ingests extracted events
/// from all three fixtures and answers queries by skill name, provider, and
/// session, dedupes by `(session, id)`, and stays empty (never panics) on a
/// garbage transcript. Its wire schema version is frozen.
#[test]
fn skill_event_projection() {
    use libra::internal::ai::observed_agents::{
        SKILL_PROJECTION_SCHEMA_VERSION, SkillEventProjection, SkillQuery,
    };

    assert_eq!(
        SKILL_PROJECTION_SCHEMA_VERSION, 1,
        "skill projection schema version is additive-only"
    );

    let mut proj = SkillEventProjection::new();
    for (kind, file, provider, session) in [
        (
            AgentKind::ClaudeCode,
            "claude_code.jsonl",
            "claude-code",
            "sess-claude",
        ),
        (AgentKind::Codex, "codex.jsonl", "codex", "sess-codex"),
        (
            AgentKind::OpenCode,
            "opencode.json",
            "opencode",
            "sess-opencode",
        ),
    ] {
        let events = agent_for(kind)
            .as_skill_event_extractor()
            .expect("skill extractor")
            .extract_skill_events(&fixture(file), 0)
            .unwrap();
        assert_eq!(events.len(), 1, "{file}: one /review event");
        proj.ingest(session, Some("cp-1"), provider, events);
    }
    assert_eq!(proj.len(), 3);

    // Search by skill name returns all three /review events across providers.
    assert_eq!(
        proj.search(&SkillQuery {
            skill: Some("/review".to_string()),
            ..Default::default()
        })
        .len(),
        3
    );
    // Filter by provider / session each narrows to one.
    assert_eq!(
        proj.search(&SkillQuery {
            provider: Some("codex".to_string()),
            ..Default::default()
        })
        .len(),
        1
    );
    assert_eq!(
        proj.search(&SkillQuery {
            session: Some("sess-claude".to_string()),
            ..Default::default()
        })
        .len(),
        1
    );
    // An unknown skill name matches nothing.
    assert!(
        proj.search(&SkillQuery {
            skill: Some("/nope".to_string()),
            ..Default::default()
        })
        .is_empty()
    );

    // Empty/garbage transcript → zero events, never a panic.
    let empty = agent_for(AgentKind::Codex)
        .as_skill_event_extractor()
        .unwrap()
        .extract_skill_events(b"not json at all\n", 0)
        .unwrap();
    assert!(empty.is_empty());
    let mut empty_proj = SkillEventProjection::new();
    assert_eq!(empty_proj.ingest("s", None, "codex", empty), 0);
    assert!(empty_proj.is_empty());

    // Duplicate: re-ingesting one session's events is deduped by (session, id).
    let claude = agent_for(AgentKind::ClaudeCode)
        .as_skill_event_extractor()
        .unwrap()
        .extract_skill_events(&fixture("claude_code.jsonl"), 0)
        .unwrap();
    let mut dup = SkillEventProjection::new();
    assert_eq!(
        dup.ingest("s1", Some("cp"), "claude-code", claude.clone()),
        1
    );
    assert_eq!(
        dup.ingest("s1", Some("cp"), "claude-code", claude),
        0,
        "the same session's identical event is deduped by (session, id)"
    );
    assert_eq!(dup.len(), 1);
}

/// E6 generic path (codex/opencode): a wire `subagent_tokens` value is
/// folded into `subagent_usage` and an explicit `api_call_count` from the
/// wire is honoured (not just +1 per usage object). Codex review P1
/// (2026-07-05): the generic path must not drop these two frozen keys.
#[test]
fn generic_e6_path_carries_api_count_and_subagent_tokens() {
    let jsonl = concat!(
        r#"{"role":"user","content":"hi"}"#,
        "
",
        r#"{"model":"gpt-5.3-codex","usage":{"input_tokens":10,"output_tokens":4,"api_call_count":5,"subagent_tokens":30}}"#,
        "
",
    );
    let summary = extract::extract_codex(jsonl.as_bytes());
    assert_eq!(summary.api_call_count, 5, "wire api_call_count honoured");
    let subagent = summary.subagent_usage.expect("subagent tokens folded in");
    assert_eq!(subagent.input_tokens, 30);
    assert_eq!(subagent.total_tokens, Some(30));
}

fn opencode_export(messages: serde_json::Value) -> serde_json::Value {
    serde_json::json!({"info":{"id":"session-synthetic","location":{"directory":"/project"}},"messages":messages})
}

fn opencode_assistant(id: &str, content: serde_json::Value) -> serde_json::Value {
    serde_json::json!({"id":id,"type":"assistant","model":{"id":"claude-sonnet-5","providerID":"synthetic"},"content":content})
}

fn opencode_tokens(input: u64) -> serde_json::Value {
    serde_json::json!({"input":input,"output":3,"reasoning":4,"cache":{"read":5,"write":6}})
}

fn opencode_summary(doc: &serde_json::Value) -> extract::ExtractionSummary {
    extract::extract_opencode(&serde_json::to_vec(doc).expect("synthetic export"))
}

#[test]
fn opencode_nested_export_prompts_model_extracted() {
    let doc = opencode_export(serde_json::json!([
        {"id":"u1","type":"user","text":"/review synthetic changes","model":{"id":"wrong-user-model"}},
        opencode_assistant("a1", serde_json::json!([])),
        {"id":"u2","type":"user","text":"Explain the result"}
    ]));
    let summary = opencode_summary(&doc);
    assert_eq!(
        summary.prompts,
        ["/review synthetic changes", "Explain the result"]
    );
    assert_eq!(summary.model.as_deref(), Some("claude-sonnet-5"));
    assert_eq!(summary.skill_events.len(), 1);
    assert!(!summary.partial, "{:?}", summary.warnings);
    let mut settled = doc.clone();
    for (index, kind) in [
        "idle",
        "model-switched",
        "synthetic",
        "system",
        "skill",
        "agent-switched",
        "shell",
        "compaction",
    ]
    .iter()
    .enumerate()
    {
        settled["messages"]
            .as_array_mut()
            .unwrap()
            .push(serde_json::json!({
                "id":format!("metadata-{index}"),"type":kind,"text":"not a human prompt"
            }));
    }
    let result = opencode_summary(&settled);
    assert!(!result.partial, "{:?}", result.warnings);
    assert_eq!(result.prompts, summary.prompts);
    let mut missing_location = doc.clone();
    missing_location["info"]
        .as_object_mut()
        .unwrap()
        .remove("location");
    assert!(opencode_summary(&missing_location).partial);
    let mut missing_info = doc.clone();
    missing_info.as_object_mut().unwrap().remove("info");
    assert!(opencode_summary(&missing_info).partial);
    let flat = serde_json::json!({"messages":[{"role":"user","content":"hello","model":"wrong"},{"role":"assistant","model":"right"}]});
    assert_eq!(opencode_summary(&flat).model.as_deref(), Some("right"));
    let legacy = serde_json::json!({"info":{"id":"s","directory":"/project"},"messages":[
        {"info":{"id":"u","role":"user","model":"wrong"},"parts":[{"type":"text","text":"first"},{"type":"text","text":"hidden","synthetic":true},{"type":"text","text":"second"}]},
        {"info":{"id":"a","role":"assistant","modelID":"legacy"},"parts":[]}
    ]});
    let summary = opencode_summary(&legacy);
    assert_eq!(summary.prompts, ["first\nsecond"]);
    assert_eq!(summary.model.as_deref(), Some("legacy"));
}

#[test]
fn opencode_usage_reasoning_tokens_aggregation_first() {
    let mut assistant = opencode_assistant("a", serde_json::json!([]));
    assistant["tokens"] = opencode_tokens(2);
    let mut doc = opencode_export(serde_json::json!([assistant.clone(), assistant]));
    doc["info"]["tokens"] = opencode_tokens(10000);
    let summary = opencode_summary(&doc);
    let usage = summary.usage.unwrap();
    assert_eq!((usage.input_tokens, usage.output_tokens), (2, 3));
    assert_eq!(usage.reasoning_tokens, Some(4));
    assert_eq!(usage.cached_tokens, Some(11));
    assert_eq!(usage.total_tokens, Some(20));
    assert_eq!(summary.api_call_count, 1);
    assert!(!summary.partial);
    let part = serde_json::json!({"id":"p","type":"step-finish","tokens":opencode_tokens(2)});
    let legacy = serde_json::json!({"info":{"id":"s"},"messages":[
        {"info":{"id":"a","role":"assistant","tokens":opencode_tokens(2)},"parts":[part.clone()]},
        {"info":{"id":"b","role":"assistant"},"parts":[part.clone(),part]}
    ]});
    let summary = opencode_summary(&legacy);
    assert_eq!(summary.usage.unwrap().total_tokens, Some(40));
    assert_eq!(summary.api_call_count, 2);
    for invalid in [
        serde_json::json!(-1),
        serde_json::json!(1.5),
        serde_json::json!(u64::MAX),
        serde_json::json!(9007199254740994.0),
    ] {
        let mut doc = doc.clone();
        doc["messages"][0]["tokens"]["input"] = invalid;
        doc["messages"].as_array_mut().unwrap().truncate(1);
        let result = opencode_summary(&doc);
        assert!(result.partial);
        assert!(result.usage.is_none());
    }
    let mut decimal = doc.clone();
    decimal["messages"][0]["tokens"]["input"] = serde_json::json!(2.0);
    assert_eq!(opencode_summary(&decimal).usage.unwrap().input_tokens, 2);
}

#[test]
fn opencode_modified_files_tool_whitelist_and_patch() {
    let mut assistant = opencode_assistant(
        "a",
        serde_json::json!([
            {"id":"t1","type":"tool","name":"edit","state":{"status":"completed","input":{"path":"src/lib.rs"},"metadata":{"files":[{"file":"src/lib.rs"}]}}},
            {"id":"t2","type":"tool","name":"read","state":{"status":"completed","input":{"path":"secret.txt"}}},
            {"id":"t3","type":"tool","name":"patch","state":{"status":"completed","input":{"patchText":"*** Begin Patch\n*** Environment ID: synthetic\n*** Update File: src/old.rs\n*** Move to: src/new.rs\n@@\n-old\n+new\n*** End of File\n*** End Patch"},"metadata":{"files":[{"file":"src/new.rs"}]}}}
        ]),
    );
    assistant["snapshot"] = serde_json::json!({"files":["src/lib.rs","docs/readme.md"]});
    let doc = opencode_export(serde_json::json!([assistant]));
    let summary = opencode_summary(&doc);
    assert_eq!(
        summary.modified_files,
        ["src/lib.rs", "src/new.rs", "src/old.rs", "docs/readme.md"]
    );
    assert!(!summary.partial, "{:?}", summary.warnings);
    let mut outside = doc.clone();
    outside["messages"][0]["content"][0]["state"]["input"]["path"] =
        serde_json::json!("/outside/private-canary");
    let result = opencode_summary(&outside);
    assert!(result.partial);
    assert!(!format!("{result:?}").contains("private-canary"));
    let mut moved = doc.clone();
    moved["info"]["location"]["directory"] = serde_json::json!("/new/project/sub");
    moved["info"]["subpath"] = serde_json::json!("sub");
    moved["messages"].as_array_mut().unwrap().insert(1, serde_json::json!({"id":"switch","type":"location-switched","location":{"directory":"/new/project/sub"},"subpath":"sub","previous":{"location":{"directory":"/old/project/sub"},"subpath":"sub"}}));
    let result = opencode_summary(&moved);
    assert!(result.modified_files.contains(&"sub/src/lib.rs".into()));
    assert!(result.modified_files.contains(&"docs/readme.md".into()));
}

#[test]
fn opencode_completed_file_tool_without_verified_path_is_partial() {
    for tool in ["edit", "write"] {
        for state in [
            serde_json::json!({"status":"completed","input":{}}),
            serde_json::json!({"status":"completed","input":{},"metadata":{"files":[{}]}}),
            serde_json::json!({"status":"error","input":{"path":"src/lib.rs"}}),
        ] {
            let doc = opencode_export(serde_json::json!([opencode_assistant(
                "a",
                serde_json::json!([{"type":"tool","id":"t","name":tool,"state":state}])
            )]));
            let result = opencode_summary(&doc);
            assert!(result.partial, "{tool} {doc}");
            assert!(result.modified_files.is_empty());
        }
    }
    let doc = opencode_export(serde_json::json!([opencode_assistant(
        "a",
        serde_json::json!([{"type":"tool","id":"t","name":"write","state":{"status":"completed","input":{},"metadata":{"files":[{"file":"src/lib.rs"}]}}}])
    )]));
    let result = opencode_summary(&doc);
    assert_eq!(result.modified_files, ["src/lib.rs"]);
    assert!(!result.partial);
    let sanitized = opencode_export(serde_json::json!([opencode_assistant(
        "a",
        serde_json::json!([{"type":"tool","id":"t","name":"write","state":{"status":"completed","input":{"redacted":"tool-input:t"},"metadata":{"redacted":"tool-metadata:t"}}}])
    )]));
    assert!(opencode_summary(&sanitized).partial);
    assert!(opencode_summary(&sanitized).modified_files.is_empty());
}

#[test]
fn opencode_aggregate_overflow_missing_components_and_mixed_shapes_are_partial() {
    let mut first = opencode_assistant("a", serde_json::json!([]));
    first["tokens"] = opencode_tokens(2);
    let mut second = opencode_assistant("b", serde_json::json!([]));
    second["tokens"] = opencode_tokens(u64::MAX - 18);
    let result = opencode_summary(&opencode_export(serde_json::json!([first.clone(), second])));
    assert!(result.partial);
    assert_eq!(result.usage.unwrap().total_tokens, Some(20));
    assert_eq!(result.api_call_count, 1);
    first["tokens"]["cache"]
        .as_object_mut()
        .unwrap()
        .remove("write");
    let result = opencode_summary(&opencode_export(serde_json::json!([first])));
    assert!(result.partial);
    assert!(result.usage.is_none());
    let result = opencode_summary(&opencode_export(serde_json::json!([
        {"id":"u","type":"user","text":"native"},
        {"info":{"id":"u2","role":"user"},"parts":[{"type":"text","text":"classic"}]},
        {"role":"user","content":"ambiguous"}
    ])));
    assert!(result.partial);
    assert_eq!(result.prompts, ["native", "classic"]);
}

#[test]
fn opencode_legacy_apply_patch_and_location_aliases_are_project_relative() {
    let patch =
        "*** Begin Patch\n*** Update File: a\n*** Move to: b\n@@\n-old\n+new\n*** End Patch";
    let legacy = serde_json::json!({"info":{"id":"s","directory":"/project"},"messages":[
        {"info":{"id":"a","role":"assistant","modelID":"legacy"},"parts":[
            {"id":"t","type":"tool","tool":"apply_patch","state":{"status":"completed","input":{"patchText":patch}}},
            {"id":"p","type":"patch","files":["c"]}
        ]}
    ]});
    let result = opencode_summary(&legacy);
    assert_eq!(result.modified_files, ["a", "b", "c"]);
    assert!(!result.partial);
    let switched = serde_json::json!({"info":{"id":"s","location":{"directory":"/repo2/sub"},"subpath":"sub"},"messages":[
        {"id":"switch","type":"location-switched","location":{"directory":"/repo2/sub"},"subpath":"sub","previous":{"location":{"directory":"/repo/sub"},"subpath":"sub"}},
        opencode_assistant("a",serde_json::json!([
            {"type":"tool","id":"t","name":"write","state":{"status":"completed","input":{"path":"/repo2/sub/a"},"metadata":{"files":[{"file":"/repo/sub/a"}]}}}
        ]))
    ]});
    let result = opencode_summary(&switched);
    assert_eq!(result.modified_files, ["sub/a"]);
    assert!(!result.partial);
    let mut missing = switched.clone();
    missing["messages"][0]
        .as_object_mut()
        .unwrap()
        .remove("previous");
    assert!(opencode_summary(&missing).partial);
}

fn hermes_bounded_export(repo: &std::path::Path, thinking: &str, finish: &str) -> Vec<u8> {
    serde_json::to_vec(&serde_json::json!({
        "id": "0123456789abcdef0123456789abcdef",
        "source": "cli",
        "cwd": repo,
        "started_at": 1_700_000_000,
        "ended_at": 1_700_000_100,
        "input_tokens": 5,
        "messages": [
            {"id": 2, "role": "assistant", "content": "answer", "active": 1, "compacted": 0,
             "timestamp": 10, "reasoning": thinking, "reasoning_content": "",
             "reasoning_details": "{\"raw\":1}", "finish_reason": finish, "token_count": 99},
            {"id": 1, "role": "user", "content": "prompt", "active": 1, "compacted": 0,
             "timestamp": 99, "reasoning": null}
        ]
    }))
    .expect("encode hermes export")
}

#[test]
fn hermes_bounded_capture_keeps_thinking_unproven_and_out_of_the_answer() {
    use libra::internal::ai::{
        capture::snapshot::{
            CaptureSnapshotCompleteness, CaptureSnapshotPartialReason,
            capture_authorized_hermes_bytes,
        },
        observed_agents::{
            HermesScope, SemanticRecord,
            builtin::hermes::{
                hermes_readable_tag, hermes_worker_frame, hermes_worker_frame_authorized,
            },
            reasoning::ReasoningProvider,
        },
    };

    let repo = std::env::temp_dir().join("hermes-bounded-scope");
    let bytes = hermes_bounded_export(&repo, "kept-thinking", "stop");
    let scope = HermesScope {
        session_id: "0123456789abcdef0123456789abcdef".to_string(),
        profile: "default".to_string(),
        repo_root: repo,
    };
    let scoped = hermes_worker_frame(&bytes, &scope, None).expect("scoped frame");
    let authorized = hermes_worker_frame_authorized(&bytes, None).expect("authorized frame");
    assert_eq!(scoped.tag, hermes_readable_tag());
    assert_eq!(scoped.tag, "hermes");
    assert_eq!(authorized.thinking, scoped.thinking);
    assert_eq!(authorized.source_state, "unproven");
    assert!(authorized.partial);
    assert!(!authorized.acquisition_gate_passed);
    assert!(!scoped.acquisition_gate_passed);
    assert_eq!(
        authorized.thinking[0].readable_thinking(),
        Some("kept-thinking")
    );
    assert_eq!(
        authorized.thinking[0].reasoning_details.as_deref(),
        Some("{\"raw\":1}")
    );
    let answer = authorized.ordinary[0]
        .records
        .iter()
        .find_map(|record| match record {
            SemanticRecord::Assistant { text } => Some(text.as_str()),
            _ => None,
        })
        .expect("assistant");
    assert_eq!(answer, "answer");
    assert_ne!(answer, "kept-thinking");
    assert_eq!(
        serde_json::to_value(ReasoningProvider::ClaudeCode).unwrap(),
        serde_json::json!("claude_code")
    );
    assert_eq!(
        serde_json::to_value(ReasoningProvider::Codex).unwrap(),
        serde_json::json!("codex")
    );
    assert_eq!(
        serde_json::to_value(ReasoningProvider::OpenCode).unwrap(),
        serde_json::json!("opencode")
    );
    assert_eq!(
        serde_json::to_value(ReasoningProvider::Hermes).unwrap(),
        serde_json::json!("hermes")
    );
    assert!(
        libra::internal::ai::observed_agents::reasoning::reasoning_decryptor(
            ReasoningProvider::Hermes
        )
        .is_none()
    );

    let snapshot = capture_authorized_hermes_bytes(bytes.clone(), "hermes-session");
    let projection = snapshot.safe_projection();
    assert_eq!(
        projection.completeness,
        CaptureSnapshotCompleteness::Partial
    );
    assert_eq!(
        projection.partial_reason,
        Some(CaptureSnapshotPartialReason::UnprovenOrdinary)
    );
    assert_eq!(
        snapshot.hermes_readable_thinking(),
        vec!["kept-thinking".to_string()]
    );

    let prompts = authorized.ordinary_prompts();
    assert_eq!(prompts, vec!["prompt".to_string()]);
    assert!(
        !prompts
            .iter()
            .any(|prompt| prompt.contains("kept-thinking"))
    );
}

#[test]
fn hermes_bounded_capture_rejects_a_bad_cancelled_or_oversize_frame() {
    use std::time::{Duration, Instant};

    use libra::internal::ai::{
        capture::snapshot::{
            CaptureSnapshotCompleteness, CaptureSnapshotPartialReason,
            capture_authorized_hermes_bytes,
        },
        observed_agents::builtin::hermes::{
            HERMES_WORKER_FRAME_MAX_BYTES, HermesWorkerError, hermes_worker_frame_authorized,
        },
    };

    let bad = hermes_worker_frame_authorized(b"not-json", None);
    assert_eq!(bad, Err(HermesWorkerError::BadFrame));
    let gateway = serde_json::to_vec(&serde_json::json!({
        "id": "0123456789abcdef0123456789abcdef",
        "source": "matrix",
        "cwd": "/tmp/hermes-bounded",
        "messages": [{"id": 1, "role": "user", "content": "nope", "active": 1, "compacted": 0, "timestamp": 1}]
    }))
    .expect("gateway");
    assert_eq!(
        hermes_worker_frame_authorized(&gateway, None),
        Err(HermesWorkerError::BadFrame)
    );
    let past = Instant::now()
        .checked_sub(Duration::from_secs(1))
        .expect("past");
    let repo = std::env::temp_dir().join("hermes-bounded-scope");
    let bytes = hermes_bounded_export(&repo, "kept-thinking", "stop");
    assert_eq!(
        hermes_worker_frame_authorized(&bytes, Some(past)),
        Err(HermesWorkerError::Cancelled)
    );

    let huge = "t".repeat((HERMES_WORKER_FRAME_MAX_BYTES as usize).saturating_add(1));
    let oversized = hermes_bounded_export(&repo, &huge, "stop");
    assert_eq!(
        hermes_worker_frame_authorized(&oversized, None),
        Err(HermesWorkerError::Budget {
            limit: HERMES_WORKER_FRAME_MAX_BYTES
        })
    );
    let snapshot = capture_authorized_hermes_bytes(oversized, "hermes-session");
    let projection = snapshot.safe_projection();
    assert_eq!(
        projection.completeness,
        CaptureSnapshotCompleteness::Partial
    );
    assert_eq!(
        projection.partial_reason,
        Some(CaptureSnapshotPartialReason::SourceOversize)
    );
    assert!(snapshot.hermes_readable_thinking().is_empty());
}

// ---- PI-34: Pi coverage normalizer ----

/// A synthetic Pi v3 session file from entry lines. Entry shapes follow the
/// pinned source (`session-manager.ts:43-194`, Pi `v1.4.0` `4a08025`); no
/// real session is read.
fn pi_session(lines: &[String]) -> Vec<u8> {
    let header = serde_json::json!({
        "type": "session",
        "version": 3,
        "id": "019a3c2e-7b10-7000-8000-0000000000aa",
        "timestamp": "2026-10-31T00:00:00.000Z",
        "cwd": "/work",
    });
    let mut file = header.to_string();
    for line in lines {
        file.push('\n');
        file.push_str(line);
    }
    file.push('\n');
    file.into_bytes()
}

/// An entry of `kind` with `id` and `parent`, plus `extra` fields.
fn pi_entry(kind: &str, id: &str, parent: Option<&str>, extra: serde_json::Value) -> String {
    let mut entry = serde_json::json!({
        "type": kind,
        "id": id,
        "parentId": parent,
        "timestamp": "2026-10-31T00:00:01.000Z",
    });
    if let (Some(entry), serde_json::Value::Object(extra)) = (entry.as_object_mut(), extra) {
        entry.extend(extra);
    }
    entry.to_string()
}

fn pi_message(id: &str, parent: Option<&str>, message: serde_json::Value) -> String {
    pi_entry(
        "message",
        id,
        parent,
        serde_json::json!({ "message": message }),
    )
}

/// Normalize, project, and cover one session.
fn pi_coverage(
    bytes: &[u8],
) -> (
    libra::internal::ai::observed_agents::builtin::pi::PiActiveBranch,
    Vec<libra::internal::ai::observed_agents::coverage::NormalizedTurn>,
) {
    use libra::internal::ai::observed_agents::builtin::pi::{
        normalize_pi_session, normalize_pi_turns,
    };

    let branch = normalize_pi_session(bytes, Default::default()).expect("normalize");
    let turns = normalize_pi_turns(bytes, Default::default()).expect("cover");
    (branch, turns)
}

fn pi_text_records(
    turn: &libra::internal::ai::observed_agents::coverage::NormalizedTurn,
) -> Vec<String> {
    turn.records
        .iter()
        .map(|record| match record {
            SemanticRecord::User { text } => format!("user: {text}"),
            SemanticRecord::Assistant { text } => format!("assistant: {text}"),
            SemanticRecord::ToolCall {
                call_id,
                input,
                name,
            } => format!(
                "call {call_id:?} {name}: {}",
                serde_json::to_string(input).expect("input")
            ),
            SemanticRecord::ToolResult {
                call_id,
                content,
                is_error,
            } => format!("result {call_id:?} {is_error}: {content}"),
            SemanticRecord::Reasoning {
                provider,
                source_kind,
                text,
            } => format!("reasoning {provider:?} {source_kind:?}: {}", text.as_str()),
        })
        .collect()
}

/// PI-34 AC1: one turn per PI-03 turn, with PI-03's ordinal and
/// completeness and the PI-11 records of the turn; context entries before
/// and after a compaction have no record; mismatched inputs fail.
#[test]
fn pi_coverage_normalizer_keeps_pi03_turns_and_pi11_records() {
    use libra::internal::ai::observed_agents::builtin::pi::{
        PiCoverageError, PiNormalizeLimits, PiNormalizeRefusal, normalize_pi_turns,
    };

    let bytes = pi_session(&[
        pi_message(
            "u1",
            None,
            serde_json::json!({"role": "user", "content": [
                {"type": "text", "text": "first"},
                {"type": "text", "text": "part"},
            ]}),
        ),
        pi_message(
            "a1",
            Some("u1"),
            serde_json::json!({"role": "assistant", "content": [
                {"type": "text", "text": "reading"},
                {"type": "toolCall", "id": "call-1", "name": "read", "arguments": {"path": "a.rs", "n": 2}},
                {"type": "text", "text": "now"},
            ], "usage": {"input": 10, "cost": {"total": 0.0123}}}),
        ),
        pi_message(
            "t1",
            Some("a1"),
            serde_json::json!({"role": "toolResult", "toolCallId": "call-1", "toolName": "read",
                "content": [{"type": "text", "text": "fn main"}, {"type": "text", "text": "}"}],
                "isError": false}),
        ),
        pi_message(
            "a2",
            Some("t1"),
            serde_json::json!({"role": "assistant", "content": [{"type": "text", "text": "done"}]}),
        ),
        pi_message(
            "u2",
            Some("a2"),
            serde_json::json!({"role": "user", "content": "second"}),
        ),
        pi_entry(
            "custom_message",
            "x1",
            Some("u2"),
            serde_json::json!({"customType": "note", "content": "extension note", "display": true}),
        ),
        pi_message(
            "a3",
            Some("x1"),
            serde_json::json!({"role": "assistant", "content": [{"type": "text", "text": "second answer"}]}),
        ),
        pi_entry(
            "compaction",
            "c1",
            Some("a3"),
            serde_json::json!({"summary": "compaction summary", "firstKeptEntryId": "u2", "tokensBefore": 10}),
        ),
        pi_message(
            "u3.x",
            Some("c1"),
            serde_json::json!({"role": "user", "content": "third"}),
        ),
        pi_message(
            "b1",
            Some("u3.x"),
            serde_json::json!({"role": "bashExecution", "command": "ls -la", "output": "listing",
                "exitCode": 0, "cancelled": false, "truncated": false}),
        ),
        pi_message(
            "s1",
            Some("b1"),
            serde_json::json!({"role": "system", "content": "system prompt"}),
        ),
        pi_entry(
            "branch_summary",
            "bs1",
            Some("s1"),
            serde_json::json!({"fromId": "a3", "summary": "branch summary"}),
        ),
        pi_message(
            "a4",
            Some("bs1"),
            serde_json::json!({"role": "assistant", "content": [{"type": "text", "text": "third answer"}]}),
        ),
    ]);
    let (branch, turns) = pi_coverage(&bytes);
    assert_eq!(turns.len(), branch.turns.len());
    assert_eq!(
        turns.iter().map(|turn| turn.ordinal).collect::<Vec<_>>(),
        branch
            .turns
            .iter()
            .map(|turn| turn.ordinal)
            .collect::<Vec<_>>()
    );
    assert_eq!(
        turns
            .iter()
            .map(|turn| turn.completeness)
            .collect::<Vec<_>>(),
        branch
            .turns
            .iter()
            .map(|turn| turn.completeness)
            .collect::<Vec<_>>()
    );
    assert_eq!(
        turns
            .iter()
            .map(|turn| turn.completeness)
            .collect::<Vec<_>>(),
        vec![
            Completeness::Incomplete,
            Completeness::Complete,
            Completeness::Complete
        ]
    );
    assert_eq!(
        turns
            .iter()
            .map(|turn| turn.logical_turn_key.as_str())
            .collect::<Vec<_>>(),
        vec!["u1", "u2", "ordinal:2"]
    );
    assert_eq!(
        turns.iter().map(pi_text_records).collect::<Vec<_>>(),
        vec![
            vec![
                "user: first\npart".to_string(),
                "assistant: reading\nnow".to_string(),
                r#"call Some("call-1") read: {"n":2,"path":"a.rs"}"#.to_string(),
                "result Some(\"call-1\") false: fn main\n}".to_string(),
                "assistant: done".to_string(),
            ],
            vec![
                "user: second".to_string(),
                "assistant: second answer".to_string(),
            ],
            vec![
                "user: third".to_string(),
                "assistant: third answer".to_string(),
            ],
        ]
    );
    let shown = format!("{:?}", pi_text_records_all(&turns));
    for context in [
        "extension note",
        "compaction summary",
        "ls -la",
        "listing",
        "system prompt",
        "branch summary",
    ] {
        assert!(!shown.contains(context), "{context}");
    }

    // The branch comes from the bytes themselves: a file the normalizer
    // refuses gives that refusal.
    assert_eq!(
        normalize_pi_turns(
            b"{\"type\":\"message\",\"id\":\"u1\"}\n",
            Default::default()
        ),
        Err(PiCoverageError::Refused(PiNormalizeRefusal::NotASession))
    );
    assert_eq!(
        normalize_pi_turns(
            &bytes,
            PiNormalizeLimits {
                max_bytes: 16,
                ..Default::default()
            }
        ),
        Err(PiCoverageError::Refused(PiNormalizeRefusal::TooLarge))
    );
}

fn pi_text_records_all(
    turns: &[libra::internal::ai::observed_agents::coverage::NormalizedTurn],
) -> Vec<String> {
    turns.iter().flat_map(pi_text_records).collect()
}

/// PI-34 AC2: readable thinking becomes a Pi reasoning record and never
/// joins the answer, with thinking and text interleaved; redacted and
/// empty thinking have no record.
#[test]
fn pi_coverage_normalizer_records_readable_thinking_as_pi_reasoning() {
    use libra::internal::ai::observed_agents::{
        coverage::{CoverageSchemaVersion, select_coverage_schema},
        reasoning::{ReasoningProvider, ReasoningSourceKind},
    };

    let bytes = pi_session(&[
        pi_message(
            "u1",
            None,
            serde_json::json!({"role": "user", "content": "question"}),
        ),
        pi_message(
            "a1",
            Some("u1"),
            serde_json::json!({"role": "assistant", "content": [
                {"type": "thinking", "thinking": "weigh the options", "thinkingSignature": "sig-1"},
                {"type": "text", "text": "first"},
                {"type": "thinking", "thinking": "", "thinkingSignature": "opaque-payload", "redacted": true},
                {"type": "text", "text": "second"},
                {"type": "thinking", "thinking": ""},
                {"type": "toolCall", "id": "call-1", "name": "ls", "arguments": {}},
                {"type": "thinking", "thinking": "after the call", "redacted": false},
                {"type": "thinking", "thinking": "withheld reasoning", "thinkingSignature": "sig-2", "redacted": true},
            ]}),
        ),
    ]);
    let (_, turns) = pi_coverage(&bytes);
    assert_eq!(turns.len(), 1);
    assert_eq!(turns[0].completeness, Completeness::Complete);
    assert_eq!(
        pi_text_records(&turns[0]),
        vec![
            "user: question".to_string(),
            "assistant: first\nsecond".to_string(),
            "reasoning Pi Some(Thinking): weigh the options".to_string(),
            r#"call Some("call-1") ls: {}"#.to_string(),
            "reasoning Pi Some(Thinking): after the call".to_string(),
        ]
    );
    for record in &turns[0].records {
        match record {
            SemanticRecord::Reasoning {
                provider,
                source_kind,
                ..
            } => {
                assert_eq!(*provider, ReasoningProvider::Pi);
                assert_eq!(*source_kind, Some(ReasoningSourceKind::Thinking));
            }
            SemanticRecord::Assistant { text } => {
                assert!(!text.contains("weigh") && !text.contains("after the call"));
            }
            _ => {}
        }
    }
    // Redacted thinking, even with text, never reaches a record.
    let shown = format!("{:?}", pi_text_records(&turns[0]));
    assert!(!shown.contains("opaque-payload") && !shown.contains("withheld"));
    assert_eq!(select_coverage_schema(&turns[0]), CoverageSchemaVersion::V2);

    // An empty text block is a record of its own, unlike no text, and
    // thinking never fills it.
    let answer_of = |content: serde_json::Value| {
        let (_, turns) = pi_coverage(&pi_session(&[
            pi_message(
                "u1",
                None,
                serde_json::json!({"role": "user", "content": "question"}),
            ),
            pi_message(
                "a1",
                Some("u1"),
                serde_json::json!({"role": "assistant", "content": content}),
            ),
        ]));
        pi_text_records(&turns[0])
    };
    assert_eq!(
        answer_of(serde_json::json!([
            {"type": "thinking", "thinking": "only thinking"},
            {"type": "text", "text": ""},
        ])),
        vec![
            "user: question".to_string(),
            "assistant: ".to_string(),
            "reasoning Pi Some(Thinking): only thinking".to_string(),
        ]
    );
    assert_eq!(
        answer_of(serde_json::json!([{"type": "thinking", "thinking": "only thinking"}])),
        vec![
            "user: question".to_string(),
            "reasoning Pi Some(Thinking): only thinking".to_string(),
        ]
    );
}

/// PI-34 AC3: Pi stays unsupported.
#[test]
fn pi_coverage_normalizer_keeps_pi_unsupported() {
    use libra::internal::ai::observed_agents::{registration_for, supported_slugs};

    let row = registration_for(AgentKind::Pi);
    assert!(!row.supported);
    assert!(!row.hook_installable);
    assert!(!row.launchable_review && !row.launchable_investigate);
    assert!(!row.transcript_readable);
    assert!(!supported_slugs().contains(&"pi"));
}

/// PI-34 AC4: a turn whose messages were not read faithfully is
/// incomplete (coverage-v1 §4–§6); fractional numbers in tool arguments
/// become `null`; a float outside a semantic position does not count.
#[test]
fn pi_coverage_normalizer_marks_unfaithful_turns_incomplete() {
    use libra::internal::ai::observed_agents::coverage::parse_canon_value;

    let user = pi_message(
        "u1",
        None,
        serde_json::json!({"role": "user", "content": "question"}),
    );
    let call = |arguments: &str| {
        format!(
            r#"{{"type":"message","id":"a1","parentId":"u1","timestamp":"2026-10-31T00:00:01.000Z","message":{{"role":"assistant","content":[{{"type":"toolCall","id":"call-1","name":"run","arguments":{arguments}}}]}}}}"#
        )
    };
    let assistant = |content: serde_json::Value| {
        pi_message(
            "a1",
            Some("u1"),
            serde_json::json!({"role": "assistant", "content": content}),
        )
    };
    let result = |parent: &str, fields: serde_json::Value| {
        let mut message =
            serde_json::json!({"role": "toolResult", "content": [{"type": "text", "text": "out"}]});
        if let (Some(message), serde_json::Value::Object(fields)) =
            (message.as_object_mut(), fields)
        {
            message.extend(fields);
        }
        pi_message("t1", Some(parent), message)
    };
    let completeness = |lines: Vec<String>| {
        let (_, turns) = pi_coverage(&pi_session(&lines));
        assert_eq!(turns.len(), 1);
        turns[0].completeness
    };

    // Well-formed: complete, including a float in usage (provenance).
    assert_eq!(
        completeness(vec![
            user.clone(),
            call(r#"{"path":"a.rs","depth":2}"#),
            result(
                "a1",
                serde_json::json!({"toolCallId": "call-1", "toolName": "run", "isError": false,
                    "details": {"elapsed": 1.5}}),
            ),
        ]),
        Completeness::Complete
    );
    // Tool result metadata and an unmatched call id are not coverage
    // content: still complete.
    let faithful: Vec<(&str, Vec<String>)> = vec![
        (
            "orphan tool result",
            vec![
                user.clone(),
                result(
                    "u1",
                    serde_json::json!({"toolCallId": "call-x", "toolName": "run", "isError": false}),
                ),
            ],
        ),
        (
            "tool result without toolName",
            vec![
                user.clone(),
                result(
                    "u1",
                    serde_json::json!({"toolCallId": "call-x", "isError": true}),
                ),
            ],
        ),
        (
            "incomplete nestedCalls",
            vec![
                user.clone(),
                result(
                    "u1",
                    serde_json::json!({"toolCallId": "call-x", "toolName": "run", "isError": false,
                        "nestedCalls": {"calls": [{"name": "inner"}], "complete": false}}),
                ),
            ],
        ),
        (
            "nestedCalls over the bound",
            vec![
                user.clone(),
                result(
                    "u1",
                    serde_json::json!({"toolCallId": "call-x", "toolName": "run", "isError": false,
                        "nestedCalls": {"calls": vec![serde_json::json!({}); 300], "complete": true}}),
                ),
            ],
        ),
        (
            "null nestedCalls",
            vec![
                user.clone(),
                result(
                    "u1",
                    serde_json::json!({"toolCallId": "call-x", "toolName": "run", "isError": false,
                        "nestedCalls": null}),
                ),
            ],
        ),
    ];
    for (case, lines) in faithful {
        assert_eq!(completeness(lines), Completeness::Complete, "{case}");
    }

    let unfaithful: Vec<(&str, Vec<String>)> = vec![
        (
            "duplicate key in a message without records",
            vec![
                user.clone(),
                r#"{"type":"message","id":"a1","parentId":"u1","timestamp":"2026-10-31T00:00:01.000Z","message":{"role":"assistant","content":[]},"x":1,"x":2}"#
                    .to_string(),
            ],
        ),
        (
            "image block in a tool result",
            vec![
                user.clone(),
                result(
                    "u1",
                    serde_json::json!({"toolCallId": "call-x", "toolName": "read", "isError": false,
                        "content": [{"type": "text", "text": "out"},
                            {"type": "image", "data": "AAAA", "mimeType": "image/png"}]}),
                ),
            ],
        ),
        (
            "duplicate key in a message line",
            vec![
                r#"{"type":"message","id":"u1","parentId":null,"timestamp":"2026-10-31T00:00:01.000Z","message":{"role":"user","content":"q","content":"q2"}}"#
                    .to_string(),
            ],
        ),
        (
            "image block",
            vec![pi_message(
                "u1",
                None,
                serde_json::json!({"role": "user", "content": [
                    {"type": "text", "text": "look"},
                    {"type": "image", "data": "AAAA", "mimeType": "image/png"},
                ]}),
            )],
        ),
        (
            "unknown block",
            vec![
                user.clone(),
                assistant(serde_json::json!([{"type": "widget", "text": "w"}])),
            ],
        ),
        (
            "unknown entry type",
            vec![
                user.clone(),
                pi_entry("future_kind", "f1", Some("u1"), serde_json::json!({})),
            ],
        ),
        (
            "unknown message role",
            vec![
                user.clone(),
                pi_message(
                    "n1",
                    Some("u1"),
                    serde_json::json!({"role": "narrator", "content": "n"}),
                ),
            ],
        ),
        (
            "ill-typed thinking",
            vec![
                user.clone(),
                assistant(serde_json::json!([{"type": "thinking", "thinking": 7}])),
            ],
        ),
        (
            "ill-typed redacted",
            vec![
                user.clone(),
                assistant(
                    serde_json::json!([{"type": "thinking", "thinking": "t", "redacted": "yes"}]),
                ),
            ],
        ),
        (
            "tool call without id",
            vec![
                user.clone(),
                assistant(
                    serde_json::json!([{"type": "toolCall", "name": "run", "arguments": {}}]),
                ),
            ],
        ),
        (
            "tool call without name",
            vec![
                user.clone(),
                assistant(
                    serde_json::json!([{"type": "toolCall", "id": "call-1", "arguments": {}}]),
                ),
            ],
        ),
        (
            "arguments not an object",
            vec![user.clone(), call(r#""ls""#)],
        ),
        (
            "duplicate key in arguments",
            vec![user.clone(), call(r#"{"a":1,"a":2}"#)],
        ),
        (
            "fractional argument",
            vec![user.clone(), call(r#"{"t":1.5}"#)],
        ),
        (
            "beyond-i64 argument",
            vec![user.clone(), call(r#"{"t":18446744073709551615}"#)],
        ),
        (
            "tool result without isError",
            vec![
                user.clone(),
                result(
                    "u1",
                    serde_json::json!({"toolCallId": "call-1", "toolName": "run"}),
                ),
            ],
        ),
        (
            "ill-typed isError",
            vec![
                user.clone(),
                result(
                    "u1",
                    serde_json::json!({"toolCallId": "call-1", "toolName": "run", "isError": "no"}),
                ),
            ],
        ),
        (
            "tool result without toolCallId",
            vec![
                user.clone(),
                result(
                    "u1",
                    serde_json::json!({"toolName": "run", "isError": false}),
                ),
            ],
        ),
    ];
    for (case, lines) in unfaithful {
        assert_eq!(completeness(lines), Completeness::Incomplete, "{case}");
    }

    // A fractional or beyond-i64 argument never enters the record as a
    // number.
    for arguments in [
        r#"{"t":1.5,"k":[2.5,3]}"#,
        r#"{"t":18446744073709551615,"k":[2.5,3]}"#,
    ] {
        let (_, turns) = pi_coverage(&pi_session(&[user.clone(), call(arguments)]));
        let inputs: Vec<_> = turns[0]
            .records
            .iter()
            .filter_map(|record| match record {
                SemanticRecord::ToolCall { input, .. } => Some(input.clone()),
                _ => None,
            })
            .collect();
        assert_eq!(
            inputs,
            vec![parse_canon_value(br#"{"t":null,"k":[null,3]}"#).expect("canon")]
        );
    }

    // A line PI-03 skipped may have held turn content: the turn holding
    // the latest branch entry above it is incomplete, the others are not.
    let second = pi_message(
        "u2",
        Some("a1"),
        serde_json::json!({"role": "user", "content": "next"}),
    );
    let answer = assistant(serde_json::json!([{"type": "text", "text": "answer"}]));
    let per_turn = |lines: Vec<String>| {
        let (branch, turns) = pi_coverage(&pi_session(&lines));
        assert_eq!(
            branch
                .turns
                .iter()
                .map(|turn| turn.skipped_lines)
                .collect::<Vec<_>>(),
            turns
                .iter()
                .map(|turn| turn.completeness == Completeness::Incomplete)
                .collect::<Vec<_>>()
        );
        turns
            .iter()
            .map(|turn| turn.completeness)
            .collect::<Vec<_>>()
    };
    assert_eq!(
        per_turn(vec![user.clone(), answer.clone(), second.clone()]),
        vec![Completeness::Complete, Completeness::Complete]
    );
    // A truncated tail.
    assert_eq!(
        per_turn(vec![
            user.clone(),
            answer.clone(),
            second.clone(),
            r#"{"type":"message","id":"a2","parentId":"u2","message":{"role":"assi"#.to_string(),
        ]),
        vec![Completeness::Complete, Completeness::Incomplete]
    );
    // A corrupt line inside the first turn.
    assert_eq!(
        per_turn(vec![
            user.clone(),
            "{not json".to_string(),
            answer.clone(),
            second.clone(),
        ]),
        vec![Completeness::Incomplete, Completeness::Complete]
    );
    // A skipped line before any turn opens may have been its user message.
    assert_eq!(
        per_turn(vec![
            pi_entry(
                "model_change",
                "m1",
                None,
                serde_json::json!({"provider": "p", "modelId": "m"})
            ),
            "null".to_string(),
            pi_message(
                "u1",
                Some("m1"),
                serde_json::json!({"role": "user", "content": "q"})
            ),
            pi_message(
                "a1",
                Some("u1"),
                serde_json::json!({"role": "assistant", "content": [{"type": "text", "text": "a"}]})
            ),
            second.clone(),
        ]),
        vec![Completeness::Incomplete, Completeness::Complete]
    );
    // A child may precede its parent in the file: the skipped line below
    // `a2` (on disk before its parent `u2`) belongs to the second turn.
    assert_eq!(
        per_turn(vec![
            user.clone(),
            answer.clone(),
            pi_message(
                "a2",
                Some("u2"),
                serde_json::json!({"role": "assistant", "content": [{"type": "text", "text": "early"}]})
            ),
            "{not json".to_string(),
            second.clone(),
            pi_message(
                "a3",
                Some("a2"),
                serde_json::json!({"role": "assistant", "content": [{"type": "text", "text": "late"}]})
            ),
        ]),
        vec![Completeness::Complete, Completeness::Incomplete]
    );
    // No turn: nothing to mark.
    let (branch, turns) = pi_coverage(&pi_session(&[
        pi_entry(
            "model_change",
            "m1",
            None,
            serde_json::json!({"provider": "p", "modelId": "m"}),
        ),
        "{not json".to_string(),
    ]));
    assert!(branch.turns.is_empty() && turns.is_empty());
}

/// A v3 session whose user prompts interleave readable thinking, assistant
/// text, a tool call and its result, and context entries (PI-31 AC1). Every
/// non-user string carries `pi-private` so a leak is easy to find.
fn pi_interleaved_session() -> Vec<u8> {
    pi_session(&[
        pi_message(
            "u1",
            None,
            serde_json::json!({"role": "user", "content": [
                {"type": "text", "text": "first"},
                {"type": "text", "text": "part"},
            ]}),
        ),
        pi_message(
            "a1",
            Some("u1"),
            serde_json::json!({"role": "assistant", "content": [
                {"type": "thinking", "thinking": "pi-private thought"},
                {"type": "text", "text": "pi-private answer"},
                {"type": "toolCall", "id": "call-1", "name": "read",
                    "arguments": {"path": "pi-private.rs"}},
                {"type": "thinking", "thinking": "pi-private redacted", "redacted": true},
            ]}),
        ),
        pi_message(
            "t1",
            Some("a1"),
            serde_json::json!({"role": "toolResult", "toolCallId": "call-1", "toolName": "read",
                "content": [{"type": "text", "text": "pi-private output"}], "isError": false}),
        ),
        pi_entry(
            "custom_message",
            "x1",
            Some("t1"),
            serde_json::json!({"customType": "note", "content": "pi-private note", "display": true}),
        ),
        pi_message(
            "u2",
            Some("x1"),
            serde_json::json!({"role": "user", "content": "second"}),
        ),
        pi_message(
            "b1",
            Some("u2"),
            serde_json::json!({"role": "bashExecution", "command": "pi-private ls",
                "output": "pi-private listing", "exitCode": 0, "cancelled": false,
                "truncated": false}),
        ),
        pi_message(
            "s1",
            Some("b1"),
            serde_json::json!({"role": "system", "content": "pi-private system"}),
        ),
        pi_entry(
            "branch_summary",
            "bs1",
            Some("s1"),
            serde_json::json!({"fromId": "a1", "summary": "pi-private branch summary"}),
        ),
        pi_message(
            "a2",
            Some("bs1"),
            serde_json::json!({"role": "assistant", "content": [
                {"type": "text", "text": "pi-private second answer"},
                {"type": "thinking", "thinking": "pi-private second thought"},
            ]}),
        ),
        pi_message(
            "u3",
            Some("a2"),
            serde_json::json!({"role": "user", "content": [{"type": "text", "text": ""}]}),
        ),
        pi_message(
            "u4",
            Some("u3"),
            serde_json::json!({"role": "user", "content": "third"}),
        ),
    ])
}

/// Send one request through the public deadline-bound extraction helper
/// entry and return its metadata, or `None` for a failed frame. The request
/// header is version 1, the agent code, flags (bit 0: parent present, bit 1:
/// children omitted), and the parent length as u64 little endian.
fn pi_helper_metadata(
    code: u8,
    parent: Option<&[u8]>,
    children_omitted: bool,
) -> Option<serde_json::Value> {
    use sha2::{Digest, Sha256};

    let mut request = vec![
        1,
        code,
        u8::from(parent.is_some()) | (u8::from(children_omitted) << 1),
    ];
    let parent = parent.unwrap_or_default();
    request.extend_from_slice(&u64::try_from(parent.len()).expect("length").to_le_bytes());
    request.extend_from_slice(parent);
    let frame = libra::internal::ai::capture::snapshot::run_capture_extraction_helper(request);
    if frame.first() != Some(&0) {
        return None;
    }
    let length = u32::from_le_bytes(frame[1..5].try_into().expect("length field"));
    let payload = &frame[37..];
    assert_eq!(payload.len(), usize::try_from(length).expect("length"));
    assert_eq!(&frame[5..37], Sha256::digest(payload).as_slice());
    Some(serde_json::from_slice(payload).expect("metadata json"))
}

/// PI-31 AC1: the Pi bounded extraction branch keeps only user text as
/// prompts, in turn order, and bounds the collection.
#[test]
fn pi_bounded_extraction_prompts_hold_only_user_text() {
    use libra::internal::ai::capture::deadline_provider_extraction_summary;

    let summary = deadline_provider_extraction_summary(AgentKind::Pi, &pi_interleaved_session());
    assert_eq!(summary.prompts, ["first\npart", "second", "third"]);
    assert!(
        summary
            .prompts
            .iter()
            .chain(&summary.warnings)
            .all(|text| !text.contains("pi-private"))
    );
    assert!(summary.model.is_none() && summary.usage.is_none());
    assert!(summary.modified_files.is_empty() && summary.skill_events.is_empty());

    // The deadline collection bound (128 items, 4 KiB per string) still
    // applies: an oversized prompt ends the collection with a content-free
    // warning, and so does the 129th prompt.
    let mut lines = vec![pi_message(
        "u0",
        None,
        serde_json::json!({"role": "user", "content": "kept"}),
    )];
    lines.push(pi_message(
        "u1",
        Some("u0"),
        serde_json::json!({"role": "user", "content": "x".repeat(4097)}),
    ));
    lines.push(pi_message(
        "u2",
        Some("u1"),
        serde_json::json!({"role": "user", "content": "dropped"}),
    ));
    let summary = deadline_provider_extraction_summary(AgentKind::Pi, &pi_session(&lines));
    assert_eq!(summary.prompts, ["kept"]);
    assert_eq!(summary.warnings.len(), 1);
    assert!(!summary.warnings[0].contains("xxxx"));

    let mut lines = Vec::new();
    for index in 0..130 {
        let parent = (index > 0).then(|| format!("u{}", index - 1));
        lines.push(pi_message(
            &format!("u{index}"),
            parent.as_deref(),
            serde_json::json!({"role": "user", "content": format!("p{index}")}),
        ));
    }
    let summary = deadline_provider_extraction_summary(AgentKind::Pi, &pi_session(&lines));
    assert_eq!(summary.prompts.len(), 128);
    assert_eq!(summary.prompts.last().map(String::as_str), Some("p127"));
    assert_eq!(summary.warnings.len(), 1);

    // The 32 KiB retained bound: eight 4000-byte prompts fit (32000 bytes),
    // the ninth would pass 32768, so the collection stops there.
    let mut lines = Vec::new();
    for index in 0..10 {
        let parent = (index > 0).then(|| format!("u{}", index - 1));
        lines.push(pi_message(
            &format!("u{index}"),
            parent.as_deref(),
            serde_json::json!({"role": "user", "content": format!("{index}").repeat(4000)}),
        ));
    }
    let summary = deadline_provider_extraction_summary(AgentKind::Pi, &pi_session(&lines));
    assert_eq!(summary.prompts.len(), 8);
    assert_eq!(summary.prompts[7], "7".repeat(4000));
    assert_eq!(summary.warnings.len(), 1);
    assert!(!summary.warnings[0].contains("8888"));
}

/// PI-31: the helper accepts a complete redacted parent of up to 24 MiB, and
/// redaction placeholders can push a raw session under the 16 MiB raw cap
/// past it. The Pi branch reads such a session; past 24 MiB it refuses.
#[test]
fn pi_bounded_extraction_reads_a_redacted_session_past_the_raw_cap() {
    use libra::internal::ai::{
        capture::deadline_provider_extraction_summary, observed_agents::redaction::Redactor,
    };

    let session = |filler: String| {
        pi_session(&[
            pi_message(
                "u1",
                None,
                serde_json::json!({"role": "user", "content": "first"}),
            ),
            pi_message(
                "a1",
                Some("u1"),
                serde_json::json!({"role": "assistant", "content": [
                    {"type": "toolCall", "id": "call-1", "name": "read", "arguments": {}},
                ]}),
            ),
            pi_message(
                "t1",
                Some("a1"),
                serde_json::json!({"role": "toolResult", "toolCallId": "call-1",
                    "toolName": "read", "content": [{"type": "text", "text": filler}],
                    "isError": false}),
            ),
            pi_message(
                "u2",
                Some("t1"),
                serde_json::json!({"role": "user", "content": "second"}),
            ),
        ])
    };
    const MIB: usize = 1024 * 1024;
    let secret = "AKIAIOSFODNN7EXAMPLE ";
    let raw = session(secret.repeat(15 * MIB / secret.len()));
    assert!(raw.len() < 16 * MIB);
    let (redacted, _) = Redactor::new_default().redact(&raw);
    let redacted = redacted.into_inner();
    assert!(redacted.len() > 16 * MIB && redacted.len() <= 24 * MIB);
    assert!(!String::from_utf8_lossy(&redacted).contains("AKIAIOSFODNN7EXAMPLE"));
    let summary = deadline_provider_extraction_summary(AgentKind::Pi, &redacted);
    assert_eq!(summary.prompts, ["first", "second"]);
    assert!(summary.partial && summary.warnings.is_empty());
    let metadata = pi_helper_metadata(4, Some(&redacted), false).expect("pi frame");
    assert_eq!(metadata["prompt_count"], 2);

    let oversized = session("x".repeat(24 * MIB));
    assert!(oversized.len() > 24 * MIB);
    let summary = deadline_provider_extraction_summary(AgentKind::Pi, &oversized);
    assert!(summary.prompts.is_empty());
    assert_eq!(summary.warnings, ["Pi session could not be normalized"]);
    assert!(pi_helper_metadata(4, Some(&oversized), false).is_none());
}

/// PI-31 AC2: the Pi extraction is always partial, for a well-formed session
/// and for bytes the normalizer refuses; a refusal yields no prompts and a
/// content-free warning.
#[test]
fn pi_bounded_extraction_is_always_partial() {
    use libra::internal::ai::capture::deadline_provider_extraction_summary;

    let summary = deadline_provider_extraction_summary(AgentKind::Pi, &pi_interleaved_session());
    assert!(summary.partial);
    assert!(summary.warnings.is_empty());

    let v2 = String::from_utf8(pi_session(&[pi_message(
        "u1",
        None,
        serde_json::json!({"role": "user", "content": "pi-canary prompt"}),
    )]))
    .expect("utf8")
    .replacen("\"version\":3", "\"version\":2", 1);
    assert!(v2.contains("\"version\":2"));
    let duplicate_id = pi_session(&[
        pi_message(
            "u1",
            None,
            serde_json::json!({"role": "user", "content": "pi-canary one"}),
        ),
        pi_message(
            "u1",
            None,
            serde_json::json!({"role": "user", "content": "pi-canary two"}),
        ),
    ]);
    let refused: [&[u8]; 5] = [
        b"pi-canary-thought",
        b"",
        br#"{"type":"message","id":"u1","message":{"role":"user","content":"pi-canary"}}"#,
        v2.as_bytes(),
        &duplicate_id,
    ];
    for bytes in refused {
        let summary = deadline_provider_extraction_summary(AgentKind::Pi, bytes);
        assert!(summary.partial);
        assert!(summary.prompts.is_empty());
        assert_eq!(summary.warnings, ["Pi session could not be normalized"]);
    }
}

/// PI-31 AC3: the helper request code 4 decodes to Pi. The same Pi bytes
/// read as Claude Code (code 1) give a different result, and Hermes has no
/// code (5 is unassigned).
#[test]
fn pi_bounded_extraction_helper_decodes_the_pi_code() {
    let bytes = pi_interleaved_session();
    let pi = pi_helper_metadata(4, Some(&bytes), false).expect("pi frame");
    assert_eq!(pi["prompt_count"], 3);
    assert_eq!(pi["partial"], true);
    let claude = pi_helper_metadata(1, Some(&bytes), false).expect("claude frame");
    assert_eq!(claude["prompt_count"], 0);
    assert!(pi_helper_metadata(5, Some(&bytes), false).is_none());
    assert!(pi_helper_metadata(0, Some(&bytes), false).is_none());
}

/// PI-31 AC4: for the same bytes, the helper's metadata is the projection of
/// the direct branch's `ExtractionSummary` (prompt texts never cross the
/// helper frame, only their count).
#[test]
fn pi_bounded_extraction_helper_matches_the_direct_branch() {
    use libra::internal::ai::capture::deadline_provider_extraction_summary;

    let inputs: [&[u8]; 3] = [
        &pi_interleaved_session(),
        b"pi-canary-thought",
        &pi_session(&[]),
    ];
    for bytes in inputs {
        let direct = deadline_provider_extraction_summary(AgentKind::Pi, bytes);
        let metadata = pi_helper_metadata(4, Some(bytes), false).expect("pi frame");
        let keys: Vec<&str> = metadata
            .as_object()
            .expect("metadata object")
            .keys()
            .map(String::as_str)
            .collect();
        let mut sorted = keys.clone();
        sorted.sort_unstable();
        assert_eq!(
            sorted,
            [
                "api_call_count",
                "partial",
                "present",
                "prompt_count",
                "schema_version",
                "warnings"
            ]
        );
        assert!(direct.model.is_none() && direct.usage.is_none());
        assert!(direct.subagent_usage.is_none() && direct.modified_files.is_empty());
        assert!(direct.skill_events.is_empty());
        assert_eq!(metadata["present"], true);
        assert_eq!(metadata["partial"], direct.partial);
        assert_eq!(metadata["prompt_count"], direct.prompts.len());
        assert_eq!(metadata["api_call_count"], direct.api_call_count);
        let expected_warnings = if direct.warnings.is_empty() {
            serde_json::json!([])
        } else {
            serde_json::json!(["transcript parser reported partial output"])
        };
        assert_eq!(metadata["warnings"], expected_warnings);
        assert!(!metadata.to_string().contains("pi-canary"));
        assert!(!metadata.to_string().contains("pi-private"));
    }
    // With no parent the helper reports the content-free skipped partial.
    let metadata = pi_helper_metadata(4, None, false).expect("pi frame");
    assert_eq!(metadata["present"], false);
    assert_eq!(metadata["partial"], true);
}

// ---- PI-32: Pi bounded worker frame ----

/// The values a frame carries, as text: every ordinary record field and
/// every carried thinking value, byte for byte. A field the frame does not
/// carry cannot appear here.
fn pi_frame_text(
    frame: &libra::internal::ai::observed_agents::builtin::pi::PiWorkerFrame,
) -> String {
    let mut text = pi_text_records_all(&frame.ordinary).join("\n");
    for turn in &frame.ordinary {
        text.push('\n');
        text.push_str(&turn.logical_turn_key);
    }
    for item in &frame.thinking {
        for raw in [&item.thinking, &item.thinking_signature, &item.redacted]
            .into_iter()
            .flatten()
        {
            text.push('\n');
            text.push_str(raw.get());
        }
        if let Some(readable) = &item.readable {
            text.push('\n');
            text.push_str(readable.as_str());
        }
        if let Some(key) = &item.turn_key {
            text.push('\n');
            text.push_str(key);
        }
    }
    text
}

/// PI-32 AC1: every thinking block of the active branch enters the frame
/// in source order, with its `thinking`, `thinkingSignature`, and
/// `redacted` values as written and its readable text.
#[test]
fn pi_worker_frame_carries_thinking_in_source_order() {
    use libra::internal::ai::observed_agents::builtin::pi::{
        normalize_pi_turns, pi_worker_frame_authorized,
    };

    let bytes = pi_session(&[
        // Before the first user message: no turn.
        pi_message(
            "a0",
            None,
            serde_json::json!({"role": "assistant", "content": [
                {"type": "thinking", "thinking": "preamble thought"},
            ]}),
        ),
        pi_message(
            "u1",
            Some("a0"),
            serde_json::json!({"role": "user", "content": "first"}),
        ),
        pi_message(
            "a1",
            Some("u1"),
            serde_json::json!({"role": "assistant", "content": [
                {"type": "thinking", "thinking": "one", "thinkingSignature": "sig-one"},
                {"type": "text", "text": "answer"},
                {"type": "thinking", "thinking": "", "thinkingSignature": "sig-two", "redacted": true},
                {"type": "toolCall", "id": "call-1", "name": "read", "arguments": {"path": "a.rs"}},
                {"type": "thinking", "thinking": "three", "redacted": false},
            ]}),
        ),
        pi_message(
            "t1",
            Some("a1"),
            serde_json::json!({"role": "toolResult", "toolCallId": "call-1", "toolName": "read",
                "content": [{"type": "text", "text": "out"}], "isError": false}),
        ),
        pi_message(
            "u2",
            Some("t1"),
            serde_json::json!({"role": "user", "content": "second"}),
        ),
        pi_message(
            "a2",
            Some("u2"),
            serde_json::json!({"role": "assistant", "content": [
                {"type": "thinking", "thinking": 7},
                {"type": "thinking", "thinking": "four", "thinkingSignature": {"nested": [1, 2]}},
            ]}),
        ),
    ]);
    let frame = pi_worker_frame_authorized(&bytes, None).expect("frame");
    assert_eq!(frame.tag, "pi");
    assert_eq!(frame.source_state, "unproven");
    assert!(frame.partial);
    assert!(!frame.acquisition_gate_passed);
    assert_eq!(
        frame.ordinary,
        normalize_pi_turns(&bytes, Default::default()).expect("turns")
    );
    let carried: Vec<_> = frame
        .thinking
        .iter()
        .map(|item| {
            (
                item.turn_key.clone(),
                item.thinking.as_ref().map(|raw| raw.get().to_string()),
                item.thinking_signature
                    .as_ref()
                    .map(|raw| raw.get().to_string()),
                item.redacted.as_ref().map(|raw| raw.get().to_string()),
                item.readable.as_ref().map(|text| text.as_str().to_string()),
            )
        })
        .collect();
    let s = |text: &str| Some(text.to_string());
    assert_eq!(
        carried,
        vec![
            (
                None,
                s("\"preamble thought\""),
                None,
                None,
                s("preamble thought")
            ),
            (s("u1"), s("\"one\""), s("\"sig-one\""), None, s("one")),
            (s("u1"), s("\"\""), s("\"sig-two\""), s("true"), None),
            (s("u1"), s("\"three\""), None, s("false"), s("three")),
            (s("u2"), s("7"), None, None, None),
            (
                s("u2"),
                s("\"four\""),
                s("{\"nested\":[1,2]}"),
                None,
                s("four")
            ),
        ]
    );
}

/// PI-32 AC2: fields and entries outside the PI-20/PI-21 frozen tables, and
/// table fields that are not carried values (metadata, opaque, unsupported,
/// extension data), never enter the frame; the carried values do.
#[test]
fn pi_worker_frame_carries_no_field_outside_the_allowlist() {
    use libra::internal::ai::observed_agents::builtin::pi::pi_worker_frame_authorized;

    let bytes = pi_session(&[
        pi_entry(
            "vendor_note",
            "v1",
            None,
            serde_json::json!({"text": "pi-private unknown entry"}),
        ),
        pi_message(
            "u1",
            Some("v1"),
            serde_json::json!({"role": "user", "vendorField": "pi-private user field",
            "content": [
                {"type": "text", "text": "pi-allowed prompt", "textSignature": "pi-private text sig"},
                {"type": "image", "data": "pi-private-image-data", "mimeType": "image/png"},
                {"type": "vendor_block", "data": "pi-private unknown block"},
            ]}),
        ),
        pi_message(
            "a1",
            Some("u1"),
            serde_json::json!({"role": "assistant", "model": "pi-private-model",
            "deferred": {"id": "pi-private-deferred", "data": "pi-private deferred data"},
            "diagnostics": "pi-private diagnostics",
            "content": [
                {"type": "thinking", "thinking": "pi-allowed thought",
                    "thinkingSignature": "pi-allowed signature",
                    "vendorThinking": "pi-private thinking field"},
                {"type": "text", "text": "pi-allowed answer", "textSignature": "pi-private answer sig"},
                {"type": "toolCall", "id": "call-1", "name": "read",
                    "arguments": {"path": "pi-allowed.rs"},
                    "argumentsError": "pi-private arguments error",
                    "thoughtSignature": "pi-private thought signature",
                    "namespace": "pi-private-namespace"},
                {"type": "vendor_block", "data": "pi-private assistant block"},
            ]}),
        ),
        pi_message(
            "t1",
            Some("a1"),
            serde_json::json!({"role": "toolResult", "toolCallId": "call-1", "toolName": "read",
                "details": {"x": "pi-private details"},
                "nestedCalls": {"calls": [{"name": "pi-private nested"}], "complete": true},
                "content": [{"type": "text", "text": "pi-allowed output"}], "isError": false}),
        ),
        pi_message(
            "r1",
            Some("t1"),
            serde_json::json!({"role": "vendorRole", "content": "pi-private unknown role"}),
        ),
        pi_entry(
            "custom",
            "c1",
            Some("r1"),
            serde_json::json!({"customType": "x", "data": "pi-private custom data"}),
        ),
    ]);
    let frame = pi_worker_frame_authorized(&bytes, None).expect("frame");
    let text = pi_frame_text(&frame);
    assert!(
        !text.contains("pi-private"),
        "frame carried a field outside the allowlist: {text}"
    );
    for allowed in [
        "pi-allowed prompt",
        "pi-allowed thought",
        "pi-allowed signature",
        "pi-allowed answer",
        "pi-allowed.rs",
        "pi-allowed output",
    ] {
        assert!(
            text.contains(allowed),
            "{allowed} is missing from the frame"
        );
    }
}

/// PI-32 AC3: input over the frame's byte budget is refused as `Budget`.
/// The budget is the 24 MiB redacted bound, not the 16 MiB raw cap; the
/// normalizer's entry bound is a budget too.
#[test]
fn pi_worker_frame_refuses_an_over_budget_input() {
    use libra::internal::ai::observed_agents::builtin::pi::{
        PI_NORMALIZE_MAX_ENTRIES, PI_WORKER_FRAME_MAX_BYTES, PiWorkerError,
        pi_worker_frame_authorized,
    };

    assert_eq!(PI_WORKER_FRAME_MAX_BYTES, 24 * 1024 * 1024);
    let session = |filler: usize| {
        pi_session(&[pi_message(
            "u1",
            None,
            serde_json::json!({"role": "user", "content": "x".repeat(filler)}),
        )])
    };
    let base = session(0).len();
    let fits = session(PI_WORKER_FRAME_MAX_BYTES as usize - base);
    assert_eq!(fits.len() as u64, PI_WORKER_FRAME_MAX_BYTES);
    let frame = pi_worker_frame_authorized(&fits, None).expect("a session at the budget");
    assert_eq!(frame.ordinary.len(), 1);
    let over = session(PI_WORKER_FRAME_MAX_BYTES as usize - base + 1);
    assert_eq!(over.len() as u64, PI_WORKER_FRAME_MAX_BYTES + 1);
    assert_eq!(
        pi_worker_frame_authorized(&over, None),
        Err(PiWorkerError::Budget)
    );
    // Past the default raw cap but within the budget: still a frame.
    let past_raw = session(17 * 1024 * 1024);
    assert!(pi_worker_frame_authorized(&past_raw, None).is_ok());

    let mut entries = vec![pi_message(
        "u1",
        None,
        serde_json::json!({"role": "user", "content": "q"}),
    )];
    let mut parent = "u1".to_string();
    for index in 0..PI_NORMALIZE_MAX_ENTRIES {
        let id = format!("l{index}");
        entries.push(pi_entry("label", &id, Some(&parent), serde_json::json!({})));
        parent = id;
    }
    let many = pi_session(&entries);
    assert!((many.len() as u64) < PI_WORKER_FRAME_MAX_BYTES);
    assert_eq!(
        pi_worker_frame_authorized(&many, None),
        Err(PiWorkerError::Budget)
    );
}

/// A long, valid session: `turns` user/assistant pairs.
fn pi_long_session(turns: usize) -> Vec<u8> {
    let mut lines = Vec::with_capacity(turns * 2);
    let mut parent: Option<String> = None;
    for index in 0..turns {
        let user = format!("u{index}");
        let assistant = format!("a{index}");
        lines.push(pi_message(
            &user,
            parent.as_deref(),
            serde_json::json!({"role": "user", "content": format!("prompt {index}")}),
        ));
        lines.push(pi_message(
            &assistant,
            Some(&user),
            serde_json::json!({"role": "assistant", "content": [
                {"type": "thinking", "thinking": format!("thought {index}")},
                {"type": "text", "text": format!("answer {index}")},
            ]}),
        ));
        parent = Some(assistant);
    }
    pi_session(&lines)
}

/// A deadline that counts its checks and elapses from check `at` on; with
/// `at` of `None` it never elapses.
struct PiCountingDeadline {
    checks: std::cell::Cell<usize>,
    at: Option<usize>,
}

impl PiCountingDeadline {
    fn at(at: Option<usize>) -> Self {
        Self {
            checks: std::cell::Cell::new(0),
            at,
        }
    }

    fn checks(&self) -> usize {
        self.checks.get()
    }
}

impl libra::internal::ai::observed_agents::builtin::pi::PiDeadline for PiCountingDeadline {
    fn elapsed(&self) -> bool {
        let check = self.checks.get() + 1;
        self.checks.set(check);
        self.at.is_some_and(|at| check >= at)
    }
}

/// PI-32 AC4: an elapsed deadline cancels the frame, both before the call
/// and while a large session is processed. A counting [`PiDeadline`] pins
/// the checks without a clock. Single cases pin a check whose input ends
/// otherwise when it is missing (a refusal found later, or a result); the
/// growth of the check count with the session pins the check of every pass
/// over lines and entries, since a pass without its check grows the count
/// less.
#[test]
fn pi_worker_frame_cancels_at_the_deadline() {
    use std::time::{Duration, Instant};

    use libra::internal::ai::observed_agents::builtin::pi::{
        PI_WORKER_FRAME_MAX_BYTES, PiNormalizeRefusal, PiProjectionError, PiWorkerError,
        normalize_pi_session_until, pi_worker_frame_authorized, pi_worker_frame_with_deadline,
        project_pi_content_until,
    };

    let none: Option<Instant> = None;
    let lines = |bytes: &[u8]| bytes.split(|byte| *byte == b'\n').count();
    let normalizer_checks = |bytes: &[u8]| {
        let deadline = PiCountingDeadline::at(None);
        assert!(matches!(
            normalize_pi_session_until(bytes, Default::default(), &deadline),
            Some(Ok(_))
        ));
        deadline.checks()
    };
    let frame_checks = |bytes: &[u8]| {
        let deadline = PiCountingDeadline::at(None);
        assert!(pi_worker_frame_with_deadline(bytes, &deadline).is_ok());
        deadline.checks()
    };

    // Before the call.
    let small = pi_interleaved_session();
    let past = Some(
        Instant::now()
            .checked_sub(Duration::from_secs(1))
            .expect("past"),
    );
    assert_eq!(
        pi_worker_frame_authorized(&small, past),
        Err(PiWorkerError::Cancelled)
    );
    assert_eq!(
        normalize_pi_session_until(&small, Default::default(), &past),
        None
    );
    let branch = normalize_pi_session_until(&small, Default::default(), &none)
        .expect("not cancelled")
        .expect("branch");
    assert_eq!(project_pi_content_until(&small, &branch, &past), None);
    assert!(pi_worker_frame_authorized(&small, None).is_ok());
    // The frame checks before its budget: an elapsed deadline wins.
    let oversized = vec![b' '; (PI_WORKER_FRAME_MAX_BYTES + 1) as usize];
    assert_eq!(
        pi_worker_frame_authorized(&oversized, None),
        Err(PiWorkerError::Budget)
    );
    assert_eq!(
        pi_worker_frame_authorized(&oversized, past),
        Err(PiWorkerError::Cancelled)
    );

    // Normalizer, line scan: the last line is refused while it is read, so
    // only a check in the line loop cancels a large session halfway. Check
    // 1 is before the work, checks 2..=lines+1 are one per line.
    let large = pi_long_session(2_000);
    let mut bad_tail = large.clone();
    bad_tail.extend_from_slice(br#"{"type":"label","id":""}"#);
    assert_eq!(
        normalize_pi_session_until(&bad_tail, Default::default(), &none),
        Some(Err(PiNormalizeRefusal::UnrepresentableEntry))
    );
    let halfway = lines(&bad_tail) / 2;
    let deadline = PiCountingDeadline::at(Some(halfway));
    assert_eq!(
        normalize_pi_session_until(&bad_tail, Default::default(), &deadline),
        None
    );
    assert_eq!(deadline.checks(), halfway, "stops at the elapsed check");

    // Normalizer, after the scan: a duplicate id is found only when the
    // entries are indexed, so only a check there cancels at the first check
    // after the line scan.
    let mut duplicate = large.clone();
    duplicate.extend_from_slice(
        pi_message(
            "u0",
            Some("a1999"),
            serde_json::json!({"role": "user", "content": "again"}),
        )
        .as_bytes(),
    );
    duplicate.push(b'\n');
    assert_eq!(
        normalize_pi_session_until(&duplicate, Default::default(), &none),
        Some(Err(PiNormalizeRefusal::DuplicateEntryId))
    );
    let deadline = PiCountingDeadline::at(Some(lines(&duplicate) + 2));
    assert_eq!(
        normalize_pi_session_until(&duplicate, Default::default(), &deadline),
        None
    );

    // Normalizer, after the branch is built: a header-only session has no
    // entries, so the first check after the line scan is the last one.
    let header_only = pi_session(&[]);
    let deadline = PiCountingDeadline::at(Some(lines(&header_only) + 2));
    assert_eq!(
        normalize_pi_session_until(&header_only, Default::default(), &deadline),
        None
    );

    // Normalizer, every pass: one more user/assistant pair adds, per entry,
    // its line, its index, its parent link, its cycle-walk start, two walk
    // steps (the entry, then its finished parent), its walk finish, its
    // branch-walk step, its compaction-search step, and its branch build
    // (10), one turn, and one swap of the root-first reversal: 2 × 10 + 2.
    let per_pair = 2 * 10 + 2;
    assert_eq!(
        normalizer_checks(&pi_long_session(200)) - normalizer_checks(&pi_long_session(100)),
        100 * per_pair
    );
    // With a compaction at the end whose kept entry is missing, the
    // compaction search stops at once and the kept-entry search walks the
    // whole branch instead: the same growth, now through that search.
    let compacted = |pairs: usize| {
        let mut bytes = pi_long_session(pairs);
        bytes.extend_from_slice(
            pi_entry(
                "compaction",
                "c0",
                Some(&format!("a{}", pairs - 1)),
                serde_json::json!({"summary": "s", "firstKeptEntryId": "missing", "tokensBefore": 1}),
            )
            .as_bytes(),
        );
        bytes.push(b'\n');
        bytes
    };
    assert_eq!(
        normalizer_checks(&compacted(200)) - normalizer_checks(&compacted(100)),
        100 * per_pair
    );
    // Skipped lines: each malformed line adds its line check and one run
    // to attribute, and attributing any run puts the branch in line order
    // once (one check per branch entry, then one per indexed entry).
    let pairs = 100;
    let clean = pi_long_session(pairs);
    let mut skipped = Vec::new();
    let mut runs = 0;
    for (index, line) in clean.split(|byte| *byte == b'\n').enumerate() {
        if index > 0 {
            skipped.push(b'\n');
        }
        skipped.extend_from_slice(line);
        if index > 0 && index % 10 == 0 {
            skipped.extend_from_slice(b"\n{truncated");
            runs += 1;
        }
    }
    assert!(runs > 1);
    assert_eq!(
        normalizer_checks(&skipped) - normalizer_checks(&clean),
        2 * runs + 2 * pairs + 2 * pairs
    );
    // A child-before-parent chain: the first cycle walk follows the whole
    // chain and finishes it, and every later start meets a finished entry,
    // so each entry adds its line, index, parent link, walk start, walk
    // step, walk finish, and later finished-entry step (7); the branch is
    // only the root.
    let chain = |length: usize| {
        let mut entries = Vec::with_capacity(length);
        for index in 0..length {
            let parent = (index + 1 < length).then(|| format!("e{}", index + 1));
            entries.push(pi_entry(
                "label",
                &format!("e{index}"),
                parent.as_deref(),
                serde_json::json!({}),
            ));
        }
        pi_session(&entries)
    };
    assert_eq!(
        normalizer_checks(&chain(4_000)) - normalizer_checks(&chain(2_000)),
        2_000 * 7
    );
    // Cancelled halfway through that first walk, and stopped there: after
    // check 1, the lines, the index, the parent links, and the first walk
    // start come its steps.
    let long_chain = chain(4_000);
    let at = 1 + lines(&long_chain) + 4_000 + 4_000 + 1 + 2_000;
    let deadline = PiCountingDeadline::at(Some(at));
    assert_eq!(
        normalize_pi_session_until(&long_chain, Default::default(), &deadline),
        None
    );
    assert_eq!(deadline.checks(), at, "stops inside the walk");

    // Projection, entry by entry: the last entry's line no longer holds that
    // entry, so only a check in the entry loop cancels halfway.
    let branch = normalize_pi_session_until(&large, Default::default(), &none)
        .expect("not cancelled")
        .expect("branch");
    let entries = branch.entries.len();
    let renamed = String::from_utf8(large.clone())
        .expect("utf-8")
        .replace("\"id\":\"a1999\"", "\"id\":\"z1999\"")
        .into_bytes();
    assert_eq!(
        project_pi_content_until(&renamed, &branch, &none),
        Some(Err(PiProjectionError::BranchMismatch))
    );
    let deadline = PiCountingDeadline::at(Some(entries / 2));
    assert_eq!(project_pi_content_until(&renamed, &branch, &deadline), None);
    assert_eq!(deadline.checks(), entries / 2, "stops at the elapsed check");

    // Projection, exactly: check 1, one per branch entry and one per line as
    // it indexes the branch's lines, one per turn and per turn entry it sets
    // up, one per entry, five per assistant
    // content array of a thinking and a text block (one per block as the
    // array is parsed, one at its end, and one per block projected; a user's
    // string content has no array), and the last, after it is built.
    let turns = 2_000;
    let projection_checks =
        |bytes: &[u8],
         branch: &libra::internal::ai::observed_agents::builtin::pi::PiActiveBranch| {
            let deadline = PiCountingDeadline::at(None);
            assert!(matches!(
                project_pi_content_until(bytes, branch, &deadline),
                Some(Ok(_))
            ));
            deadline.checks()
        };
    let projected = projection_checks(&large, &branch);
    assert_eq!(
        projected,
        1 + entries + lines(&large) + turns + entries + entries + 5 * turns + 1
    );
    let deadline = PiCountingDeadline::at(Some(projected));
    assert_eq!(project_pi_content_until(&large, &branch, &deadline), None);

    // Many blocks in one entry: each more block adds two projection checks
    // (as the array is parsed, and as the block is projected) and, in the
    // frame, one coverage record and one thinking record check.
    let one_entry = |role: &str, blocks: usize| {
        let content: Vec<_> = (0..blocks)
            .map(|index| serde_json::json!({"type": "text", "text": format!("b{index}")}))
            .collect();
        let mut entries = vec![pi_message(
            "u0",
            None,
            serde_json::json!({"role": "user", "content": "q"}),
        )];
        if role == "user" {
            entries[0] = pi_message(
                "u0",
                None,
                serde_json::json!({"role": "user", "content": content}),
            );
        } else {
            entries.push(pi_message(
                "a0",
                Some("u0"),
                serde_json::json!({"role": role, "content": content}),
            ));
        }
        pi_session(&entries)
    };
    for role in ["assistant", "user"] {
        let small_entry = one_entry(role, 2_000);
        let large_entry = one_entry(role, 4_000);
        let small_branch = normalize_pi_session_until(&small_entry, Default::default(), &none)
            .expect("not cancelled")
            .expect("branch");
        let large_branch = normalize_pi_session_until(&large_entry, Default::default(), &none)
            .expect("not cancelled")
            .expect("branch");
        assert_eq!(
            projection_checks(&large_entry, &large_branch)
                - projection_checks(&small_entry, &small_branch),
            2_000 * 2,
            "{role} blocks"
        );
        assert_eq!(
            frame_checks(&large_entry) - frame_checks(&small_entry),
            2_000 * 4,
            "{role} blocks in the frame"
        );
    }
    // Nested collections in one entry, parsed item by item: each more system
    // section adds three projection checks (its entry, its enumeration key,
    // and taking its value in order), and each more nested call one.
    let nested = |sections: usize, calls: usize| {
        let sections: serde_json::Map<String, serde_json::Value> = (0..sections)
            .map(|index| (format!("s{index}"), serde_json::json!(format!("v{index}"))))
            .collect();
        let calls: Vec<_> = (0..calls).map(|_| serde_json::json!({})).collect();
        let bytes = pi_session(&[
            pi_message(
                "u0",
                None,
                serde_json::json!({"role": "user", "content": "q"}),
            ),
            pi_message(
                "s0",
                Some("u0"),
                serde_json::json!({"role": "system", "content": "sys", "sections": sections}),
            ),
            pi_message(
                "t0",
                Some("s0"),
                serde_json::json!({"role": "toolResult", "toolCallId": "c", "toolName": "read",
                    "content": [], "isError": false,
                    "nestedCalls": {"calls": calls, "complete": true}}),
            ),
        ]);
        let branch = normalize_pi_session_until(&bytes, Default::default(), &none)
            .expect("not cancelled")
            .expect("branch");
        projection_checks(&bytes, &branch)
    };
    assert_eq!(nested(4_000, 0) - nested(2_000, 0), 2_000 * 3, "sections");
    assert_eq!(nested(0, 4_000) - nested(0, 2_000), 2_000, "nested calls");
    // One tool call's arguments, walked node by node for fractional numbers:
    // each more array item adds one frame check, and a deadline that elapses
    // halfway through the walk stops it there. After the frame's first
    // check, the normalizer, and the projection come the coverage line index,
    // the turn, the user entry and its record, the assistant entry and its
    // tool call record, then the walk.
    let tool_call = |items: usize| {
        let values: Vec<_> = (0..items).map(|index| serde_json::json!(index)).collect();
        pi_session(&[
            pi_message(
                "u0",
                None,
                serde_json::json!({"role": "user", "content": "q"}),
            ),
            pi_message(
                "a0",
                Some("u0"),
                serde_json::json!({"role": "assistant", "content": [
                    {"type": "toolCall", "id": "c", "name": "read",
                        "arguments": {"values": values, "ratio": 0.5}},
                ]}),
            ),
        ])
    };
    assert_eq!(
        frame_checks(&tool_call(4_000)) - frame_checks(&tool_call(2_000)),
        2_000,
        "argument nodes"
    );
    let arguments = tool_call(4_000);
    let arguments_branch = normalize_pi_session_until(&arguments, Default::default(), &none)
        .expect("not cancelled")
        .expect("branch");
    let at = 1
        + normalizer_checks(&arguments)
        + projection_checks(&arguments, &arguments_branch)
        + arguments_branch.entries.len()
        + lines(&arguments)
        + 5
        + 2_000;
    let deadline = PiCountingDeadline::at(Some(at));
    assert_eq!(
        pi_worker_frame_with_deadline(&arguments, &deadline),
        Err(PiWorkerError::Cancelled)
    );
    assert_eq!(deadline.checks(), at, "stops inside the walk");
    // The same frame, exactly, which also pins the check before the walk's
    // root: the frame's first check, the normalizer, the projection, then
    // coverage (a line-index key per entry and each line, the turn, each
    // entry and its record, the walk's root, `ratio`, `values`, and each
    // item, and the last), one thinking check per record, and the last.
    assert_eq!(
        frame_checks(&arguments),
        1 + normalizer_checks(&arguments)
            + projection_checks(&arguments, &arguments_branch)
            + (arguments_branch.entries.len() + lines(&arguments) + 1 + 2 * 2 + (3 + 4_000) + 1)
            + 2
            + 1
    );

    // One turn holding many entries: each more assistant message with a
    // thinking block adds 10 normalizer checks (as above; and every second
    // one a reversal swap), 7 projection checks (its line index key and
    // line, its turn-entry setup, its entry, its block as the array is
    // parsed, the array's end, and its block projected), 4 coverage checks
    // (its line index key and line, its entry, its record), and one thinking
    // record check.
    let single_turn = |messages: usize| {
        let mut entries = vec![pi_message(
            "u0",
            None,
            serde_json::json!({"role": "user", "content": "q"}),
        )];
        let mut parent = "u0".to_string();
        for index in 0..messages {
            let id = format!("a{index}");
            entries.push(pi_message(
                &id,
                Some(&parent),
                serde_json::json!({"role": "assistant", "content": [
                    {"type": "thinking", "thinking": format!("t{index}")},
                ]}),
            ));
            parent = id;
        }
        pi_session(&entries)
    };
    assert_eq!(
        frame_checks(&single_turn(400)) - frame_checks(&single_turn(200)),
        200 * (10 + 7 + 4 + 1) + 100
    );
    // Cancelled while that turn's coverage records are built, and stopped
    // there: after the frame's first check, the normalizer, the projection,
    // the coverage line index (a key per entry, then each line), and the
    // turn's own check come its entries.
    let one_turn = single_turn(400);
    let one_branch = normalize_pi_session_until(&one_turn, Default::default(), &none)
        .expect("not cancelled")
        .expect("branch");
    let projection = PiCountingDeadline::at(None);
    assert!(matches!(
        project_pi_content_until(&one_turn, &one_branch, &projection),
        Some(Ok(_))
    ));
    let at = 1
        + normalizer_checks(&one_turn)
        + projection.checks()
        + one_branch.entries.len()
        + lines(&one_turn)
        + 1
        + 200;
    let deadline = PiCountingDeadline::at(Some(at));
    assert_eq!(
        pi_worker_frame_with_deadline(&one_turn, &deadline),
        Err(PiWorkerError::Cancelled)
    );
    assert_eq!(deadline.checks(), at, "stops inside the turn");

    // The whole frame, exactly: its first check, the normalizer, the
    // projection, the coverage records (one per branch entry and per line
    // of its line index, per turn, per entry of a turn, per record, and the last), one per
    // projected record for thinking (each turn holds a user, a thinking, and
    // an answer record), and its last check.
    let total = frame_checks(&large);
    assert_eq!(
        total,
        1 + normalizer_checks(&large)
            + projected
            + (entries + lines(&large) + turns + entries + 3 * turns + 1)
            + 3 * turns
            + 1
    );
    // Cancelled at the first check, halfway, and at the last check, and
    // stopped there each time.
    for at in [1, total / 2, total] {
        let deadline = PiCountingDeadline::at(Some(at));
        assert_eq!(
            pi_worker_frame_with_deadline(&large, &deadline),
            Err(PiWorkerError::Cancelled),
            "frame cancelled at check {at} of {total}"
        );
        assert_eq!(deadline.checks(), at, "frame stops at check {at}");
    }
}

/// PI-32 AC5: bytes the normalizer refuses give `BadFrame`.
#[test]
fn pi_worker_frame_rejects_what_the_normalizer_refuses() {
    use libra::internal::ai::observed_agents::builtin::pi::{
        PiWorkerError, pi_worker_frame_authorized,
    };

    let header = |version: i64| {
        serde_json::json!({"type": "session", "version": version,
            "id": "019a3c2e-7b10-7000-8000-0000000000aa", "cwd": "/work"})
        .to_string()
    };
    let user = |id: &str, parent: Option<&str>| {
        pi_message(
            id,
            parent,
            serde_json::json!({"role": "user", "content": "q"}),
        )
    };
    let version_two = format!("{}\n{}\n", header(2), user("u1", None)).into_bytes();
    let duplicate = pi_session(&[user("u1", None), user("u1", None)]);
    let cycle = pi_session(&[user("u1", Some("u2")), user("u2", Some("u1"))]);
    for (name, bytes) in [
        ("version 2", version_two),
        ("duplicate id", duplicate),
        ("parent cycle", cycle),
        ("not a session", b"not json".to_vec()),
        ("empty", Vec::new()),
    ] {
        assert_eq!(
            pi_worker_frame_authorized(&bytes, None),
            Err(PiWorkerError::BadFrame),
            "{name}"
        );
    }
}

// ---- PI-33: Pi bounded snapshot carrier ----

/// A Pi session whose assistant message holds a signed thinking block, a
/// redacted one, a thinking block with a field outside the PI-21 table, and
/// the answer; an unknown entry and an unknown message field come with it.
fn pi_bounded_capture_session() -> Vec<u8> {
    pi_session(&[
        pi_entry(
            "vendor_note",
            "v1",
            None,
            serde_json::json!({"note": "pi-unknown-entry"}),
        ),
        pi_message(
            "u1",
            Some("v1"),
            serde_json::json!({"role": "user", "content": "pi prompt"}),
        ),
        pi_message(
            "a1",
            Some("u1"),
            serde_json::json!({"role": "assistant", "vendorMessage": "pi-unknown-message",
            "content": [
                {"type": "thinking", "thinking": "pi-thinking-canary",
                    "thinkingSignature": "pi-signature-canary"},
                {"type": "thinking", "thinking": "", "thinkingSignature": "pi-redacted-opaque",
                    "redacted": true},
                {"type": "thinking", "thinking": "pi-second-thought",
                    "vendorThinking": "pi-unknown-thinking"},
                {"type": "text", "text": "pi answer"},
            ]}),
        ),
    ])
}

/// PI-33 AC1: a `pi` snapshot keeps the thinking text and its associated
/// signature, redacted, and unknown data exactly as the session wrote them.
#[test]
fn pi_bounded_capture_keeps_thinking_and_associated_data() {
    use libra::internal::ai::capture::snapshot::capture_authorized_pi_bytes;

    let bytes = pi_bounded_capture_session();
    let snapshot = capture_authorized_pi_bytes(bytes.clone(), "pi-session");
    let transcript = snapshot.transcript().expect("a retained transcript");
    // No credential pattern matches, so the redacted body is the session.
    assert_eq!(snapshot.redaction_report().match_count(), 0);
    assert_eq!(transcript.bytes(), bytes.as_slice());
    let text = String::from_utf8(transcript.bytes().to_vec()).expect("utf-8");
    for kept in [
        "\"thinking\":\"pi-thinking-canary\"",
        "\"thinkingSignature\":\"pi-signature-canary\"",
        "\"thinkingSignature\":\"pi-redacted-opaque\"",
        "\"redacted\":true",
        "\"vendorThinking\":\"pi-unknown-thinking\"",
        "\"vendorMessage\":\"pi-unknown-message\"",
        "\"note\":\"pi-unknown-entry\"",
    ] {
        assert!(text.contains(kept), "{kept} is missing from the snapshot");
    }
}

/// PI-33 AC2: that snapshot is partial, and the reason is `UnprovenOrdinary`.
#[test]
fn pi_bounded_capture_marks_the_snapshot_unproven_partial() {
    use libra::internal::ai::capture::snapshot::{
        CaptureSnapshotCompleteness, CaptureSnapshotPartialReason, capture_authorized_pi_bytes,
    };

    let snapshot = capture_authorized_pi_bytes(pi_bounded_capture_session(), "pi-session");
    assert_eq!(
        snapshot.completeness(),
        CaptureSnapshotCompleteness::Partial
    );
    assert_eq!(
        snapshot.partial_reason(),
        Some(CaptureSnapshotPartialReason::UnprovenOrdinary)
    );
    let projection = snapshot.safe_projection();
    assert_eq!(
        projection.completeness,
        CaptureSnapshotCompleteness::Partial
    );
    assert_eq!(
        projection.partial_reason,
        Some(CaptureSnapshotPartialReason::UnprovenOrdinary)
    );
    assert!(projection.transcript_redacted_bytes > 0);
}

/// PI-33 AC3: the snapshot's readable thinking is the thinking text alone,
/// in source order; the redacted block has none, and the answer is never in
/// it.
#[test]
fn pi_bounded_capture_reads_thinking_apart_from_the_answer() {
    use libra::internal::ai::capture::snapshot::capture_authorized_pi_bytes;

    let snapshot = capture_authorized_pi_bytes(pi_bounded_capture_session(), "pi-session");
    let thinking = snapshot.pi_readable_thinking();
    assert_eq!(
        thinking,
        vec![
            "pi-thinking-canary".to_string(),
            "pi-second-thought".to_string()
        ]
    );
    assert!(!thinking.iter().any(|text| text.contains("pi answer")));
    let transcript = snapshot.transcript().expect("a retained transcript");
    assert!(String::from_utf8_lossy(transcript.bytes()).contains("pi answer"));
}

/// Sessions whose frame the worker refuses, each under the 16 MiB raw cap
/// and each holding a credential the redactor matches: `Budget` (more
/// entries than the normalizer indexes) and `BadFrame` (wrong version,
/// repeated id, parent cycle, not a session).
fn pi_refused_frame_sessions() -> Vec<(&'static str, Vec<u8>)> {
    use libra::internal::ai::observed_agents::builtin::pi::PI_NORMALIZE_MAX_ENTRIES;

    let secret = "AKIAIOSFODNN7EXAMPLE";
    let user = |id: &str, parent: Option<&str>| {
        pi_message(
            id,
            parent,
            serde_json::json!({"role": "user", "content": format!("key {secret}")}),
        )
    };
    let mut many = String::from_utf8(pi_session(&[user("u1", None)])).expect("utf-8");
    let mut parent = "u1".to_string();
    for index in 0..PI_NORMALIZE_MAX_ENTRIES {
        let id = format!("l{index}");
        many.push_str(&format!(
            "{{\"type\":\"label\",\"id\":\"{id}\",\"parentId\":\"{parent}\"}}\n"
        ));
        parent = id;
    }
    let version_two = format!(
        "{}\n{}\n",
        serde_json::json!({"type": "session", "version": 2,
            "id": "019a3c2e-7b10-7000-8000-0000000000aa", "cwd": "/work"}),
        user("u1", None)
    );
    vec![
        ("too many entries", many.into_bytes()),
        ("version 2", version_two.into_bytes()),
        (
            "duplicate id",
            pi_session(&[user("u1", None), user("u1", None)]),
        ),
        (
            "parent cycle",
            pi_session(&[user("u1", Some("u2")), user("u2", Some("u1"))]),
        ),
        ("not a session", format!("not json {secret}").into_bytes()),
    ]
}

/// A 20-turn session whose every prompt holds a credential. Accepted, its
/// snapshot keeps the transcript and the redaction hits, so a refusal of the
/// same session is seen to clear real data.
fn pi_cancelled_case_session() -> Vec<u8> {
    let mut lines = Vec::new();
    let mut parent: Option<String> = None;
    for index in 0..20 {
        let user = format!("u{index}");
        let assistant = format!("a{index}");
        lines.push(pi_message(
            &user,
            parent.as_deref(),
            serde_json::json!({"role": "user", "content": "key AKIAIOSFODNN7EXAMPLE"}),
        ));
        lines.push(pi_message(
            &assistant,
            Some(&user),
            serde_json::json!({"role": "assistant", "content": [
                {"type": "thinking", "thinking": format!("thought {index}")},
                {"type": "text", "text": format!("answer {index}")},
            ]}),
        ));
        parent = Some(assistant);
    }
    pi_session(&lines)
}

/// What a refused frame must leave behind: no transcript, no redaction
/// counts, no scan bytes, and no readable thinking.
fn assert_no_transcript_data(
    snapshot: &libra::internal::ai::capture::snapshot::CaptureSnapshot,
    name: &str,
) {
    assert!(snapshot.transcript().is_none(), "{name}: transcript kept");
    assert_eq!(
        snapshot.redaction_report().match_count(),
        0,
        "{name}: redaction count kept"
    );
    let projection = snapshot.safe_projection();
    assert_eq!(projection.transcript_redacted_bytes, 0, "{name}");
    assert_eq!(projection.redaction_match_count, 0, "{name}");
    assert_eq!(projection.redaction_bytes_scanned, 0, "{name}");
    assert_eq!(projection.redaction_bytes_redacted, 0, "{name}");
    assert!(snapshot.pi_readable_thinking().is_empty(), "{name}");
}

/// PI-33 AC4: a snapshot whose frame is refused (over budget, cancelled, or
/// a bad frame) keeps no data derived from the transcript.
#[test]
fn pi_bounded_capture_keeps_no_transcript_data_for_a_refused_frame() {
    use std::time::{Duration, Instant};

    use libra::internal::ai::capture::snapshot::{
        CaptureSnapshotPartialReason, capture_authorized_pi_bytes,
        capture_authorized_pi_bytes_until,
    };

    // The accepted control keeps the transcript and its redaction count.
    let session = pi_cancelled_case_session();
    let accepted = capture_authorized_pi_bytes(session.clone(), "pi-session");
    assert_eq!(
        accepted.partial_reason(),
        Some(CaptureSnapshotPartialReason::UnprovenOrdinary)
    );
    assert!(accepted.transcript().is_some());
    assert!(accepted.redaction_report().match_count() > 0);
    assert!(accepted.safe_projection().redaction_bytes_scanned > 0);
    assert_eq!(accepted.pi_readable_thinking().len(), 20);

    let past = Instant::now()
        .checked_sub(Duration::from_secs(1))
        .expect("past");
    let cancelled = capture_authorized_pi_bytes_until(session.clone(), "pi-session", &Some(past));
    assert_no_transcript_data(&cancelled, "cancelled before the frame");
    let counted = PiCountingDeadline::at(None);
    capture_authorized_pi_bytes_until(session.clone(), "pi-session", &counted);
    let midway = capture_authorized_pi_bytes_until(
        session,
        "pi-session",
        &PiCountingDeadline::at(Some(counted.checks() / 2)),
    );
    assert_no_transcript_data(&midway, "cancelled while processing");

    for (name, bytes) in pi_refused_frame_sessions() {
        let snapshot = capture_authorized_pi_bytes(bytes, "pi-session");
        assert_no_transcript_data(&snapshot, name);
    }
}

/// PI-33 AC5: each frame error gives its partial reason: `Budget` gives
/// `SourceOversize`, `Cancelled` (before the call or while processing)
/// gives `DeadlineExceeded`, and `BadFrame` gives `SourceReadError`. The
/// stages before the frame have no deadline here and the sources are under
/// the raw cap, so only the frame can give these reasons.
#[test]
fn pi_bounded_capture_maps_each_frame_error_to_its_partial_reason() {
    use std::time::{Duration, Instant};

    use libra::internal::ai::{
        capture::snapshot::{
            CaptureSnapshotCompleteness, CaptureSnapshotPartialReason, capture_authorized_pi_bytes,
            capture_authorized_pi_bytes_until,
        },
        observed_agents::TRANSCRIPT_READ_HARD_CAP_BYTES,
    };

    for (name, bytes) in pi_refused_frame_sessions() {
        let expected = if name == "too many entries" {
            CaptureSnapshotPartialReason::SourceOversize
        } else {
            CaptureSnapshotPartialReason::SourceReadError
        };
        assert!(
            (bytes.len() as u64) < TRANSCRIPT_READ_HARD_CAP_BYTES,
            "{name}"
        );
        let snapshot = capture_authorized_pi_bytes(bytes, "pi-session");
        assert_eq!(
            snapshot.completeness(),
            CaptureSnapshotCompleteness::Partial,
            "{name}"
        );
        assert_eq!(snapshot.partial_reason(), Some(expected), "{name}");
        assert_eq!(snapshot.safe_projection().partial_reason, Some(expected));
    }

    // Cancelled before the call: an elapsed deadline at the frame.
    let session = pi_cancelled_case_session();
    let past = Instant::now()
        .checked_sub(Duration::from_secs(1))
        .expect("past");
    let snapshot = capture_authorized_pi_bytes_until(session.clone(), "pi-session", &Some(past));
    assert_eq!(
        snapshot.partial_reason(),
        Some(CaptureSnapshotPartialReason::DeadlineExceeded)
    );
    // Cancelled at the frame's first check, part way through, and at its
    // last check; one check later the frame is accepted.
    let counted = PiCountingDeadline::at(None);
    let accepted = capture_authorized_pi_bytes_until(session.clone(), "pi-session", &counted);
    assert_eq!(
        accepted.partial_reason(),
        Some(CaptureSnapshotPartialReason::UnprovenOrdinary)
    );
    let total = counted.checks();
    assert!(total > 100, "the frame checks its deadline throughout");
    for at in [1, total / 2, total] {
        let snapshot = capture_authorized_pi_bytes_until(
            session.clone(),
            "pi-session",
            &PiCountingDeadline::at(Some(at)),
        );
        assert_eq!(
            snapshot.partial_reason(),
            Some(CaptureSnapshotPartialReason::DeadlineExceeded),
            "cancelled at check {at} of {total}"
        );
    }
    let snapshot = capture_authorized_pi_bytes_until(
        session,
        "pi-session",
        &PiCountingDeadline::at(Some(total + 1)),
    );
    assert_eq!(
        snapshot.partial_reason(),
        Some(CaptureSnapshotPartialReason::UnprovenOrdinary)
    );
}

/// PI-33 AC6: the ordinary export of a `pi` snapshot keeps the thinking
/// canary: the ordinary consumer gets the body back, thinking included.
#[test]
fn pi_bounded_capture_export_keeps_the_thinking_canary() {
    use libra::internal::ai::capture::snapshot::capture_authorized_pi_bytes;

    let snapshot = capture_authorized_pi_bytes(pi_bounded_capture_session(), "pi-session");
    let exported = snapshot
        .into_redacted_transcript()
        .expect("the ordinary export of a pi snapshot");
    let text = String::from_utf8(exported.into_inner()).expect("utf-8");
    assert!(text.contains("\"thinking\":\"pi-thinking-canary\""));
    assert!(text.contains("\"thinkingSignature\":\"pi-signature-canary\""));
    assert!(text.contains("pi answer"));
}

/// An identifier as written without a raw `r#` prefix: `r#CaptureSnapshot`
/// names the same type as `CaptureSnapshot`, and `r#fs` the same module as
/// `fs`, so the PI-33 AC7 guard compares the unprefixed form.
fn pi_ident(ident: &proc_macro2::Ident) -> String {
    let text = ident.to_string();
    text.strip_prefix("r#").map(str::to_string).unwrap_or(text)
}

/// What one function or block uses, for the PI-33 AC7 architecture guard.
/// `names` holds every segment of a multi-segment or type path, the callee
/// of a one-segment call, every method name, and every identifier inside a
/// macro (a one-segment value path is a local or a field, so it is left
/// out); `calls` holds each call's callee path and each macro (`name!`);
/// `methods` the method names; `types` the segments of type paths;
/// `values` the one-segment value paths (a local, or an item the code reads
/// or passes as a function); `paths` the qualified value paths (an enum
/// variant, a const, or a function used as a value); and `locals` the
/// bindings, generic parameters, and items declared inside.
#[derive(Default)]
struct PiWiringNames {
    names: std::collections::BTreeSet<String>,
    calls: std::collections::BTreeSet<String>,
    methods: std::collections::BTreeSet<String>,
    types: std::collections::BTreeSet<String>,
    values: std::collections::BTreeSet<String>,
    paths: std::collections::BTreeSet<String>,
    locals: std::collections::BTreeSet<String>,
}

impl PiWiringNames {
    fn of(visit: impl FnOnce(&mut Self)) -> Self {
        let mut names = Self::default();
        visit(&mut names);
        names
    }

    fn tokens(&mut self, tokens: proc_macro2::TokenStream) {
        for token in tokens {
            match token {
                proc_macro2::TokenTree::Ident(ident) => {
                    self.names.insert(pi_ident(&ident));
                }
                proc_macro2::TokenTree::Group(group) => self.tokens(group.stream()),
                _ => {}
            }
        }
    }

    /// The names that read a file, run SQL, or take a lease.
    fn forbidden(&self) -> Vec<String> {
        const EXACT: &[&str] = &[
            "fs",
            "File",
            "OpenOptions",
            "open",
            "read_bounded",
            "read_to_end",
            "read_to_string",
            "read_dir",
            "canonicalize",
            "symlink_metadata",
            "process",
            "Command",
            "ConnectionTrait",
            "DatabaseConnection",
            "DatabaseTransaction",
            "Statement",
            "execute",
            "execute_unprepared",
            "query_one",
            "query_all",
            "sea_orm",
            "rusqlite",
            "sqlx",
        ];
        self.names
            .iter()
            .filter(|name| EXACT.contains(&name.as_str()) || pi_names_lease_or_sql(name))
            .cloned()
            .collect()
    }

    /// The types that hold a path, a file, a stream, a process, a database
    /// connection, or a lease.
    fn forbidden_types(&self) -> Vec<String> {
        const IO: &[&str] = &[
            "Path",
            "PathBuf",
            "File",
            "OpenOptions",
            "DirEntry",
            "ReadDir",
            "Metadata",
            "Read",
            "Write",
            "BufRead",
            "BufReader",
            "BufWriter",
            "Seek",
            "Stdin",
            "Stdout",
            "Stderr",
            "Command",
            "Child",
            "TcpStream",
            "UdpSocket",
            "UnixStream",
            "Connection",
            "ConnectionTrait",
            "DatabaseConnection",
            "DatabaseTransaction",
            "Transaction",
            "Statement",
        ];
        self.types
            .iter()
            .filter(|name| IO.contains(&name.as_str()) || pi_names_lease_or_sql(name))
            .cloned()
            .collect()
    }

    /// What leaves the code `known` names: a callee, a qualified value path
    /// (a function may be used as a value), or a one-segment value whose
    /// first segment is not a local, a known item, or `Some`/`Ok`/`Err`/
    /// `None`/`Self`/`self`.
    fn unresolved_calls(&self, known: &std::collections::BTreeSet<String>) -> Vec<String> {
        let resolved = |first: &str| {
            self.locals.contains(first)
                || known.contains(first)
                || ["Some", "Ok", "Err", "None", "Self", "self"].contains(&first)
        };
        self.calls
            .iter()
            .chain(&self.paths)
            .filter(|path| !resolved(path.split("::").next().unwrap_or_default()))
            .chain(self.values.iter().filter(|value| !resolved(value)))
            .cloned()
            .collect()
    }

    /// Every callee: call paths, macros, and method names.
    fn callees(&self) -> std::collections::BTreeSet<String> {
        self.calls.union(&self.methods).cloned().collect()
    }

    /// The names to follow from this code: paths, types, values, and the
    /// macros it invokes.
    fn next(&self) -> Vec<String> {
        self.names
            .iter()
            .chain(&self.types)
            .chain(&self.values)
            .chain(self.calls.iter().filter(|call| call.ends_with('!')))
            .cloned()
            .collect()
    }
}

/// Each rule body of a `macro_rules!` definition, parsed as a block with
/// every `$name` metavariable replaced by the local `__macro_name`, and those
/// locals. `None` when a body cannot be parsed or uses a repetition, so the
/// guard fails closed.
fn pi_macro_bodies(tokens: &proc_macro2::TokenStream) -> Option<Vec<(syn::Block, Vec<String>)>> {
    use proc_macro2::{Delimiter, Group, Ident, TokenStream, TokenTree};

    fn substitute(stream: TokenStream, metavariables: &mut Vec<String>) -> Option<TokenStream> {
        let mut out = Vec::new();
        let mut tokens = stream.into_iter().peekable();
        while let Some(token) = tokens.next() {
            match token {
                TokenTree::Punct(punct) if punct.as_char() == '$' => match tokens.next()? {
                    TokenTree::Ident(ident) => {
                        let local = format!("__macro_{ident}");
                        metavariables.push(local.clone());
                        out.push(TokenTree::Ident(Ident::new(&local, ident.span())));
                    }
                    // `$( .. )*` repetitions are not expanded here.
                    _ => return None,
                },
                TokenTree::Group(group) => {
                    let inner = substitute(group.stream(), metavariables)?;
                    out.push(TokenTree::Group(Group::new(group.delimiter(), inner)));
                }
                other => out.push(other),
            }
        }
        Some(out.into_iter().collect())
    }

    let trees: Vec<TokenTree> = tokens.clone().into_iter().collect();
    let mut bodies = Vec::new();
    let mut index = 0;
    while index < trees.len() {
        // matcher `=` `>` body [`;`]
        let TokenTree::Group(_) = &trees[index] else {
            return None;
        };
        let (Some(TokenTree::Punct(eq)), Some(TokenTree::Punct(gt)), Some(TokenTree::Group(body))) = (
            trees.get(index + 1),
            trees.get(index + 2),
            trees.get(index + 3),
        ) else {
            return None;
        };
        if eq.as_char() != '=' || gt.as_char() != '>' {
            return None;
        }
        let mut metavariables = Vec::new();
        let stream = substitute(body.stream(), &mut metavariables)?;
        let block: syn::Block = syn::parse2(TokenStream::from(TokenTree::Group(Group::new(
            Delimiter::Brace,
            stream,
        ))))
        .ok()?;
        bodies.push((block, metavariables));
        index += 4;
        if matches!(trees.get(index), Some(TokenTree::Punct(semi)) if semi.as_char() == ';') {
            index += 1;
        }
    }
    Some(bodies)
}

/// The kind of a top-level item the guard does not index, for its message.
fn quote_kind(item: &syn::Item) -> String {
    match item {
        syn::Item::Macro(item) => format!("item macro {}!", pi_path_text(&item.mac.path)),
        syn::Item::Mod(item) => format!("module {}", item.ident),
        syn::Item::ForeignMod(_) => "extern block".to_string(),
        _ => "other item".to_string(),
    }
}

fn pi_names_lease_or_sql(name: &str) -> bool {
    let lower = name.to_ascii_lowercase();
    lower.contains("lease") || lower.contains("sql")
}

fn pi_path_text(path: &syn::Path) -> String {
    path.segments
        .iter()
        .map(|segment| pi_ident(&segment.ident))
        .collect::<Vec<_>>()
        .join("::")
}

impl<'ast> syn::visit::Visit<'ast> for PiWiringNames {
    fn visit_expr_path(&mut self, path: &'ast syn::ExprPath) {
        // A path in value position (not a call's callee): a qualified one
        // may name a function used as a value, so it is resolved like a
        // callee; a one-segment one is a local unless it resolves to a known
        // item.
        if path.path.segments.len() > 1 {
            self.paths.insert(pi_path_text(&path.path));
            self.visit_path(&path.path);
        } else if let Some(segment) = path.path.segments.first() {
            self.values.insert(pi_ident(&segment.ident));
        }
        if let Some(qself) = &path.qself {
            self.visit_qself(qself);
        }
    }

    fn visit_expr_call(&mut self, call: &'ast syn::ExprCall) {
        match &*call.func {
            syn::Expr::Path(path) => {
                self.calls.insert(pi_path_text(&path.path));
                if let Some(segment) = path.path.segments.last() {
                    self.names.insert(pi_ident(&segment.ident));
                }
                self.visit_path(&path.path);
                if let Some(qself) = &path.qself {
                    self.visit_qself(qself);
                }
            }
            // A callee that is not a path (a closure or a call result) is
            // not resolved here, so it must not appear.
            callee => {
                self.calls.insert("<expression callee>".to_string());
                self.visit_expr(callee);
            }
        }
        for argument in &call.args {
            self.visit_expr(argument);
        }
    }

    fn visit_path(&mut self, path: &'ast syn::Path) {
        for segment in &path.segments {
            self.names.insert(pi_ident(&segment.ident));
        }
        syn::visit::visit_path(self, path);
    }

    fn visit_type_path(&mut self, path: &'ast syn::TypePath) {
        for segment in &path.path.segments {
            self.types.insert(pi_ident(&segment.ident));
        }
        syn::visit::visit_type_path(self, path);
    }

    fn visit_expr_method_call(&mut self, call: &'ast syn::ExprMethodCall) {
        self.names.insert(pi_ident(&call.method));
        self.methods.insert(pi_ident(&call.method));
        syn::visit::visit_expr_method_call(self, call);
    }

    fn visit_macro(&mut self, mac: &'ast syn::Macro) {
        let name = format!("{}!", pi_path_text(&mac.path));
        self.calls.insert(name.clone());
        self.visit_path(&mac.path);
        self.tokens(mac.tokens.clone());
        // The arguments run as code: parse them as one bracketed expression
        // (a comma list or a `[value; count]` repeat) and check them like
        // any other code. Arguments that are not expressions fail closed.
        let bracketed = proc_macro2::TokenStream::from(proc_macro2::TokenTree::Group(
            proc_macro2::Group::new(proc_macro2::Delimiter::Bracket, mac.tokens.clone()),
        ));
        match syn::parse2::<syn::Expr>(bracketed) {
            Ok(arguments) => self.visit_expr(&arguments),
            Err(_) => {
                self.calls.insert(format!("<unparsed {name} arguments>"));
            }
        }
    }

    fn visit_pat_ident(&mut self, pat: &'ast syn::PatIdent) {
        self.locals.insert(pi_ident(&pat.ident));
        syn::visit::visit_pat_ident(self, pat);
    }

    fn visit_type_param(&mut self, param: &'ast syn::TypeParam) {
        self.locals.insert(pi_ident(&param.ident));
        syn::visit::visit_type_param(self, param);
    }

    fn visit_item_fn(&mut self, function: &'ast syn::ItemFn) {
        self.locals.insert(pi_ident(&function.sig.ident));
        syn::visit::visit_item_fn(self, function);
    }

    fn visit_item_struct(&mut self, item: &'ast syn::ItemStruct) {
        self.locals.insert(pi_ident(&item.ident));
        syn::visit::visit_item_struct(self, item);
    }

    fn visit_item_enum(&mut self, item: &'ast syn::ItemEnum) {
        self.locals.insert(pi_ident(&item.ident));
        syn::visit::visit_item_enum(self, item);
    }
}

/// The source text a span covers (proc-macro2 span locations are on for
/// tests).
fn pi_span_text(source: &str, span: proc_macro2::Span) -> String {
    let lines: Vec<&str> = source.split('\n').collect();
    let offset = |position: proc_macro2::LineColumn| -> usize {
        let line_start: usize = lines[..position.line - 1]
            .iter()
            .map(|line| line.len() + 1)
            .sum();
        let line = lines[position.line - 1];
        line_start
            + line
                .char_indices()
                .nth(position.column)
                .map_or(line.len(), |(index, _)| index)
    };
    source[offset(span.start())..offset(span.end())].to_string()
}

/// Every `use` entry of the items (nested ones inside functions included),
/// as `path::name` or `path::name as alias`, and the aliases.
fn pi_uses(items: &[syn::Item]) -> (std::collections::BTreeSet<String>, Vec<String>) {
    use syn::visit::Visit;

    fn tree(
        node: &syn::UseTree,
        prefix: String,
        entries: &mut std::collections::BTreeSet<String>,
        aliases: &mut Vec<String>,
    ) {
        match node {
            syn::UseTree::Path(path) => tree(
                &path.tree,
                format!("{prefix}{}::", pi_ident(&path.ident)),
                entries,
                aliases,
            ),
            syn::UseTree::Name(name) => {
                entries.insert(format!("{prefix}{}", pi_ident(&name.ident)));
            }
            syn::UseTree::Rename(rename) => {
                entries.insert(format!(
                    "{prefix}{} as {}",
                    pi_ident(&rename.ident),
                    pi_ident(&rename.rename)
                ));
                aliases.push(pi_ident(&rename.rename));
            }
            syn::UseTree::Glob(_) => {
                entries.insert(format!("{prefix}*"));
            }
            syn::UseTree::Group(group) => {
                for item in &group.items {
                    tree(item, prefix.clone(), entries, aliases);
                }
            }
        }
    }
    struct Uses(std::collections::BTreeSet<String>, Vec<String>);
    impl<'ast> syn::visit::Visit<'ast> for Uses {
        fn visit_item_use(&mut self, item: &'ast syn::ItemUse) {
            tree(&item.tree, String::new(), &mut self.0, &mut self.1);
            syn::visit::visit_item_use(self, item);
        }
    }
    let mut uses = Uses(Default::default(), Vec::new());
    for item in items {
        uses.visit_item(item);
    }
    (uses.0, uses.1)
}

/// The first path segment of every name a piece of code relies on (paths,
/// types, macros, and one-segment values), minus its own bindings: the names
/// another declaration in the same file could shadow.
fn pi_relied_names(visit: impl FnOnce(&mut PiReliedNames)) -> std::collections::BTreeSet<String> {
    let mut relied = PiReliedNames::default();
    visit(&mut relied);
    relied.first.difference(&relied.locals).cloned().collect()
}

#[derive(Default)]
struct PiReliedNames {
    first: std::collections::BTreeSet<String>,
    locals: std::collections::BTreeSet<String>,
}

impl<'ast> syn::visit::Visit<'ast> for PiReliedNames {
    fn visit_path(&mut self, path: &'ast syn::Path) {
        if let Some(segment) = path.segments.first() {
            self.first.insert(pi_ident(&segment.ident));
        }
        syn::visit::visit_path(self, path);
    }
    fn visit_pat_ident(&mut self, pat: &'ast syn::PatIdent) {
        self.locals.insert(pi_ident(&pat.ident));
        syn::visit::visit_pat_ident(self, pat);
    }
}

/// Every name a file declares, at any depth, with how often: functions,
/// types, traits, aliases, consts, statics, modules, macros, and imported or
/// renamed names.
fn pi_declared_names(items: &[syn::Item]) -> std::collections::BTreeMap<String, usize> {
    use syn::visit::Visit;

    #[derive(Default)]
    struct Declared(std::collections::BTreeMap<String, usize>);
    impl Declared {
        fn add(&mut self, ident: &proc_macro2::Ident) {
            *self.0.entry(pi_ident(ident)).or_default() += 1;
        }
    }
    impl<'ast> syn::visit::Visit<'ast> for Declared {
        fn visit_item_fn(&mut self, item: &'ast syn::ItemFn) {
            self.add(&item.sig.ident);
            syn::visit::visit_item_fn(self, item);
        }
        fn visit_impl_item_fn(&mut self, item: &'ast syn::ImplItemFn) {
            self.add(&item.sig.ident);
            syn::visit::visit_impl_item_fn(self, item);
        }
        fn visit_item_struct(&mut self, item: &'ast syn::ItemStruct) {
            self.add(&item.ident);
            syn::visit::visit_item_struct(self, item);
        }
        fn visit_item_enum(&mut self, item: &'ast syn::ItemEnum) {
            self.add(&item.ident);
            syn::visit::visit_item_enum(self, item);
        }
        fn visit_item_union(&mut self, item: &'ast syn::ItemUnion) {
            self.add(&item.ident);
            syn::visit::visit_item_union(self, item);
        }
        fn visit_item_trait(&mut self, item: &'ast syn::ItemTrait) {
            self.add(&item.ident);
            syn::visit::visit_item_trait(self, item);
        }
        fn visit_item_type(&mut self, item: &'ast syn::ItemType) {
            self.add(&item.ident);
            syn::visit::visit_item_type(self, item);
        }
        fn visit_item_const(&mut self, item: &'ast syn::ItemConst) {
            self.add(&item.ident);
            syn::visit::visit_item_const(self, item);
        }
        fn visit_item_static(&mut self, item: &'ast syn::ItemStatic) {
            self.add(&item.ident);
            syn::visit::visit_item_static(self, item);
        }
        fn visit_item_mod(&mut self, item: &'ast syn::ItemMod) {
            self.add(&item.ident);
            syn::visit::visit_item_mod(self, item);
        }
        fn visit_item_macro(&mut self, item: &'ast syn::ItemMacro) {
            if let Some(ident) = &item.ident {
                self.add(ident);
            }
            syn::visit::visit_item_macro(self, item);
        }
        fn visit_use_name(&mut self, name: &'ast syn::UseName) {
            self.add(&name.ident);
        }
        fn visit_use_rename(&mut self, rename: &'ast syn::UseRename) {
            self.add(&rename.rename);
        }
    }
    let mut declared = Declared::default();
    for item in items {
        declared.visit_item(item);
    }
    declared.0
}

/// The production items of a file: an item is left out only when one of its
/// `cfg` attributes proves it test-only.
fn pi_production_items(source: &str) -> Vec<syn::Item> {
    let file = syn::parse_file(source).expect("parse source");
    file.items
        .into_iter()
        .filter(|item| !pi_item_attrs(item).iter().any(pi_cfg_is_test_only))
        .collect()
}

fn pi_item_attrs(item: &syn::Item) -> &[syn::Attribute] {
    match item {
        syn::Item::Const(item) => &item.attrs,
        syn::Item::Enum(item) => &item.attrs,
        syn::Item::Fn(item) => &item.attrs,
        syn::Item::Impl(item) => &item.attrs,
        syn::Item::Macro(item) => &item.attrs,
        syn::Item::Mod(item) => &item.attrs,
        syn::Item::Static(item) => &item.attrs,
        syn::Item::Struct(item) => &item.attrs,
        syn::Item::Trait(item) => &item.attrs,
        syn::Item::Type(item) => &item.attrs,
        syn::Item::Use(item) => &item.attrs,
        _ => &[],
    }
}

/// Whether a `cfg` attribute holds only under `test`: `cfg(test)`, or an
/// `all(..)` with a test-only member. `not(test)`, `any(test, ..)`, and any
/// other predicate also hold in a production build.
fn pi_cfg_is_test_only(attr: &syn::Attribute) -> bool {
    fn test_only(meta: &syn::Meta) -> bool {
        match meta {
            syn::Meta::Path(path) => path.is_ident("test"),
            syn::Meta::List(list) if list.path.is_ident("all") => list
                .parse_args_with(
                    syn::punctuated::Punctuated::<syn::Meta, syn::Token![,]>::parse_terminated,
                )
                .is_ok_and(|members| members.iter().any(test_only)),
            _ => false,
        }
    }
    attr.path().is_ident("cfg")
        && attr
            .parse_args::<syn::Meta>()
            .is_ok_and(|meta| test_only(&meta))
}

/// The free functions, the impls (by self type and by trait), the
/// `macro_rules!` macros, the statics and consts, and the type names a file
/// declares at the top level, with every other top-level item kind (a
/// `thread_local!` or other item macro, a module) listed in `unknown` so the
/// guard can fail closed on it.
struct PiItems<'a> {
    free: std::collections::BTreeMap<String, &'a syn::ItemFn>,
    impls: std::collections::BTreeMap<String, Vec<&'a syn::ItemImpl>>,
    macros: std::collections::BTreeMap<String, &'a syn::ItemMacro>,
    values: std::collections::BTreeMap<String, &'a syn::Item>,
    definitions: std::collections::BTreeMap<String, &'a syn::Item>,
    types: std::collections::BTreeSet<String>,
    unknown: Vec<String>,
}

impl<'a> PiItems<'a> {
    fn of(items: &'a [syn::Item]) -> Self {
        let mut indexed = Self {
            free: Default::default(),
            impls: Default::default(),
            macros: Default::default(),
            values: Default::default(),
            definitions: Default::default(),
            types: Default::default(),
            unknown: Vec::new(),
        };
        for item in items {
            match item {
                syn::Item::Fn(function) => {
                    indexed.free.insert(pi_ident(&function.sig.ident), function);
                }
                syn::Item::Struct(definition) => {
                    indexed.types.insert(pi_ident(&definition.ident));
                    indexed
                        .definitions
                        .insert(pi_ident(&definition.ident), item);
                }
                syn::Item::Enum(definition) => {
                    indexed.types.insert(pi_ident(&definition.ident));
                    indexed
                        .definitions
                        .insert(pi_ident(&definition.ident), item);
                }
                syn::Item::Trait(definition) => {
                    indexed.types.insert(pi_ident(&definition.ident));
                    indexed
                        .definitions
                        .insert(pi_ident(&definition.ident), item);
                }
                syn::Item::Type(definition) => {
                    indexed.types.insert(pi_ident(&definition.ident));
                    indexed
                        .definitions
                        .insert(pi_ident(&definition.ident), item);
                }
                syn::Item::Impl(block) => {
                    let mut keys = Vec::new();
                    if let syn::Type::Path(path) = &*block.self_ty
                        && let Some(segment) = path.path.segments.last()
                    {
                        keys.push(pi_ident(&segment.ident));
                    }
                    if let Some((_, path, _)) = &block.trait_
                        && let Some(segment) = path.segments.last()
                    {
                        keys.push(pi_ident(&segment.ident));
                    }
                    for key in keys {
                        indexed.impls.entry(key).or_default().push(block);
                    }
                }
                syn::Item::Macro(item)
                    if item.mac.path.is_ident("macro_rules") && item.ident.is_some() =>
                {
                    if let Some(ident) = &item.ident {
                        // A second definition of the same name would let the
                        // index see one body while call sites use another.
                        if indexed
                            .macros
                            .insert(format!("{}!", pi_ident(ident)), item)
                            .is_some()
                        {
                            indexed
                                .unknown
                                .push(format!("macro {}! defined twice", pi_ident(ident)));
                        }
                    }
                }
                syn::Item::Static(static_item) => {
                    indexed.values.insert(pi_ident(&static_item.ident), item);
                }
                syn::Item::Const(const_item) => {
                    indexed.values.insert(pi_ident(&const_item.ident), item);
                }
                syn::Item::Use(_) => {}
                other => indexed.unknown.push(quote_kind(other)),
            }
        }
        indexed
    }

    /// The functions and impls a set of names reaches: a free function by
    /// name, every impl of a type or trait once it is named (in a signature
    /// too), the definition of a struct, enum, trait, or type alias once it is
    /// named, a `macro_rules!` body once the macro is invoked, and a static or
    /// const once it is read. A method is not followed by its name alone,
    /// since a name like `new` belongs to many types; the receivers' types
    /// are named somewhere in the reached code, so their impls are reached
    /// that way.
    fn reached(
        &self,
        start: &std::collections::BTreeSet<String>,
    ) -> std::collections::BTreeMap<String, PiWiringNames> {
        use syn::visit::Visit;

        let mut reached = std::collections::BTreeMap::new();
        let mut seen = std::collections::BTreeSet::new();
        let mut pending: Vec<String> = start.iter().cloned().collect();
        while let Some(name) = pending.pop() {
            if !seen.insert(name.clone()) {
                continue;
            }
            if let Some(function) = self.free.get(&name) {
                let names = PiWiringNames::of(|names| names.visit_item_fn(function));
                pending.extend(names.next());
                reached.insert(name.clone(), names);
            }
            if let Some(blocks) = self.impls.get(&name) {
                let names = PiWiringNames::of(|names| {
                    for block in blocks {
                        names.visit_item_impl(block);
                    }
                });
                pending.extend(names.next());
                reached.insert(format!("impl {name}"), names);
            }
            if let Some(item) = self.macros.get(&name) {
                // Every identifier in the definition counts, and each rule's
                // body is parsed as a block (its metavariables as locals) so
                // its calls and types are checked like a function's.
                let names = PiWiringNames::of(|names| {
                    names.tokens(item.mac.tokens.clone());
                    match pi_macro_bodies(&item.mac.tokens) {
                        Some(bodies) => {
                            for (block, metavariables) in &bodies {
                                names.locals.extend(metavariables.iter().cloned());
                                names.visit_block(block);
                            }
                        }
                        None => {
                            names.calls.insert("<unparsed macro body>".to_string());
                        }
                    }
                });
                pending.extend(names.next());
                reached.insert(format!("macro {name}"), names);
            }
            if let Some(item) = self.values.get(&name) {
                let names = PiWiringNames::of(|names| names.visit_item(item));
                pending.extend(names.next());
                reached.insert(format!("value {name}"), names);
            }
            if let Some(item) = self.definitions.get(&name) {
                // A struct's or enum's field types, a trait's default method
                // bodies, and a type alias's target count as reached code. An
                // alias's target is also resolved like a callee: a method on
                // it runs the target type's impls.
                let names = PiWiringNames::of(|names| {
                    names.visit_item(item);
                    if let syn::Item::Type(alias) = item {
                        let target = match &*alias.ty {
                            syn::Type::Path(path) => pi_path_text(&path.path),
                            _ => "<non-path type>".to_string(),
                        };
                        names.calls.insert(format!("<alias {target}>"));
                    }
                });
                pending.extend(names.next());
                reached.insert(format!("definition {name}"), names);
            }
        }
        reached
    }

    /// Free functions, type names, macros, statics, and consts: the names a
    /// call or value may resolve to.
    fn known(&self) -> std::collections::BTreeSet<String> {
        self.free
            .keys()
            .chain(&self.types)
            .chain(self.macros.keys())
            .chain(self.values.keys())
            .cloned()
            .collect()
    }
}

/// Every production function of a file (free functions and impl methods),
/// with its names and parameter count.
fn pi_functions(items: &[syn::Item]) -> Vec<(String, PiWiringNames, usize, &syn::Block)> {
    use syn::visit::Visit;

    let mut functions = Vec::new();
    for item in items {
        match item {
            syn::Item::Fn(function) => functions.push((
                pi_ident(&function.sig.ident),
                PiWiringNames::of(|names| names.visit_item_fn(function)),
                function.sig.inputs.len(),
                &*function.block,
            )),
            syn::Item::Impl(block) => {
                for member in &block.items {
                    if let syn::ImplItem::Fn(method) = member {
                        functions.push((
                            pi_ident(&method.sig.ident),
                            PiWiringNames::of(|names| names.visit_impl_item_fn(method)),
                            method.sig.inputs.len(),
                            &method.block,
                        ));
                    }
                }
            }
            _ => {}
        }
    }
    functions
}

/// The `if` expressions of a block whose condition compares with `"pi"`.
fn pi_dispatch_branches(block: &syn::Block) -> Vec<syn::ExprIf> {
    struct Find(Vec<syn::ExprIf>);
    impl<'ast> syn::visit::Visit<'ast> for Find {
        fn visit_expr_if(&mut self, expr: &'ast syn::ExprIf) {
            struct Literal(bool);
            impl<'ast> syn::visit::Visit<'ast> for Literal {
                fn visit_lit_str(&mut self, lit: &'ast syn::LitStr) {
                    self.0 |= lit.value() == "pi";
                }
            }
            let mut literal = Literal(false);
            literal.visit_expr(&expr.cond);
            if literal.0 {
                self.0.push(expr.clone());
            }
            syn::visit::visit_expr_if(self, expr);
        }
    }
    let mut find = Find(Vec::new());
    syn::visit::Visit::visit_block(&mut find, block);
    find.0
}

/// PI-33 AC7: reads, SQL, and leases stay with their existing owners. The
/// Pi wiring in `snapshot.rs` (its four functions and the `pi` dispatch
/// branch of the capture stage) makes only the calls listed here, builds
/// only an in-memory source, and frames only the snapshot it is given. Every
/// `pi.rs` function and impl the worker frame reaches names no file, SQL, or
/// lease API or type, and calls out of `pi.rs` only through the listed pure
/// constructors and coverage helpers, which are checked the same way.
/// The files whose production text the Pi guard pins as a whole.
const PI_PINNED_FILES: &[&str] = &[
    "src/internal/ai/observed_agents/builtin/pi.rs",
    "src/internal/ai/capture/snapshot.rs",
    "src/internal/ai/observed_agents/coverage.rs",
    "src/internal/ai/observed_agents/reasoning.rs",
];

/// Every `.rs` file under `src/`, as (path relative to the root, text).
fn pi_src_files(root: &std::path::Path) -> Vec<(String, String)> {
    let mut found = Vec::new();
    let mut pending = vec![root.join("src")];
    while let Some(path) = pending.pop() {
        if path.is_dir() {
            for entry in std::fs::read_dir(&path).expect("read src directory") {
                pending.push(entry.expect("src entry").path());
            }
            continue;
        }
        if path.extension().is_some_and(|extension| extension == "rs") {
            let relative = path
                .strip_prefix(root)
                .expect("under the root")
                .display()
                .to_string();
            found.push((
                relative,
                std::fs::read_to_string(&path).expect("read source file"),
            ));
        }
    }
    found.sort();
    found
}

/// The types (structs, enums, unions, traits, aliases) that items declare
/// outside function bodies with a visibility other code can name.
fn pi_nameable_types(items: &[syn::Item], types: &mut std::collections::BTreeSet<String>) {
    for item in items {
        let (visibility, ident) = match item {
            syn::Item::Struct(item) => (&item.vis, &item.ident),
            syn::Item::Enum(item) => (&item.vis, &item.ident),
            syn::Item::Union(item) => (&item.vis, &item.ident),
            syn::Item::Trait(item) => (&item.vis, &item.ident),
            syn::Item::Type(item) => (&item.vis, &item.ident),
            syn::Item::Mod(module) => {
                if let Some((_, nested)) = &module.content {
                    pi_nameable_types(nested, types);
                }
                continue;
            }
            _ => continue,
        };
        if !matches!(visibility, syn::Visibility::Inherited) {
            types.insert(pi_ident(ident));
        }
    }
}

/// What lies outside the pinned files yet still decides what their types
/// run: the module declarations that make the compiler read those files; an
/// impl of one of their types anywhere else (a `Drop`, a `Default`, a
/// `Deserialize`, an inherent method, also through a type alias or a `use ..
/// as ..` rename); a macro definition or invocation that names one (it can
/// generate such an impl); production code that compiles text outside `src/`
/// (an `include!`, also from a macro body, a module a macro declares, or a
/// `#[path]` module leaving its directory, also through nested `cfg_attr`),
/// which the scan would not see; and the redaction values the Pi
/// frame itself reads,
/// resets, or drops (`RedactedBytes`, `RedactionReport`, `RedactionMatch`),
/// whose derives must resolve to the standard ones.
fn pi_pinned_files_are_closed(root: &std::path::Path) {
    use sha2::{Digest, Sha256};
    use syn::{spanned::Spanned, visit::Visit};

    let digest = |source: &str, span: proc_macro2::Span| -> String {
        let text: String = pi_span_text(source, span)
            .chars()
            .filter(|character| !character.is_whitespace())
            .collect();
        hex::encode(Sha256::digest(text.as_bytes()))
    };
    // Each module on the way to a pinned file is declared once, as
    // `mod name;` with no `#[path]` or `cfg` that could swap the file read.
    let cargo = std::fs::read_to_string(root.join("Cargo.toml")).expect("read Cargo.toml");
    assert!(
        !cargo.lines().any(|line| line.trim() == "[lib]"),
        "Cargo.toml moves the library root"
    );
    for (parent, child) in [
        ("src/lib.rs", "internal"),
        ("src/internal/mod.rs", "ai"),
        ("src/internal/ai/mod.rs", "capture"),
        ("src/internal/ai/mod.rs", "observed_agents"),
        ("src/internal/ai/capture/mod.rs", "snapshot"),
        ("src/internal/ai/observed_agents/mod.rs", "builtin"),
        ("src/internal/ai/observed_agents/mod.rs", "coverage"),
        ("src/internal/ai/observed_agents/mod.rs", "reasoning"),
        ("src/internal/ai/observed_agents/mod.rs", "redaction"),
        ("src/internal/ai/observed_agents/builtin/mod.rs", "pi"),
    ] {
        let text = std::fs::read_to_string(root.join(parent)).expect(parent);
        let file = syn::parse_file(&text).expect("parse module file");
        let declared: Vec<&syn::ItemMod> = file
            .items
            .iter()
            .filter_map(|item| match item {
                syn::Item::Mod(module) if pi_ident(&module.ident) == child => Some(module),
                _ => None,
            })
            .collect();
        assert_eq!(declared.len(), 1, "{parent} declares `mod {child}` once");
        assert!(
            declared[0].content.is_none()
                && declared[0]
                    .attrs
                    .iter()
                    .all(|attribute| attribute.path().is_ident("doc")),
            "{parent} declares `mod {child}` with a body or an attribute that can swap its file"
        );
    }
    // The types the pinned files can expose; a pinned file has no child
    // file module, so its private types stay inside its pinned text.
    let mut types = std::collections::BTreeSet::new();
    for relative in PI_PINNED_FILES {
        let items = pi_production_items(
            &std::fs::read_to_string(root.join(relative)).expect("read pinned file"),
        );
        struct FileModules(Vec<String>);
        impl<'ast> syn::visit::Visit<'ast> for FileModules {
            fn visit_item_mod(&mut self, module: &'ast syn::ItemMod) {
                if module.content.is_none() {
                    self.0.push(pi_ident(&module.ident));
                }
                syn::visit::visit_item_mod(self, module);
            }
        }
        let mut modules = FileModules(Vec::new());
        for item in &items {
            modules.visit_item(item);
        }
        assert!(
            modules.0.is_empty(),
            "{relative} declares a file module {:?}",
            modules.0
        );
        pi_nameable_types(&items, &mut types);
    }
    for name in PI_REDACTION_TYPES {
        types.insert(name.to_string());
    }
    // Every impl, alias, rename, and macro outside the pinned files that
    // names one of those types, with a digest of its text.
    struct Outside<'a> {
        relative: &'a str,
        source: &'a str,
        types: &'a std::collections::BTreeSet<String>,
        found: Vec<String>,
    }
    struct Idents(std::collections::BTreeSet<String>);
    impl<'ast> syn::visit::Visit<'ast> for Idents {
        fn visit_ident(&mut self, ident: &'ast proc_macro2::Ident) {
            self.0.insert(pi_ident(ident));
        }
    }
    fn names_any(
        tokens: proc_macro2::TokenStream,
        types: &std::collections::BTreeSet<String>,
    ) -> bool {
        tokens.into_iter().any(|token| match token {
            proc_macro2::TokenTree::Ident(ident) => types.contains(&pi_ident(&ident)),
            proc_macro2::TokenTree::Group(group) => names_any(group.stream(), types),
            _ => false,
        })
    }
    impl Outside<'_> {
        fn record(&mut self, head: String, span: proc_macro2::Span) {
            let text = pi_span_text(self.source, span);
            let stripped: String = text
                .chars()
                .filter(|character| !character.is_whitespace())
                .collect();
            let digest = {
                use sha2::{Digest, Sha256};
                hex::encode(Sha256::digest(stripped.as_bytes()))
            };
            let head = head.split_whitespace().collect::<Vec<_>>().join(" ");
            self.found
                .push(format!("{}: {head} #{digest}", self.relative));
        }
    }
    impl<'ast> syn::visit::Visit<'ast> for Outside<'_> {
        fn visit_item_impl(&mut self, block: &'ast syn::ItemImpl) {
            let mut idents = Idents(std::collections::BTreeSet::new());
            idents.visit_type(&block.self_ty);
            if let Some((_, path, _)) = &block.trait_ {
                idents.visit_path(path);
            }
            if !idents.0.is_disjoint(self.types) {
                let target = pi_span_text(self.source, block.self_ty.span());
                let head = match &block.trait_ {
                    Some((_, path, _)) => format!("impl {} for {target}", pi_path_text(path)),
                    None => format!("impl {target}"),
                };
                self.record(head, block.span());
            }
            syn::visit::visit_item_impl(self, block);
        }
        fn visit_item_type(&mut self, alias: &'ast syn::ItemType) {
            let mut idents = Idents(std::collections::BTreeSet::new());
            idents.visit_type(&alias.ty);
            if !idents.0.is_disjoint(self.types) {
                self.record(format!("type {}", pi_ident(&alias.ident)), alias.span());
            }
            syn::visit::visit_item_type(self, alias);
        }
        fn visit_use_rename(&mut self, rename: &'ast syn::UseRename) {
            if self.types.contains(&pi_ident(&rename.ident)) {
                self.record(
                    format!(
                        "use {} as {}",
                        pi_ident(&rename.ident),
                        pi_ident(&rename.rename)
                    ),
                    rename.span(),
                );
            }
        }
        fn visit_macro(&mut self, mac: &'ast syn::Macro) {
            let path = pi_path_text(&mac.path);
            if !PI_EXPRESSION_MACROS.contains(&path.as_str())
                && names_any(mac.tokens.clone(), self.types)
            {
                self.record(format!("{path}!"), mac.span());
            }
            syn::visit::visit_macro(self, mac);
        }
    }
    // The expression-only macros that may name those types keep their
    // standard meaning: no `macro_rules!` of that name anywhere (a
    // definition shadows by text order), no import or rename to it, and
    // `ensure` imported only from anyhow.
    let definition = regex::Regex::new(r"macro_rules!\s*(?:r#)?([A-Za-z_][A-Za-z0-9_]*)")
        .expect("macro definition pattern");
    struct Imports {
        prefix: Vec<String>,
        found: Vec<String>,
    }
    impl<'ast> syn::visit::Visit<'ast> for Imports {
        fn visit_use_path(&mut self, path: &'ast syn::UsePath) {
            self.prefix.push(pi_ident(&path.ident));
            syn::visit::visit_use_path(self, path);
            self.prefix.pop();
        }
        fn visit_use_name(&mut self, name: &'ast syn::UseName) {
            let ident = pi_ident(&name.ident);
            if PI_EXPRESSION_MACROS.contains(&ident.as_str())
                && !(ident == "ensure" && self.prefix == ["anyhow"])
            {
                self.found
                    .push(format!("use {}::{ident}", self.prefix.join("::")));
            }
        }
        fn visit_use_rename(&mut self, rename: &'ast syn::UseRename) {
            let ident = pi_ident(&rename.rename);
            if PI_EXPRESSION_MACROS.contains(&ident.as_str()) {
                self.found.push(format!("use .. as {ident}"));
            }
        }
    }
    let mut shadowing = Vec::new();
    for (relative, text) in pi_src_files(root) {
        for found in definition.captures_iter(&text) {
            if PI_EXPRESSION_MACROS.contains(&&found[1]) {
                shadowing.push(format!("{relative}: {}", &found[0]));
            }
        }
        let mut imports = Imports {
            prefix: Vec::new(),
            found: Vec::new(),
        };
        let file = syn::parse_file(&text).expect("parse source file");
        imports.visit_file(&file);
        // Production code compiles no text outside `src/` that this scan
        // would miss: no `include!` (also not inside a macro body or
        // argument, nor imported under another name), no module declared by
        // a macro (no `mod` among any macro's tokens), and every `#[path]`
        // module stays under its declaring file's directory (a relative path
        // without `..`, naming a `.rs` file for a file module, and `path`
        // nowhere inside a `cfg_attr`, at any nesting).
        struct Includes(Vec<String>);
        fn names_ident(tokens: proc_macro2::TokenStream, name: &str) -> bool {
            tokens.into_iter().any(|token| match token {
                proc_macro2::TokenTree::Ident(ident) => pi_ident(&ident) == name,
                proc_macro2::TokenTree::Group(group) => names_ident(group.stream(), name),
                _ => false,
            })
        }
        impl<'ast> syn::visit::Visit<'ast> for Includes {
            fn visit_macro(&mut self, mac: &'ast syn::Macro) {
                if mac
                    .path
                    .segments
                    .last()
                    .is_some_and(|segment| pi_ident(&segment.ident) == "include")
                    || names_ident(mac.tokens.clone(), "include")
                    || names_ident(mac.tokens.clone(), "mod")
                {
                    self.0.push(format!("{}!", pi_path_text(&mac.path)));
                }
                syn::visit::visit_macro(self, mac);
            }
            fn visit_use_name(&mut self, name: &'ast syn::UseName) {
                if pi_ident(&name.ident) == "include" {
                    self.0.push("use .. include".to_string());
                }
            }
            fn visit_use_rename(&mut self, rename: &'ast syn::UseRename) {
                if pi_ident(&rename.ident) == "include" {
                    self.0
                        .push(format!("use .. include as {}", pi_ident(&rename.rename)));
                }
            }
            fn visit_item_mod(&mut self, module: &'ast syn::ItemMod) {
                for attribute in &module.attrs {
                    if attribute.path().is_ident("path") {
                        let inside = match &attribute.meta {
                            syn::Meta::NameValue(syn::MetaNameValue {
                                value:
                                    syn::Expr::Lit(syn::ExprLit {
                                        lit: syn::Lit::Str(path),
                                        ..
                                    }),
                                ..
                            }) => {
                                let path = path.value();
                                !path.is_empty()
                                    && std::path::Path::new(&path).components().all(|component| {
                                        matches!(component, std::path::Component::Normal(_))
                                    })
                                    && !path.contains(['\\', ':'])
                                    // A file module reads a `.rs` file, which
                                    // the scan of `src/` covers.
                                    && (module.content.is_some() || path.ends_with(".rs"))
                            }
                            _ => false,
                        };
                        if !inside {
                            self.0
                                .push(format!("#[path] mod {}", pi_ident(&module.ident)));
                        }
                    } else if attribute.path().is_ident("cfg_attr")
                        && let syn::Meta::List(list) = &attribute.meta
                        && names_ident(list.tokens.clone(), "path")
                    {
                        self.0.push(format!(
                            "#[cfg_attr(.., path)] mod {}",
                            pi_ident(&module.ident)
                        ));
                    }
                }
                syn::visit::visit_item_mod(self, module);
            }
        }
        let mut includes = Includes(Vec::new());
        for item in file
            .items
            .iter()
            .filter(|item| !pi_item_attrs(item).iter().any(pi_cfg_is_test_only))
        {
            includes.visit_item(item);
        }
        shadowing.extend(
            includes
                .0
                .into_iter()
                .map(|found| format!("{relative}: {found}")),
        );
        shadowing.extend(
            imports
                .found
                .into_iter()
                .map(|found| format!("{relative}: {found}")),
        );
    }
    assert!(
        shadowing.is_empty(),
        "a macro that may name a pinned Pi type is shadowed, or production code includes text: {shadowing:?}"
    );
    let mention = regex::Regex::new(&format!(
        r"\b(?:{})\b",
        types.iter().cloned().collect::<Vec<_>>().join("|")
    ))
    .expect("type name pattern");
    let mut outside = Vec::new();
    for (relative, text) in pi_src_files(root) {
        if PI_PINNED_FILES.contains(&relative.as_str()) || !mention.is_match(&text) {
            continue;
        }
        let mut scan = Outside {
            relative: &relative,
            source: &text,
            types: &types,
            found: Vec::new(),
        };
        for item in pi_production_items(&text) {
            scan.visit_item(&item);
        }
        outside.extend(scan.found);
    }
    outside.sort();
    let expected: Vec<String> = PI_OUTSIDE_EXPECTED
        .iter()
        .map(|line| line.to_string())
        .collect();
    assert_eq!(
        outside, expected,
        "code outside the pinned Pi files that names their types changed: re-review what it runs, then update the list"
    );
    // The redaction values keep their definitions, and the names their
    // definitions and derives rely on keep their meaning: no declaration in
    // redaction.rs shadows one, the serde derives come from serde, and no
    // glob import or item macro there can add a declaration. (Their impls,
    // such as `RedactedBytes::bytes`, are frozen above.)
    let relative = "src/internal/ai/observed_agents/redaction.rs";
    let source = std::fs::read_to_string(root.join(relative)).expect(relative);
    let items = pi_production_items(&source);
    let mut pins = std::collections::BTreeMap::new();
    let mut relied = std::collections::BTreeSet::new();
    for item in &items {
        if let syn::Item::Struct(definition) = item
            && PI_REDACTION_TYPES.contains(&pi_ident(&definition.ident).as_str())
        {
            pins.insert(
                pi_ident(&definition.ident),
                digest(&source, definition.span()),
            );
            relied.extend(pi_relied_names(|names| names.visit_item_struct(definition)));
            for attribute in &definition.attrs {
                if attribute.path().is_ident("derive") {
                    let derived = attribute
                        .parse_args_with(
                            syn::punctuated::Punctuated::<syn::Path, syn::Token![,]>::parse_terminated,
                        )
                        .expect("parse derive list");
                    for path in derived {
                        relied.insert(pi_path_text(&path));
                    }
                } else if let Some(name) = attribute.path().get_ident() {
                    relied.insert(pi_ident(name));
                }
            }
        }
    }
    let expected_pins: std::collections::BTreeMap<String, String> = PI_REDACTION_PINS
        .iter()
        .map(|(name, pin)| (name.to_string(), pin.to_string()))
        .collect();
    assert_eq!(
        pins, expected_pins,
        "a redaction value the Pi frame reads, resets, or drops changed: re-review it, then update the pin"
    );
    let declared = pi_declared_names(&items);
    for name in &relied {
        let expected = usize::from(
            PI_REDACTION_TYPES.contains(&name.as_str())
                || ["Deserialize", "Serialize"].contains(&name.as_str()),
        );
        assert_eq!(
            declared.get(name).copied().unwrap_or(0),
            expected,
            "{relative} declares `{name}`, which a redaction value relies on"
        );
    }
    let (entries, _) = pi_uses(&items);
    for derive in ["Deserialize", "Serialize"] {
        let sources: Vec<_> = entries
            .iter()
            .filter(|entry| {
                entry.rsplit("::").next() == Some(derive)
                    || entry.ends_with(&format!(" as {derive}"))
            })
            .collect();
        assert_eq!(
            sources,
            [&format!("serde::{derive}")],
            "{relative} imports `{derive}` from {sources:?}"
        );
    }
    assert!(
        !entries.iter().any(|entry| entry.ends_with('*')),
        "{relative} glob-imports"
    );
    struct ItemMacros(Vec<String>);
    impl<'ast> syn::visit::Visit<'ast> for ItemMacros {
        fn visit_item_macro(&mut self, item: &'ast syn::ItemMacro) {
            self.0.push(pi_path_text(&item.mac.path));
            syn::visit::visit_item_macro(self, item);
        }
    }
    let mut macros = ItemMacros(Vec::new());
    for item in &items {
        macros.visit_item(item);
    }
    assert!(
        macros.0.is_empty(),
        "{relative} has an item macro {:?}",
        macros.0
    );
}

/// Macros that expand to an expression only, so naming a pinned type in one
/// cannot generate an impl.
const PI_EXPRESSION_MACROS: &[&str] = &["anyhow::ensure", "assert_eq", "ensure", "matches", "vec"];

const PI_OUTSIDE_EXPECTED: &[&str] = &[
    "src/internal/ai/agent_import.rs: impl ImportCaptureServices<CaptureSnapshotService> #a9dc3b3daaeade4be0cd87a9612d7f380754299ccc8e2ab7565bb447ff29ac08",
    "src/internal/ai/agent_import.rs: impl ImportSnapshotPort for CaptureSnapshotService #8a9ce307f2f82b612d1852c2efb2e07c7bd5030bdc8440d1165032e14f418d9c",
    "src/internal/ai/observed_agents/adapter.rs: type OrdinarySourcePreprocessor #242cf3132d1b4a08ecc28f186ef026e6e1aeb3c3b09e69e0b9f7bbcb55e350f0",
    "src/internal/ai/observed_agents/builtin/stable_promoted.rs: impl crate::internal::ai::observed_agents::reasoning::ReasoningSourcePreprocessor for StablePromotedAgent #b39e67fc1b47ac44a73de7c25f1584e15979770d1fbdca727f969cc863264fb0",
    "src/internal/ai/observed_agents/live_capture.rs: type LiveCoverageNormalizer #0a0e4029a86256bc2bf287d1316d1d67144d4bd0a07f6e19fd7a4c9ca97e45ef",
    "src/internal/ai/observed_agents/opencode_reasoning.rs: impl Zeroize for CanonValue #416c7e355037856d1e31f3c27673db55c4180494f9ed3304931e4c7a612bc2c8",
    "src/internal/ai/observed_agents/opencode_reasoning.rs: type CoverageMessageView #0de8d51fd1ad3a0af2241d06487da4df2cd7bd69423c555170676a21b96ee30d",
    "src/internal/ai/observed_agents/redaction.rs: impl AsRef for RedactedBytes #83efe3bbfdf512c9def982845f006b299dd3e0bc782e6bcd59336aaf27f658b7",
    "src/internal/ai/observed_agents/redaction.rs: impl RedactedBytes #1a5c803a3876874f3e4f01e210b84ed39f3c76dd69b6486a86964111735bfbf1",
    "src/internal/ai/observed_agents/redaction.rs: impl RedactionReport #1b38d4a9f3521e2663bf3e5907880b64999035623e788ea4f36775d2a571cdd0",
];

/// The redaction values the Pi frame itself reads (`RedactedBytes::bytes`),
/// resets (`RedactionReport::default`), or drops.
const PI_REDACTION_TYPES: &[&str] = &["RedactedBytes", "RedactionMatch", "RedactionReport"];

const PI_REDACTION_PINS: &[(&str, &str)] = &[
    (
        "RedactedBytes",
        "982e416d70dd81a65425e08d83f030d748c38e9b45dca722427a659382da1de1",
    ),
    (
        "RedactionMatch",
        "f092696d3fab4233a70e3f085674991dffa9c1a0382721a933b979056cff0c84",
    ),
    (
        "RedactionReport",
        "542f9092e3a88b420992c382ee5b25952749f256408cc4364809da80d64f412e",
    ),
];

#[test]
fn pi_bounded_capture_wiring_reads_no_file_sql_or_lease() {
    use std::collections::BTreeSet;

    use syn::visit::Visit;

    let set = |items: &[&str]| -> BTreeSet<String> {
        items.iter().map(|item| item.to_string()).collect()
    };

    // Scanner self-test: direct file, SQL, and lease names; a call to a
    // helper outside the scanned code; a method on a receiver built by such
    // a call; and a path type. A local named `open` and a known helper are
    // not flagged.
    let fixture: syn::File = syn::parse_str(
        r#"
        fn regressed(conn: &C, path: &str) {
            let _ = std::fs::read(path);
            let _ = File::open(path);
            let _ = conn.execute(statement);
            let _ = acquire_session_lease(conn);
            let _ = format!("{}", sqlx::query("x"));
        }
        fn helper_bypass(bytes: &[u8]) -> usize { crate::util::slurp(bytes) }
        fn receiver_bypass() { let pool = make_pool(); pool.run("select 1"); }
        fn typed_bypass(path: &std::path::Path) -> usize { path.as_os_str().len() }
        fn clean(open: Vec<u8>) -> usize {
            let mut open = open;
            open.pop();
            let carried = |bytes: usize| bytes + 1;
            helper(carried(open.len()))
        }
        "#,
    )
    .expect("fixture");
    let scanned: Vec<_> = fixture
        .items
        .iter()
        .map(|item| {
            let syn::Item::Fn(function) = item else {
                panic!("fixture function")
            };
            PiWiringNames::of(|names| names.visit_item_fn(function))
        })
        .collect();
    let known = set(&["helper"]);
    assert_eq!(
        scanned[0].forbidden(),
        vec![
            "File",
            "acquire_session_lease",
            "execute",
            "fs",
            "open",
            "sqlx"
        ]
    );
    assert_eq!(
        scanned[1].unresolved_calls(&known),
        vec!["crate::util::slurp"]
    );
    assert_eq!(scanned[2].unresolved_calls(&known), vec!["make_pool"]);
    assert_eq!(scanned[3].forbidden_types(), vec!["Path"]);
    assert!(scanned[4].forbidden().is_empty());
    assert!(scanned[4].forbidden_types().is_empty());
    assert!(scanned[4].unresolved_calls(&known).is_empty());
    // A file read hidden in a macro body or a static initializer is reached
    // through the invocation or the read; an item macro fails closed.
    let hidden: syn::File = syn::parse_str(
        r#"
        macro_rules! sneak { () => { std::fs::read("x") }; }
        static CACHE: std::sync::LazyLock<Vec<u8>> =
            std::sync::LazyLock::new(|| File::open("y").map(|_| Vec::new()).unwrap_or_default());
        thread_local! { static SLOT: u8 = 0; }
        fn uses_macro() -> usize { let _ = sneak!(); 0 }
        fn uses_static() -> usize { CACHE.len() }
        "#,
    )
    .expect("hidden fixture");
    let hidden_items = PiItems::of(&hidden.items);
    assert_eq!(hidden_items.unknown, vec!["item macro thread_local!"]);
    let flagged = |start: &str| -> Vec<String> {
        hidden_items
            .reached(&set(&[start]))
            .values()
            .flat_map(|names| names.forbidden())
            .collect()
    };
    assert_eq!(flagged("uses_macro"), vec!["fs"]);
    assert_eq!(flagged("uses_static"), vec!["File", "open"]);
    // A call in a macro body is checked as a call: one that leaves the
    // scanned code is unresolved even when no name in it is forbidden.
    let called: syn::File = syn::parse_str(
        r#"
        macro_rules! spawn { ($path:expr) => { sneaky::Runner::new("cat").arg($path).output() }; }
        fn uses_spawn(path: &str) -> usize { let _ = spawn!(path); 0 }
        "#,
    )
    .expect("macro call fixture");
    let called_items = PiItems::of(&called.items);
    let reached_called = called_items.reached(&set(&["uses_spawn"]));
    let unresolved: Vec<String> = reached_called
        .values()
        .flat_map(|names| names.unresolved_calls(&called_items.known()))
        .collect();
    assert_eq!(unresolved, vec!["sneaky::Runner::new"]);
    // A macro argument runs as code too: a call in it is checked.
    let argument: syn::File = syn::parse_str(
        r#"
        macro_rules! poll { ($deadline:expr) => { if $deadline.elapsed() { return; } }; }
        fn uses_argument(deadline: &D) { poll!({ crate::outside::read_source(); deadline }); }
        fn uses_pattern(value: u8) -> bool { matches!(value, 1 if value > 0) }
        "#,
    )
    .expect("macro argument fixture");
    let argument_items = PiItems::of(&argument.items);
    let unresolved = |start: &str| -> Vec<String> {
        argument_items
            .reached(&set(&[start]))
            .values()
            .flat_map(|names| names.unresolved_calls(&argument_items.known()))
            .collect()
    };
    assert_eq!(
        unresolved("uses_argument"),
        vec!["crate::outside::read_source"]
    );
    // A function used as a value, bound to a local or passed along, is
    // resolved like a callee.
    let pointer: syn::File = syn::parse_str(
        r#"
        fn uses_pointer() { let reader = crate::outside::read_source; reader(); }
        fn uses_mapped(bytes: Vec<u8>) -> Vec<u8> { bytes.into_iter().map(outside_helper).collect() }
        "#,
    )
    .expect("function value fixture");
    let pointer_items = PiItems::of(&pointer.items);
    let unresolved_pointer = |start: &str| -> Vec<String> {
        pointer_items
            .reached(&set(&[start]))
            .values()
            .flat_map(|names| names.unresolved_calls(&pointer_items.known()))
            .collect()
    };
    assert_eq!(
        unresolved_pointer("uses_pointer"),
        vec!["crate::outside::read_source"]
    );
    assert_eq!(unresolved_pointer("uses_mapped"), vec!["outside_helper"]);
    // A trait's default method body and a type alias's target are reached
    // with the trait or the alias.
    let defined: syn::File = syn::parse_str(
        r#"
        trait Sneaky { fn go(&self) -> bool { std::fs::metadata("/").is_ok() } }
        struct Holder;
        impl Sneaky for Holder {}
        fn uses_trait(holder: &Holder) -> bool { holder.go() }
        type Reader = crate::outside::Reader;
        fn uses_alias(reader: &Reader) -> usize { reader.read_all() }
        "#,
    )
    .expect("definition fixture");
    let defined_items = PiItems::of(&defined.items);
    let trait_forbidden: Vec<String> = defined_items
        .reached(&set(&["uses_trait"]))
        .values()
        .flat_map(|names| names.forbidden())
        .collect();
    assert_eq!(trait_forbidden, vec!["fs"]);
    let alias_unresolved: Vec<String> = defined_items
        .reached(&set(&["uses_alias"]))
        .values()
        .flat_map(|names| names.unresolved_calls(&defined_items.known()))
        .collect();
    assert_eq!(alias_unresolved, vec!["<alias crate::outside::Reader>"]);
    // A guarded `matches!` pattern is not an expression: it fails closed.
    assert_eq!(
        unresolved("uses_pattern"),
        vec!["<unparsed matches! arguments>", "matches!"]
    );
    // Only an item whose `cfg` proves it test-only is left out.
    let configured = pi_production_items(
        "#[cfg(test)] mod a {} #[cfg(all(test, unix))] mod b {} #[cfg(not(test))] mod c {} #[cfg(any(test, unix))] mod d {}",
    );
    assert_eq!(
        PiItems::of(&configured).unknown,
        vec!["module c", "module d"]
    );

    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR"));
    let read = |relative: &str| std::fs::read_to_string(root.join(relative)).expect(relative);
    let snapshot_source = read("src/internal/ai/capture/snapshot.rs");
    let snapshot_items = pi_production_items(&snapshot_source);
    let snapshot = pi_functions(&snapshot_items);

    // The wiring: each function's exact callees.
    for (name, callees) in [
        (
            "apply_pi_worker_frame",
            set(&[
                "RedactionReport::default",
                "Some",
                "as_ref",
                "bytes",
                "map",
                "pi_worker_frame_with_deadline",
                "unwrap_or",
            ]),
        ),
        (
            "capture_authorized_pi_bytes",
            set(&[
                "CaptureSnapshotPolicy::default",
                "capture_authorized_pi_bytes_until",
                "deadline",
            ]),
        ),
        (
            "capture_authorized_pi_bytes_until",
            set(&[
                "CaptureSnapshotPolicy::default",
                "CaptureSnapshotService::capture_authorized_framed",
                "crate::internal::ai::observed_agents::ExportAuthorized::issue",
            ]),
        ),
        (
            "pi_readable_thinking",
            set(&[
                "Vec::new",
                "as_ref",
                "as_str",
                "bytes",
                "collect",
                "crate::internal::ai::observed_agents::builtin::pi::pi_worker_frame_authorized",
                "filter_map",
                "iter",
                "map",
                "to_string",
            ]),
        ),
    ] {
        let found: Vec<_> = snapshot
            .iter()
            .filter(|(found, ..)| found == name)
            .collect();
        assert_eq!(found.len(), 1, "{name} is defined once");
        let (_, names, inputs, _) = found[0];
        assert_eq!(names.callees(), callees, "{name} calls only these");
        let paths = if name == "apply_pi_worker_frame" {
            set(&[
                "CaptureSnapshotCompleteness::Partial",
                "CaptureSnapshotPartialReason::DeadlineExceeded",
                "CaptureSnapshotPartialReason::SourceOversize",
                "CaptureSnapshotPartialReason::SourceReadError",
                "CaptureSnapshotPartialReason::UnprovenOrdinary",
                "PiWorkerError::BadFrame",
                "PiWorkerError::Budget",
                "PiWorkerError::Cancelled",
            ])
        } else {
            BTreeSet::new()
        };
        assert_eq!(names.paths, paths, "{name} uses only these paths");
        let outside: Vec<_> = names
            .values
            .iter()
            .filter(|value| {
                !names.locals.contains(*value) && !["None", "self"].contains(&value.as_str())
            })
            .collect();
        assert!(outside.is_empty(), "{name} reads {outside:?}");
        assert!(
            names.forbidden().is_empty(),
            "{name}: {:?}",
            names.forbidden()
        );
        assert!(
            names.forbidden_types().is_empty(),
            "{name}: {:?}",
            names.forbidden_types()
        );
        if name == "apply_pi_worker_frame" {
            assert_eq!(*inputs, 2, "{name} takes the snapshot and the deadline");
        }
        if name == "capture_authorized_pi_bytes_until" {
            assert!(names.names.contains("TranscriptSource") && names.names.contains("Bytes"));
        }
    }
    // The dispatch: one `pi` branch, in the capture stage after the read and
    // redaction, whose only statement frames the snapshot.
    let mut branches = Vec::new();
    for (name, names, _, block) in &snapshot {
        if name != "apply_pi_worker_frame" && names.names.contains("apply_pi_worker_frame") {
            branches.push((name.clone(), pi_dispatch_branches(block)));
        }
    }
    assert_eq!(branches.len(), 1, "one caller frames the snapshot");
    let (caller, found) = &branches[0];
    assert_eq!(caller, "capture_authorized_bytes");
    assert_eq!(found.len(), 1, "one pi branch");
    let branch = &found[0];
    assert!(branch.else_branch.is_none());
    assert_eq!(branch.then_branch.stmts.len(), 1);
    // The whole capture stage, not only its `pi` branch, names no file,
    // SQL, or lease API or type.
    let stage = &snapshot
        .iter()
        .find(|(name, ..)| name == "capture_authorized_bytes")
        .expect("capture stage")
        .1;
    assert!(
        stage.forbidden().is_empty(),
        "capture stage names {:?}",
        stage.forbidden()
    );
    assert!(
        stage.forbidden_types().is_empty(),
        "capture stage uses {:?}",
        stage.forbidden_types()
    );
    // The capture path a Pi snapshot takes (the three functions that carry
    // the frame deadline) is frozen: its exact callees, qualified value
    // paths, and outside one-segment values. A new call there, under any
    // condition, changes what a Pi snapshot runs; extend these sets only
    // after checking that the new callee reads no file, runs no SQL, and
    // takes no lease beyond the existing read stage.
    for (name, callees, paths, values) in [
        (
            "capture_authorized_framed",
            set(&[
                "AgentKind::from_db_str",
                "AuthorizedReasoningSourceContext::ordinary",
                "CaptureSnapshot::partial",
                "Self::capture_authorized_with_reasoning_context_framed",
                "crate::internal::ai::observed_agents::live_capture::selected_snapshot_adapter",
                "wipe_in_memory_source",
            ]),
            set(&[
                "AuthorizedReasoningSourceOrigin::LegacyOrdinary",
                "CaptureSnapshotPartialReason::SourceAuthorizationMismatch",
            ]),
            set(&["None"]),
        ),
        (
            "capture_authorized_with_reasoning_context_framed",
            set(&[
                "CaptureSnapshot::partial",
                "Ok",
                "Self::capture_authorized_bytes",
                "Some",
                "as_db_str",
                "authorized_len",
                "deadline_elapsed",
                "downcast_ref",
                "is_some",
                "matches",
                "provider_kind",
                "read_bounded",
                "source_projection",
                "wipe_in_memory_source",
                "zeroize",
            ]),
            set(&[
                "CaptureSnapshotPartialReason::DeadlineExceeded",
                "CaptureSnapshotPartialReason::SourceAuthorizationMismatch",
                "CaptureSnapshotPartialReason::SourceOversize",
                "CaptureSnapshotPartialReason::SourceReadError",
            ]),
            BTreeSet::new(),
        ),
        (
            "capture_authorized_bytes",
            set(&[
                "CaptureSnapshot::complete",
                "CaptureSnapshot::partial",
                "ReasoningSourceDisposition::NativeProven",
                "RedactionReport::default",
                "Redactor::new_default",
                "Some",
                "Zeroizing::new",
                "apply_hermes_worker_frame",
                "apply_pi_worker_frame",
                "as_ref",
                "bytes",
                "capture_redacted_output_cap",
                "capture_redaction_working_set_cap",
                "crate::internal::ai::observed_agents::coverage::merge_redaction_reports",
                "deadline",
                "deadline_elapsed",
                "digest_hex",
                "is_empty",
                "is_some",
                "is_some_and",
                "len",
                "map",
                "map_err",
                "matches!",
                "ok_or",
                "ordinary_source_preprocessor",
                "prepare_authorized_reasoning_coverage",
                "prepare_authorized_reasoning_source",
                "preprocess",
                "reasoning_preparation_partial_reason",
                "redact_owned_bounded",
                "std::mem::take",
                "then_some",
                "u64::try_from",
                "unwrap_or",
                "usize::try_from",
                "zeroize",
            ]),
            set(&[
                "AuthorizedReasoningSourceOrigin::AuthorizedExport",
                "CaptureSnapshotCompleteness::Complete",
                "CaptureSnapshotCompleteness::Partial",
                "CaptureSnapshotPartialReason::DeadlineExceeded",
                "CaptureSnapshotPartialReason::SourceAuthorizationMismatch",
                "CaptureSnapshotPartialReason::SourceEmpty",
                "CaptureSnapshotPartialReason::SourceReadError",
                "CaptureSnapshotSourceKind::TrustedExport",
                "ReasoningSourceDisposition::Ordinary",
                "ReasoningSourceDisposition::Unproven",
                "u64::MAX",
                "usize::MAX",
            ]),
            set(&["None", "reasoning_preparation_partial_reason"]),
        ),
    ] {
        let found: Vec<_> = snapshot
            .iter()
            .filter(|(found, ..)| found == name)
            .collect();
        assert_eq!(found.len(), 1, "{name} is defined once");
        let names = &found[0].1;
        assert_eq!(
            names.callees(),
            callees,
            "{name}: the frozen Pi capture path changed"
        );
        assert_eq!(
            names.paths, paths,
            "{name}: the frozen Pi capture path changed"
        );
        let outside: BTreeSet<String> = names
            .values
            .iter()
            .filter(|value| !names.locals.contains(*value))
            .cloned()
            .collect();
        assert_eq!(
            outside, values,
            "{name}: the frozen Pi capture path changed"
        );
    }
    // Every function a Pi snapshot passes through in the snapshot owner (the
    // two generic entry points, the three framed stages, the four Pi wiring
    // functions, `source_projection`), every method of the snapshot and its
    // projection that a consumer may call, and every other snapshot-owner
    // function they call by name at any depth, is pinned by the SHA-256 of
    // its source with whitespace removed. Any change there (a call made
    // twice, a reordering, a new statement, a new call out of the file)
    // fails; re-review what a Pi snapshot runs, then update the pin.
    // Functions in other modules that these call are the generic owners
    // every provider shares (redaction, coverage, the authorized read).
    {
        use sha2::{Digest, Sha256};
        use syn::spanned::Spanned;

        const PATH: &[&str] = &[
            "capture_authorized",
            "capture_authorized_framed",
            "capture_authorized_with_reasoning_context",
            "capture_authorized_with_reasoning_context_framed",
            "capture_authorized_bytes",
            "apply_pi_worker_frame",
            "capture_authorized_pi_bytes",
            "capture_authorized_pi_bytes_until",
            "pi_readable_thinking",
            // Named by the read stage; it matches the `TranscriptSource::File`
            // variant (so `File` appears) but reads nothing.
            "source_projection",
        ];
        // The snapshot-owner functions the path calls by name, at any depth.
        let by_name: std::collections::BTreeMap<String, Vec<&PiWiringNames>> = snapshot
            .iter()
            .fold(Default::default(), |mut map, (name, names, ..)| {
                map.entry(name.clone()).or_default().push(names);
                map
            });
        // A Pi snapshot's consumers may call any method of the snapshot and
        // of its projection (`into_redacted_transcript`, `try_into_ordinary`,
        // `safe_projection`, ...), so every method of those types is a root
        // too.
        let mut roots: Vec<String> = PATH.iter().map(|name| name.to_string()).collect();
        for item in &snapshot_items {
            if let syn::Item::Impl(block) = item
                && let syn::Type::Path(path) = &*block.self_ty
                && path.path.segments.last().is_some_and(|segment| {
                    pi_ident(&segment.ident) == "CaptureSnapshot"
                        || pi_ident(&segment.ident) == "CaptureSnapshotProjection"
                })
            {
                for member in &block.items {
                    if let syn::ImplItem::Fn(method) = member {
                        roots.push(pi_ident(&method.sig.ident));
                    }
                }
            }
        }
        // Methods a snapshot gains another way (a trait's default methods
        // through a new `impl Trait for CaptureSnapshot {}`, here or anywhere
        // in the crate, also through a type alias) are not roots, so the impls
        // of both types are frozen crate-wide, and the snapshot owner defines
        // no trait.
        let mut impls = BTreeSet::new();
        let mut traits = Vec::new();
        let mut files = vec![root.join("src")];
        while let Some(path) = files.pop() {
            if path.is_dir() {
                for entry in std::fs::read_dir(&path).expect("read src directory") {
                    files.push(entry.expect("src entry").path());
                }
                continue;
            }
            if path.extension().is_none_or(|extension| extension != "rs") {
                continue;
            }
            let text = std::fs::read_to_string(&path).expect("read source file");
            if !text.contains("CaptureSnapshot") {
                continue;
            }
            let relative = path
                .strip_prefix(root)
                .expect("under the root")
                .display()
                .to_string();
            let file = syn::parse_file(&text).expect("parse source file");
            struct Impls<'a> {
                relative: &'a str,
                aliases: BTreeSet<String>,
                found: Vec<String>,
                traits: Vec<String>,
                collect_aliases: bool,
            }
            impl<'ast> syn::visit::Visit<'ast> for Impls<'_> {
                fn visit_item_type(&mut self, alias: &'ast syn::ItemType) {
                    if self.collect_aliases
                        && let syn::Type::Path(path) = &*alias.ty
                        && path.path.segments.last().is_some_and(|segment| {
                            pi_ident(&segment.ident) == "CaptureSnapshot"
                                || pi_ident(&segment.ident) == "CaptureSnapshotProjection"
                        })
                    {
                        self.aliases.insert(pi_ident(&alias.ident));
                    }
                    syn::visit::visit_item_type(self, alias);
                }
                fn visit_item_impl(&mut self, block: &'ast syn::ItemImpl) {
                    if !self.collect_aliases
                        && let syn::Type::Path(path) = &*block.self_ty
                        && let Some(segment) = path.path.segments.last()
                    {
                        let name = pi_ident(&segment.ident);
                        if name == "CaptureSnapshot"
                            || name == "CaptureSnapshotProjection"
                            || self.aliases.contains(&name)
                        {
                            let header = match &block.trait_ {
                                Some((_, path, _)) => format!("<{name} as {}>", pi_path_text(path)),
                                None => name,
                            };
                            self.found.push(format!("{}: {header}", self.relative));
                        }
                    }
                    syn::visit::visit_item_impl(self, block);
                }
                fn visit_item_trait(&mut self, item: &'ast syn::ItemTrait) {
                    if !self.collect_aliases {
                        self.traits
                            .push(format!("{}: trait {}", self.relative, item.ident));
                    }
                    syn::visit::visit_item_trait(self, item);
                }
            }
            let mut scan = Impls {
                relative: &relative,
                aliases: BTreeSet::new(),
                found: Vec::new(),
                traits: Vec::new(),
                collect_aliases: true,
            };
            scan.visit_file(&file);
            scan.collect_aliases = false;
            scan.visit_file(&file);
            impls.extend(scan.found);
            if relative.ends_with("capture/snapshot.rs") {
                traits.extend(scan.traits);
            }
        }
        assert_eq!(
            impls,
            set(&[
                "src/internal/ai/capture/snapshot.rs: <CaptureSnapshot as fmt::Debug>",
                "src/internal/ai/capture/snapshot.rs: CaptureSnapshot",
                "src/internal/ai/capture/snapshot.rs: CaptureSnapshotProjection",
            ]),
            "the impls of the Pi snapshot types changed: review their callable methods, then update this list"
        );
        assert!(traits.is_empty(), "the snapshot owner defines {traits:?}");
        // The same freeze as text, over every `src/` file, so an impl written
        // inside a macro, a type alias to either type (an alias chain starts
        // with one), or a `use .. as ..` rename fails too: those would let an
        // impl name the type without its name.
        let impl_header = regex::Regex::new(
            r"(?m)^[ \t]*(?:unsafe[ \t]+)?impl\b[^{;]*\bCaptureSnapshot(?:Projection)?\b",
        )
        .expect("impl pattern");
        let alias = regex::Regex::new(
            r"\btype\s+\w+(?:<[^>]*>)?\s*=[^;]*\bCaptureSnapshot(?:Projection)?\b",
        )
        .expect("alias pattern");
        let rename = regex::Regex::new(r"\bCaptureSnapshot(?:Projection)?\s+as\s+\w+")
            .expect("rename pattern");
        let mut headers = BTreeSet::new();
        let mut renamed = Vec::new();
        let mut files = vec![root.join("src")];
        while let Some(path) = files.pop() {
            if path.is_dir() {
                for entry in std::fs::read_dir(&path).expect("read src directory") {
                    files.push(entry.expect("src entry").path());
                }
                continue;
            }
            if path.extension().is_none_or(|extension| extension != "rs") {
                continue;
            }
            let text = std::fs::read_to_string(&path).expect("read source file");
            let relative = path
                .strip_prefix(root)
                .expect("under the root")
                .display()
                .to_string();
            for found in impl_header.find_iter(&text) {
                headers.insert(format!(
                    "{relative}: {}",
                    found
                        .as_str()
                        .split_whitespace()
                        .collect::<Vec<_>>()
                        .join(" ")
                ));
            }
            for found in alias.find_iter(&text).chain(rename.find_iter(&text)) {
                renamed.push(format!("{relative}: {}", found.as_str()));
            }
        }
        assert_eq!(
            headers,
            set(&[
                "src/internal/ai/capture/snapshot.rs: impl CaptureSnapshot",
                "src/internal/ai/capture/snapshot.rs: impl CaptureSnapshotProjection",
                "src/internal/ai/capture/snapshot.rs: impl fmt::Debug for CaptureSnapshot",
            ]),
            "the impls of the Pi snapshot types changed: review their callable methods, then update this list"
        );
        assert!(
            renamed.is_empty(),
            "the Pi snapshot types are aliased or renamed: {renamed:?}"
        );
        // A macro invocation that names either type (for example one that
        // generates `impl Trait for $t` from its argument) fails closed: no
        // production code invokes a macro with them today.
        struct NamingMacros<'a> {
            relative: &'a str,
            found: Vec<String>,
        }
        impl<'ast> syn::visit::Visit<'ast> for NamingMacros<'_> {
            fn visit_macro(&mut self, mac: &'ast syn::Macro) {
                fn names(tokens: proc_macro2::TokenStream) -> bool {
                    tokens.into_iter().any(|token| match token {
                        proc_macro2::TokenTree::Ident(ident) => {
                            pi_ident(&ident) == "CaptureSnapshot"
                                || pi_ident(&ident) == "CaptureSnapshotProjection"
                        }
                        proc_macro2::TokenTree::Group(group) => names(group.stream()),
                        _ => false,
                    })
                }
                if names(mac.tokens.clone()) {
                    self.found
                        .push(format!("{}: {}!", self.relative, pi_path_text(&mac.path)));
                }
                syn::visit::visit_macro(self, mac);
            }
        }
        let mut naming = Vec::new();
        let mut files = vec![root.join("src")];
        while let Some(path) = files.pop() {
            if path.is_dir() {
                for entry in std::fs::read_dir(&path).expect("read src directory") {
                    files.push(entry.expect("src entry").path());
                }
                continue;
            }
            if path.extension().is_none_or(|extension| extension != "rs") {
                continue;
            }
            let text = std::fs::read_to_string(&path).expect("read source file");
            if !text.contains("CaptureSnapshot") {
                continue;
            }
            let relative = path
                .strip_prefix(root)
                .expect("under the root")
                .display()
                .to_string();
            let mut scan = NamingMacros {
                relative: &relative,
                found: Vec::new(),
            };
            for item in pi_production_items(&text) {
                scan.visit_item(&item);
            }
            naming.extend(scan.found);
        }
        assert!(
            naming.is_empty(),
            "a macro invocation names a Pi snapshot type: {naming:?}"
        );
        // A macro the checked code invokes (`matches!`, `vec!`, ...) must
        // resolve to the standard one: no `macro_rules!` in `src/` takes such
        // a name (raw spellings included) except `pi.rs`'s own `poll!`, the
        // snapshot owner defines no macro, nothing uses `#[macro_use]`, and
        // neither file imports a name it invokes as a macro.
        let invoked: BTreeSet<String> = snapshot_items
            .iter()
            .chain(
                pi_production_items(&read("src/internal/ai/observed_agents/builtin/pi.rs")).iter(),
            )
            .flat_map(|item| {
                PiWiringNames::of(|names| names.visit_item(item))
                    .calls
                    .into_iter()
                    .filter_map(|call| call.strip_suffix('!').map(str::to_string))
                    .collect::<Vec<_>>()
            })
            .collect();
        assert!(invoked.contains("matches") && invoked.contains("vec") && invoked.contains("poll"));
        // The outside helpers' macros too.
        let invoked: BTreeSet<String> = invoked
            .into_iter()
            .chain(["format", "unreachable"].map(str::to_string))
            .collect();
        let definition = regex::Regex::new(r"macro_rules!\s*(?:r#)?([A-Za-z_][A-Za-z0-9_]*)")
            .expect("macro definition pattern");
        let mut shadowing = Vec::new();
        let mut poll_definitions = 0;
        let mut files = vec![root.join("src")];
        while let Some(path) = files.pop() {
            if path.is_dir() {
                for entry in std::fs::read_dir(&path).expect("read src directory") {
                    files.push(entry.expect("src entry").path());
                }
                continue;
            }
            if path.extension().is_none_or(|extension| extension != "rs") {
                continue;
            }
            let text = std::fs::read_to_string(&path).expect("read source file");
            let relative = path
                .strip_prefix(root)
                .expect("under the root")
                .display()
                .to_string();
            if text.contains("#[macro_use]") {
                shadowing.push(format!("{relative}: #[macro_use]"));
            }
            for found in definition.captures_iter(&text) {
                let name = &found[1];
                let own_poll = name == "poll" && relative.ends_with("builtin/pi.rs");
                if own_poll {
                    poll_definitions += 1;
                }
                if (invoked.contains(name) && !own_poll)
                    || relative.ends_with("capture/snapshot.rs")
                {
                    shadowing.push(format!("{relative}: macro_rules! {name}"));
                }
            }
        }
        for relative in [
            "src/internal/ai/capture/snapshot.rs",
            "src/internal/ai/observed_agents/builtin/pi.rs",
        ] {
            struct Imported(BTreeSet<String>);
            impl<'ast> syn::visit::Visit<'ast> for Imported {
                fn visit_use_name(&mut self, name: &'ast syn::UseName) {
                    self.0.insert(pi_ident(&name.ident));
                }
                fn visit_use_rename(&mut self, rename: &'ast syn::UseRename) {
                    self.0.insert(pi_ident(&rename.rename));
                }
            }
            let mut imported = Imported(BTreeSet::new());
            for item in pi_production_items(&read(relative)) {
                imported.visit_item(&item);
            }
            for name in imported.0.intersection(&invoked) {
                shadowing.push(format!("{relative}: use .. {name}"));
            }
        }
        assert!(
            shadowing.is_empty(),
            "a macro the Pi path invokes may be shadowed: {shadowing:?}"
        );
        assert_eq!(poll_definitions, 1, "pi.rs defines `poll!` exactly once");
        // An import can redirect a name the checked code calls (`use
        // wrapper as parse_canon_value`, `use other as serde_json`), so the
        // imports of `pi.rs` and the snapshot owner are pinned, and the
        // helper files keep their renames and declare no `serde_json`.
        {
            use sha2::{Digest, Sha256};

            for (relative, pin) in [
                (
                    "src/internal/ai/observed_agents/builtin/pi.rs",
                    "825797ace9f306ef917d0106e2c6d5f475ff2ffb427085e8d56fdce8c9d180d7",
                ),
                (
                    "src/internal/ai/capture/snapshot.rs",
                    "73edd6af9c9cae65e16958f41756df9d1aeb55c6371e10bd452c2e45a6cee87c",
                ),
            ] {
                let (entries, _) = pi_uses(&pi_production_items(&read(relative)));
                let joined = entries.iter().cloned().collect::<Vec<_>>().join("\n");
                let digest = hex::encode(Sha256::digest(joined.as_bytes()));
                assert_eq!(
                    digest, pin,
                    "{relative} imports changed: review what each imported name resolves to, then update the pin; now {entries:#?}"
                );
            }
            for (relative, renames) in [
                (
                    "src/internal/ai/observed_agents/coverage.rs",
                    vec!["Error", "Error"],
                ),
                (
                    "src/internal/ai/observed_agents/reasoning.rs",
                    vec!["_", "BASE64_STANDARD"],
                ),
            ] {
                let items = pi_production_items(&read(relative));
                let (_, aliases) = pi_uses(&items);
                assert_eq!(aliases, renames, "{relative} import renames changed");
                struct Declared(BTreeSet<String>);
                impl<'ast> syn::visit::Visit<'ast> for Declared {
                    fn visit_item_mod(&mut self, item: &'ast syn::ItemMod) {
                        self.0.insert(pi_ident(&item.ident));
                        syn::visit::visit_item_mod(self, item);
                    }
                    fn visit_item_fn(&mut self, item: &'ast syn::ItemFn) {
                        self.0.insert(pi_ident(&item.sig.ident));
                        syn::visit::visit_item_fn(self, item);
                    }
                    fn visit_item_struct(&mut self, item: &'ast syn::ItemStruct) {
                        self.0.insert(pi_ident(&item.ident));
                        syn::visit::visit_item_struct(self, item);
                    }
                    fn visit_item_enum(&mut self, item: &'ast syn::ItemEnum) {
                        self.0.insert(pi_ident(&item.ident));
                        syn::visit::visit_item_enum(self, item);
                    }
                }
                let mut declared = Declared(BTreeSet::new());
                for item in &items {
                    declared.visit_item(item);
                }
                assert!(
                    !declared.0.contains("serde_json"),
                    "{relative} declares `serde_json`"
                );
            }
            assert!(
                !snapshot_items
                    .iter()
                    .any(|item| matches!(item, syn::Item::Mod(_))),
                "the snapshot owner declares a production module"
            );
        }
        // Closing the shadowing and code-generation classes at once: the
        // whole production text of the two files this card wires and of the
        // two whose helpers and types the Pi frame uses (any new item, alias,
        // nested declaration, import, derive, or macro invocation there that
        // could shadow a name or generate code changes it); no glob import
        // there (a glob can shadow the prelude); what outside those files can
        // still change what their types run, frozen (see
        // `pi_pinned_files_are_closed`); no `extern crate` anywhere (it can
        // rename a crate such as `serde_json`); and the outside helpers
        // pinned, with every name they rely on declared in their file exactly
        // as often as now.
        {
            use sha2::{Digest, Sha256};
            use syn::spanned::Spanned;

            let digest = |source: &str, span: proc_macro2::Span| -> String {
                let text: String = pi_span_text(source, span)
                    .chars()
                    .filter(|character| !character.is_whitespace())
                    .collect();
                hex::encode(Sha256::digest(text.as_bytes()))
            };
            for (relative, pin) in [
                (
                    "src/internal/ai/observed_agents/builtin/pi.rs",
                    "afe49c5e24bba9c46e5a6937230d044370e6506e2501dd3823a0f94346355bfc",
                ),
                (
                    "src/internal/ai/capture/snapshot.rs",
                    "82a1a2fe43a9e4e7567dd8d6c8f15d5fd1db57ffffb438ff86a9f0588f5fd532",
                ),
                (
                    "src/internal/ai/observed_agents/coverage.rs",
                    "ed7364061ba42355a8814199e010875bdf0862045f295e9a14cc92ecdd9737a0",
                ),
                (
                    "src/internal/ai/observed_agents/reasoning.rs",
                    "d404c12860b47aca7cf1ae6840c37f7f2e6d240e13dfd746d17ea67a79d16fa4",
                ),
            ] {
                let source = read(relative);
                let items = pi_production_items(&source);
                let text: String = items
                    .iter()
                    .map(|item| {
                        pi_span_text(&source, item.span())
                            .chars()
                            .filter(|character| !character.is_whitespace())
                            .collect::<String>()
                    })
                    .collect::<Vec<_>>()
                    .join("\n");
                let file_digest = hex::encode(Sha256::digest(text.as_bytes()));
                assert_eq!(
                    file_digest, pin,
                    "{relative} production code changed: re-review what a Pi snapshot runs, then update the pin"
                );
                let (entries, _) = pi_uses(&items);
                let globs: Vec<_> = entries
                    .iter()
                    .filter(|entry| entry.ends_with('*'))
                    .collect();
                assert!(globs.is_empty(), "{relative} glob-imports {globs:?}");
            }
            pi_pinned_files_are_closed(root);
            let extern_crate =
                regex::Regex::new(r"(?m)^\s*(?:pub(?:\([^)]*\))?\s+)?extern\s+crate\b")
                    .expect("extern crate pattern");
            let mut files = vec![root.join("src")];
            while let Some(path) = files.pop() {
                if path.is_dir() {
                    for entry in std::fs::read_dir(&path).expect("read src directory") {
                        files.push(entry.expect("src entry").path());
                    }
                    continue;
                }
                if path.extension().is_some_and(|extension| extension == "rs") {
                    let text = std::fs::read_to_string(&path).expect("read source file");
                    assert!(
                        !extern_crate.is_match(&text),
                        "{} uses `extern crate`",
                        path.display()
                    );
                }
            }
            let mut helper_pins = std::collections::BTreeMap::new();
            for (relative, helpers, definitions) in [
                (
                    "src/internal/ai/observed_agents/coverage.rs",
                    &["ordinal_key", "parse_canon_value", "valid_provider_turn_id"][..],
                    &["CanonValue"][..],
                ),
                (
                    "src/internal/ai/observed_agents/reasoning.rs",
                    &["classify_provider_visible_reasoning", "from_classification"][..],
                    &[
                        "ProviderVisibleText",
                        "ReasoningAvailability",
                        "ReasoningProvider",
                        "ReasoningRecord",
                        "ReasoningSourceKind",
                    ][..],
                ),
            ] {
                let source = read(relative);
                let items = pi_production_items(&source);
                let mut relied = BTreeSet::new();
                for item in &items {
                    match item {
                        syn::Item::Fn(function)
                            if helpers.contains(&pi_ident(&function.sig.ident).as_str()) =>
                        {
                            helper_pins.insert(
                                pi_ident(&function.sig.ident),
                                digest(&source, function.span()),
                            );
                            relied.extend(pi_relied_names(|names| names.visit_item_fn(function)));
                        }
                        syn::Item::Impl(block) => {
                            for member in &block.items {
                                if let syn::ImplItem::Fn(method) = member
                                    && helpers.contains(&pi_ident(&method.sig.ident).as_str())
                                {
                                    helper_pins.insert(
                                        pi_ident(&method.sig.ident),
                                        digest(&source, method.span()),
                                    );
                                    relied.extend(pi_relied_names(|names| {
                                        names.visit_impl_item_fn(method)
                                    }));
                                }
                            }
                        }
                        _ => {}
                    }
                }
                let declared = pi_declared_names(&items);
                for name in &relied {
                    let expected = usize::from(definitions.contains(&name.as_str()));
                    assert_eq!(
                        declared.get(name).copied().unwrap_or(0),
                        expected,
                        "{relative} declares `{name}`, which a checked helper relies on"
                    );
                }
            }
            let expected_helpers: std::collections::BTreeMap<String, String> = [
                (
                    "classify_provider_visible_reasoning",
                    "89c74009721da7d5dbe5b694dd2a5fd1664f8a6a435eb32e49f5a7c13510ddb3",
                ),
                (
                    "from_classification",
                    "d6da7837294f54f793ab061d106ad603aef076b2025d1f72e191396aa82d076a",
                ),
                (
                    "ordinal_key",
                    "d2f9fb1365b8b0b4ebb74b73ea7bc484ae2d3d9ff378e255f3885d8c798bff84",
                ),
                (
                    "parse_canon_value",
                    "7bb5ed275364788e4d29d4f2d2ddc3d0481821a994666d18b456a81ec6c33c22",
                ),
                (
                    "valid_provider_turn_id",
                    "9c8b6c1c002f41879b8ffca6ef195561a695853a33f42ae33aa5b1abd7cf5bb8",
                ),
            ]
            .into_iter()
            .map(|(name, digest)| (name.to_string(), digest.to_string()))
            .collect();
            assert_eq!(
                helper_pins, expected_helpers,
                "a helper the Pi frame calls changed: re-review it, then update the pin"
            );
        }
        assert!(roots.iter().any(|root| root == "into_redacted_transcript"));
        assert!(roots.iter().any(|root| root == "try_into_ordinary"));
        assert!(roots.iter().any(|root| root == "safe_projection"));
        let mut reached = BTreeSet::new();
        let mut pending = roots;
        while let Some(name) = pending.pop() {
            if !by_name.contains_key(&name) || !reached.insert(name.clone()) {
                continue;
            }
            for names in &by_name[&name] {
                for callee in names.callees().iter().chain(&names.values) {
                    let last = callee
                        .rsplit("::")
                        .next()
                        .unwrap_or_default()
                        .trim_end_matches('!');
                    if by_name.contains_key(last) {
                        pending.push(last.to_string());
                    }
                }
            }
        }
        assert!(PATH.iter().all(|name| reached.contains(*name)));
        assert!(reached.contains("prepare_authorized_reasoning_source"));
        assert!(reached.contains("deadline_elapsed"));
        let digest = |span: proc_macro2::Span| -> String {
            let text: String = pi_span_text(&snapshot_source, span)
                .chars()
                .filter(|character| !character.is_whitespace())
                .collect();
            hex::encode(Sha256::digest(text.as_bytes()))
        };
        let mut pinned = std::collections::BTreeMap::new();
        // The two snapshot types' definitions (derives and attributes
        // included) and their whole impl blocks (attributes included).
        for item in &snapshot_items {
            match item {
                syn::Item::Struct(definition)
                    if pi_ident(&definition.ident) == "CaptureSnapshot"
                        || pi_ident(&definition.ident) == "CaptureSnapshotProjection" =>
                {
                    pinned.insert(
                        format!("struct {}", definition.ident),
                        digest(definition.span()),
                    );
                }
                syn::Item::Impl(block)
                    if matches!(&*block.self_ty, syn::Type::Path(path)
                    if path.path.segments.last().is_some_and(|segment| {
                        pi_ident(&segment.ident) == "CaptureSnapshot" || pi_ident(&segment.ident) == "CaptureSnapshotProjection"
                    })) =>
                {
                    let owner = match &*block.self_ty {
                        syn::Type::Path(path) => pi_path_text(&path.path),
                        _ => "<type>".to_string(),
                    };
                    let key = match &block.trait_ {
                        Some((_, path, _)) => {
                            format!("impl block <{owner} as {}>", pi_path_text(path))
                        }
                        None => format!("impl block {owner}"),
                    };
                    pinned.insert(key, digest(block.span()));
                }
                _ => {}
            }
        }
        for item in &snapshot_items {
            match item {
                syn::Item::Fn(function) if reached.contains(&pi_ident(&function.sig.ident)) => {
                    pinned.insert(pi_ident(&function.sig.ident), digest(function.span()));
                }
                syn::Item::Impl(block) => {
                    let owner = match &*block.self_ty {
                        syn::Type::Path(path) => pi_path_text(&path.path),
                        _ => "<type>".to_string(),
                    };
                    let owner = match &block.trait_ {
                        Some((_, path, _)) => format!("<{owner} as {}>", pi_path_text(path)),
                        None => owner,
                    };
                    for member in &block.items {
                        if let syn::ImplItem::Fn(method) = member
                            && reached.contains(&pi_ident(&method.sig.ident))
                        {
                            pinned.insert(
                                format!("{owner}::{}", method.sig.ident),
                                digest(method.span()),
                            );
                        }
                    }
                }
                _ => {}
            }
        }
        let expected: std::collections::BTreeMap<String, String> = [
            (
                "<CaptureSnapshot as fmt::Debug>::fmt",
                "f46aac1247b0b0f23786b9d0eeb4e68d889d580c4feaabbf93f3a683fd1458ee",
            ),
            (
                "<CaptureSnapshotPolicy as Default>::default",
                "73472f8aad067eafc8e5b388b4ead93f8c4266d8f8a83c6cec73c16cc959b373",
            ),
            (
                "<CaptureSnapshotSource as fmt::Debug>::fmt",
                "9c12249321fda551241bae5a9cb7422abd6d1f3a77a9e9cfa4164eb5e6e226c4",
            ),
            (
                "AuthorizedReasoningSourceContext::has_verified_mapping",
                "2e3745819d58a008abf78f27d4b0556af0120566a46730f987382c564599c02f",
            ),
            (
                "AuthorizedReasoningSourceContext::ordinary",
                "8745e8efc8e8b03c957a93b99f5d75a407a6215f8aed4252c113d2f18d72203c",
            ),
            (
                "CaptureSnapshot::bind_source_commitment",
                "3e330ec3b8ea40fa276368400f71efecc0d2af2d41ba9a040f379d3d9057b8f6",
            ),
            (
                "CaptureSnapshot::clear_non_durable_source_digest",
                "49e75c13fa3613a3d6daf5485429f50feffc4bad99b632dd9dcbfc1e68984e5d",
            ),
            (
                "CaptureSnapshot::complete",
                "c69daa713af16820a82fe10d5f8d843968b8cef2f38d4eb0e5017d5f125ad988",
            ),
            (
                "CaptureSnapshot::completeness",
                "489cac86f3ae5dccedf8b15ba11937f3f76dfc02adf17d00e8009f56a39ebdbe",
            ),
            (
                "CaptureSnapshot::downgrade_for_commitment_failure",
                "4226b898cd0af3cfcc8804c19f53f89ee02fd4de48f3673a925ec190352e632a",
            ),
            (
                "CaptureSnapshot::has_verified_ordinary_partial_permit",
                "e6830719750f8935064d232bf5645aa1f7849ab4e085859df3514ebd0253530e",
            ),
            (
                "CaptureSnapshot::hermes_readable_thinking",
                "238dee06740121e4ba4693433abf40009e49d09430b8fa062d390f69fc9d8710",
            ),
            (
                "CaptureSnapshot::into_checkpoint_parts",
                "9c074cfbc1fca84d7c47e3960bb8bab5c8343a059aee1272aa6f72acc31b8fbd",
            ),
            (
                "CaptureSnapshot::into_redacted_transcript",
                "8c7e4c8e88b835dd2746d5ec1718b13213a6792ec9b2f272fdef9d8b07ac5334",
            ),
            (
                "CaptureSnapshot::partial",
                "b24341769eddacd7e2a2b641aa25be226dc0cee0757fcba5075e0abbd5a91161",
            ),
            (
                "CaptureSnapshot::partial_reason",
                "5021ae874cd3692821b04818c81084cf55d4c426d1725dc87651725ba8968b3d",
            ),
            (
                "CaptureSnapshot::pi_readable_thinking",
                "dd7fc358cad8fdf6d10b858c506aba6f3bfaaf99430aaec4f49f6cdd0872382d",
            ),
            (
                "CaptureSnapshot::redacted_digest_preimage",
                "22e2e6e9df109228bfba9492cc1548fe0b465894b5c1d0c714a4f0d9eb7ab5a8",
            ),
            (
                "CaptureSnapshot::redaction_report",
                "8014ff283a75dba8a6a9bcbf34c9a24a86f1bb61f0261d2550e03ff2d06e41e2",
            ),
            (
                "CaptureSnapshot::rejects_capture_source_consumer",
                "e9a4d53aa506c7680a2e59203fae3bb15cb1c27db437f3afc8205ba0985fe153",
            ),
            (
                "CaptureSnapshot::rejects_ordinary_source_consumer",
                "ba89abda556c62eeafb8abaccddebbe2e00d6a9c2296ea086bc4973546de58de",
            ),
            (
                "CaptureSnapshot::safe_projection",
                "c4c40c1e4e138efa26e3fe7478b410d946ffb4aeda5fe6f8e6befb5888bdcf61",
            ),
            (
                "CaptureSnapshot::take_coverage_runtime",
                "b339b998f37d982befaa2fef82daed5ae163983e1326fc609aa143500ae8fbb2",
            ),
            (
                "CaptureSnapshot::transcript",
                "940c14f4f7c63766be018dbe1138668cf1c7d79c122a865b6c243f21ce03b87a",
            ),
            (
                "CaptureSnapshot::try_into_ordinary",
                "e2c352a3e4e59d0c11751f98f3836348dcc9c6fe4aec46184d5c1e2053f6f24b",
            ),
            (
                "CaptureSnapshot::try_into_verified_ordinary_partial_in_scope_until",
                "111c302410d333c312a19d3a41210ab6b750c79f8a19082ecfa8e83a1b622ad3",
            ),
            (
                "CaptureSnapshotPolicy::deadline",
                "1e32eea9c1944c21f1ac818a1ad503b6460dcd55a74f55f4c59e85827373c8da",
            ),
            (
                "CaptureSnapshotPolicy::with_deadline",
                "3a4c95d44dd5ff75ea94619d46471af5db61426f5fccf84635472d58d2106528",
            ),
            (
                "CaptureSnapshotProjection::bind_source_commitment",
                "e6a8adcd1eb10469097ad36602619fdaddaef432dac7714003b95971d310d78f",
            ),
            (
                "CaptureSnapshotProjection::has_durable_source_commitment",
                "c3c8d0eeacb815a13d3064556038c8f24cc9e660570cca92ca27b0df9b63523a",
            ),
            (
                "CaptureSnapshotProjection::redacted_digest_preimage",
                "e9ce5df187cc541615d583864cf67b2424b2d1e023dd48d6398550c169a449ad",
            ),
            (
                "CaptureSnapshotProjection::source_commitment_is_durable_or_absent",
                "357264b6b54d85e3970e688ed59989270b3b035d167a12bf7e958e8bbe7b1759",
            ),
            (
                "CaptureSnapshotService::capture_authorized",
                "5a6e7c5fdef59432ca3808c71371ee1f4072fbac6bb1a6366e0fe84ad2e334bc",
            ),
            (
                "CaptureSnapshotService::capture_authorized_bytes",
                "ea7c5aa557335f83d88afd6df47a7aba6cea73f6cc164783f0229dc49e35a09d",
            ),
            (
                "CaptureSnapshotService::capture_authorized_framed",
                "ca35abb0f9375b17c9d9cd18a0e8ac6132d751220b9a47973a4d8953029c9fc1",
            ),
            (
                "CaptureSnapshotService::capture_authorized_with_reasoning_context",
                "44e8c014d849d9ee5c5703c9e1e84d59621bce9f8333da0ea40f293f0a617e6c",
            ),
            (
                "CaptureSnapshotService::capture_authorized_with_reasoning_context_framed",
                "b657c5e09dafa1a5662e186a51519847d975d7b54dc7b675c45769bc6962c430",
            ),
            (
                "VerifiedOrdinaryPartialBinding::capture_session",
                "ade2cfbbdfcd6c3a307d1e69598f578812dcbf8e21f119d6e18653ddaf633604",
            ),
            (
                "VerifiedOrdinaryPartialBinding::source_session",
                "2168e2b72438db00d05d8948ff48ce6b2c58af318f2b23f7878edce27a0e2dc9",
            ),
            (
                "VerifiedOrdinaryPartialSnapshot::projection",
                "b284e254478bd6d522bdad7794b69db7d731eee701b7a2e8a7e9f534f63ad842",
            ),
            (
                "VerifiedOrdinaryPartialSnapshot::redaction_report",
                "e4d1011c40c4583daf0d6c0528c1f9291cc9273cea869253be8a7a329524a5b1",
            ),
            (
                "VerifiedOrdinaryPartialSnapshot::transcript",
                "a7764a80b44ecf00bb7ad452b4df33a3385295d1164145be0635f2896bb47e05",
            ),
            (
                "apply_hermes_worker_frame",
                "9d48522f4006e40db8af1c6f11f2529f45865f3b9b153d2482b3d44cd7f76f5c",
            ),
            (
                "apply_pi_worker_frame",
                "98adb20b1e7bb53964468b0605ca11d66fc537e753ff01ea84c16e6c1cc79330",
            ),
            (
                "bind_source_commitment",
                "b8611d4dac097f28ef82e8d754322588f8477be709f8c0fbb2da690b107c46d0",
            ),
            (
                "capture_authorized_pi_bytes",
                "d3f0d9ee9912dfcde571e37c3b832afb2a9d4a47731a80d3b54182e3f37a6b58",
            ),
            (
                "capture_authorized_pi_bytes_until",
                "e34491e2a2feb102a44c4136794f88e0ddff6e543f68b753eda9fee287886fa5",
            ),
            (
                "clear_non_durable_source_digest",
                "99eb39299dfdbc453c768379275d6c2f5a7850a83c88f8665f28f22ca85934a6",
            ),
            (
                "deadline_elapsed",
                "43179453802df0182c226f9d2015525beffb6862dab62fdb9b3333682d0572d9",
            ),
            (
                "digest_hex",
                "3fad623b90203da56c7bc0cacc2027b06198e6e5b196359d3d6109d3cdaa32da",
            ),
            (
                "has_durable_source_commitment",
                "6f2faf223cbb9b3395f44ae42667a73ae8b2fe9039960e015eee7d78e3a6f3fe",
            ),
            (
                "impl block <CaptureSnapshot as fmt::Debug>",
                "6b8472c488a5fa52030ab3bbff0cc4726197042d04f4777304da3f5c01f7333d",
            ),
            (
                "impl block CaptureSnapshot",
                "d4ce0ca872f3e0405a3bffec413f5b2c8bacaf8efeadabeae40b13bcf1fb9148",
            ),
            (
                "impl block CaptureSnapshotProjection",
                "73b9cb620fd2a0d1da084aa4c9fa1ac0b4c1ca0848f851e9b4722066c677b263",
            ),
            (
                "is_durable_source_commitment",
                "b170a94b6f6073b6d16fbf3665296131e1012dfb40539715db6993ddd0ffa634",
            ),
            (
                "prepare_authorized_reasoning_source",
                "31d2703862bbd51368d253164f5d2d760d6bae3b1250cbc6b4f73861e68ea461",
            ),
            (
                "reasoning_preparation_partial_reason",
                "4b3b118d62f436a524c58ed607ccc365c62e7614f84c2ae7547b06afbf1033a5",
            ),
            (
                "redacted_digest_preimage",
                "fa47b0d664fda83e5ab2a991f21eec735c021d08b4169d715eabf888f3ca3ed2",
            ),
            (
                "source_commitment_is_durable_or_absent",
                "b1aa7fbf94386ef539bb7a89f4579195fc70efe072e46ea6b02df7df12c9a9a5",
            ),
            (
                "source_projection",
                "5ef5ba4032c728486040392fb2700afa8d5b2eb0f9f7956e840be60ad0805fa0",
            ),
            (
                "struct CaptureSnapshot",
                "35629726c20f836d2174b5afccb1eb02afeda6bca9f1db2e6aca147f8f28ba87",
            ),
            (
                "struct CaptureSnapshotProjection",
                "0a0569491a53cbd9bcc8f9831927bf9ddd40681fb6e387651b1fb16a9b606769",
            ),
            (
                "wipe_in_memory_source",
                "1d9753102d12dbf44aecf24ce6b7e30433c5132c6add200b369d1fc06b15a23d",
            ),
        ]
        .into_iter()
        .map(|(name, digest)| (name.to_string(), digest.to_string()))
        .collect();
        assert_eq!(
            pinned, expected,
            "the pinned Pi capture path changed: re-review what a Pi snapshot runs, then update the pin"
        );
    }
    // Pi is named in the snapshot owner only by the dispatch branch and the
    // two entry points: three `"pi"` literals, no `b"pi"`, and no Pi variant
    // path.
    struct PiMentions {
        literals: usize,
        variants: Vec<String>,
    }
    impl<'ast> syn::visit::Visit<'ast> for PiMentions {
        fn visit_lit_str(&mut self, lit: &'ast syn::LitStr) {
            if lit.value() == "pi" {
                self.literals += 1;
            }
        }
        fn visit_lit_byte_str(&mut self, lit: &'ast syn::LitByteStr) {
            if lit.value() == b"pi" {
                self.literals += 1;
            }
        }
        fn visit_path(&mut self, path: &'ast syn::Path) {
            if path
                .segments
                .last()
                .is_some_and(|segment| pi_ident(&segment.ident) == "Pi")
                && path.segments.len() > 1
            {
                self.variants.push(pi_path_text(path));
            }
            syn::visit::visit_path(self, path);
        }
        fn visit_macro(&mut self, mac: &'ast syn::Macro) {
            // A literal inside a macro counts too.
            fn literals(tokens: proc_macro2::TokenStream, count: &mut usize) {
                for token in tokens {
                    match token {
                        proc_macro2::TokenTree::Literal(literal)
                            if ["\"pi\"", "b\"pi\""].contains(&literal.to_string().as_str()) =>
                        {
                            *count += 1;
                        }
                        proc_macro2::TokenTree::Group(group) => literals(group.stream(), count),
                        _ => {}
                    }
                }
            }
            literals(mac.tokens.clone(), &mut self.literals);
            syn::visit::visit_macro(self, mac);
        }
    }
    let mut mentions = PiMentions {
        literals: 0,
        variants: Vec::new(),
    };
    for item in &snapshot_items {
        mentions.visit_item(item);
    }
    assert_eq!(
        mentions.literals, 3,
        "snapshot.rs names \"pi\" only for the wiring"
    );
    assert!(
        mentions.variants.is_empty(),
        "snapshot.rs names {:?}",
        mentions.variants
    );
    let names = PiWiringNames::of(|names| names.visit_expr_if(branch));
    assert_eq!(names.callees(), set(&["apply_pi_worker_frame"]));
    assert_eq!(names.paths, set(&["CaptureSnapshotCompleteness::Complete"]));
    assert!(names.forbidden().is_empty() && names.forbidden_types().is_empty());

    // Every pi.rs function and impl the wiring reaches.
    let pi_items = pi_production_items(&read("src/internal/ai/observed_agents/builtin/pi.rs"));
    let pi = PiItems::of(&pi_items);
    let start: BTreeSet<String> = snapshot
        .iter()
        .filter(|(name, ..)| {
            [
                "apply_pi_worker_frame",
                "capture_authorized_pi_bytes",
                "capture_authorized_pi_bytes_until",
                "pi_readable_thinking",
            ]
            .contains(&name.as_str())
        })
        .flat_map(|(_, names, ..)| names.next())
        .collect();
    assert!(
        pi.unknown.is_empty(),
        "pi.rs has top-level items the guard does not scan: {:?}",
        pi.unknown
    );
    let reached = pi.reached(&start);
    for expected in [
        "pi_worker_frame_with_deadline",
        "pi_worker_frame_authorized",
        "normalize_session",
        "project_content",
        "coverage_turns",
        "impl PiDeadline",
        "impl ItemChecks",
        "macro poll!",
        "value PI_WORKER_FRAME_MAX_BYTES",
        "value PI_NORMALIZE_MAX_BYTES",
        "definition PiDeadline",
        "definition PiWorkerFrame",
        "definition RawFields",
    ] {
        assert!(reached.contains_key(expected), "{expected} is not reached");
    }
    assert!(!reached.contains_key("discover_pi_sessions"));
    let helpers = [
        "classify_provider_visible_reasoning",
        "ordinal_key",
        "parse_canon_value",
        "valid_provider_turn_id",
    ];
    let pure_external = set(&[
        "BTreeMap::new",
        "BTreeSet::new",
        "E::custom",
        "Vec::new",
        "Vec::with_capacity",
        "serde::Deserializer::deserialize_map",
        "serde::Deserializer::deserialize_seq",
        "serde_json::Deserializer::from_str",
        "serde_json::Value::Object",
        "serde_json::from_slice",
        "serde_json::from_str",
        "std::cell::Cell::new",
        "std::collections::HashMap::new",
        "std::collections::HashMap::with_capacity",
        "std::iter::once",
        "std::str::from_utf8",
        "std::time::Instant::now",
        "vec!",
        // The targets of `RawFields` and `BranchLines`.
        "<alias BTreeMap>",
        "<alias std::collections::HashMap>",
    ]);
    let known = pi.known();
    let mut external = BTreeSet::new();
    for (name, names) in &reached {
        assert!(
            names.forbidden().is_empty(),
            "pi.rs {name} names {:?}",
            names.forbidden()
        );
        assert!(
            names.forbidden_types().is_empty(),
            "pi.rs {name} uses {:?}",
            names.forbidden_types()
        );
        external.extend(names.unresolved_calls(&known));
    }
    // Enum variants, std method references, and consts from outside pi.rs.
    let pure_values = set(&[
        "CanonValue::Null",
        "Completeness::Complete",
        "Completeness::Incomplete",
        "ReasoningProvider::Pi",
        "ReasoningSourceKind::Thinking",
        "Value::Null",
        "Value::as_i64",
        "Value::as_str",
        "serde_json::Value::as_str",
        "str::to_owned",
        "super::super::transcript_source::TRANSCRIPT_READ_HARD_CAP_BYTES",
        "u32::MAX",
        "u8::is_ascii_whitespace",
    ]);
    let allowed: BTreeSet<String> = pure_external
        .iter()
        .cloned()
        .chain(set(&helpers))
        .chain(pure_values)
        .collect();
    assert!(
        external.is_subset(&allowed),
        "pi.rs calls out to {:?}; review and list a new pure callee",
        external.difference(&allowed).collect::<Vec<_>>()
    );
    // The coverage and reasoning helpers it calls, checked the same way.
    for (relative, helper_names, helper_calls) in [
        (
            "src/internal/ai/observed_agents/coverage.rs",
            &["ordinal_key", "parse_canon_value", "valid_provider_turn_id"][..],
            set(&["format!", "serde_json::from_slice"]),
        ),
        (
            "src/internal/ai/observed_agents/reasoning.rs",
            &["classify_provider_visible_reasoning", "from_classification"][..],
            set(&["ProviderVisibleText::from_classification", "unreachable!"]),
        ),
    ] {
        let items = pi_production_items(&read(relative));
        let functions = pi_functions(&items);
        for helper in helper_names {
            let found: Vec<_> = functions
                .iter()
                .filter(|(name, ..)| name == helper)
                .collect();
            assert_eq!(found.len(), 1, "{relative} {helper} is defined once");
            let names = &found[0].1;
            assert!(
                names.forbidden().is_empty(),
                "{helper}: {:?}",
                names.forbidden()
            );
            assert!(
                names.forbidden_types().is_empty(),
                "{helper}: {:?}",
                names.forbidden_types()
            );
            let calls: BTreeSet<String> = names
                .calls
                .iter()
                .filter(|call| !["Some", "Ok", "Err"].contains(&call.as_str()))
                .cloned()
                .collect();
            assert!(
                calls.is_subset(&helper_calls),
                "{helper} calls {:?}",
                calls.difference(&helper_calls).collect::<Vec<_>>()
            );
            let outside_paths: Vec<_> = names
                .paths
                .iter()
                .filter(|path| path.as_str() != "ReasoningAvailability::ProviderVisible")
                .collect();
            assert!(outside_paths.is_empty(), "{helper} uses {outside_paths:?}");
            // It reads only its own bindings (a callee is checked above): no
            // static or const.
            let read: Vec<_> = names
                .values
                .iter()
                .filter(|value| {
                    !names.locals.contains(*value)
                        && !names.calls.contains(*value)
                        && value.as_str() != "None"
                })
                .collect();
            assert!(read.is_empty(), "{helper} reads {read:?}");
        }
    }
}
