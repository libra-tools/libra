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
fn claude_codex_opencode_fixtures_extract_metadata_or_partial() {
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

    // Codex: prompts / model / tokens / skills (best-effort rollout form).
    let codex = agent_for(AgentKind::Codex);
    let data = fixture("codex.jsonl");
    let prompts = codex
        .as_prompt_extractor()
        .expect("codex prompts")
        .extract_prompts(&data, 0)
        .unwrap();
    assert_eq!(prompts.len(), 2);
    let usage = codex
        .as_token_calculator()
        .expect("codex tokens")
        .calculate_token_usage(&data, 0)
        .unwrap();
    assert_eq!(usage.total_tokens, Some(260));
    let model = codex
        .as_model_extractor()
        .expect("codex model")
        .extract_model(&data)
        .unwrap();
    assert_eq!(model.as_deref(), Some("gpt-5.3-codex"));
    // Codex format carries no worktree modification records — the
    // analyzer capability is deliberately not exposed.
    assert!(codex.as_transcript_analyzer().is_none());
    assert!(codex.as_subagent_aware_extractor().is_none());

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
