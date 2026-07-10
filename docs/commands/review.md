# `libra review`

Run read-only code reviews with external agent CLIs (AG-22).

## Synopsis

```bash
libra review --agent <slug>... [--since <rev>] [--checkpoint <id>] [--json]
libra review list [--json] [--limit <n>] [--cursor <token>]
libra review show <run_id> [--json]
libra review cancel <run_id>
libra review clean [--run <run_id>] [--all]
libra review attach <run_id> <file> [--json]
```

## Artifacts

A finished run objectizes its `findings.md` into the object store: the run
manifest's `findings_oid` is a content-addressed blob, `object_index`-tagged
so cloud sync and retention track it and `libra agent doctor` can repair it if
it goes missing. `libra review attach <run_id> <file>` records an external
file on the run's audit chain with `provenance=manual` — the bytes are
redacted, objectized the same way, and appended to the manifest's
`manual_attach` list. Attach never modifies findings or run state.

## Description

`libra review` fans a fixed review prompt out to one or more external
reviewer CLIs and records their findings as an auditable run. The
first-batch launchable reviewers are `claude-code`, `codex`, and
`opencode`; any other agent slug is refused with an actionable error
before anything is spawned.

Every reviewer runs in an **isolated workspace** — a mirror of the
repository materialized with ignore rules applied (gitignored secret
files such as `.env.test` never enter it) — with a minimal read-only
CLI invocation and an environment cleared down to a documented
allowlist. Reviewers never run in the repository worktree itself.

A run blocks in the foreground until every reviewer reaches an outcome
and ends in exactly one of five terminal states: `success`, `error`,
`cancelled`, `timeout`, or `partial`. Pressing Ctrl-C (or sending
SIGTERM) cancels the run through the same cleanup path as
`libra review cancel`: reviewer process trees are killed, reader tasks
drained, the workspace released, and the run stamped `cancelled`.

Run state is persisted under `.libra/sessions/agent-runs/<run_id>/`:
`state.json`, `manifest.json`, `findings.md`, and per-reviewer
`reviewers/<slug>.stdout.redacted.log` / `.stderr.redacted.log`. All
persisted reviewer output goes through the secret-redaction pipeline;
reviewer output is capped at 64 KiB per stream (a flooding reviewer is
truncated with a marker and never blocks its siblings).

Reviewer findings are **untrusted free text**. `libra review show`
always strips ANSI/terminal control sequences before rendering
`findings.md`, so a hostile reviewer cannot forge terminal output, and
the JSON output carries the same sanitized rendering.

### Scope selection

The recorded `target_scope` labels what the reviewers were asked to
review:

- default: `HEAD~1..HEAD` (the last commit's changes);
- `--since <rev>`: `<rev>..HEAD`;
- `--checkpoint <id>`: `checkpoint:<id>` (an agent checkpoint from
  `libra agent checkpoint list`). **Not implemented yet** — the command
  fails closed instead of silently reviewing the current worktree under
  a checkpoint label; use `libra agent checkpoint show <id>` to inspect
  the captured state directly.

### Pagination

`libra review list` uses the unified keyset pagination contract:
default `--limit 50`, capped at 500, ordered `created_at DESC, run_id
DESC`. The JSON envelope carries `schema_version`, `items`,
`next_cursor` (opaque — round-trip it verbatim), and `has_more`.

### `--fix`

`libra review --fix` requires the internal AgentRuntime fix bridge,
which has not landed yet. It always fails with the stable error code
`LBR-AGENT-010` — it never fakes success. Read-only findings stay
available via `libra review show`.

## Examples

```bash
# Review the last commit with one reviewer
libra review --agent codex

# Fan the same review out to two reviewers concurrently
libra review --agent codex --agent claude-code

# Review everything since a revision
libra review --agent codex --since v1.2.0

# Checkpoint-scoped review fails closed until checkpoint
# materialization lands (see --checkpoint above)
libra review --agent codex --checkpoint <checkpoint_id>

# Structured run result (terminal state, per-reviewer outcomes)
libra review --agent codex --json

# List runs, then fetch the next page
libra review list
libra review list --limit 10 --cursor <token>

# Inspect one run (state, manifest summary, sanitized findings)
libra review show <run_id>
libra review show <run_id> --json

# Cancel a running review (same cleanup as Ctrl-C)
libra review cancel <run_id>

# Remove run directories
libra review clean --run <run_id>
libra review clean --all
```

## Concurrency

`review` and `investigate` share one run-level concurrency budget across the
repository. At most `agent.max_concurrent_runs` runs (default `2`) execute at
once; a run started while the budget is saturated waits in a queue (blocking
the foreground process — `Ctrl-C` cancels the wait and advances the queue for
others). If the wait queue is already at its cap (10), the new run is refused
fail-closed with the stable code `LBR-AGENT-014` (exit 128) rather than
overrunning the budget. Raise the limit with
`libra config set agent.max_concurrent_runs <N>`.

## Exit Status

- `0` — the run reached `success`, `partial`, `timeout`, or `cancelled`
  (the terminal state is reported in the output); subcommands succeeded.
- non-zero — usage errors, a run that ended in the `error` terminal
  state, unknown run ids, `--fix` (stable code `LBR-AGENT-010`), or a full
  run queue (stable code `LBR-AGENT-014`).

## See Also

- `libra agent` — external-agent capture, checkpoints, and hooks
- `docs/development/commands/review.md` — architecture and security notes
