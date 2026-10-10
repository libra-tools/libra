# Agent transcript fixtures — provenance

Consumed by `tests/agent_transcript_intelligence_test.rs` (AG-21 / E6 / E7 /
A0-07). Assertion failures should be triaged against this table:
implementation regression vs upstream format drift.

**Provenance note (2026-07-15)**: the original fixture files were referenced
by the test suite but never committed to the repository, so a fresh checkout
could not run the target. These files are **synthetic reconstructions**
hand-authored against the frozen parser shapes in
`src/internal/ai/observed_agents/extract.rs` — they are *not* captures of
real agent sessions. Each file exercises exactly the values the test pins.

| File | Format modeled | Pinned expectations |
|---|---|---|
| `claude_code.jsonl` | Claude Code session JSONL (`type`/`uuid`/`timestamp` envelope, `message.content` string or block array, Claude-native `usage` keys `cache_creation_input_tokens`/`cache_read_input_tokens`, `tool_use` blocks with `input.file_path`) | 2 prompts; 1 `/review` skill event (`input_slash_command`); merged usage input=200 output=65 cached=40; model `claude-sonnet-5`; modified files `src/lib.rs`, `docs/readme.md` (in order); no `Task` tool so subagent-aware total equals session usage |
| `codex.jsonl` | Codex rollout JSONL (generic `message` wrapper records; E6 wire `usage` keys) | 2 prompts; 1 `/review` skill event; merged `total_tokens=260` (155+105); model `gpt-5.3-codex` |
| `opencode.json` | Synthetic OpenCode 2.0.26 native `{info,messages}` export; fixed official commit `9b4ec5714d481559990db0a816d5dec19541a814`, schema session-message.ts/session-transfer.ts, core session/transfer.ts and token-usage.ts; never live/native-origin proof | 2 user.text prompts; 1 `/review` skill event; assistant model.id `claude-sonnet-5`; tokens input=2 output=3 reasoning=4 cache.read=5 cache.write=6, cached=11 total=20 api_call_count=1; modified files `src/lib.rs`, `docs/readme.md`; snapshot files are project-relative |

Editing rules:

- Values are load-bearing: the test asserts exact prompt counts, token sums,
  model ids, file lists, and single-skill-event extraction. Update the test
  and this table together with any fixture change.
- The second user prompt in each fixture must NOT start with a curated skill
  command (`/review`, `/security-review`, `/simplify`) or contain a
  `<command-name>` tag, so exactly one skill event is extracted per fixture.
- Keep `claude_code.jsonl` free of `Task` tool_use blocks — introducing one
  flips `subagent_usage` to the session aggregate and marks the summary
  partial, breaking `total_token_usage_including_subagents` expectations.
