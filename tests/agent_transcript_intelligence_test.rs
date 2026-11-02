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
