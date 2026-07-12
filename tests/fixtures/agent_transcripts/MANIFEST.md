# Agent transcript fixtures — provenance manifest (AG-21 / plan.md A6)

Per `docs/development/tracing/plan.md` Task A6: every fixture group records
its source agent, CLI version, capture date and construction method so a
parse-assertion failure can be triaged as implementation regression vs
upstream format drift.

| fixture | agent slug | CLI version (dev machine) | date | method |
|---|---|---|---|---|
| `claude_code.jsonl` | `claude-code` | claude 2.1.201 | 2026-07-05 | 手工构造：按 Claude Code session JSONL 公开形态（`type`/`message.role`/`content` blocks/`usage`/`tool_use`）缩编；不含真实用户内容，token 数为虚构值 |
| `codex.jsonl` | `codex` | codex-cli 0.142.4 | 2026-07-05 | 手工构造：rollout JSONL 通用形态（`role:user` 文本、`model` 键、E6 形 usage 对象）；A6.5 真实采集后若形态有差须刷新本组并更新本表 |
| `opencode.json` | `opencode` | opencode 1.17.13 | 2026-07-05 | 手工构造：session export JSON（`messages[]` role/content/model）；同上刷新约定 |

刷新协议：A6.5 本地 smoke 观察到 transcript 格式差异时，按 §0.3.4 重新采集
（提交前 redact + 最小化），并更新本表的版本与 method 字段。

核对记录（无漂移确认，避免"静默满足"歧义）：

- 2026-07-12（darwin arm64）：A6.5 真实三 agent smoke 重跑，CLI 版本
  claude 2.1.207 / codex-cli 0.144.1 / opencode 1.17.18（均高于上表记录）。
  claude/codex 采集断言全绿（extraction present、transcript 非空、
  metadata-first 输出无正文泄露），opencode 保持 lifecycle-only pin
  （`extraction.present=false`，见 agent.md「OpenCode 安装流程契约」§5）——
  解析层未观察到 transcript/hook 格式漂移，判定无需刷新本组 fixture；
  上表版本字段保留为 fixture 字节的采集/构造来源。
