# `libra investigate`

Run read-only, strict round-robin investigations with external agent CLIs (AG-23).

## Synopsis

```bash
libra investigate start --topic <text> --agent <slug>... [--max-turns <n>] [--quorum <n>]
libra investigate list [--json] [--limit <n>] [--cursor <token>]
libra investigate show <run_id> [--json]
libra investigate continue <run_id>
libra investigate cancel <run_id>
libra investigate clean [--run <run_id>] [--all]
libra investigate fix <run_id>
libra investigate attach <run_id> <file> [--json]
```

## Artifacts

A finished run objectizes its `findings.md` into the object store (the
manifest's `findings_oid` is a content-addressed, `object_index`-tagged,
doctor-repairable blob). `libra investigate attach <run_id> <file>` records an
external file on the run's audit chain with `provenance=manual`: the bytes are
redacted, objectized, and appended to the manifest's `manual_attach` list. It
never modifies findings or run state.

## Description

`libra investigate` drives a **strict round-robin** investigation of a
topic: one investigator runs at a time, in `--agent` order, each spawned
as a minimal read-only external CLI. This is deliberately *not* the
concurrent fan-in model of `libra review` — investigators never run in
parallel. The first-batch launchable investigators are `claude-code`,
`codex`, and `opencode`; any other agent slug is refused with an
actionable error before anything is spawned.

Every investigator runs in an **isolated workspace** — a mirror of the
repository materialized with ignore rules applied (gitignored secret
files such as `.env.test` never enter it) — with a minimal read-only CLI
invocation and an environment cleared down to a documented allowlist.
Investigators never run in the repository worktree itself.

Each turn collects the investigator's stance from its stdout (redacted),
appends it to the run's `stances` list and single-writer `findings.md`,
and advances the round-robin position (`next_agent_idx` / `turn` /
`completed_rounds`).

### Terminal states and pauses

A drive pass ends either **terminal** or **paused**:

- **terminal** (`terminal_state` recorded):
  - `quorum` — at least `--quorum` distinct investigators submitted a
    concluding stance (an investigator signals a conclusion by including
    the word "conclude" in its output);
  - `max_turns` — the turn budget was exhausted before quorum (read as
    "success" or "partial" informationally, per whether any findings were
    recorded);
  - `cancelled` — the run was cancelled (`investigate cancel` / Ctrl-C /
    SIGTERM, one shared cleanup path);
  - `timeout` — the run-level wall-clock budget (`max_turns × 120s`,
    capped at 3600s) was exceeded; fail-closed, every process/lock/
    workspace released.
- **paused** (`pending_turn` recorded, resumable with `continue`):
  - `stalled` — a successful turn produced no new findings (empty output);
  - `agent_failure` — the investigator failed to launch, exited non-zero,
    or hit its per-turn deadline.

`libra investigate continue <run_id>` resumes a paused run from its
pending turn. An OS-level run lock makes a concurrent `continue` on the
same run fail closed with an actionable error, so a run is never driven
by two processes at once.

Run state is persisted under `.libra/sessions/agent-runs/<run_id>/`:
`state.json` (round-robin state — `turn`, `next_agent_idx`, `stances`,
`pending_turn`, `quorum`, …), `manifest.json` (`kind: "investigate"`),
`findings.md`, and per-investigator
`reviewers/<slug>.stdout.redacted.log` / `.stderr.redacted.log`. All
persisted investigator output goes through the secret-redaction pipeline;
output is capped at 64 KiB per stream (a flooding investigator is
truncated with a marker).

### Untrusted seed and findings

The investigation topic is an **untrusted seed** (an issue link or
operator text). It — and every prior investigator stance injected as
context — is redacted and wrapped in explicit spotlighting delimiters
before it ever reaches an agent prompt, so it can never be mistaken for
instructions. Investigator findings are **untrusted free text**;
`libra investigate show` always strips ANSI/terminal control sequences
before rendering `findings.md` (and the topic), so a hostile investigator
cannot forge terminal output. The JSON output carries the same sanitized
rendering.

### Quorum and turns

- `--max-turns <n>` bounds the number of investigator turns (default 6).
- `--quorum <n>` is the number of **distinct** investigators that must
  submit a concluding stance to converge (default: the number of
  `--agent` given — a full consensus). A value larger than the agent
  count is clamped with a note.

### Pagination

`libra investigate list` uses the unified keyset pagination contract:
default `--limit 50`, capped at 500, ordered `started_at DESC, run_id
DESC`. The JSON envelope carries `schema_version`, `items`, `next_cursor`
(opaque — round-trip it verbatim), and `has_more`.

### `fix`

`libra investigate fix <run_id>` requires the internal AgentRuntime fix
bridge, which has not landed yet. It always fails with the stable error
code `LBR-AGENT-010` — it never fakes success. Because the topic is an
untrusted seed, a mutating fix additionally requires explicit approval;
once the bridge lands, an unapproved untrusted-seed mutation fails with
`LBR-AGENT-011`. Read-only findings stay available via
`libra investigate show`.

## Examples

```bash
# Start a round-robin investigation with one agent
libra investigate start --topic "why is startup slow" --agent codex

# Round-robin across two agents (strict, one at a time)
libra investigate start --topic "auth bug" --agent codex --agent claude-code

# Bound turns and require two concluding agents
libra investigate start --topic "memory leak" --agent codex --max-turns 8 --quorum 2

# List runs, then fetch the next page
libra investigate list
libra investigate list --limit 10 --cursor <token>

# Inspect one run (state, stances, sanitized findings)
libra investigate show <run_id>
libra investigate show <run_id> --json

# Resume a paused (stalled / agent-failure) run
libra investigate continue <run_id>

# Cancel a running investigation (same cleanup as Ctrl-C)
libra investigate cancel <run_id>

# Remove run directories
libra investigate clean --run <run_id>
libra investigate clean --all
```

## Concurrency

`investigate` and `review` share one run-level concurrency budget across the
repository. At most `agent.max_concurrent_runs` runs (default `2`) execute at
once; a run started while the budget is saturated waits in a queue (blocking
the foreground process — `Ctrl-C` cancels the wait). A run started when the
wait queue is already at its cap (10) is refused fail-closed with the stable
code `LBR-AGENT-014` (exit 128). Raise the limit with
`libra config set agent.max_concurrent_runs <N>`.

## Exit Status

- `0` — the run reached `quorum`, `max_turns`, `timeout`, or `cancelled`,
  or PAUSED (`stalled` / `agent_failure`); subcommands succeeded.
- non-zero — usage errors, a run that ended in the `error` terminal state,
  unknown run ids, a concurrent `continue` on a locked run, `fix`
  (stable code `LBR-AGENT-010`), or a full run queue (`LBR-AGENT-014`).

## See Also

- `libra review` — read-only concurrent agent code review (AG-22)
- `libra agent` — external-agent capture, checkpoints, and hooks
- `docs/development/commands/investigate.md` — architecture and security notes
