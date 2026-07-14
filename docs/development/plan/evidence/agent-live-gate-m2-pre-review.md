# Live agent gate evidence — M2 (pre-review)

> plan-20260713「本机 live agent 执行验证门」固定字段记录。sanitized-only。

- release/tag: `pre-review`
- commit: `1fa5d0a`
- UTC time (evidence commit): 2026-07-13T17:02:37Z; the exact live-run
  instant was not retained in this sanitized pre-review record
- providers: `claude` (Claude Code CLI 2.1.207) real session store; `codex`
  real `~/.codex/sessions` store (layout verification only)
- scope: M2 = DR-01 flush-wait (live path) + DR-02 Claude discovery contract
  + DR-03 Codex rollout discovery contract
- procedure & results:
  - real Claude transcript (~451 KiB) ingested twice through the NEW live
    path (flush-wait now runs inside `resolve_transcript_source`): both
    ingests succeeded; repeat fully deduplicated (checkpoints = 1)
  - real `~/.claude/projects/` layout matches the DR-02 contract on this
    machine: slug dirs including `-run-media-eli-data-gitmono-libra`
    (pinned vector), sessions stored as `<uuid>.jsonl`
  - real `~/.codex/sessions/2026/07/…` matches the DR-03 contract:
    date-partitioned `YYYY/MM/DD` dirs holding
    `rollout-<timestamp>-<session-id>.jsonl`
- stable result: success
- note: DR-02/03 discovery functions are groundwork consumed by M4 import /
  M5 subagent scan; per-DR live exercise of the full lookup happens there.
  OpenCode not exercised (content source lands in M3).
