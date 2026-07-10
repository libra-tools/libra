//! AG-21 transcript intelligence over the first-batch adapters
//! (`docs/development/tracing/agent.md` E6/E7; plan.md Task A6).
//!
//! Fixtures live in `tests/fixtures/agent_transcripts/` with a provenance
//! manifest (`MANIFEST.md`) — assertion failures should be triaged against
//! that table (implementation regression vs upstream format drift).

use libra::internal::ai::observed_agents::{
    AgentKind, agent_for,
    extract::{self, CLAUDE_CODE_SKILL_REGISTRY, CODEX_SKILL_REGISTRY, OPENCODE_SKILL_REGISTRY},
};

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
