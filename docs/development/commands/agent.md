# Agent Command Development

`libra agent` is an intentionally different external-agent capture extension,
not a Git-compatible command.

The active development contract, backlog, and compatibility guardrails live in
[`../tracing/agent.md`](../tracing/agent.md). Keep this file as the command
development index entry so `docs/development/commands/README.md` can list every
public CLI command without duplicating the Agent planning document.

## Deferred / Non-goal parity

The following external-agent parity surfaces are decided **non-goals** for the
current wave. Each is recorded — with its handling and restart condition — in
the 「还未实现的功能」 table of [`../tracing/agent.md`](../tracing/agent.md)
(the canonical Agent contract); they are surfaced to users in
[`docs/commands/agent.md`](../../commands/agent.md) and in the `agent` row of
[`COMPATIBILITY.md`](../../../COMPATIBILITY.md):

1. **`agent add`/`remove` `--local-dev` / `--force`** — unpublished; canonical
   `status` / `enable` / `disable` (+ `add` / `remove` aliases) only. If
   implemented, each must hang on both the canonical verb and its alias.
2. **Provider-specific transcript compaction/reassemble trait** — deferred parity
   on top of the landed manifest-relative chunking (no provider-specific
   compactor yet).
3. **Optional capability traits** (`ProtectedFilesProvider`, `TranscriptCompactor`,
   `HookResponseWriter`, `RestoredSessionPathResolver`, …) beyond the landed
   `DeclaredAgentCaps` set — no public behavior yet.
4. **External-RPC method family beyond the v2 `info`/capability gate** —
   undeclared capabilities stay fail-closed.
5. **Non-first-batch supported roster** — `gemini` / `cursor` / `copilot` /
   `factory-ai` stay `supported=false` (unsupported, not hook-installable, not
   launchable); the first batch is `claude-code` / `codex` / `opencode`. Pinned
   by `tests/command/agent_roster_test.rs::agent_roster_surface` and
   `tests/compat/agent_capability_matrix_pin.rs`.
