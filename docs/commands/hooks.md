# `libra hooks`

Internal entry point invoked by external AI agent hook configurations
that capture lifecycle events (session start, prompt submission, tool
use, model updates, compaction, stop, session end) into the libra
session store. Operators almost never type `libra hooks ...` directly —
the hook configs installed by `libra agent enable` reference these
sub-commands.

## Synopsis

```
libra hooks claude   {session-start|prompt|tool-use|model-update|compaction|stop|session-end}
libra hooks codex    {session-start|prompt|tool-use|model-update|compaction|stop|session-end|subagent-start|subagent-end}
libra hooks gemini   <event>   # rejected with a hint: gemini is uninstall-only (AG-17)
```

## Description

`libra hooks` is the **hidden** (`hide = true` in clap) compatibility
surface invoked by Claude Code / Codex hook configs. Each invocation
reads a single hook event payload as JSON on stdin, validates it
against the provider-specific schema, and records the redacted
projection into the external-agent capture store (`agent_session` /
`agent_checkpoint` + `refs/libra/traces`).

The command is hidden because:

- It is not part of the user-facing CLI contract — it must remain
  invocable by hook configs whose format is owned by the upstream
  provider (Claude Code / Codex), not by Libra. Treating it as a
  public surface would require freezing the JSON payload schema
  Libra-side, which is impossible because the providers can change
  the payload at any release.
- The events it produces are read by `libra agent session list`,
  `libra agent checkpoint *`, and `libra agent doctor`. The public
  surface for inspecting captured sessions is the `agent` sub-command
  ([agent.md](agent.md)), not `hooks`.

`libra hooks claude <verb>` is the stable surface written into the
project `.claude/settings.json` by `libra agent enable --agent
claude-code`, and `libra hooks codex <verb>` (AG-19) is the stable
surface written into `$CODEX_HOME/hooks.json` by `libra agent enable
--agent codex`. Both record into the AgentTraces capture store
(`refs/libra/traces`); claude's historical routing into the
`refs/libra/intent` writer was retired by the Task A6.5 local capture
smoke, which requires installed hooks to surface in `libra agent
session/checkpoint list`. Codex additionally emits native sub-agent
boundaries (`subagent-start` / `subagent-end`).

Codex callbacks are advisory to the host agent. If a callback is invoked
outside a Libra repository, or capture cannot complete because repository
storage is unavailable, Libra records a non-sensitive retryable diagnostic on
the captured session and exits successfully. The affected callback may be
absent from capture output or lack its checkpoint, but capture availability
never fails the Codex task. A later Codex `Stop` can re-own the abandoned
coverage claim; inspect the state with `libra agent session show <session>`.

`libra hooks gemini <verb>` no longer ingests: gemini is uninstall-only
(AG-17), so stale hook configs installed before the demotion get an
actionable error pointing at `libra agent remove gemini` instead of
silently capturing data.

To enable capture, run `libra agent enable --agent <name>` for a
supported roster agent; this installs the provider hook config.
To disable capture, run `libra agent disable --agent <name>`.

## Providers and Events

Claude Code and Codex expose the same seven Claude-Code-style
lifecycle events (Codex additionally forwards `subagent-start` /
`subagent-end`):

| Event | Trigger |
|-------|---------|
| `session-start` | New session opened (provider startup or `/new` slash) |
| `prompt` | User submitted a prompt (UserPromptSubmit hook) |
| `tool-use` | Tool invocation (PreToolUse / PostToolUse hook) |
| `model-update` | Model swap inside a turn |
| `compaction` | Provider compacted its in-memory context |
| `stop` | User pressed Esc / hit the Stop button mid-turn |
| `session-end` | Session closed cleanly |

For Claude Code the `tool-use` verb is installed for **both** `PreToolUse`
(fired before a tool runs — an early liveness signal) and `PostToolUse`
(fired after the tool returns); both map to the `ToolUse` lifecycle event,
which refreshes the active session's liveness (last-event) state via the
capture/traces path but does **not** write a committed checkpoint —
checkpoints are materialized only at `stop` / `session-end`. No `Subagent*`
boundary event is registered for Claude (only Codex emits native sub-agent
boundaries), so Claude's on-disk sub-agent content is captured as `unresolved`.

Each event reads its provider-specific JSON payload from stdin, runs
the redaction pipeline (secrets / tokens / file content >256 KiB), and
appends an `AgentTraceEvent` JSONL record into the active session
store. Codex always acknowledges its callback after an advisory capture
failure; Claude keeps its validation failure status. Provider hooks must
never block on Libra-side processing.

## Options

`libra hooks` takes no flags besides the global ones (`--json`,
`--quiet`, etc.). The event kind is selected by the positional
sub-command path.

## Examples

```bash
# Claude Code SessionStart hook (typical hook config invocation)
libra hooks claude session-start

# Claude Code UserPromptSubmit hook
libra hooks claude prompt

# Claude Code PreToolUse / PostToolUse hook
libra hooks claude tool-use

# Claude Code Stop hook
libra hooks claude stop

# Claude Code SessionEnd hook
libra hooks claude session-end

# Codex SessionStart hook (AG-19 capture path)
libra hooks codex session-start

# Codex SubagentStart hook (native sub-agent boundary)
libra hooks codex subagent-start

# Gemini hooks are rejected with a hint (uninstall-only, AG-17):
#   libra hooks gemini <event>  ->  'libra agent remove gemini'
```

The Claude Code hook config installed by `libra agent enable --agent
claude` looks roughly like:

```json
{
  "hooks": {
    "SessionStart": [{"command": "libra hooks claude session-start"}],
    "UserPromptSubmit": [{"command": "libra hooks claude prompt"}],
    "PreToolUse": [{"command": "libra hooks claude tool-use"}],
    "PostToolUse": [{"command": "libra hooks claude tool-use"}],
    "Stop": [{"command": "libra hooks claude stop"}],
    "SessionEnd": [{"command": "libra hooks claude session-end"}]
  }
}
```

## Related Commands

- `libra agent enable` / `libra agent disable` — install / uninstall
  the provider hook config that invokes `libra hooks`.
- `libra agent status` — show capture coverage and the most recent
  hook timestamps.
- `libra agent session list` / `libra agent checkpoint list` — inspect
  events recorded by `libra hooks`.
- `libra agent doctor` — diagnose hook installation problems.

## Exit Codes

| Code | Meaning |
|------|---------|
| `0` | Codex event recorded, skipped, or acknowledged after an advisory capture failure; other providers may also return success after a normal skip |
| `1` | A non-Codex stdin payload failed schema validation; the hook caller may surface a warning, but provider hook flows treat this as non-fatal |
| `128` | A non-Codex hook encountered a fatal initialization error before its payload could be processed |
