# Live agent gate evidence — M2 (pre-review)

> plan-20260713「本机 live agent 执行验证门」固定字段记录。sanitized-only。
> Gate identity: `LIBRA_RUN_LIVE_AGENT_GATE=1` + `--features test-live-agent`
> (registered this milestone; test target `tests/agent_live_gate_test.rs`).

- release/tag: `pre-review`
- commit: `4e0d33d` + R2 fixes (bounded fan-out, live gate registration)
- UTC time: 2026-07-14T01:36:15Z
- providers: `claude` (Claude Code CLI 2.1.207), `codex` (real local store)
- scope: M2 = DR-01 flush-wait (live path) + DR-02 Claude discovery +
  DR-03 Codex rollout discovery
- procedure & results:
  - real Claude transcript (~451 KiB) ingested twice through the live path
    (flush-wait active in `resolve_transcript_source`): success; repeat
    fully deduplicated (checkpoints = 1)
  - **real BY-ID lookups** via `LIBRA_RUN_LIVE_AGENT_GATE=1 cargo test
    --features test-live-agent --test agent_live_gate_test`:
    - `live_claude_session_resolves_by_id` — a real session id taken from
      this repo's real `~/.claude/projects/<slug>` dir resolved through
      `resolve_session_file`: **ok** (2/2 tests passed)
    - `live_codex_rollout_resolves_by_id` — a real session id extracted
      from a real rollout filename resolved through `find_codex_rollout`:
      **ok**
- stable result: success
- note: OpenCode not exercised (content source lands in M3).
